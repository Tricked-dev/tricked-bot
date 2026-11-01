//! Recorded speech: durable at-most-once STT claims, cached conversation text and Discord-ready audio.

use super::{AiRequest, openrouter};
use crate::{bot::Bot, db, ffmpeg};
use base64::{Engine, engine::general_purpose::STANDARD};
use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};
use twilight_model::channel::{Message, message::MessageFlags};

const UNAVAILABLE: &str = "[voice message: transcription unavailable; do not guess its contents]";

#[derive(Default)]
pub struct VoiceState {
    texts: RwLock<BTreeMap<u64, String>>,
    loaded_channels: Mutex<HashSet<u64>>,
}

impl VoiceState {
    pub fn remember(&self, message: u64, text: String) {
        let mut texts = self.texts.write().unwrap();
        texts.insert(message, text);
        while texts.len() > 1000 {
            texts.pop_first();
        }
    }

    pub fn content(&self, message: u64, original: &str) -> String {
        self.texts.read().unwrap().get(&message).map_or_else(
            || original.to_owned(),
            |text| format!("{original}\n[voice transcript] {text}").trim_start().to_owned(),
        )
    }
}

/// Transcribe before gating/memory decisions, while preserving raw content for edit/delete validation.
pub async fn prepare(bot: &Arc<Bot>, message: &Message) -> Result<AiRequest> {
    if bot.config.disable_voice || bot.ai.is_none() {
        return Ok(bot.snapshot(message));
    }
    let channel = message.channel_id.get();
    let loaded = bot.voice.loaded_channels.lock().unwrap().contains(&channel);
    if !loaded {
        for (id, text) in db::channel_voice_transcripts(&bot.db, channel).await? {
            bot.voice.remember(id, text);
        }
        bot.voice.loaded_channels.lock().unwrap().insert(channel);
    }
    for msg in std::iter::once(message).chain(message.referenced_message.as_deref()) {
        if !msg.flags.is_some_and(|f| f.contains(MessageFlags::IS_VOICE_MESSAGE)) {
            continue;
        }
        if bot.voice.texts.read().unwrap().contains_key(&msg.id.get()) {
            continue;
        }
        let text = match transcript(bot, msg).await {
            Ok(text) => text,
            Err(error) => {
                tracing::warn!(target: "voice", msg_id = %msg.id, %error, "Voice transcription unavailable");
                UNAVAILABLE.to_owned()
            }
        };
        bot.voice.remember(msg.id.get(), text);
    }
    Ok(bot.snapshot(message))
}

