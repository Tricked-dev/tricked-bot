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
    pub fn generate<R: Rng>(rng: &mut R) -> Self {
        Self {
            r: rng.gen_range(0..=255),
            g: rng.gen_range(0..=255),
            b: rng.gen_range(0..=255),
        }
    }

    pub fn generate_image(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let width = 640u32;
        let height = 360u32;

        let mut buffer = Cursor::new(Vec::new());

        {
            let mut encoder = Encoder::new(&mut buffer, width, height);
            encoder.set_color(ColorType::Rgb);
            encoder.set_depth(BitDepth::Eight);

            let mut writer = encoder.write_header()?;

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

    pub fn validate_answer(&self, user_answer: &str) -> bool {
        let answer = user_answer.trim();

        if !answer.starts_with('#') {
            return false;
        }

        let hex = answer.trim_start_matches('#');

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

            return diff <= 25;
        }

        false
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

        assert!(quiz.validate_answer("#c8c8c8"));
        assert!(quiz.validate_answer("#bec8ca"));
        assert!(quiz.validate_answer("#b3c8c8"));
        assert!(!quiz.validate_answer("#aec8c8"));
    }
}
