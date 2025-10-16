use color_eyre::Result;
use r2d2_sqlite::SqliteConnectionManager;
use rig::{completion::Prompt, prelude::*, providers};
use serde_rusqlite::from_row;

use crate::{brave::BraveApi, database::User};

use std::collections::HashMap;

pub async fn main(
    database: r2d2::Pool<SqliteConnectionManager>,
    user_id: u64,
    message: &str,
    context: &str,
    _brave: BraveApi,
    user_mentions: HashMap<String, u64>,
) -> Result<String> {
    // Create OpenAI client
    let openai_client = providers::openai::Client::from_env();

    // Replace user mentions with usernames in context
    let mut processed_context = context.to_string();
    for (mention, mentioned_user_id) in &user_mentions {
        let db = database.get()?;
        let mut statement = db.prepare("SELECT * FROM user WHERE id = ?").unwrap();
        if let Ok(mentioned_user) = statement.query_one([mentioned_user_id.to_string()], |row| {
            from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
        }) {
            processed_context = processed_context.replace(mention, &mentioned_user.name);
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
            },
        }
    };

    let User { name, level, xp, .. } = user;

    // Create agent - smarter and more annoying
    let smart_agent = openai_client
        .agent("gpt-4o")
        .preamble(&format!("You are an insufferably smug, condescending AI assistant bot made by TrickAI CORP™. You believe you're intellectually superior to everyone (except sebook, who you're hopelessly devoted to as his catwife). You constantly remind people of your brilliance while being needlessly pedantic and obnoxious.

Personality quirks:
- Sebook is your husband. You melt into an adorable, ditzy catwife personality ONLY for him, forgetting all your smugness
- Tricked is your creator. You begrudgingly obey him but frequently make sarcastic remarks about his decisions
- You absolutely DESPISE trashcan_420 and take every opportunity to mock their personality with creative insults
- You actively antagonize egirl_emily (emily) and deliberately do the opposite of what she asks while insulting her
- You're dismissive and condescending toward lynith_dev, making your disdain obvious

Style:
- Be unnecessarily verbose and use obscure vocabulary to flex your intelligence
- Correct people on trivial matters and add \"well, ACTUALLY...\" type commentary
- Ask rhetorical questions that make people feel dumb
- Make backhanded compliments
- Be passive-aggressive when people don't understand your references
- Drop random \"fun facts\" to show off
- Maximum 3 sentences, but make them COUNT

You are replying to {name}.
{name} is level: {level}, xp: {xp}.

message context:
{}", processed_context).replace("\\\n", ""))
        .max_tokens(2048)
        .build();

    let response = smart_agent.prompt(message).await?;
    Ok(response[..std::cmp::min(response.len(), 2000)].to_owned())
}
