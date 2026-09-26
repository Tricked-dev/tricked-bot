use crate::{
    ai_message, brave::BraveApi, config::Config, decider::Decider, memory_creator, message_handler, questions, recall,
};
use deadpool_postgres::Pool;
use std::{collections::HashMap, sync::Arc};
use twilight_http::Client as HttpClient;
use twilight_model::id::Id;

#[derive(Clone)]
pub struct AiRequest {
    pub db: Pool,
    pub config: Arc<Config>,
    pub brave: BraveApi,
    pub decider: Option<Decider>,
    pub user_id: u64,
    pub user_name: String,
    pub channel_id: u64,
    pub message_id: u64,
    pub message: String,
    pub context: String,
    pub user_mentions: HashMap<String, u64>,
    pub ask_durable: bool,
}

/// Both DM authors and users intercepted by responders must have a stable DB identity.
pub async fn ensure_author(req: &AiRequest) -> color_eyre::Result<()> {
    crate::db::insert_user(
        &req.db,
        &crate::database::User {
            id: req.user_id as i64,
            name: req.user_name.clone(),
            level: 0,
            xp: 0,
            social_credit: 0,
            relationship: String::new(),
            example_input: String::new(),
            example_output: String::new(),
        },
    )
    .await
}

pub fn spawn_ai_reply(req: AiRequest, http: Arc<HttpClient>) {
    tokio::spawn(async move {
        let channel = Id::new(req.channel_id);
        let reply_to = Id::new(req.message_id);
        let result = async {
            ensure_author(&req).await?;
            let recalled = recall::select(&req).await?;
            if recalled.durable {
                tokio::spawn(memory_creator::write_for_author(
                    req.db.clone(),
                    req.config.clone(),
                    req.user_id,
                    req.message_id,
                    req.message.clone(),
                    questions::short_context(&req.context),
                ));
            }
            let fallback_memories = recalled.memories.clone();
            let rx = ai_message::main(&req, recalled.memories, &req.config.openrouter_model).await?;
            if let Some(posted) = message_handler::handle_streaming_response(rx, channel, reply_to, http.clone()).await
            {
                crate::refusal::check_and_replace(&req, fallback_memories, http.clone(), channel, posted).await;
            }
            Ok::<(), color_eyre::Report>(())
        }
        .await;
        if let Err(error) = result {
            tracing::error!(target: "decider", kind = "reply", msg_id = req.message_id, "AI reply failed: {error}");
            if let Ok(request) = http
                .create_message(channel)
                .content("AI Error: reply or memory recall unavailable.")
            {
                let _ = request.reply(reply_to).exec().await;
            }
        }
    });
}
