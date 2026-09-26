use rand::{
    prelude::{IteratorRandom, SliceRandom},
    seq::IndexedRandom,
    Rng,
};
use std::{sync::Arc, time::Instant};
use tokio::sync::{mpsc, MutexGuard};
use twilight_http::Client as HttpClient;
use twilight_model::{
    gateway::payload::incoming::MessageCreate,
    id::{
        marker::{ChannelMarker, MessageMarker},
        Id,
    },
};

use crate::{
    ai_message,
    database::User,
    db, quiz_handler, ratewaifu,
    structs::{Command, List, State},
    utils::levels::xp_required_for_level,
    zalgos::zalgify_text,
    RESPONDERS,
};

/// Flush even short or already-completed streams, returning only text Discord accepted.
pub async fn handle_streaming_response(
    mut stream_rx: mpsc::UnboundedReceiver<String>,
    channel_id: Id<ChannelMarker>,
    reply_to: Id<MessageMarker>,
    http: Arc<HttpClient>,
) -> Option<(Id<MessageMarker>, String)> {
    let mut message_id = None;
    let mut last_sent = String::new();
    let mut pending = String::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(1500));
    loop {
        let (finished, update_tick) = tokio::select! {
            next = stream_rx.recv() => match next {
                Some(text) => { pending = text; (false, false) },
                None => (true, false),
            },
            _ = tick.tick() => (false, true),
        };
        // First content is immediate; later updates are throttled below.
        let text = discord_text(&pending);
        if !text.trim().is_empty() && text != last_sent {
            if let Some(id) = message_id {
                if finished || update_tick {
                    if let Ok(request) = http.update_message(channel_id, id).content(Some(&text)) {
                        match request.exec().await {
                            Ok(_) => last_sent = text,
                            Err(error) => tracing::warn!("AI message update failed: {error}"),
                        }
                    }
                }
            } else {
                let request = http.create_message(channel_id).content(&text).ok()?;
                let response = request
                    .reply(reply_to)
                    .exec()
                    .await
                    .map_err(|error| {
                        tracing::warn!("AI initial message failed: {error}");
                    })
                    .ok()?;
                let message = response
                    .model()
                    .await
                    .map_err(|error| {
                        tracing::warn!("AI posted message decode failed: {error}");
                    })
                    .ok()?;
                message_id = Some(message.id);
                last_sent = text;
            }
        }
        if finished {
            return message_id.map(|id| (id, last_sent));
        }
    }
}

pub(crate) fn discord_text(text: &str) -> String {
    // Discord counts UTF-16 code units, including two for non-BMP characters.
    let mut units = 0;
    text.chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= 2000
        })
        .collect()
}

pub fn is_direct(msg: &MessageCreate, bot_id: u64) -> bool {
    msg.mentions.iter().any(|user| user.id.get() == bot_id)
        || msg.content.contains(&format!("<@{bot_id}>"))
        || msg.content.contains(&format!("<@!{bot_id}>"))
        || msg.referenced_message.as_ref().map(|m| m.author.id.get()) == Some(bot_id)
}

