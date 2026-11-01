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
    /// Guild that slash commands are registered in.
    #[arg(short, long, env)]
    pub discord: u64,
    #[arg(long, env, default_value = "postgres://localhost/trickedbot")]
    pub database_url: String,
    /// Channels whose topic is occasionally replaced with a recent message.
    #[arg(long, env, value_delimiter = ',')]
    pub rename_channels: Vec<u64>,
    #[arg(short, long, env, default_value = "I am tricked bot!")]
    pub status: String,
    #[arg(long, env)]
    pub openrouter_api_key: Option<String>,
    #[arg(long, env, default_value = "https://openrouter.ai/api/v1")]
    pub openrouter_base_url: String,
    #[arg(long, env, default_value = "anthropic/claude-haiku-5.5")]
    pub openrouter_model: String,
    /// Estimated per-request token target including prompt text, images and maximum output (10% headroom by default).
    #[arg(long, env, default_value = "90000", value_parser = clap::value_parser!(u32).range(4096..=99000))]
    pub openrouter_max_request_tokens: u32,
    #[arg(long, env, default_value = "1024", value_parser = clap::value_parser!(u32).range(1..=4096))]
    pub openrouter_max_reply_tokens: u32,
    /// Memory extraction needs deterministic JSON, not the chat model's long reasoning mode.
    #[arg(long, env, default_value = "qwen/qwen3.5-9b")]
    pub openrouter_memory_model: String,
    /// Embeds memories so recall can shortlist them by content. Empty disables embeddings.
    #[arg(long, env, default_value = "openai/text-embedding-3-small")]
    pub openrouter_embedding_model: String,
    /// Speech synthesis model. Empty disables spoken replies.
    #[arg(long, env, default_value = "fish-audio/s1-20260729")]
    pub openrouter_tts_model: String,
    /// Optional Fish voice ID; unset uses the provider's default voice.
    #[arg(long, env)]
    pub openrouter_tts_voice: Option<String>,
    /// Voice-message transcription model. Empty disables incoming transcription.
    #[arg(long, env, default_value = "mistralai/voxtral-small-transcribe-3-0")]
    pub openrouter_stt_model: String,
    /// Disables both incoming transcription and outgoing voice messages.
    #[arg(long, env)]
    pub disable_voice: bool,
    /// Optional model for refusal style rewrites. Disabled by default: style false positives can corrupt valid replies.
    #[arg(long, env)]
    pub openrouter_fallback_model: Option<String>,
    /// Lets replies search any GIF through Klipy. Without it they pick from a fixed set of keyless anime reactions.
    #[arg(long, env)]
    pub klipy_api_key: Option<String>,
    /// Stops replies from sending GIFs at all.
    #[arg(long, env)]
    pub disable_gifs: bool,
    #[arg(long, env)]
    pub openrouter_site_url: Option<String>,
    #[arg(long, env)]
    pub openrouter_site_name: Option<String>,
    /// Channel where every message has to start with "today i".
    #[arg(long, env)]
    pub today_i_channel: Option<u64>,
    /// Channel that the daily profile picture is picked from.
    #[arg(long, env)]
    pub pfp_channel: Option<u64>,
    #[arg(long, env)]
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
    /// Confidence that a message confidently states something false before a Community Note is drafted.
    #[arg(long, env, default_value = "0.9")]
    pub decider_wrong_claim_threshold: f32,
    /// Confidence that people are arguing before a ruling or poll is drafted.
    #[arg(long, env, default_value = "0.85")]
    pub decider_argument_threshold: f32,
    /// Minimum seconds between two Community Notes in one channel.
    #[arg(long, env, default_value = "1800")]
    pub decider_note_cooldown_secs: u64,
    /// Minimum seconds between two rulings or polls in one channel.
    #[arg(long, env, default_value = "3600")]
    pub decider_ruling_cooldown_secs: u64,
    #[arg(long, env, default_value = "0.9")]
    pub decider_refusal_threshold: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_are_comma_separated_and_optional() {
        let cfg = Config::parse_from(["bot", "--token", "t", "--discord", "1", "--rename-channels", "1,2"]);
        assert_eq!(cfg.rename_channels, [1, 2]);
        assert_eq!(cfg.decider_reply_mode, ReplyMode::Off);
    }
}
