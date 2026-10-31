//! Surprise quizzes: an AI-written mental math question or a "guess this colour" swatch.

use std::{
    io::Cursor,
    ops::Range,
    time::{Duration, Instant, SystemTime},
};

use color_eyre::{Result, eyre::eyre};
use rand::{RngExt, seq::IndexedRandom};
use twilight_model::{
    channel::Message,
    http::attachment::Attachment,
    id::{
        Id,
        marker::{GuildMarker, UserMarker},
    },
    util::Timestamp,
};

use crate::{
    ai::openrouter::{ChatRequest, Message as ChatMessage, Role},
    bot::Bot,
    db,
    messages::Response,
};

/// Each message has a 1 in this many chance of starting each kind of quiz.
const QUIZ_ODDS: u32 = 500;
const MATH_TIME: Duration = Duration::from_secs(30);
const COLOR_TIME: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(r, g, b) = self;
        write!(f, "`rgb({r}, {g}, {b})` or `#{r:02x}{g:02x}{b:02x}`")
    }
}

impl Rgb {
    fn random() -> Self {
        let mut rng = rand::rng();
        Self(rng.random(), rng.random(), rng.random())
    }

    /// A 640x360 PNG filled with this colour.
    fn swatch(self) -> Result<Vec<u8>> {
        const WIDTH: u32 = 640;
        const HEIGHT: u32 = 360;
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(Cursor::new(&mut png), WIDTH, HEIGHT);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        let pixels = [self.0, self.1, self.2].repeat((WIDTH * HEIGHT) as usize);
        encoder.write_header()?.write_image_data(&pixels)?;
        Ok(png)
    }

