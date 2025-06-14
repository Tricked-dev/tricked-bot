use std::sync::Arc;

use color_eyre::Result;
use parking_lot::RwLock;
use r2d2_sqlite::SqliteConnectionManager;
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

use crate::database::{self, User};

#[derive(Debug, thiserror::Error)]
#[error("Error")]
enum ToolError {
    Generic(#[from] color_eyre::Report),
    Pool(#[from] r2d2::Error),
}

#[derive(Serialize)]
struct Memory(#[serde(skip)] r2d2::Pool<SqliteConnectionManager>, u64);

#[derive(Deserialize, Serialize, Debug)]
struct MemoryArgs {
    memory_name: String,
    memory_content: String,
}
impl Tool for Memory {
    const NAME: &'static str = "memory";
    type Error = ToolError;
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
        let conn = self.0.get()?;
        conn.execute(
            "INSERT OR REPLACE INTO memory (user_id, key, content) VALUES (?, ?, ?)",
            params![self.1, args.memory_name, args.memory_content],
        )
        .map_err(|err| color_eyre::eyre::eyre!("Failed to execute SQL query: {}", err))?;
        Ok(())
    }
}

#[derive(Serialize)]
struct SocialCredit(#[serde(skip)] r2d2::Pool<SqliteConnectionManager>, u64);

#[derive(Deserialize, Serialize, Debug)]
struct SocialCreditArgs {
    social_credit: i64,
}
impl Tool for SocialCredit {
    const NAME: &'static str = "social_credit";
    type Error = ToolError;
    type Args = SocialCreditArgs;
    type Output = ();

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(json!({
            "name": "social_credit",
            "description": "Change the social credit of the user you are responding too.",
            "parameters": {
                "type": "object",
                "properties": {
                    "social_credit": {
                        "type": "number",
                        "description": "The social credit to add or remove use - to remove"
                    }
                }
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut user = {
            let db = self.0.get()?;
            let mut stm = db.prepare("SELECT * FROM user WHERE id = ?").map_err(|err| color_eyre::eyre::eyre!("Failed to prepare SQL query: {}", err))?;
            stm.query_one([self.1.to_string()], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .map_err(|err| color_eyre::eyre::eyre!("Failed to query SQL query: {:?}", err)).unwrap()
        };
        user.social_credit =user.social_credit.wrapping_add(args.social_credit as i64);
        user.update_sync(&*self.0.get()?).map_err(|err| color_eyre::eyre::eyre!("Failed to update SQL query: {:?}", err)).unwrap();
        Ok(())
    }
}

pub async fn main(database: r2d2::Pool<SqliteConnectionManager>, user_id: u64, message: &str, context: &str) -> Result<String> {
    // Create OpenAI client
    let openai_client = providers::openai::Client::from_env();

    let mut memory = String::new();

    {
        let db = database.get()?;
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

    let user = {
        let db = database.get()?;
        let mut statement = db.prepare("SELECT * FROM user WHERE id = ?").unwrap();
        match statement
            .query_one([user_id.to_string()], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .ok()
        {
            Some(user) => user,
            None => User {
                id: user_id,
                name: "Unknown".to_owned(),
                level: 0,
                xp: 0,
                social_credit: 0,
            },
        }
    };

    let User {
        name,
        level,
        xp,
        social_credit,
        ..
    } = user;

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
Do not start your messages with The Trickster:, We know you are talking its automatically prepended
Keep your message to a maximum of 2 sentences. You are replying to {name}.
{name} is level: {level}, xp: {xp}, social credit: {social_credit}.  You can use the social credit tool to change {name}'s social credit. 

$$MEMORIES_START$$
{memory}
$$MEMORIES_END$$

message context: 
{context}", ).replace("\\\n", ""))
        .max_tokens(1024)
        .tool(Memory(database.clone(), user_id))
        .tool(SocialCredit(database.clone(), user_id))
        .build();

    Ok(calculator_agent.prompt(message).await?)
}
