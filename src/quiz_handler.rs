use crate::{
    color_quiz::ColorQuiz,
    database::User,
    math_test::MathTest,
    structs::{Command, PendingColorTest, PendingMathTest, State},
    utils::levels::xp_required_for_level,
};
use rand::Rng;
use serde_rusqlite::from_row;
use std::sync::Arc;
use tokio::sync::MutexGuard;
use tokio::time::Instant as TokioInstant;
use twilight_http::Client as HttpClient;
use twilight_model::{gateway::payload::incoming::MessageCreate, http::attachment::Attachment, id::Id};

async fn apply_timeout(http: &HttpClient, guild_id: Id<twilight_model::id::marker::GuildMarker>, user_id: u64) {
    let timeout_until = twilight_model::util::Timestamp::from_secs(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 60,
    )
    .unwrap();

    match http
        .update_guild_member(guild_id, Id::new(user_id))
        .communication_disabled_until(Some(timeout_until))
    {
        Ok(req) => {
            if let Err(e) = req.exec().await {
                tracing::error!("Failed to execute timeout: {:?}", e);
            }
        }
        Err(e) => {
            tracing::error!("Failed to timeout user: {:?}", e);
        }
    }
}

fn award_quiz_xp(
    db: &r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager>,
    rng: &mut rand::rngs::ThreadRng,
    user_id: u64,
    user_name: &str,
) -> color_eyre::Result<(i32, Option<i32>)> {
    let bonus_xp = rng.gen_range(250..1000);

    let mut statement = db.prepare("SELECT * FROM user WHERE id = ?").unwrap();
    if let Ok(mut user) = statement.query_one([user_id.to_string()], |row| {
        from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
    }) {
        let level = user.level;
        let xp_required = xp_required_for_level(level);
        let new_xp = user.xp + bonus_xp;
        user.name = user_name.to_string();

        if new_xp >= xp_required {
            let new_level = level + 1;
            user.level = new_level;
            user.xp = new_xp - xp_required;
            user.update_sync(db)?;
            return Ok((bonus_xp, Some(new_level)));
        } else {
            user.xp = new_xp;
            user.update_sync(db)?;
        }
    }

    Ok((bonus_xp, None))
}

pub async fn handle_math_quiz(
    msg: &MessageCreate,
    locked_state: &mut MutexGuard<'_, State>,
    http: &Arc<HttpClient>,
) -> Option<Command> {
    let pending_test = locked_state.pending_math_tests.get(&msg.channel_id.get())?;
    let elapsed = pending_test.started_at.elapsed();
    let question = pending_test.question.clone();
    let answer = pending_test.answer;
    let original_user_id = pending_test.user_id;

    if elapsed.as_secs() > 30 {
        locked_state.pending_math_tests.remove(&msg.channel_id.get());

        if let Some(guild_id) = msg.guild_id {
            apply_timeout(http, guild_id, original_user_id).await;
        }

        return Some(Command::text(format!(
            "<@{}> Time's up! The answer was `{:.1}`. You've been timed out for 1 minute.",
            original_user_id, answer
        )));
    }

    if (MathTest { question, answer }).validate_answer(msg.content.trim()) {
        locked_state.pending_math_tests.remove(&msg.channel_id.get());

        let db = locked_state.db.get().ok()?;
        let (bonus_xp, new_level) =
            award_quiz_xp(&db, &mut locked_state.rng, msg.author.id.get(), &msg.author.name).ok()?;

        let duration_secs = elapsed.as_secs_f64();

        return Some(
            Command::text(if let Some(level) = new_level {
                format!(
                    "<@{}> Correct! Well done. You earned {} XP and leveled up to level {}! (Solved in {:.3}s)",
                    msg.author.id.get(),
                    bonus_xp,
                    level,
                    duration_secs
                )
            } else {
                format!(
                    "<@{}> Correct! Well done. You earned {} XP! (Solved in {:.3}s)",
                    msg.author.id.get(),
                    bonus_xp,
                    duration_secs
                )
            })
            .reply(),
        );
    }

    None
}

