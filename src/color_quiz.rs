use png::{BitDepth, ColorType, Encoder};
use rand::Rng;
use std::io::Cursor;

#[derive(Debug, Clone)]
pub struct ColorQuiz {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ColorQuiz {
    /// Generate a random color quiz
    pub fn generate<R: Rng>(rng: &mut R) -> Self {
        Self {
            r: rng.gen_range(0..=255),
            g: rng.gen_range(0..=255),
            b: rng.gen_range(0..=255),
        }
    }

    /// Generate a 16:9 image (640x360) with the color
    pub fn generate_image(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let width = 640u32;
        let height = 360u32;

        let mut buffer = Cursor::new(Vec::new());

        {
            let mut encoder = Encoder::new(&mut buffer, width, height);
            encoder.set_color(ColorType::Rgb);
            encoder.set_depth(BitDepth::Eight);

            let mut writer = encoder.write_header()?;

            // Create image data - each pixel is RGB (3 bytes)
            let pixel_count = (width * height) as usize;
            let mut data = Vec::with_capacity(pixel_count * 3);

            for _ in 0..pixel_count {
                data.push(self.r);
                data.push(self.g);
                data.push(self.b);
            }

            writer.write_image_data(&data)?;
        }

        Ok(buffer.into_inner())
    }

    /// Validate user's answer in hex or oklch format
    pub fn validate_answer(&self, user_answer: &str) -> bool {
        let answer = user_answer.trim();

        // Try hex format first
        if answer.starts_with('#') {
            return self.validate_hex(answer);
        }

        // Try oklch format
        self.validate_oklch(answer)
    }

    /// Validate hex color format: #RRGGBB
    /// Tolerance: total difference of 20 across all channels
    fn validate_hex(&self, hex: &str) -> bool {
        // Remove the # and parse
        let hex = hex.trim_start_matches('#');

        if hex.len() != 6 {
            return false;
        }

        let r = u8::from_str_radix(&hex[0..2], 16).ok();
        let g = u8::from_str_radix(&hex[2..4], 16).ok();
        let b = u8::from_str_radix(&hex[4..6], 16).ok();

        if let (Some(r), Some(g), Some(b)) = (r, g, b) {
            let diff = (self.r as i32 - r as i32).abs()
                + (self.g as i32 - g as i32).abs()
                + (self.b as i32 - b as i32).abs();

            return diff <= 20;
        }

        false
    }

    /// Validate oklch color format
    /// Supports: oklch(45.0% 0.306 65.4), 45.0% 0.306 65.4, 45.0%, 0.306, 65.4
    /// Converts RGB to OKLCH and checks similarity
    fn validate_oklch(&self, input: &str) -> bool {
        // Parse the input
        let parsed = self.parse_oklch(input);
        if parsed.is_none() {
            return false;
        }

        let (l_user, c_user, h_user) = parsed.unwrap();

        // Convert our RGB to OKLCH
        let (l_actual, c_actual, h_actual) = rgb_to_oklch(self.r, self.g, self.b);

        // Check if close enough
        // L: within 5% (0-100 scale)
        // C: within 0.05
        // H: within 10 degrees (handle wrap-around at 360)
        let l_diff = (l_actual - l_user).abs();
        let c_diff = (c_actual - c_user).abs();
        let h_diff = {
            let diff = (h_actual - h_user).abs();
            diff.min(360.0 - diff) // Handle wrap-around
        };

        l_diff <= 5.0 && c_diff <= 0.05 && h_diff <= 10.0
    }

    /// Parse OKLCH from various formats
    fn parse_oklch(&self, input: &str) -> Option<(f64, f64, f64)> {
        let input = input.trim();

        // Remove oklch( and ) if present
        let inner = input
            .strip_prefix("oklch(")
            .and_then(|s| s.strip_suffix(')'))
            .unwrap_or(input);

        // Split by whitespace or comma
        let parts: Vec<&str> = inner
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .collect();

        if parts.len() != 3 {
            return None;
        }

        // Parse L (can have % suffix)
        let l = parts[0].trim_end_matches('%').parse::<f64>().ok()?;

        // Parse C
        let c = parts[1].parse::<f64>().ok()?;

        // Parse H
        let h = parts[2].parse::<f64>().ok()?;

        Some((l, c, h))
    }
}

/// Convert RGB to OKLCH color space
/// This is a simplified approximation
fn rgb_to_oklch(r: u8, g: u8, b: u8) -> (f64, f64, f64) {
    // First convert to linear RGB
    let r_linear = srgb_to_linear(r as f64 / 255.0);
    let g_linear = srgb_to_linear(g as f64 / 255.0);
    let b_linear = srgb_to_linear(b as f64 / 255.0);

    // Convert to OKLab using the matrix transformation
    let l = 0.4122214708 * r_linear + 0.5363325363 * g_linear + 0.0514459929 * b_linear;
    let m = 0.2119034982 * r_linear + 0.6806995451 * g_linear + 0.1073969566 * b_linear;
    let s = 0.0883024619 * r_linear + 0.2817188376 * g_linear + 0.6299787005 * b_linear;

    let l_ = l.cbrt();
    let m_ = m.cbrt();
    let s_ = s.cbrt();

    let l_oklab = 0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_;
    let a_oklab = 1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_;
    let b_oklab = 0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_;

    // Convert OKLab to OKLCH
    let l_oklch = l_oklab * 100.0; // Convert to percentage
    let c_oklch = (a_oklab * a_oklab + b_oklab * b_oklab).sqrt();
    let h_oklch = b_oklab.atan2(a_oklab).to_degrees();
    let h_oklch = if h_oklch < 0.0 {
        h_oklch + 360.0
    } else {
        h_oklch
    };

    (l_oklch, c_oklch, h_oklch)
}

/// Convert sRGB to linear RGB
fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_validation() {
        let quiz = ColorQuiz {
            r: 200,
            g: 200,
            b: 200,
        };

        // Exact match
        assert!(quiz.validate_answer("#c8c8c8"));

        // Within tolerance (diff = 10)
        assert!(quiz.validate_answer("#bec8ca"));

        // At boundary (diff = 20)
        assert!(quiz.validate_answer("#b4c8c8"));

        // Outside tolerance (diff = 21)
        assert!(!quiz.validate_answer("#b3c8c8"));
    }

    #[test]
    fn test_oklch_parsing() {
        let quiz = ColorQuiz {
            r: 200,
            g: 200,
            b: 200,
        };

        // Test various formats
        assert!(quiz.parse_oklch("oklch(45.0% 0.306 65.4)").is_some());
        assert!(quiz.parse_oklch("45.0% 0.306 65.4").is_some());
        assert!(quiz.parse_oklch("45.0%, 0.306, 65.4").is_some());
        assert!(quiz.parse_oklch("45 0.306 65.4").is_some());
    }

    #[test]
    fn test_rgb_to_oklch() {
        // Test with a known color (approximate values)
        let (l, c, h) = rgb_to_oklch(255, 0, 0); // Red
        assert!(l > 60.0 && l < 70.0); // Lightness around 62-63%
        assert!(c > 0.25); // High chroma
        assert!(h > 20.0 && h < 40.0); // Hue around 29 degrees
    }
}