async fn transcript(bot: &Arc<Bot>, message: &Message) -> Result<String> {
    let model = &bot.config.openrouter_stt_model;
    if model.is_empty() {
        bail!("transcription disabled");
    }
    let attachment = message.attachments.first().ok_or_else(|| eyre!("voice message has no attachment"))?;
    if db::claim_voice(&bot.db, message.id.get(), attachment.id.get(), message.channel_id.get(), model).await? {
        // Let the owner persist its result even if the gateway handler is cancelled mid-request.
        let (bot, msg) = (Arc::clone(bot), message.clone());
        tokio::spawn(async move {
            let result = super::timing::measure(msg.id, "stt", transcribe_attachment(&bot, &msg)).await;
            db::finish_voice(&bot.db, msg.id.get(), result.as_deref().ok()).await?;
            if let Ok(text) = &result {
                bot.voice.remember(msg.id.get(), text.clone());
            }
            result
        })
        .await??;
    }
    tokio::time::timeout(Duration::from_secs(100), async {
        loop {
            match db::voice_transcript(&bot.db, message.id.get()).await? {
                Some((status, text)) if status == "ready" => return Ok(text),
                Some((status, _)) if status == "failed" => bail!("previous transcription failed; clip is not rebilled"),
                Some(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                None => bail!("transcription claim missing"),
            }
        }
    })
    .await
    .map_err(|_| eyre!("transcription is pending or was interrupted; clip is not rebilled"))?
}

fn allowed_audio_url(raw: &str) -> bool {
    reqwest::Url::parse(raw).is_ok_and(|u| {
        u.scheme() == "https"
            && u.username().is_empty()
            && u.password().is_none()
            && u.port_or_known_default() == Some(443)
            && matches!(u.host_str(), Some("cdn.discordapp.com" | "media.discordapp.net"))
    })
}

async fn transcribe_attachment(bot: &Bot, message: &Message) -> Result<String> {
    let attachment = &message.attachments[0];
    if !allowed_audio_url(&attachment.url) {
        bail!("unsupported voice attachment host");
    }
    if attachment.size > 8 * 1024 * 1024 {
        bail!("voice attachment exceeds 8 MiB");
    }
    let response = bot.media.get(&attachment.url).send().await?.error_for_status()?;
    let bytes = openrouter::audio_bytes(response).await?;
    let pcm = ffmpeg::run(bytes, &[], &["-t", "121", "-vn", "-ac", "1", "-ar", "16000", "-f", "s16le"]).await?;
    if pcm.len() > 120 * 16000 * 2 {
        bail!("voice message exceeds two minutes");
    }
    let wav = ffmpeg::run(pcm, &["-f", "s16le", "-ac", "1", "-ar", "16000"], &["-f", "wav"]).await?;
    bot.ai
        .as_ref()
        .ok_or_else(|| eyre!("OpenRouter not configured"))?
        .transcribe(&bot.config.openrouter_stt_model, &wav)
        .await
}

pub struct VoiceClip {
    pub bytes: Vec<u8>,
    pub duration: f64,
    pub waveform: String,
    pub text: String,
}

/// Twilight 0.17's file helper cannot set per-file MIME types or voice metadata.
/// Keep its authenticated, rate-limited client, supplying only the missing multipart serialization.
pub fn upload_request(
    channel: u64,
    reply_to: Option<u64>,
    clip: &VoiceClip,
) -> Result<twilight_http::request::Request> {
    use reqwest::header::{CONTENT_TYPE, HeaderValue};
    use twilight_http::{request::Request, routing::Route};
    let boundary = format!("trickster-{:032x}", rand::random::<u128>());
    let mut payload = serde_json::json!({"flags": 8192, "allowed_mentions": {"parse": [], "replied_user": false},
        "attachments": [{"id": 0, "filename": "voice.ogg", "duration_secs": clip.duration, "waveform": clip.waveform}]});
    if let Some(source) = reply_to {
        payload["message_reference"] =
            serde_json::json!({"message_id": source.to_string(), "fail_if_not_exists": false});
    }
    let mut body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{payload}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"voice.ogg\"\r\nContent-Type: audio/ogg\r\n\r\n").into_bytes();
    body.extend_from_slice(&clip.bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok(Request::builder(&Route::CreateMessage { channel_id: channel })
        .body(body)
        .headers(std::iter::once((
            CONTENT_TYPE,
            HeaderValue::from_str(&format!("multipart/form-data; boundary={boundary}"))?,
        )))
        .build()?)
}

pub async fn synthesize(bot: &Bot, text: &str) -> Result<VoiceClip> {
    let bytes = bot
        .ai
        .as_ref()
        .ok_or_else(|| eyre!("OpenRouter not configured"))?
        .speech(&bot.config.openrouter_tts_model, bot.config.openrouter_tts_voice.as_deref(), text)
        .await?;
    encode(bytes, text.to_owned()).await
}

/// Convert through bounded mono PCM so duration/waveform describe the exact Ogg/Opus payload.
async fn encode(bytes: Vec<u8>, text: String) -> Result<VoiceClip> {
    let pcm = ffmpeg::run(bytes, &[], &["-t", "60", "-vn", "-ac", "1", "-ar", "48000", "-f", "s16le"]).await?;
    let duration = f64::from(u32::try_from(pcm.len() / 2)?) / 48000.0;
    let waveform = waveform(&pcm);
    let bytes = ffmpeg::run(
        pcm,
        &["-f", "s16le", "-ac", "1", "-ar", "48000"],
        &["-c:a", "libopus", "-b:a", "32k", "-f", "ogg"],
    )
    .await?;
    Ok(VoiceClip { bytes, duration, waveform, text })
}

fn waveform(pcm: &[u8]) -> String {
    let samples = pcm.len() / 2;
    let points = samples.div_ceil(4800).clamp(1, 256);
    let chunk = samples.div_ceil(points).max(1) * 2;
    let values: Vec<u8> = pcm
        .chunks(chunk)
        .take(256)
        .map(|part| {
            let (total, count) = part.as_chunks::<2>().0.iter().fold((0_u64, 0_u64), |(sum, n), s| {
                (sum + u64::from(i16::from_le_bytes([s[0], s[1]]).unsigned_abs()), n + 1)
            });
            u8::try_from((total / count.max(1)) * 255 / 32768).unwrap_or(255)
        })
        .collect();
    STANDARD.encode(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn waveform_is_bounded_and_silence_stays_zero() {
        assert_eq!(STANDARD.decode(waveform(&vec![0; 96000])).unwrap(), vec![0; 10]);
        let loud: Vec<_> = std::iter::repeat_n(i16::MAX.to_le_bytes(), 48000 * 60).flatten().collect();
        let values = STANDARD.decode(waveform(&loud)).unwrap();
        assert_eq!(values.len(), 256);
        assert!(values.iter().all(|x| *x >= 254));
    }
    #[test]
    fn native_upload_contains_voice_metadata_mime_and_only_optional_reply_reference() {
        let clip = VoiceClip {
            bytes: b"OggS-fixture".to_vec(),
            duration: 1.25,
            waveform: "AAEC".into(),
            text: "hello".into(),
        };
        for reply in [None, Some(42)] {
            let request = upload_request(1, reply, &clip).unwrap();
            let body = String::from_utf8(request.body().unwrap().to_vec()).unwrap();
            assert!(body.contains("Content-Type: audio/ogg"));
            assert!(body.contains("filename=\"voice.ogg\""));
            let json =
                body.split("Content-Type: application/json\r\n\r\n").nth(1).unwrap().split("\r\n--").next().unwrap();
            let json: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(json["flags"], 8192);
            assert_eq!(json["attachments"][0]["duration_secs"], 1.25);
            assert_eq!(json["attachments"][0]["waveform"], "AAEC");
            assert!(json.get("content").is_none());
            assert_eq!(json.get("message_reference").is_some(), reply.is_some());
            assert!(
                request.headers().unwrap()[reqwest::header::CONTENT_TYPE]
                    .to_str()
                    .unwrap()
                    .starts_with("multipart/form-data; boundary=")
            );
        }
    }

    #[test]
    fn attachment_downloads_only_use_discord_https() {
        assert!(allowed_audio_url("https://cdn.discordapp.com/attachments/1/voice.ogg?x=1"));
        for url in [
            "http://cdn.discordapp.com/a",
            "https://cdn.discordapp.com.evil.test/a",
            "https://user:pass@cdn.discordapp.com/a",
            "http://127.0.0.1/a",
        ] {
            assert!(!allowed_audio_url(url));
        }
    }
    #[tokio::test]
    async fn transcript_content_does_not_replace_raw_gateway_text() {
        let bot = crate::bot::tests::test_bot();
        let msg = crate::bot::tests::message(10, 2, "alice", "caption");
        bot.voice.remember(10, "hello from audio".into());
        bot.cache.update(&twilight_gateway::Event::MessageCreate(Box::new(
            twilight_model::gateway::payload::incoming::MessageCreate(msg.clone()),
        )));
        let req = bot.snapshot(&msg);
        assert!(
            super::super::reply::is_current(&bot, &req),
            "transcription must not invalidate a raw gateway snapshot"
        );
        assert_eq!(req.source_text, "caption");
        assert!(req.message.contains("hello from audio"));
    }
    #[tokio::test]
    #[ignore = "calls the selected live TTS and STT APIs on synthetic text; no Discord posts"]
    async fn live_selected_speech_models_round_trip_native_audio() {
        let mut bot = crate::bot::tests::test_bot();
        bot.config.openrouter_api_key = Some(std::env::var("OPENROUTER_API_KEY").expect("API key"));
        bot.config.openrouter_base_url = "https://openrouter.ai/api/v1".into();
        bot.config.openrouter_tts_model = "fish-audio/s1-20260729".into();
        bot.config.openrouter_stt_model = "mistralai/voxtral-small-transcribe-3-0".into();
        bot.ai = super::super::openrouter::OpenRouter::from_config(bot.web.clone(), &bot.config);
        let clip = synthesize(&bot, "The server is offline. Please try again later.").await.unwrap();
        assert!(clip.bytes.starts_with(b"OggS"));
        let wav = ffmpeg::run(clip.bytes.clone(), &[], &["-ac", "1", "-ar", "16000", "-f", "wav"]).await.unwrap();
        let text = bot.ai.as_ref().unwrap().transcribe(&bot.config.openrouter_stt_model, &wav).await.unwrap();
        println!(
            "{}",
            serde_json::json!({"transcript":text,"duration_secs":clip.duration,"ogg_bytes":clip.bytes.len(),"waveform_points":STANDARD.decode(&clip.waveform).unwrap().len()})
        );
        assert!(text.to_lowercase().contains("offline"));
        std::fs::write("/tmp/trickster-native-voice-preview.ogg", clip.bytes).unwrap();
    }

    #[tokio::test]
    async fn encoding_produces_native_opus_with_duration_and_waveform() {
        let pcm: Vec<_> = std::iter::repeat_n(2000_i16.to_le_bytes(), 48000).flatten().collect();
        let wav = ffmpeg::run(pcm, &["-f", "s16le", "-ac", "1", "-ar", "48000"], &["-f", "wav"]).await.unwrap();
        let clip = encode(wav, "test".into()).await.unwrap();
        assert!(clip.bytes.starts_with(b"OggS"));
        assert!(clip.bytes.windows(8).any(|w| w == b"OpusHead"));
        assert!((clip.duration - 1.0).abs() < 0.02);
        assert_eq!(STANDARD.decode(clip.waveform).unwrap().len(), 10);
    }
}
