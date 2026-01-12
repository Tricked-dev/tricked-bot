#![allow(clippy::unused_unit)]

use std::sync::Arc;

use tokio::sync::Mutex;
use vesper::prelude::*;

use crate::structs::State;

#[command]
#[description = "Translate text between languages (uses Google Translate API)"]
pub async fn translate(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Source language code (en, es, fr, de, pl, ja, zh, ru, it, pt, auto)"] from: String,
    #[description = "Target language code (en, es, fr, de, pl, ja, zh, ru, it, pt)"] to: String,
    #[description = "Text to translate"] text: String,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let client = &state.client;

    // Using Google Translate's public API (free tier)
    let url = format!(
        "https://translate.googleapis.com/translate_a/single?client=gtx&sl={}&tl={}&dt=t&q={}",
        from,
        to,
        urlencoding::encode(&text)
    );

    let response = match client.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            ctx.interaction_client
                .create_response(
                    ctx.interaction.id,
                    &ctx.interaction.token,
                    &vesper::twilight_exports::InteractionResponse {
                        kind: vesper::twilight_exports::InteractionResponseType::ChannelMessageWithSource,
                        data: Some(vesper::twilight_exports::InteractionResponseData {
                            content: Some(format!("Failed to fetch translation: {}", e)),
                            ..Default::default()
                        }),
                    },
                )
                .await?;
            return Ok(());
        }
    };

    let json: serde_json::Value = match response.json().await {
        Ok(j) => j,
        Err(e) => {
            ctx.interaction_client
                .create_response(
                    ctx.interaction.id,
                    &ctx.interaction.token,
                    &vesper::twilight_exports::InteractionResponse {
                        kind: vesper::twilight_exports::InteractionResponseType::ChannelMessageWithSource,
                        data: Some(vesper::twilight_exports::InteractionResponseData {
                            content: Some(format!("Failed to parse translation response: {}", e)),
                            ..Default::default()
                        }),
                    },
                )
                .await?;
            return Ok(());
        }
    };

    // Parse the translation from the response
    let mut translated_text = String::new();
    if let Some(translations) = json.get(0).and_then(|v| v.as_array()) {
        for translation in translations {
            if let Some(text) = translation.get(0).and_then(|v| v.as_str()) {
                translated_text.push_str(text);
            }
        }
    }

    if translated_text.is_empty() {
        translated_text = "Translation failed or returned empty result".to_string();
    }

    let from_lang = if from == "auto" {
        "Auto Detected".to_string()
    } else {
        from.to_uppercase()
    };

    let message = format!(
        "🌐 **Translation** ({} → {})\n\n**Original:**\n{}\n\n**Translated:**\n{}",
        from_lang,
        to.to_uppercase(),
        text,
        translated_text
    );

    ctx.interaction_client
        .create_response(
            ctx.interaction.id,
            &ctx.interaction.token,
            &vesper::twilight_exports::InteractionResponse {
                kind: vesper::twilight_exports::InteractionResponseType::ChannelMessageWithSource,
                data: Some(vesper::twilight_exports::InteractionResponseData {
                    content: Some(message),
                    ..Default::default()
                }),
            },
        )
        .await?;

    Ok(())
}
