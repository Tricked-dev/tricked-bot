//! Cleanup of what the model sends back, before it is shown in Discord.

use super::PERSONA;

/// Discord rejects messages longer than this many UTF-16 code units.
const DISCORD_LIMIT: usize = 2000;

/// Turns the accumulated model output into the text to show, or `None` while there is nothing presentable.
pub fn presentable(accumulated: &str) -> Option<String> {
    if accumulated.is_empty() || is_classifier_output(accumulated) {
        return None;
    }
    Some(discord_text(&casual_case(&strip_self_labels(accumulated))))
}

/// Truncates to Discord's limit, which counts UTF-16 code units (two for non-BMP characters).
pub fn discord_text(text: &str) -> String {
    let mut units = 0;
    text.chars()
        .take_while(|ch| {
            units += ch.len_utf16();
            units <= DISCORD_LIMIT
        })
        .collect()
}

/// Some models answer with a safety classification instead of a reply.
fn is_classifier_output(text: &str) -> bool {
    let normalized = text.trim_start().to_ascii_lowercase();
    normalized.starts_with("user safety:")
        || normalized.starts_with("safety categories:")
        || (normalized.contains("user safety:") && normalized.contains("safety categories:"))
}

/// The line without a leading `The Trickster:`, `**The Trickster:**` or `**The Trickster**:` label, in any case.
fn strip_self_label(line: &str) -> Option<&str> {
    let line = line.trim_start();
    let (bold, line) = line.strip_prefix("**").map_or((false, line), |rest| (true, rest));
    let rest =
        line.get(..PERSONA.len()).filter(|head| head.eq_ignore_ascii_case(PERSONA)).map(|_| &line[PERSONA.len()..])?;
    let rest =
        if bold { rest.strip_prefix(":**").or_else(|| rest.strip_prefix("**:")) } else { rest.strip_prefix(':') };
    rest.map(str::trim_start)
}

/// Drops self labels and turns em dashes, a tell of model-written text, into commas.
fn strip_self_labels(text: &str) -> String {
    text.lines()
        .map(|line| strip_self_label(line).unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .replace(" — ", ", ")
        .replace("— ", ", ")
        .replace(" —", ", ")
        .replace('—', ", ")
}

/// Discord shitposters don't capitalize or end on a full stop. Code and links keep their case.
fn casual_case(text: &str) -> String {
    if text.contains('`') || text.contains("http") {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    match lower.strip_suffix('.') {
        Some(rest) if !rest.ends_with('.') => rest.to_owned(),
        _ => lower,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casual_case_lowercases_prose_but_not_code_or_links() {
        assert_eq!(casual_case("Still typing. Fine, you asked."), "still typing. fine, you asked");
        assert_eq!(casual_case("Wait..."), "wait...");
        assert_eq!(casual_case("Use `Vec::new()`."), "Use `Vec::new()`.");
        assert_eq!(casual_case("See https://Example.com"), "See https://Example.com");
    }

    #[test]
    fn bounds_discord_utf16_without_splitting_unicode() {
        assert_eq!(discord_text("Hi"), "Hi");
        assert_eq!(discord_text(&"a".repeat(2100)).len(), 2000);
        assert_eq!(discord_text(&"😀".repeat(1100)).chars().count(), 1000);
        assert_eq!(discord_text(&("a".repeat(1999) + "😀")), "a".repeat(1999));
    }

    #[test]
    fn presentable_hides_classifier_output_and_self_labels() {
        assert_eq!(presentable(""), None);
        assert_eq!(presentable("User safety: safe\nSafety categories: none"), None);
        assert_eq!(presentable("The Trickster: Skill issue."), Some("skill issue".into()));
        assert_eq!(presentable("**the trickster:** ok\n**THE TRICKSTER**: fine").as_deref(), Some("ok\nfine"));
        assert_eq!(presentable("The Tricksters: hi").as_deref(), Some("the tricksters: hi"));
        assert_eq!(presentable("a — b").as_deref(), Some("a, b"));
    }
}