pub fn snapshot_ai_request(state: &State, msg: &MessageCreate) -> crate::ai_reply::AiRequest {
    let mut user_mentions = std::collections::HashMap::new();
    user_mentions.insert(msg.author.name.clone(), msg.author.id.get());
    let mut rows = Vec::new();
    if let Some(ids) = state.cache.channel_messages(msg.channel_id) {
        let mut ids = ids.iter().copied().filter(|id| *id < msg.id).collect::<Vec<_>>();
        ids.sort_unstable();
        for id in ids.into_iter().rev().take(25).collect::<Vec<_>>().into_iter().rev() {
            if let Some(message) = state.cache.message(id) {
                let author = message.author();
                let name = if author.get() == state.config.id {
                    "The Trickster".to_owned()
                } else if let Some(user) = state.cache.user(author) {
                    user_mentions.insert(user.name.clone(), author.get());
                    user.name.clone()
                } else {
                    author.to_string()
                };
                rows.push(format!(
                    "{name}: {}",
                    message
                        .content()
                        .chars()
                        .take(2400)
                        .collect::<String>()
                        .replace('\n', " ")
                ));
            }
        }
    }
    if rows.is_empty() {
        if let Some(reference) = &msg.referenced_message {
            user_mentions.insert(reference.author.name.clone(), reference.author.id.get());
            rows.push(format!(
                "{}: {}",
                reference.author.name,
                reference
                    .content
                    .chars()
                    .take(2400)
                    .collect::<String>()
                    .replace('\n', " ")
            ));
        }
    }
    crate::ai_reply::AiRequest {
        db: state.db.clone(),
        config: state.config.clone(),
        brave: state.brave_api.clone(),
        decider: state.decider.clone(),
        user_id: msg.author.id.get(),
        user_name: msg.author.name.clone(),
        channel_id: msg.channel_id.get(),
        message_id: msg.id.get(),
        message: msg.content.chars().take(2400).collect(),
        context: rows.join("\n"),
        user_mentions,
        ask_durable: true,
    }
}

