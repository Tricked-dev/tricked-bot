use std::{collections::HashMap, io, num::ParseIntError, sync::Arc};

use clap::Parser;

pub const DEFAULT_FOLLOWUP_THRESHOLD: f32 = 0.6;

#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplyMode {
    /// Only pings/replies trigger the AI; the reply question is not asked.
    #[default]
    Off,
    /// Reply when the score clears the threshold and the channel cooldown.
    Live,
}

#[derive(Parser, Clone, Debug, Default)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    #[arg(short, long, env)]
    pub token: String,
    #[arg(short, long, env)]
    pub discord: u64,
    #[arg(short, long, env)]
    pub join_channel: u64,
    #[arg(long, env, value_parser = vec_u64_parser)]
    pub message_indicator_channels: Arc<Vec<u64>>,
    #[arg(long, env, default_value = "postgres://localhost/trickedbot")]
    pub database_url: String,
    #[arg(short, long, env, default_value = "0")]
    pub id: u64,
    #[arg(long, env, value_parser(vec_u64_parser))]
    pub rename_channels: Arc<Vec<u64>>,
    #[arg(long, env, value_parser = parse_invites)]
    pub invites: HashMap<String, String>,
    #[arg(short, long, env, value_parser = parse_invites)]
    pub responders: HashMap<String, String>,
    #[arg(long, env, value_parser = parse_str_array)]
    pub shit_reddits: Arc<Vec<String>>,
    #[arg(short, long, env, default_value = "I am tricked bot!")]
    pub status: String,
    #[arg(long, env)]
    pub openrouter_api_key: Option<String>,
    #[arg(long, env, default_value = "https://openrouter.ai/api/v1")]
    pub openrouter_base_url: String,
    #[arg(long, env, default_value = "openai/gpt-6-luna")]
    pub openrouter_model: String,
    /// Conservative per-request token ceiling including all prompt text, images and maximum output.
    #[arg(long, env, default_value = "90000", value_parser = clap::value_parser!(u32).range(4096..=99000))]
    pub openrouter_max_request_tokens: u32,
    #[arg(long, env, default_value = "1024", value_parser = clap::value_parser!(u32).range(1..=4096))]
    pub openrouter_max_reply_tokens: u32,
    #[arg(long, env)]
    pub openrouter_memory_model: Option<String>,
    #[arg(long, env)]
    pub openrouter_site_url: Option<String>,
    #[arg(long, env)]
    pub openrouter_site_name: Option<String>,
    #[arg(long, env)]
    pub today_i_channel: Option<u64>,
    #[arg(long, env)]
    pub brave_api: Option<String>,
    #[arg(long, env)]
    pub pfp_channel: Option<u64>,
    #[arg(long, env, default_value = "false")]
    pub pfp_on_startup: bool,
    #[arg(long, env)]
    pub web_port: Option<u16>,
    /// Full /v1/systemone URL.
    #[arg(long, env)]
    pub decider_url: Option<String>,
    /// Optional faster decision-model endpoint used only for memory-name ranking.
    #[arg(long, env)]
    pub decider_recall_url: Option<String>,
    /// Shared with mail-triage/sure through jev.env.
    #[arg(long, env = "JEV_API_KEY")]
    pub decider_api_key: Option<String>,
    #[arg(long, env, default_value = "5000")]
    pub decider_check_timeout_ms: u64,
    #[arg(long, env, default_value = "120000")]
    pub decider_recall_timeout_ms: u64,
    #[arg(long, env, default_value = "0.6")]
    pub decider_durable_threshold: f32,
    #[arg(long, env, value_enum, default_value = "off")]
    pub decider_reply_mode: ReplyMode,
    #[arg(long, env, default_value = "0.9")]
    pub decider_reply_threshold: f32,
    /// Confidence required for a conversational response to the bot, without the spontaneous-reply cooldown.
    #[arg(long, env, default_value_t = DEFAULT_FOLLOWUP_THRESHOLD)]
    pub decider_followup_threshold: f32,
    /// Minimum guild messages between two unprompted replies in one channel.
    #[arg(long, env, default_value = "15")]
    pub decider_reply_cooldown: u32,
    /// Optional model for refusal style rewrites. Disabled by default: style false positives can corrupt valid replies.
    #[arg(long, env, default_value = "")]
    pub openrouter_fallback_model: String,
    #[arg(long, env, default_value = "0.9")]
    pub decider_refusal_threshold: f32,
}

fn parse_str_array(src: &str) -> Result<Arc<Vec<String>>, io::Error> {
    Ok(Arc::new(src.split(',').map(|x| x.to_owned()).collect()))
}

fn parse_invites(src: &str) -> Result<HashMap<String, String>, io::Error> {
    let mut map = HashMap::new();
    for pair in src.split(',') {
        let (key, value) = match pair.split_once(':') {
            Some(v) => v,
            None => return Err(io::Error::other("Invalid invite format")),
        };
        map.insert(key.to_string(), value.parse().unwrap());
    }
    Ok(map)
}
fn vec_u64_parser(src: &str) -> Result<Arc<Vec<u64>>, ParseIntError> {
    let mut vec = Vec::new();
    for pair in src.split(',') {
        vec.push(pair.parse()?);
    }
    Ok(Arc::new(vec))
}
