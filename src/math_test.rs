use crate::database::MathQuestion;
use color_eyre::Result;
use openrouter_api::{OpenRouterClient, types::chat::{ChatCompletionRequest, Message, MessageContent}};
use rand::Rng;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde_rusqlite::from_rows;

const MATH_PROMPTS: [&str; 3] = [
    "Generate a simple mental math expression that can be solved in your head. Use basic operations like addition, subtraction, multiplication, or division with small numbers (prefer numbers under 20, maximum 100). Output ONLY the mathematical expression, nothing else. Examples: {ex1}, {ex2}, {ex3}, {ex4}",
    "Create an easy math problem with 2-4 numbers that someone can calculate mentally. Use addition, subtraction, multiplication, or simple division. Keep numbers small and friendly (single or double digits preferred). Output ONLY the expression. Examples: {ex1}, {ex2}, {ex3}, {ex4}",
    "Write a simple arithmetic calculation using small, friendly numbers that can be computed without a calculator. Stick to basic operations (+, -, *, /). Output ONLY the math expression. Examples: {ex1}, {ex2}, {ex3}, {ex4}"
];

pub struct MathTest {
    pub question: String,
    pub answer: f64,
}

impl MathTest {
    pub async fn generate(
        openai_api_key: &str,
        model: &str,
        db: &Pool<SqliteConnectionManager>,
        rng: &mut impl Rng,
    ) -> Result<Self> {
        let client = OpenRouterClient::from_api_key(openai_api_key)?;

        // Retry loop to avoid recursion
        for attempt in 0..5 {
            // Generate random example questions to show the AI
            // Example 1: Addition
            let ex1_a = rng.gen_range(5..50);
            let ex1_b = rng.gen_range(5..50);
            let ex1 = format!("{} + {}", ex1_a, ex1_b);

            // Example 2: Multiplication
            let ex2_a = rng.gen_range(3..15);
            let ex2_b = rng.gen_range(3..15);
            let ex2 = format!("{} * {}", ex2_a, ex2_b);

            // Example 3: Division or Subtraction
            let ex3 = if rng.gen_bool(0.5) {
                let divisor = rng.gen_range(2..13);
                let result = rng.gen_range(5..20);
                format!("{} / {}", divisor * result, divisor)
            } else {
                let ex3_a = rng.gen_range(30..100);
                let ex3_b = rng.gen_range(5..30);
                format!("{} - {}", ex3_a, ex3_b)
            };

            // Example 4: Multi-operation or simple operation
            let ex4 = if rng.gen_bool(0.3) {
                // Multi-operation
                let a = rng.gen_range(3..20);
                let b = rng.gen_range(3..20);
                let c = rng.gen_range(3..20);
                match rng.gen_range(0..3) {
                    0 => format!("{} + {} + {}", a, b, c),
                    1 => format!("{} - {} + {}", a + b + c, b, c),
                    _ => format!("{} + {} - {}", a, b, c),
                }
            } else {
                // Simple division
                let divisor = rng.gen_range(2..11);
                let result = rng.gen_range(5..15);
                format!("{} / {}", divisor * result, divisor)
            };

            // Select a random prompt template and fill in the example questions
            let prompt_template = MATH_PROMPTS[rng.gen_range(0..MATH_PROMPTS.len())];
            let user_prompt = prompt_template
                .replace("{ex1}", &ex1)
                .replace("{ex2}", &ex2)
                .replace("{ex3}", &ex3)
                .replace("{ex4}", &ex4);

            let system_prompt = "You are a math expression generator for mental math challenges. \
                Generate simple arithmetic expressions that can be solved mentally. \
                Output ONLY the mathematical expression with numbers and operators, no explanations, no greetings, no additional text.";

            // Generate question using AI
            let request = ChatCompletionRequest {
                model: model.to_string(),
                messages: vec![
                    Message {
                        role: "system".to_string(),
                        content: MessageContent::Text(system_prompt.to_string()),
                        ..Default::default()
                    },
                    Message {
                        role: "user".to_string(),
                        content: MessageContent::Text(user_prompt),
                        ..Default::default()
                    },
                ],
                temperature: Some(0.9),
                max_tokens: Some(50),
                ..Default::default()
            };

            let question = match client.chat()?.chat_completion(request).await {
                Ok(response) => {
                    match response.choices.first() {
                        Some(choice) => match &choice.message.content {
                            MessageContent::Text(text) => text.trim().to_string(),
                            MessageContent::Parts(_) => {
                                tracing::error!("Unexpected multipart content in response");
                                continue;
                            }
                        },
                        None => {
                            tracing::error!("No response from API");
                            continue;
                        }
                    }
                },
                Err(e) => {
                    tracing::error!("AI generation failed on attempt {}: {:?}", attempt + 1, e);
                    continue;
                }
            };

            // Validate the expression with fasteval
            let mut ns = fasteval::EmptyNamespace;
            let answer = match fasteval::ez_eval(&question, &mut ns) {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Invalid math expression generated '{}': {:?}", question, e);
                    continue;
                }
            };

            // Check if this question already exists in the database
            let conn = db.get()?;
            let mut stmt = conn.prepare("SELECT * FROM math_question WHERE question = ?")?;
            let existing: Result<Vec<MathQuestion>, _> = from_rows(stmt.query([&question])?).collect();

            // If question exists, try again
            if existing.is_ok() && !existing.as_ref().unwrap().is_empty() {
                tracing::debug!("Question '{}' already exists, retrying", question);
                continue;
            }

            // Store in database - use manual INSERT to let SQLite handle autoincrement
            conn.execute(
                "INSERT INTO math_question (question, answer) VALUES (?, ?)",
                rusqlite::params![&question, &answer],
            )?;

            return Ok(MathTest { question, answer });
        }

        // If all attempts failed, return error
        Err(color_eyre::eyre::eyre!("Failed to generate valid math question after 5 attempts"))
    }

    pub fn validate_answer(&self, user_answer: &str) -> bool {
        // Parse user answer
        let user_answer = match user_answer.trim().parse::<f64>() {
            Ok(v) => v,
            Err(_) => return false,
        };

        // Check if answer is within 0.1 tolerance (1 decimal place)
        (self.answer - user_answer).abs() <= 0.1
    }
}