    /// Accepts `#rrggbb` guesses within a total channel distance of 25.
    fn is_close(self, guess: &str) -> bool {
        let Some(hex) = guess.trim().strip_prefix('#').filter(|hex| hex.len() == 6) else {
            return false;
        };
        let channel = |i: usize| hex.get(i..i + 2).and_then(|byte| u8::from_str_radix(byte, 16).ok());
        let (Some(r), Some(g), Some(b)) = (channel(0), channel(2), channel(4)) else {
            return false;
        };
        self.0.abs_diff(r) as u32 + self.1.abs_diff(g) as u32 + self.2.abs_diff(b) as u32 <= 25
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Quiz {
    Math { question: String, answer: f64 },
    Color(Rgb),
}

impl Quiz {
    const fn time_limit(&self) -> Duration {
        match self {
            Self::Math { .. } => MATH_TIME,
            Self::Color(_) => COLOR_TIME,
        }
    }

    fn is_correct(&self, guess: &str) -> bool {
        match self {
            // One decimal place of tolerance.
            Self::Math { answer, .. } => {
                guess.trim().parse::<f64>().is_ok_and(|guess| (answer - guess).abs() <= 0.1 + 1e-9)
            }
            Self::Color(rgb) => rgb.is_close(guess),
        }
    }
}

/// A quiz waiting for an answer in a channel.
#[derive(Debug)]
pub struct PendingQuiz {
    quiz: Quiz,
    user_id: Id<UserMarker>,
    started_at: Instant,
}

enum Outcome {
    Expired(PendingQuiz),
    Solved(PendingQuiz),
}

/// Checks the message against the channel's pending quiz. Either outcome ends the quiz.
pub async fn answer(bot: &Bot, msg: &Message) -> Option<Response> {
    let outcome = {
        let mut quizzes = bot.quizzes.lock().unwrap();
        let pending = quizzes.get(&msg.channel_id)?;
        if pending.started_at.elapsed() > pending.quiz.time_limit() {
            Outcome::Expired(quizzes.remove(&msg.channel_id)?)
        } else if pending.quiz.is_correct(&msg.content) {
            Outcome::Solved(quizzes.remove(&msg.channel_id)?)
        } else {
            return None;
        }
    };
    match outcome {
        Outcome::Expired(PendingQuiz { quiz: Quiz::Math { answer, .. }, user_id, .. }) => {
            if let Some(guild) = msg.guild_id {
                time_out(bot, guild, user_id).await;
            }
            Some(Response::text(format!(
                "<@{user_id}> Time's up! The answer was `{answer:.1}`. You've been timed out for 1 minute."
            )))
        }
        Outcome::Expired(PendingQuiz { quiz: Quiz::Color(rgb), user_id, .. }) => {
            Some(Response::text(format!("<@{user_id}> Time's up! The color was {rgb}.")))
        }
        Outcome::Solved(pending) => {
            let bonus = rand::random_range(250..1000);
            let level = db::award_xp(&bot.db, msg.author.id.get(), &msg.author.name, bonus, true)
                .await
                .inspect_err(|error| tracing::error!("Failed to award quiz XP: {error:?}"))
                .ok()?;
            let earned = match level {
                Some(level) => format!("You earned {bonus} XP and leveled up to level {level}!"),
                None => format!("You earned {bonus} XP!"),
            };
            let author = msg.author.id;
            let text = match pending.quiz {
                Quiz::Math { .. } => format!(
                    "<@{author}> Correct! Well done. {earned} (Solved in {:.3}s)",
                    pending.started_at.elapsed().as_secs_f64()
                ),
                Quiz::Color(rgb) => format!("<@{author}> Correct! The color was {rgb}. {earned}"),
            };
            Some(Response::text(text).reply())
        }
    }
}

async fn time_out(bot: &Bot, guild: Id<GuildMarker>, user: Id<UserMarker>) {
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let Ok(until) = Timestamp::from_secs(now.as_secs() as i64 + 60) else {
        return;
    };
    if let Err(error) = bot.http.update_guild_member(guild, user).communication_disabled_until(Some(until)).await {
        tracing::error!("Failed to time out user: {error:?}");
    }
}

/// Occasionally starts a quiz in the message's channel when none is running.
pub async fn maybe_start(bot: &Bot, msg: &Message) -> Option<Response> {
    let busy = || bot.quizzes.lock().unwrap().contains_key(&msg.channel_id);
    let quiz = if bot.ai.is_some() && rand::random_ratio(1, QUIZ_ODDS) && !busy() {
        let (question, answer) = math_question(bot)
            .await
            .inspect_err(|error| tracing::error!("Failed to generate math test: {error:?}"))
            .ok()?;
        Quiz::Math { question, answer }
    } else if rand::random_ratio(1, QUIZ_ODDS) && !busy() {
        Quiz::Color(Rgb::random())
    } else {
        return None;
    };
    let response = match &quiz {
        Quiz::Math { question, .. } => Response::text(format!(
            "<@{}> **MATH TEST TIME!** Solve this in 30 seconds:\n`{question}`\n(Answer to 1 decimal place)",
            msg.author.id
        )),
        Quiz::Color(rgb) => {
            let swatch = rgb
                .swatch()
                .inspect_err(|error| tracing::error!("Failed to generate color quiz image: {error:?}"))
                .ok()?;
            Response::text("**COLOR QUIZ TIME!** Guess this color in 60 seconds!\nFormat: `#RRGGBB`")
                .attachments(vec![Attachment::from_bytes("color.png".into(), swatch, 1)])
        }
    };
    // Another message may have started a quiz while the question was being generated.
    let mut quizzes = bot.quizzes.lock().unwrap();
    if quizzes.contains_key(&msg.channel_id) {
        return None;
    }
    quizzes.insert(msg.channel_id, PendingQuiz { quiz, user_id: msg.author.id, started_at: Instant::now() });
    Some(response)
}

const MATH_PROMPTS: [&str; 3] = [
    "Generate a simple mental math expression that can be solved in your head. Use basic operations like addition, subtraction, multiplication, or division with small numbers (prefer numbers under 20, maximum 100). Output ONLY the mathematical expression, nothing else. Examples: {examples}",
    "Create an easy math problem with 2-4 numbers that someone can calculate mentally. Use addition, subtraction, multiplication, or simple division. Keep numbers small and friendly (single or double digits preferred). Output ONLY the expression. Examples: {examples}",
    "Write a simple arithmetic calculation using small, friendly numbers that can be computed without a calculator. Stick to basic operations (+, -, *, /). Output ONLY the math expression. Examples: {examples}",
];

/// Four random example expressions so each prompt nudges the model somewhere new.
fn math_examples() -> String {
    use rand::{random_bool, random_range};
    let division = |divisors: Range<i32>, results: Range<i32>| {
        let divisor = random_range(divisors);
        format!("{} / {divisor}", divisor * random_range(results))
    };
    let examples = [
        format!("{} + {}", random_range(5..50), random_range(5..50)),
        format!("{} * {}", random_range(3..15), random_range(3..15)),
        if random_bool(0.5) {
            division(2..13, 5..20)
        } else {
            format!("{} - {}", random_range(30..100), random_range(5..30))
        },
        if random_bool(0.3) {
            let [a, b, c] = [(); 3].map(|()| random_range(3..20));
            match random_range(0..3) {
                0 => format!("{a} + {b} + {c}"),
                1 => format!("{} - {b} + {c}", a + b + c),
                _ => format!("{a} + {b} - {c}"),
            }
        } else {
            division(2..11, 5..15)
        },
    ];
    examples.join(", ")
}

/// Asks the model for a fresh expression, retrying until one evaluates and hasn't been asked before.
async fn math_question(bot: &Bot) -> Result<(String, f64)> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    for attempt in 1..=5 {
        let prompt = MATH_PROMPTS.choose(&mut rand::rng()).unwrap().replace("{examples}", &math_examples());
        let request = ChatRequest {
            model: bot.config.openrouter_model.clone(),
            messages: vec![
                ChatMessage::new(
                    Role::System,
                    "You are a math expression generator for mental math challenges. Generate simple arithmetic \
                     expressions that can be solved mentally. Output ONLY the mathematical expression with numbers \
                     and operators, no explanations, no greetings, no additional text.",
                ),
                ChatMessage::new(Role::User, prompt),
            ],
            temperature: Some(0.9),
            max_tokens: 50,
            ..Default::default()
        };
        let question = match ai.complete(&request, Duration::from_secs(30)).await {
            Ok(question) => question,
            Err(error) => {
                tracing::warn!("Math question generation failed on attempt {attempt}: {error:?}");
                continue;
            }
        };
        let answer = match fasteval::ez_eval(&question, &mut fasteval::EmptyNamespace) {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!("Invalid math expression generated '{question}': {error:?}");
                continue;
            }
        };
        if db::remember_math_question(&bot.db, &question, answer).await? {
            return Ok((question, answer));
        }
        tracing::debug!("Question '{question}' already exists, retrying");
    }
    Err(eyre!("Failed to generate a valid math question after 5 attempts"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_guesses_within_25_are_close() {
        let quiz = Quiz::Color(Rgb(200, 200, 200));
        assert!(quiz.is_correct("#c8c8c8"));
        assert!(quiz.is_correct(" #bec8ca "));
        assert!(quiz.is_correct("#b3c8c8"));
        assert!(!quiz.is_correct("#aec8c8"));
        assert!(!quiz.is_correct("c8c8c8"));
        assert!(!quiz.is_correct("#c8c8é"));
    }

    #[test]
    fn math_answers_allow_one_decimal_of_slack() {
        let quiz = Quiz::Math { question: "10 / 4".into(), answer: 2.5 };
        assert!(quiz.is_correct("2.5"));
        assert!(quiz.is_correct("2.6"));
        assert!(!quiz.is_correct("2.7"));
        assert!(!quiz.is_correct("two"));
    }

    #[test]
    fn swatch_is_a_png_and_colours_display_both_ways() {
        assert!(Rgb(1, 2, 3).swatch().unwrap().starts_with(b"\x89PNG"));
        assert_eq!(Rgb(255, 0, 16).to_string(), "`rgb(255, 0, 16)` or `#ff0010`");
    }

    #[test]
    fn examples_are_four_expressions_that_evaluate() {
        let examples = math_examples();
        let parts: Vec<_> = examples.split(", ").collect();
        assert_eq!(parts.len(), 4);
        for part in parts {
            fasteval::ez_eval(part, &mut fasteval::EmptyNamespace).unwrap();
        }
    }
}
