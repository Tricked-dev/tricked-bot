use color_eyre::{eyre::eyre, Result};
use openrouter_api::types::chat::{ChatCompletionRequest, MessageContent};
use serde_json::Value;

// Count UTF-8 bytes rather than characters/4: this deliberately overestimates
// text tokens, including Unicode and the newer Claude tokenizer. Leave room for
// provider message framing and reserve the entire possible completion.
const FRAMING_RESERVE: u64 = 2048;
// Claude 4.7+ caps each image at 4784 visual tokens. Our media pipeline also
// limits images to 1600px and four images per request. Base64 is transport data,
// not text fed to the tokenizer.
const CLAUDE_IMAGE_RESERVE: u64 = 4800;

fn estimate(request: &Value) -> Result<u64> {
    let output = request["max_tokens"]
        .as_u64()
        .ok_or_else(|| eyre!("AI request needs an explicit output-token limit"))?;
    let claude = request["model"].as_str().is_some_and(|m| m.starts_with("anthropic/"));
    let mut text_request = request.clone();
    let mut image_tokens: u64 = 0;
    for message in text_request["messages"]
        .as_array_mut()
        .ok_or_else(|| eyre!("AI request needs messages"))?
    {
        if let Some(parts) = message["content"].as_array_mut() {
            for part in parts {
                match part["type"].as_str() {
                    Some("text") => {}
                    Some("image_url") => {
                        image_tokens = image_tokens.saturating_add(if claude { CLAUDE_IMAGE_RESERVE } else { 16_384 });
                        part["image_url"]["url"] = Value::String(String::new());
                    }
                    _ => return Err(eyre!("AI budget cannot safely count this media type")),
                }
            }
        }
    }
    let total = (serde_json::to_vec(&text_request)?.len() as u64)
        .saturating_add(image_tokens)
        .saturating_add(FRAMING_RESERVE)
        .saturating_add(output);
    Ok(total)
}

pub fn check(request: &Value, max_total: u32) -> Result<u64> {
    let total = estimate(request)?;
    if total > u64::from(max_total) {
        return Err(eyre!(
            "AI request blocked by cost limit: conservative prompt + output budget {total} exceeds {max_total}"
        ));
    }
    Ok(total)
}

/// Trim optional background before the newest conversation. The persona and
/// current user message stay intact; stored memories are never modified.
pub fn fit_chat(request: &mut ChatCompletionRequest, max_total: u32) -> Result<u64> {
    loop {
        let value = serde_json::to_value(&*request)?;
        let total = estimate(&value)?;
        if total <= u64::from(max_total) {
            return Ok(total);
        }
        let excess = (total - u64::from(max_total)) as usize;
        let mut trimmed = false;
        for message in &mut request.messages {
            if message.role != "system" {
                continue;
            }
            let MessageContent::Text(prompt) = &mut message.content else {
                continue;
            };
            for tag in ["memories", "past_exchanges", "relationships", "recent_conversation"] {
                let open = format!("<{tag}>");
                let close = format!("</{tag}>");
                let Some(start) = prompt.find(&open).map(|p| p + open.len()) else {
                    continue;
                };
                let Some(end) = prompt[start..].find(&close).map(|p| p + start) else {
                    continue;
                };
                if start == end {
                    continue;
                }
                let content = &prompt[start..end];
                if tag == "recent_conversation" {
                    // Remove complete oldest transcript lines where possible.
                    let cut = content.ceil_char_boundary(excess.min(content.len()));
                    let cut = content[cut..].find('\n').map(|n| cut + n + 1).unwrap_or(cut);
                    prompt.replace_range(start..start + cut, "");
                } else {
                    let keep = content.floor_char_boundary(content.len().saturating_sub(excess));
                    prompt.replace_range(start + keep..end, "");
                }
                trimmed = true;
                break;
            }
            if trimmed {
                break;
            }
        }
        if !trimmed {
            // Normal Discord messages and the fixed persona fit comfortably;
            // retain a final check for malformed or externally oversized inputs.
            return check(&value, max_total);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_all_prompt_sections_and_reserves_the_completion() {
        let request = json!({"model":"anthropic/claude-haiku-5.5", "max_tokens":1024,
            "messages":[{"role":"system","content":"x".repeat(87_000)},
                        {"role":"user","content":"こんにちは🙂".repeat(100)}]});
        assert!(check(&request, 90_000).is_err());
        let mut reduced = request;
        reduced["messages"][0]["content"] = json!("x".repeat(80_000));
        let upper = check(&reduced, 90_000).unwrap();
        assert!(check(&reduced, upper as u32 - 1).is_err());
        assert!(check(&reduced, upper as u32).is_ok());
        reduced["max_tokens"] = json!(10_000);
        assert!(check(&reduced, 90_000).is_err());
    }

    #[test]
    fn images_count_as_visual_tokens_and_can_push_a_request_over_budget() {
        let mut request = json!({"model":"anthropic/claude-haiku-5.5", "max_tokens":1024,
            "messages":[{"role":"system","content":"x".repeat(80_000)},
                {"role":"user","content":[{"type":"text","text":"describe these"},
                    {"type":"image_url","image_url":{"url":"data:image/png;base64,".to_owned()+&"a".repeat(1_000_000)}}]}]});
        assert!(check(&request, 90_000).is_ok());
        request["messages"][1]["content"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"image_url","image_url":{"url":"data:image/png;base64,test"}}));
        assert!(check(&request, 90_000).is_err());
    }

    #[test]
    fn unbounded_output_and_unknown_media_fail_closed() {
        assert!(check(&json!({"messages":[]}), 90_000).is_err());
        assert!(check(
            &json!({"max_tokens":1024,"messages":[{"content":[{"type":"file_url"}]}]}),
            90_000
        )
        .is_err());
    }

    #[test]
    fn oversized_chat_keeps_persona_current_message_and_newest_context() {
        use openrouter_api::types::chat::Message;
        let mut request = ChatCompletionRequest {
            model: "anthropic/claude-haiku-5.5".into(),
            max_tokens: Some(1024),
            messages: vec![
                Message::text("system",format!("PERSONA\n<memories>{}</memories>\n<recent_conversation>{}\nThe Trickster: newest reply</recent_conversation>\nREPLY IN CHARACTER",
                    "🙂".repeat(30_000), "old message\n".repeat(10_000))),
                Message::text("user","what did you mean by that?"),
            ],
            ..Default::default()
        };
        let before = serde_json::to_value(&request).unwrap();
        assert!(check(&before, 90_000).is_err());
        assert!(fit_chat(&mut request, 90_000).unwrap() <= 90_000);
        let MessageContent::Text(prompt) = &request.messages[0].content else {
            panic!()
        };
        assert!(prompt.starts_with("PERSONA"));
        assert!(prompt.ends_with("REPLY IN CHARACTER"));
        assert!(prompt.contains("The Trickster: newest reply"));
        assert_eq!(
            request.messages[1].content,
            MessageContent::Text("what did you mean by that?".into())
        );
        assert!(check(&serde_json::to_value(&request).unwrap(), 90_000).is_ok());
        assert_eq!(
            before["messages"][0]["content"].as_str().unwrap().matches("🙂").count(),
            30_000
        );
    }
}
