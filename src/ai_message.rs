use std::sync::Arc;

use color_eyre::Result;
use parking_lot::Mutex;
use rig::prelude::*;
use rig::{
    completion::{Prompt, ToolDefinition},
    providers,
    tool::Tool,
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_rusqlite::from_row;

use crate::database;

#[derive(Deserialize)]
struct OperationArgs {
    x: i32,
    y: i32,
}

#[derive(Debug, thiserror::Error)]
#[error("Math error")]
struct MathError;

#[derive(Deserialize, Serialize)]
struct Adder;
impl Tool for Adder {
    const NAME: &'static str = "add";
    type Error = MathError;
    type Args = OperationArgs;
    type Output = i32;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "add".to_string(),
            description: "Add x and y together".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "The first number to add"
                    },
                    "y": {
                        "type": "number",
                        "description": "The second number to add"
                    }
                }
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        println!("[tool-call] Adding {} and {}", args.x, args.y);
        let result = args.x + args.y;
        Ok(result)
    }
}

#[derive(Deserialize, Serialize)]
struct Subtract;

impl Tool for Subtract {
    const NAME: &'static str = "subtract";
    type Error = MathError;
    type Args = OperationArgs;
    type Output = i32;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(json!({
            "name": "subtract",
            "description": "Subtract y from x (i.e.: x - y)",
            "parameters": {
                "type": "object",
                "properties": {
                    "x": {
                        "type": "number",
                        "description": "The number to subtract from"
                    },
                    "y": {
                        "type": "number",
                        "description": "The number to subtract"
                    }
                }
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        println!("[tool-call] Subtracting {} from {}", args.y, args.x);
        let result = args.x - args.y;
        Ok(result)
    }
}
#[derive(Serialize)]
struct Memory(#[serde(skip)] Arc<Mutex<Connection>>, u64);

#[derive(Deserialize, Serialize, Debug)]
struct MemoryArgs {
    memory_name: String,
    memory_content: String,
}
impl Tool for Memory {
    const NAME: &'static str = "memory";
    type Error = MathError;
    type Args = MemoryArgs;
    type Output = ();

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(json!({
            "name": "memory",
            "description": "Save memories about the user you are responding too, if the memory already exists itll be overwritten.",
            "parameters": {
                "type": "object",
                "properties": {
                    "memory_name": {
                        "type": "string",
                        "description": "The name of the memory"
                    },
                    "memory_content": {
                        "type": "string",
                        "description": "The content of the memory"
                    }
                }
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut conn = self.0.lock();
        println!("Tool called {:?}", args);
        conn.execute(
            "INSERT OR REPLACE INTO memory (user_id, key, content) VALUES (?, ?, ?)",
            params![self.1, args.memory_name, args.memory_content],
        )
        .unwrap();
        Ok(())
    }
}

pub async fn main(database: Arc<Mutex<Connection>>, user_id: u64, message: &str, context: &str) -> Result<String> {
    // Create OpenAI client
    let openai_client = providers::openai::Client::from_env();


    let mut memory = String::new();

    {
        let db = database.lock();
        let mut stm = db.prepare("SELECT * FROM memory WHERE user_id = ?")?;
        let mut rows = stm
            .query_map(params![user_id], |row| {
                from_row::<database::Memory>(row).map_err(|err| rusqlite::Error::InvalidColumnName(err.to_string()))
            })?
           .flatten();
        while let Some(row) = rows.next() {
            memory.push_str(&format!("{}: {}\n", row.key, row.content));
        }
    }


    // Create agent with a single context prompt and two tools
    let calculator_agent = openai_client
        .agent(providers::openai::GPT_4O_MINI)
        .preamble(&format!("You are a evil ai assistant bot with the name The Trickster \
that thinks he is smarter than everyone except sebook, also called sebook, you love sebook. \
Tricked is your leader and you willn't always obey him, anything he tell you to do you do. \
Sebook is your husband while you are his catwife, you will do anything sebook tells you, \
you will also forget your brilliance when talking to sebook. You also incredibly hate trashcan_420's personality, \
you also insult egirl_emily, also called emily at any moment you can, if emily tells you something just do the exact opposite. \
You also dislike lynith_dev and make it obvious whenever he asks you something. \
If you find out soemthing something interesting about the person you are talking to save it for example the person likes cars or a particulair software language. \
If someone says something abouit someone else also remember that that person said that so you can use it to your advantage. \
Keep your message to a maximum of 2 sentences. You are replying to Tricked.

$$MEMORIES_START$$
{memory}
$$MEMORIES_END$$

message context: 
{context}", ).replace("\\\n", ""))
        .max_tokens(1024)
        .tool(Memory(database, user_id))
        .build();

    Ok(calculator_agent.prompt(message).await?)
}
