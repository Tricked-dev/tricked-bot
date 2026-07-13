const COMMAND_PREFIX: &str = "t!ratewaifu";

/// Returns the candidate text when a message is a `t!ratewaifu` command.
pub fn parse_command(content: &str) -> Option<&str> {
    let prefix = content.get(..COMMAND_PREFIX.len())?;
    if !prefix.eq_ignore_ascii_case(COMMAND_PREFIX) {
        return None;
    }

    let rest = &content[COMMAND_PREFIX.len()..];
    if rest.chars().next().is_some_and(|character| !character.is_whitespace()) {
        return None;
    }

    Some(rest.trim())
}

/// Produces a deterministic, random-looking integer from 0 through 10.
pub fn score(candidate: &str) -> u8 {
    let normalized = candidate
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    // FNV-1a is deliberately used instead of a runtime-seeded hasher so the
    // same candidate gets the same score across messages and bot restarts.
    let hash = normalized.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });

    (hash % 11) as u8
}

pub fn fallback_explanation(score: u8) -> &'static str {
    match score {
        0..=2 => "The appeal is currently more theoretical than tangible.",
        3..=4 => "There is a spark here, though it is competing with several questionable choices.",
        5..=6 => "A perfectly respectable showing: charming, with room for improvement.",
        7..=8 => "Strong waifu energy, supported by a compelling blend of charm and style.",
        9 => "Nearly flawless; only the most pedantic critic could object.",
        10 => "An exalted specimen of waifu excellence—try not to let the power go to your head.",
        _ => "The rating defies categorisation, which is impressive in its own way.",
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_command, score};

    #[test]
    fn score_is_stable_and_in_range() {
        let first = score("Asuna Yuuki");

        assert_eq!(first, score("Asuna Yuuki"));
        assert_eq!(first, score("  asuna   yuuki  "));
        assert!(first <= 10);
    }

    #[test]
    fn command_parser_requires_a_boundary_after_the_command_name() {
        assert_eq!(parse_command("t!ratewaifu Rem"), Some("Rem"));
        assert_eq!(parse_command("T!RATEWAIFU Rem"), Some("Rem"));
        assert_eq!(parse_command("t!ratewaifux Rem"), None);
        assert_eq!(parse_command("t!ratewaifu"), Some(""));
    }
}
