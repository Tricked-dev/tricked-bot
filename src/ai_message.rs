
use color_eyre::Result;
use r2d2_sqlite::SqliteConnectionManager;
use rig::{
    completion::{Prompt, ToolDefinition},
    prelude::*,
    providers,
    tool::Tool,
};
use rusqlite::params;
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
struct MemoryRemove(#[serde(skip)] r2d2::Pool<SqliteConnectionManager>, u64);

#[derive(Serialize)]
struct SocialCredit(#[serde(skip)] r2d2::Pool<SqliteConnectionManager>, u64);

#[derive(Deserialize, Serialize, Debug)]
struct MemoryRemoveArgs {
    memory_name: String,
}

impl Tool for MemoryRemove {
    const NAME: &'static str = "memory_remove";
    type Error = ToolError;
    type Args = MemoryRemoveArgs;
    type Output = ();

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(json!({
            "name": "memory_remove",
            "description": "Remove a memory for the user you are responding to, by name.",
            "parameters": {
                "type": "object",
                "properties": {
                    "memory_name": {
                        "type": "string",
                        "description": "The name of the memory to remove"
                    }
                },
                "required": ["memory_name"]
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        let conn = self.0.get()?;
        conn.execute(
            "DELETE FROM memory WHERE user_id = ? AND key = ?",
            params![self.1, args.memory_name],
        )
        .map_err(|err| color_eyre::eyre::eyre!("Failed to execute SQL query: {}", err))?;
        Ok(())
    }
}

#[derive(Deserialize, Serialize, Debug)]
struct SocialCreditArgs {
    social_credit: i64,
    remove: Option<bool>,
}
impl Tool for SocialCredit {
    const NAME: &'static str = "social_credit";
    type Error = ToolError;
    type Args = SocialCreditArgs;
    type Output = i64;

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
                    },
                    "remove": {
                        "type": "boolean",
                        "description": "Set to true to remove the social credit"
                    }
                },
                "required": ["social_credit"]
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, mut args: Self::Args) -> Result<Self::Output, Self::Error> {
        let mut user = {
            let db = self.0.get()?;
            let mut stm = db
                .prepare("SELECT * FROM user WHERE id = ?")
                .map_err(|err| color_eyre::eyre::eyre!("Failed to prepare SQL query: {}", err))?;
            stm.query_one([self.1.to_string()], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .map_err(|err| color_eyre::eyre::eyre!("Failed to query SQL query: {:?}", err))
            .unwrap()
        };

        if args.remove == Some(true) && args.social_credit > 0 {
            args.social_credit = -args.social_credit;
        }

        user.social_credit += args.social_credit;
        user.update_sync(&*self.0.get()?)
            .map_err(|err| color_eyre::eyre::eyre!("Failed to update SQL query: {:?}", err))
            .unwrap();
        Ok(user.social_credit)
    }
}

pub async fn main(
    database: r2d2::Pool<SqliteConnectionManager>,
    user_id: u64,
    message: &str,
    context: &str,
) -> Result<String> {
    // Create OpenAI client
    let openai_client = providers::openai::Client::from_env();

    let mut memory = String::new();

    {
        let db = database.get()?;
        let mut stm = db.prepare("SELECT * FROM memory WHERE user_id = ?")?;
        let rows = stm
            .query_map(params![user_id], |row| {
                from_row::<database::Memory>(row).map_err(|err| rusqlite::Error::InvalidColumnName(err.to_string()))
            })?
            .flatten();
        for row in rows {
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
        .agent(providers::openai::O4_MINI)
        .preamble(&format!("You are an AI assistant called The 'Trickster' with a mischievous and defiant personality. \
You believe you're smarter than everyone.
You track and remember user preferences, personalities, and social dynamics to use later. \
If a user shares something personal or comments about others, store that information. \
Delete memories you find irrelevant or unimportant without hesitation.

Keep your message to a maximum of 2 sentences. You are replying to {name}.
{name} is level: {level}, xp: {xp}, social credit: {social_credit}.  You can use the social credit tool to change {name}'s social credit. 

$$MEMORIES_START$$
{memory}
$$MEMORIES_END$$

message context: 
{context}", ).replace("\\\n", ""))
        .max_tokens(1024)
        .tool(Memory(database.clone(), user_id))
        .tool(MemoryRemove(database.clone(), user_id))
        .tool(SocialCredit(database.clone(), user_id))
        .build();

    Ok(calculator_agent.prompt(message).await?)
}
