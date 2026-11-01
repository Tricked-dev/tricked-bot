//! Token estimates that keep each AI request under its cost target.

use std::io;

use color_eyre::{Result, eyre::bail};

use super::openrouter::{ChatRequest, Content, Part};

// Estimate text at three UTF-8 bytes per token to allow for Claude's newer
// tokenizer. The 90k default leaves 10% headroom below the 100k pricing tier.
// Unusual content can exceed the estimate. Reserve framing and completion too.
const BYTES_PER_TOKEN: u64 = 3;
const FRAMING_RESERVE: u64 = 2048;
// Claude 4.7+ caps each image at 4784 visual tokens. Our media pipeline also
// limits images to 1600px and four images per request. Base64 is transport data,
// not text fed to the tokenizer.
const CLAUDE_IMAGE_RESERVE: u64 = 4800;
const OTHER_IMAGE_RESERVE: u64 = 16_384;

/// An `io::Write` that only counts, so measuring serialized size allocates nothing.
#[derive(Default)]
struct ByteCount(u64);

impl io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_len(value: &impl serde::Serialize) -> u64 {
    let mut count = ByteCount::default();
    // Writing to a counter can't fail, and neither can serializing strings and plain structs.
    serde_json::to_writer(&mut count, value).expect("request serializes");
    count.0
}

/// Length of `text` inside a JSON string, escapes included.
pub fn json_len(text: &str) -> u64 {
    serialized_len(&text) - 2
}

/// Estimated prompt and output tokens of `request`. Images count as their visual token cost, not their base64 size.
pub fn estimate(request: &ChatRequest) -> u64 {
    let images: Vec<&str> = request
        .messages
        .iter()
        .filter_map(|message| match &message.content {
            Content::Parts(parts) => Some(parts),
            Content::Text(_) => None,
        })
        .flatten()
        .filter_map(|part| match part {
            Part::ImageUrl { image_url } => Some(image_url.url.as_str()),
            Part::Text { .. } => None,
        })
        .collect();
    let reserve = if request.model.starts_with("anthropic/") { CLAUDE_IMAGE_RESERVE } else { OTHER_IMAGE_RESERVE };
    let text = serialized_len(request) - images.iter().map(|url| json_len(url)).sum::<u64>();
    text.div_ceil(BYTES_PER_TOKEN) + reserve * images.len() as u64 + FRAMING_RESERVE + u64::from(request.max_tokens)
}

/// Takes `cost` bytes from `spare` if it has that many left. Returns whether it did.
pub fn spend(spare: &mut u64, cost: u64) -> bool {
    spare.checked_sub(cost).inspect(|left| *spare = *left).is_some()
}

/// The longest prefix of `text` whose JSON-escaped length fits in `spare`, which it takes from `spare`.
pub fn fitting_prefix<'a>(text: &'a str, spare: &mut u64) -> &'a str {
    let mut end = 0;
    for (start, ch) in text.char_indices() {
        // Control characters escape to at most six bytes (`\u001f`).
        let cost = match ch {
            '"' | '\\' => 2,
            ch if ch.is_ascii_control() => 6,
            ch => ch.len_utf8() as u64,
        };
        if !spend(spare, cost) {
            break;
        }
        end = start + ch.len_utf8();
    }
    &text[..end]
}

/// Bytes of JSON text that can still be added to `request` without exceeding `max_total` tokens.
pub fn spare_bytes(request: &ChatRequest, max_total: u32) -> u64 {
    u64::from(max_total).saturating_sub(estimate(request)) * BYTES_PER_TOKEN
}

/// The estimate for `request`, or an error when it exceeds `max_total` tokens.
pub fn check(request: &ChatRequest, max_total: u32) -> Result<u64> {
    let total = estimate(request);
    if total > u64::from(max_total) {
        bail!("AI request exceeds cost target: estimated prompt + output budget {total} exceeds {max_total}");
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::openrouter::{ImageUrl, Message, Role};

    fn request(system: String, user: Content) -> ChatRequest {
        ChatRequest {
            model: "anthropic/claude-haiku-5.5".into(),
            messages: vec![Message::new(Role::System, system), Message { role: Role::User, content: user }],
            max_tokens: 1024,
            ..Default::default()
        }
    }

    fn image(base64: &str) -> Part {
        Part::ImageUrl { image_url: ImageUrl { url: format!("data:image/png;base64,{base64}") } }
    }

    #[test]
    fn counts_all_text_and_reserves_the_completion() {
        let mut request = request("x".repeat(240_000), Content::Text("こんにちは🙂".repeat(100)));
        let total = check(&request, 90_000).unwrap();
        assert!(check(&request, u32::try_from(total).unwrap() - 1).is_err());
        assert_eq!(spare_bytes(&request, u32::try_from(total).unwrap() + 10), 30);
        request.max_tokens = 10_000;
        assert!(check(&request, 90_000).is_err());
        assert_eq!(json_len("a\"\n"), 5);
    }

    #[test]
    fn fitting_prefix_counts_escapes_and_keeps_whole_characters() {
        let mut spare = 5;
        assert_eq!(fitting_prefix("a\"🙂b", &mut spare), "a\"");
        assert_eq!(spare, 2);
        let mut spare = 100;
        assert_eq!(fitting_prefix("short", &mut spare), "short");
        assert_eq!(spare, 95);
    }

    #[test]
    fn images_count_as_visual_tokens_and_can_push_a_request_over_budget() {
        let text = Part::text("describe these");
        let one = request("x".repeat(240_000), Content::Parts(vec![text.clone(), image(&"a".repeat(1_000_000))]));
        assert!(check(&one, 90_000).is_ok());
        let two = request("x".repeat(240_000), Content::Parts(vec![text, image(&"a".repeat(1_000_000)), image("b")]));
        // The second image's base64 is free; only its JSON framing counts as text.
        assert!((CLAUDE_IMAGE_RESERVE..CLAUDE_IMAGE_RESERVE + 20).contains(&(estimate(&two) - estimate(&one))));
        assert!(check(&two, 90_000).is_err());
    }
}
