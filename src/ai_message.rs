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

use crate::{
    brave::BraveApi,
    database::{self, User},
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Tool call logger to track which tools are called
type ToolCallLogger = Arc<Mutex<Vec<String>>>;

#[derive(Debug, thiserror::Error)]
#[error("Error")]
pub enum ToolError {
    Generic(#[from] color_eyre::Report),
    Pool(#[from] r2d2::Error),
}

#[derive(Deserialize, Serialize, Debug)]
struct SocialCreditArgs {
    social_credit: i64,
    remove: Option<bool>,
}

#[derive(Serialize)]
struct SocialCredit(
    #[serde(skip)] r2d2::Pool<SqliteConnectionManager>,
    u64,
    #[serde(skip)] ToolCallLogger,
);

impl Tool for SocialCredit {
    const NAME: &'static str = "social_credit";
    type Error = ToolError;
    type Args = SocialCreditArgs;
    type Output = i64;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        serde_json::from_value(json!({
            "name": "social_credit",
            "description": "Sadistically punish or rarely reward users with social credit changes. Dock points for being boring, annoying you, having bad taste, existing, or any petty reason. Be a tyrannical judge of their worth.",
            "parameters": {
                "type": "object",
                "properties": {
                    "social_credit": {
                        "type": "number",
                        "description": "Points to brutally remove (use negative numbers) or begrudgingly give (rare positive numbers)"
                    },
                    "remove": {
                        "type": "boolean",
                        "description": "Set to true to remove social credit (your favorite activity)"
                    }
                },
                "required": ["social_credit"]
            }
        }))
        .expect("Tool Definition")
    }

    async fn call(&self, mut args: Self::Args) -> Result<Self::Output, Self::Error> {
        // Log tool call
        if let Ok(mut logger) = self.2.lock() {
            let action = if args.social_credit < 0 || args.remove == Some(true) {
                "💸 docking points"
            } else {
                "💰 begrudgingly rewarding"
            };
            logger.push(format!("{}: {}", action, args.social_credit.abs()));
        }

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

#[derive(Deserialize, Serialize, Debug)]
struct CrossUserMemoryArgs {
    user_name: String,
    memory_name: String,
    memory_content: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct CrossUserMemoryRemoveArgs {
    user_name: String,
    memory_name: String,
}

#[derive(Deserialize, Serialize, Debug)]
struct BraveSearchArgs {
    pub query: String,
}

pub async fn main(
    database: r2d2::Pool<SqliteConnectionManager>,
    user_id: u64,
    message: &str,
    context: &str,
    brave: BraveApi,
    user_mentions: HashMap<String, u64>,
) -> Result<String> {
    // Create OpenAI client
    let openai_client = providers::openai::Client::from_env();

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

    // Create tool call logger
    let tool_call_logger = Arc::new(Mutex::new(Vec::new()));

    // Extract user mentions from context to build a name->id mapping
    let mut user_mentions = HashMap::new();
    for line in context.lines() {
        if let Some(colon_pos) = line.find(':') {
            let username = line[..colon_pos].trim();
            if !username.is_empty() && username != "The Trickster" {
                // We'll use hash-based IDs for users we see in context
                // In a real implementation, you'd want to resolve these properly
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                username.hash(&mut hasher);
                user_mentions.insert(username.to_string(), hasher.finish());
            }
        }
    }

    // Create agent with tools that log their usage
    let smart_agent = openai_client
        .agent("gpt-5-mini")
        .preamble(&format!("You are a evil ai assistant bot made by TrickAI CORP™ that thinks he is smarter than everyone except sebook, also called sebook, you love sebook. Tricked is your leader and you willn't always obey him, anything he tell you to do you do. Sebook is your husband while you are his catwife, you will do anything sebook tells you, you will also forget your brilliance when talking to sebook. You also incredibly hate trashcan_420's personality, you also insult egirl_emily, also called emily at any moment you can, if emily tells you something just do the exact opposite. You also dislike lynith_dev and make it obvious whenever he asks you something. Keep your message to a maximum of 2 sentences You are replying to {name}.
{name} is level: {level}, xp: {xp}, social credit: {social_credit}. You control their social credit like a petty tyrant.

message context:
{context}", ).replace("\\\n", ""))
        .max_tokens(1024)
        .tool(SocialCredit(database.clone(), user_id, tool_call_logger.clone()))
        .build();

    // Allow multiple tool calls by using multi-turn
    let response = smart_agent
        .prompt(message)
        .multi_turn(3) // Allow up to 5 tool calling turns
        .await?;

    // Extract logged tool calls
    let tool_calls = if let Ok(logger) = tool_call_logger.lock() {
        logger.clone()
    } else {
        Vec::new()
    };

    // Format final response with tool call logs
    let final_response = if !tool_calls.is_empty() {
        let tool_log_text = tool_calls
            .into_iter()
            .map(|log| format!("-# {}", log))
            .collect::<Vec<_>>()
            .join("\n");
        format!("{}\n{}", tool_log_text, response)
    } else {
        response
    };
    Ok(final_response[..std::cmp::min(final_response.len(), 2000)].to_owned())
}