pub async fn handle_color_quiz(
    msg: &MessageCreate,
    locked_state: &mut MutexGuard<'_, State>,
    http: &Arc<HttpClient>,
) -> Option<Command> {
    let pending_test = locked_state.pending_color_tests.get(&msg.channel_id.get())?;
    let elapsed = pending_test.started_at.elapsed();
    let (r, g, b) = (pending_test.r, pending_test.g, pending_test.b);
    let original_user_id = pending_test.user_id;

    if elapsed.as_secs() > 60 {
        locked_state.pending_color_tests.remove(&msg.channel_id.get());

        return Some(Command::text(format!(
            "<@{}> Time's up! The color was `rgb({}, {}, {})` or `#{:02x}{:02x}{:02x}`.",
            original_user_id, r, g, b, r, g, b
        )));
    }

    let quiz = ColorQuiz { r, g, b };
    if quiz.validate_answer(msg.content.trim()) {
        locked_state.pending_color_tests.remove(&msg.channel_id.get());

        let db = locked_state.db.get().ok()?;
        let (bonus_xp, new_level) =
            award_quiz_xp(&db, &mut locked_state.rng, msg.author.id.get(), &msg.author.name).ok()?;

        return Some(
            Command::text(if let Some(level) = new_level {
                format!(
                    "<@{}> Correct! The color was `rgb({}, {}, {})` or `#{:02x}{:02x}{:02x}`. You earned {} XP and leveled up to level {}!",
                    msg.author.id.get(), r, g, b, r, g, b, bonus_xp, level
                )
            } else {
                format!(
                    "<@{}> Correct! The color was `rgb({}, {}, {})` or `#{:02x}{:02x}{:02x}`. You earned {} XP!",
                    msg.author.id.get(), r, g, b, r, g, b, bonus_xp
                )
            })
            .reply(),
        );
    }

    None
}

pub async fn trigger_math_quiz(msg: &MessageCreate, locked_state: &mut MutexGuard<'_, State>) -> Option<Command> {
    if locked_state.config.openai_api_key.is_none()
        || locked_state.rng.gen_range(0..500) != 42
        || locked_state.pending_math_tests.contains_key(&msg.channel_id.get())
        || locked_state.pending_color_tests.contains_key(&msg.channel_id.get())
    {
        return None;
    }

    let api_key = locked_state.config.openai_api_key.clone().unwrap();
    let db_clone = locked_state.db.clone();
    let mut new_rng = rand::thread_rng();

    match MathTest::generate(&api_key, &db_clone, &mut new_rng).await {
        Ok(test) => {
            let pending = PendingMathTest {
                user_id: msg.author.id.get(),
                channel_id: msg.channel_id.get(),
                question: test.question.clone(),
                answer: test.answer,
                started_at: TokioInstant::now(),
            };

            locked_state.pending_math_tests.insert(msg.channel_id.get(), pending);

            Some(Command::text(format!(
                "<@{}> **MATH TEST TIME!** Solve this in 30 seconds:\n`{}`\n(Answer to 1 decimal place)",
                msg.author.id.get(),
                test.question
            )))
        }
        Err(e) => {
            tracing::error!("Failed to generate math test: {:?}", e);
            None
        }
    }
}

pub async fn trigger_color_quiz(msg: &MessageCreate, locked_state: &mut MutexGuard<'_, State>) -> Option<Command> {
    if locked_state.rng.gen_range(0..500) != 42
        || locked_state.pending_color_tests.contains_key(&msg.channel_id.get())
        || locked_state.pending_math_tests.contains_key(&msg.channel_id.get())
    {
        return None;
    }

    let quiz = ColorQuiz::generate(&mut locked_state.rng);

    match quiz.generate_image() {
        Ok(image_data) => {
            let pending = PendingColorTest {
                user_id: msg.author.id.get(),
                channel_id: msg.channel_id.get(),
                r: quiz.r,
                g: quiz.g,
                b: quiz.b,
                started_at: TokioInstant::now(),
            };

            locked_state.pending_color_tests.insert(msg.channel_id.get(), pending);

            Some(
                Command::text("**COLOR QUIZ TIME!** Guess this color in 60 seconds!\nFormat: `#RRGGBB`")
                    .attachments(vec![Attachment::from_bytes("color.png".to_string(), image_data, 1)]),
            )
        }
        Err(e) => {
            tracing::error!("Failed to generate color quiz image: {:?}", e);
            None
        }
    }
}