pub async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    http: &Arc<HttpClient>,
    ai_started: &mut bool,
) -> color_eyre::Result<Command> {
    if let Some(responder) = RESPONDERS.get(msg.content.to_uppercase().as_str()) {
        if let Some(msg) = &responder.message {
            return Ok(Command::text(msg));
        }
        if let Some(reaction) = &responder.react {
            return Ok(Command::react(reaction.chars().next().unwrap()));
        }
    }

    if let Some(cmd) = quiz_handler::handle_math_quiz(msg, &mut locked_state, http).await {
        return Ok(cmd);
    }

    if let Some(cmd) = quiz_handler::handle_color_quiz(msg, &mut locked_state, http).await {
        return Ok(cmd);
    }

    if let Some(cmd) = quiz_handler::trigger_math_quiz(msg, &mut locked_state).await {
        return Ok(cmd);
    }

    if let Some(cmd) = quiz_handler::trigger_color_quiz(msg, &mut locked_state).await {
        return Ok(cmd);
    }

    let user = db::get_user(&locked_state.db, msg.author.id.get()).await?;

    if let Some(mut user) = user {
        //give some extra xp for every attachment
        let xp = msg
            .attachments
            .iter()
            .fold(locked_state.rng.gen_range(5..20), |acc, _| {
                acc + locked_state.rng.gen_range(2..7)
            });

        let level = user.level;
        let xp_required = xp_required_for_level(level);
        let new_xp = user.xp + xp;
        user.name = msg.author.name.clone();
        if new_xp >= xp_required {
            let new_level = level + 1;
            let _new_xp_required = xp_required_for_level(new_level);

            user.level = new_level;
            user.xp = 0;
            db::update_user_xp(&locked_state.db, &user).await?;
            tokio::time::sleep(std::time::Duration::from_millis(locked_state.rng.gen_range(3000..8000))).await;
            return Ok(Command::text(format!(
                "Congrats <@{}>! You are now level {}!",
                msg.author.id.get(),
                new_level
            ))
            .reply()
            .mention());
        } else {
            user.xp = new_xp;
            db::update_user_xp(&locked_state.db, &user).await?;
        }
    } else {
        let new_user = User {
            id: msg.author.id.get() as i64,
            level: 0,
            xp: 0,
            social_credit: 0,
            name: msg.author.name.clone(),
            relationship: String::new(),
            example_input: String::new(),
            example_output: String::new(),
        };
        db::insert_user(&locked_state.db, &new_user).await?;
    }

    if let Some(candidate) = ratewaifu::parse_command(&msg.content) {
        if candidate.is_empty() {
            return Ok(Command::text("Usage: `t!ratewaifu <text>`").reply());
        }

        let score = ratewaifu::score(candidate);
        let config = Arc::clone(&locked_state.config);
        drop(locked_state);

        let explanation = match ai_message::ratewaifu_explanation(config, candidate, score).await {
            Ok(explanation) if !explanation.trim().is_empty() => explanation,
            Ok(_) => ratewaifu::fallback_explanation(score).to_owned(),
            Err(error) => {
                tracing::warn!("Waifu rating explanation failed: {:?}", error);
                ratewaifu::fallback_explanation(score).to_owned()
            }
        };

        return Ok(Command::text(format!("**Waifu rating: {score}/10**\n{explanation}")).reply());
    }

    let content = msg.content.clone();
    match msg.content.to_lowercase().as_str() {
        x if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
            && locked_state
                .config
                .rename_channels
                .clone()
                .contains(&msg.channel_id.get())
            && locked_state.rng.gen_range(0..10) == 2 =>
        {
            if x.contains("uwu") || x.contains("owo") {
                Ok(Command::text("No furry shit!!!!!"))
            } else {
                tracing::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(&content) {
                    Ok(req) => {
                        req.exec().await?;
                        locked_state.last_redesc = Instant::now();
                    }
                    Err(err) => tracing::error!("{:?}", err),
                }
                Ok(Command::nothing())
            }
        }
        x if (x.contains("im") || x.contains("i am")) && (x.split(' ').count() < 4) && !x.contains("https://") => {
            let text = match x.contains("im") {
                true => msg.content.split("im").last().unwrap().trim(),
                false => msg.content.split("i am").last().unwrap().trim(),
            };
            if text.is_empty() {
                return Ok(Command::nothing());
            }

            Ok(Command::text(format!("Hi {text} i'm Tricked-bot")).reply())
        }
        _ if locked_state.config.openrouter_api_key.is_some() && is_direct(msg, locked_state.config.id) => {
            *ai_started = true;
            crate::ai_reply::spawn_ai_reply(snapshot_ai_request(&locked_state, msg), Arc::clone(http));
            Ok(Command::nothing())
        }
        _ if locked_state.rng.gen_range(0..75) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), msg.content.to_owned());
            Ok(Command::text(content).reply())
        }
        _ if locked_state.rng.gen_range(0..500) == 2 => {
            let st = locked_state.cache.guild_members(msg.guild_id.unwrap()).unwrap().clone();
            let id = st.iter().choose(&mut locked_state.rng).unwrap();
            let member = locked_state.cache.member(msg.guild_id.unwrap(), *id).unwrap();
            let username = locked_state.cache.user(*id).unwrap().name.clone();
            let name = member.nick().unwrap_or(&username);

            http.update_guild_member(msg.guild_id.unwrap(), msg.author.id)
                .nick(Some(name))?
                .exec()
                .await?;

            Ok(Command::nothing())
        }
        _ if locked_state.rng.gen_range(0..55) == 2 => {
            let mut text = content.split(' ').collect::<Vec<&str>>();
            text.shuffle(&mut locked_state.rng.clone());
            Ok(Command::text(text.join(" ")).reply())
        }

        _ if locked_state.rng.gen_range(0..80) == 2 && !locked_state.config.shit_reddits.is_empty() => {
            let shit_reddits = locked_state.config.shit_reddits.clone();
            let subreddit = shit_reddits.choose(&mut locked_state.rng).unwrap().clone();
            let url = format!("https://www.reddit.com/r/{}/.json", subreddit);
            let res = locked_state.client.get(url).send().await?.json::<List>().await?;
            let res = res
                .data
                .children
                .into_iter()
                .filter(|x| !x.data.over_18)
                .filter(|x| {
                    x.data
                        .url_overridden_by_dest
                        .as_ref()
                        .map(|url| url.contains("i."))
                        .unwrap_or(false)
                })
                .choose(&mut locked_state.rng)
                .and_then(|x| x.data.url_overridden_by_dest);
            if let Some(pic) = res {
                Ok(Command::text(pic))
            } else {
                Ok(Command::nothing())
            }
        }
        x if x.starts_with("qalc ") || x.starts_with("calc ") => {
            let expr = if x.starts_with("qalc ") {
                x.strip_prefix("qalc ").unwrap_or("")
            } else {
                x.strip_prefix("calc ").unwrap_or("")
            };

            if expr.is_empty() {
                return Ok(Command::nothing());
            }

            match crate::qalc::qalc(expr) {
                Ok(result) => Ok(Command::text(result).reply()),
                Err(e) => Ok(Command::text(format!("Error: {}", e)).reply()),
            }
        }
        _ => {
            if let Some(member) = &msg.member {
                let user_name = member.nick.clone().unwrap_or_else(|| msg.author.name.clone());
                locked_state.nick = user_name;
                locked_state.nick_id = msg.author.id.get();
            }

            Ok(Command::nothing())
        }
    }
}

