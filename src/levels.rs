#[expect(clippy::cast_possible_truncation, reason = "the curve stays far below i32::MAX for any reachable level")]
pub fn xp_required_for_level(level: i32) -> i32 {
    (20.0 * f64::from(level).powf(1.3)) as i32
}

/// Renders `xp / required` as a 20-cell progress bar.
#[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the ratio is non-negative and clamped")]
pub fn progress_bar(xp: i32, required: i32) -> String {
    const CELLS: usize = 20;
    let filled =
        if required <= 0 { CELLS } else { (f64::from(xp.max(0)) / f64::from(required) * 20.0).min(20.0) as usize };
    format!("{}{}", "█".repeat(filled), "░".repeat(CELLS - filled))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_curve_matches_the_original_formula() {
        let curve = [0, 1, 5, 10, 100].map(xp_required_for_level);
        assert_eq!(curve, [0, 20, 162, 399, 7962]);
    }

    #[test]
    fn progress_bar_is_always_twenty_cells() {
        assert_eq!(progress_bar(0, 100), "░".repeat(20));
        assert_eq!(progress_bar(50, 100), format!("{}{}", "█".repeat(10), "░".repeat(10)));
        assert_eq!(progress_bar(500, 100), "█".repeat(20));
        assert_eq!(progress_bar(3, 0), "█".repeat(20));
    }
}
