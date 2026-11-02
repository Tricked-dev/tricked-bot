//! Replays recorded reply contexts against the live chat model to compare prompt changes. Needs the production env
//! and `REPLAY_SCENARIOS`, `REPLAY_USERS`, `REPLAY_OUT` and `REPLAY_RUNS`; run with `cargo test replay -- --ignored`.

use std::{collections::HashMap, time::Duration};

use clap::Parser;
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::json;
use twilight_model::id::Id;

use super::{
    AiRequest,
    openrouter::{Content, OpenRouter},
    prompt,
};
use crate::{config::Config, db::User};

#[derive(Deserialize)]
struct Scenario {
    name: String,
    channel_id: String,
    message_id: String,
    user_id: String,
    user_name: String,
    message: String,
    conversation_ids: Vec<String>,
    conversation: Vec<String>,
    reply_target: Option<(String, String)>,
    user_mentions: HashMap<String, String>,
}

#[derive(Deserialize)]
struct Profile {
    id: i64,
    level: i32,
    xp: i32,
    name: String,
    relationship: String,
    example_input: String,
    example_output: String,
}

fn id<T>(text: &str) -> Id<T> {
    Id::new(text.parse().expect("snowflake"))
}

impl Scenario {
    fn request(&self) -> AiRequest {
        AiRequest {
            user_id: id(&self.user_id),
            user_name: self.user_name.clone(),
            channel_id: id(&self.channel_id),
            message_id: id(&self.message_id),
            message: self.message.clone(),
            source_text: self.message.clone(),
            media: Vec::new(),
            conversation: self.conversation.clone(),
            conversation_ids: self.conversation_ids.iter().map(|text| id(text)).collect(),
            reply_target: self.reply_target.as_ref().map(|(_, line)| line.clone()),
            reply_target_id: self.reply_target.as_ref().map(|(target, _)| id(target)),
            replyable: true,
            user_mentions: self.user_mentions.iter().map(|(name, user)| (name.clone(), id(user))).collect(),
        }
    }
}

#[tokio::test]
#[ignore = "calls the live model"]
async fn replay() {
    let env = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("{name} unset"));
    let config = Config::parse_from(["bot"]);
    let ai = OpenRouter::from_config(reqwest::Client::new(), &config).expect("OpenRouter key");
    let mut scenarios = Vec::new();
    for path in env("REPLAY_SCENARIOS").split(',') {
        let mut loaded: Vec<Scenario> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        scenarios.append(&mut loaded);
    }
    let profiles: Vec<Profile> = serde_json::from_str(&std::fs::read_to_string(env("REPLAY_USERS")).unwrap()).unwrap();
    let users: HashMap<String, User> = profiles
        .into_iter()
        .map(|p| {
            let user = User {
                id: p.id,
                level: p.level,
                xp: p.xp,
                name: p.name,
                relationship: p.relationship,
                example_input: p.example_input,
                example_output: p.example_output,
            };
            (p.id.to_string(), user)
        })
        .collect();
    let runs: usize = env("REPLAY_RUNS").parse().unwrap();
    let jobs = scenarios.iter().flat_map(|scenario| (0..runs).map(move |run| (scenario, run)));
    let results: Vec<_> = stream::iter(jobs)
        .map(|(scenario, run)| {
            let (ai, config, users) = (&ai, &config, &users);
            async move {
                let mut req = scenario.request();
                if std::env::var_os("REPLAY_NO_AGES").is_some() {
                    req.conversation_ids.clear();
                    req.reply_target_id = None;
                }
                let turn = Content::Text(prompt::text_turn(&req));
                let user = users.get(&scenario.user_id);
                let request = prompt::build_request(config, &req, user, &[], turn, &config.openrouter_model).unwrap();
                if let Ok(dump) = std::env::var("REPLAY_DUMP") {
                    std::fs::write(format!("{dump}/{}.json", scenario.name), serde_json::to_string(&request).unwrap())
                        .unwrap();
                }
                let output = ai.complete(&request, Duration::from_secs(90)).await.map_err(|error| error.to_string());
                json!({ "name": scenario.name, "run": run, "output": output.as_ref().ok(), "error": output.err() })
            }
        })
        .buffer_unordered(8)
        .collect()
        .await;
    let lines: Vec<String> = results.iter().map(ToString::to_string).collect();
    std::fs::write(env("REPLAY_OUT"), lines.join("\n")).unwrap();
}