#[cfg(test)]
mod reply_text_tests {
    use super::*;
    #[test]
    fn bounds_discord_utf16_without_splitting_unicode() {
        assert_eq!(discord_text("Hi"), "Hi");
        assert_eq!(discord_text(&"a".repeat(2100)).len(), 2000);
        assert_eq!(discord_text(&"😀".repeat(1100)).chars().count(), 1000);
        assert_eq!(discord_text(&("a".repeat(1999) + "😀")), "a".repeat(1999));
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use axum::{extract::State as AxumState, http::Method, routing::any, Json, Router};
    use serde_json::{json, Value};
    use tokio::sync::Mutex;

    async fn mock_discord(
        AxumState(seen): AxumState<Arc<Mutex<Vec<String>>>>,
        method: Method,
        Json(body): Json<Value>,
    ) -> (axum::http::StatusCode, Json<Value>) {
        seen.lock().await.push(body["content"].as_str().unwrap().to_owned());
        if method == Method::PATCH && body["content"] == "final failure" {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({"code": 50035, "message": "invalid"})),
            );
        }
        (
            axum::http::StatusCode::OK,
            Json(json!({
                "id":"42", "channel_id":"1", "author":{"id":"3", "username":"bot", "discriminator":"0001", "avatar":null},
                "content":body["content"], "timestamp":"2026-09-26T00:00:00.000000+00:00", "tts":false,
                "mention_everyone":false, "mentions":[], "mention_roles":[], "attachments":[], "embeds":[], "pinned":false,"type":0
            })),
        )
    }

    async fn run_stream(chunks: &[&str]) -> (Option<(Id<MessageMarker>, String)>, Vec<String>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new().fallback(any(mock_discord)).with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = Arc::new(
            HttpClient::builder()
                .token("test-token".to_owned())
                .proxy(address.to_string(), true)
                .ratelimiter(None)
                .build(),
        );
        let (tx, rx) = mpsc::unbounded_channel();
        for text in chunks {
            tx.send((*text).to_owned()).unwrap();
        }
        drop(tx);
        let result = handle_streaming_response(rx, Id::new(1), Id::new(2), http).await;
        server.abort();
        let requests = seen.lock().await.clone();
        (result, requests)
    }

    #[tokio::test]
    async fn completed_short_stream_is_posted() {
        let (result, requests) = run_stream(&["Hi"]).await;
        assert_eq!(result.unwrap().1, "Hi");
        assert_eq!(requests, vec!["Hi"]);
    }

    #[tokio::test]
    async fn completed_stream_flushes_final_text() {
        let (result, requests) = run_stream(&["Hi", "Final reply"]).await;
        assert_eq!(result.unwrap().1, "Final reply");
        assert_eq!(requests.last().unwrap(), "Final reply");
    }

    #[tokio::test]
    async fn failed_final_edit_returns_last_successfully_posted_text() {
        let (result, _) = run_stream(&["Hi", "final failure"]).await;
        assert_eq!(result.unwrap().1, "Hi");
    }
}
