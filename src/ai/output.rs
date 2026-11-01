//! Cleanup of what the model sends back, before it is shown in Discord, and the directives hidden in it.

use std::time::Duration;

use super::PERSONA;

/// Discord rejects messages longer than this many UTF-16 code units.
const DISCORD_LIMIT: usize = 2000;

/// Shortest and longest wait, in hours, the model may ask for before checking back on a prediction.
const PREDICTION_HOURS: (u64, u64) = (1, 30 * 24);

/// What a reply asks for besides its text, on lines of their own: `[gif: query]` and `[predict 2w: claim]`.
#[derive(Debug, Default, PartialEq)]
pub struct Directives {
    pub gif: Option<String>,
    pub prediction: Option<Prediction>,
}

/// A prediction to check back on after `wait`.
#[derive(Debug, PartialEq)]
pub struct Prediction {
    pub wait: Duration,
    pub claim: String,
}

/// A `[name: value]` or `[name arg: value]` directive line, split into its head and value.
fn directive(line: &str) -> Option<(&str, &str)> {
    let (head, value) = line.trim().strip_prefix('[')?.strip_suffix(']')?.split_once(':')?;
    let head = head.trim();
    (head == "gif" || head.starts_with("predict ")).then(|| (head, value.trim()))
}

/// Whether `line` is, or while streaming may still become, a directive.
fn is_directive_line(line: &str) -> bool {
    let line = line.trim_start();
    directive(line).is_some() || (line.starts_with('[') && !line.contains(']'))
}

/// `2w`, `3d` or `12h`, clamped to [`PREDICTION_HOURS`].
fn parse_wait(text: &str) -> Option<Duration> {
    let text = text.trim();
    let hours = match text.chars().last()? {
        'h' => 1,
        'd' => 24,
        'w' => 7 * 24,
        _ => return None,
    };
    let count: u64 = text[..text.len() - 1].parse().ok()?;
    Some(Duration::from_hours(count.saturating_mul(hours).clamp(PREDICTION_HOURS.0, PREDICTION_HOURS.1)))
}

/// The directives in a finished reply. Malformed ones are ignored.
pub fn directives(reply: &str) -> Directives {
    let mut found = Directives::default();
    for (head, value) in reply.lines().filter_map(directive).filter(|(_, value)| !value.is_empty()) {
        if head == "gif" {
            found.gif = Some(value.to_owned());
        } else if let Some(wait) = head.strip_prefix("predict ").and_then(parse_wait) {
            found.prediction = Some(Prediction { wait, claim: value.to_owned() });
        }
    }
    found
}

/// The reply without its directive lines, including one that is still streaming in.
fn without_directives(text: &str) -> String {
    text.lines().filter(|line| !is_directive_line(line)).collect::<Vec<_>>().join("\n")
}

/// Turns the accumulated model output into the text to show, or `None` while there is nothing presentable.
pub fn presentable(accumulated: &str) -> Option<String> {
    if accumulated.is_empty() || is_classifier_output(accumulated) {
        return None;
    }
    let text = discord_text(&casual_case(&strip_self_labels(&without_directives(accumulated))));
    (!text.trim().is_empty()).then_some(text)
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
    fn directives_are_parsed_and_hidden_even_while_streaming() {
        let reply = "skill issue\n[gif: facepalm]\n[predict 2w: back on windows]";
        assert_eq!(
            directives(reply),
            Directives {
                gif: Some("facepalm".into()),
                prediction: Some(Prediction { wait: Duration::from_hours(14 * 24), claim: "back on windows".into() }),
            }
        );
        assert_eq!(presentable(reply).as_deref(), Some("skill issue"));
        assert_eq!(presentable("skill issue\n[gi").as_deref(), Some("skill issue"));
        assert_eq!(presentable("[gif: smug]"), None);
        assert_eq!(presentable("[citation needed] lol").as_deref(), Some("[citation needed] lol"));
    }

    #[test]
    fn malformed_directives_and_waits_are_ignored_or_clamped() {
        assert_eq!(directives("[gif: ]\n[predict soon: x]\n[predict 2y: x]"), Directives::default());
        assert_eq!(directives("[predict 90d: x]").prediction.unwrap().wait, Duration::from_hours(PREDICTION_HOURS.1));
        assert_eq!(directives("[predict 0h: x]").prediction.unwrap().wait, Duration::from_hours(PREDICTION_HOURS.0));
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
