use rand::{prelude::ThreadRng, Rng};

pub fn zalgify_text(mut rng: ThreadRng, s: String) -> String {
    let mut new_text = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        new_text.push(c);
        for _ in 0..rng.gen_range(0..3) / 2 + 1 {
            new_text.push(ZALGO_UP[rng.gen_range(0..ZALGO_UP.len())]);
        }
        for _ in 0..rng.gen_range(0..2) / 2 {
            new_text.push(ZALGO_MID[rng.gen_range(0..ZALGO_MID.len())]);
        }
        for _ in 0..rng.gen_range(0..3) / 2 + 1 {
            new_text.push(ZALGO_DOWN[rng.gen_range(0..ZALGO_DOWN.len())]);
        }
    }
    new_text
}

pub const ZALGO_UP: [char; 50] = [
    '\u{030e}', /* ̎ */ '\u{0304}', /* ̄ */ '\u{0305}', /* ̅ */
    '\u{033f}', /* ̿ */ '\u{0311}', /* ̑ */ '\u{0306}', /* ̆ */ '\u{0310}', /* ̐ */
    '\u{0352}', /* ͒ */ '\u{0357}', /* ͗ */ '\u{0351}', /* ͑ */ '\u{0307}', /* ̇ */
    '\u{0308}', /* ̈ */ '\u{030a}', /* ̊ */ '\u{0342}', /* ͂ */ '\u{0343}', /* ̓ */
    '\u{0344}', /* ̈́ */ '\u{034a}', /* ͊ */ '\u{034b}', /* ͋ */ '\u{034c}', /* ͌ */
    '\u{0303}', /* ̃ */ '\u{0302}', /* ̂ */ '\u{030c}', /* ̌ */ '\u{0350}', /* ͐ */
    '\u{0300}', /* ̀ */ '\u{0301}', /* ́ */ '\u{030b}', /* ̋ */ '\u{030f}', /* ̏ */
    '\u{0312}', /* ̒ */ '\u{0313}', /* ̓ */ '\u{0314}', /* ̔ */ '\u{033d}', /* ̽ */
    '\u{0309}', /* ̉ */ '\u{0363}', /* ͣ */ '\u{0364}', /* ͤ */ '\u{0365}', /* ͥ */
    '\u{0366}', /* ͦ */ '\u{0367}', /* ͧ */ '\u{0368}', /* ͨ */ '\u{0369}', /* ͩ */
    '\u{036a}', /* ͪ */ '\u{036b}', /* ͫ */ '\u{036c}', /* ͬ */ '\u{036d}', /* ͭ */
    '\u{036e}', /* ͮ */ '\u{036f}', /* ͯ */ '\u{033e}', /* ̾ */ '\u{035b}', /* ͛ */
    '\u{0346}', /* ͆ */ '\u{031a}', /* ̚ */ '\u{030d}', /* ̍ */
];

pub const ZALGO_DOWN: [char; 40] = [
    '\u{0317}', /* ̗ */ '\u{0318}', /* ̘ */ '\u{0319}', /* ̙ */
    '\u{031c}', /* ̜ */ '\u{031d}', /* ̝ */ '\u{031e}', /* ̞ */ '\u{031f}', /* ̟ */
    '\u{0320}', /* ̠ */ '\u{0324}', /* ̤ */ '\u{0325}', /* ̥ */ '\u{0326}', /* ̦ */
    '\u{0329}', /* ̩ */ '\u{032a}', /* ̪ */ '\u{032b}', /* ̫ */ '\u{032c}', /* ̬ */
    '\u{032d}', /* ̭ */ '\u{032e}', /* ̮ */ '\u{032f}', /* ̯ */ '\u{0330}', /* ̰ */
    '\u{0331}', /* ̱ */ '\u{0332}', /* ̲ */ '\u{0333}', /* ̳ */ '\u{0339}', /* ̹ */
    '\u{033a}', /* ̺ */ '\u{033b}', /* ̻ */ '\u{033c}', /* ̼ */ '\u{0345}', /* ͅ */
    '\u{0347}', /* ͇ */ '\u{0348}', /* ͈ */ '\u{0349}', /* ͉ */ '\u{034d}', /* ͍ */
    '\u{034e}', /* ͎ */ '\u{0353}', /* ͓ */ '\u{0354}', /* ͔ */ '\u{0355}', /* ͕ */
    '\u{0356}', /* ͖ */ '\u{0359}', /* ͙ */ '\u{035a}', /* ͚ */ '\u{0323}', /* ̣ */
    '\u{0316}', /* ̖ */
];

pub const ZALGO_MID: [char; 23] = [
    '\u{031b}', /* ̛ */ '\u{0340}', /* ̀ */ '\u{0341}', /* ́ */
    '\u{0358}', /* ͘ */ '\u{0321}', /* ̡ */ '\u{0322}', /* ̢ */ '\u{0327}', /* ̧ */
    '\u{0328}', /* ̨ */ '\u{0334}', /* ̴ */ '\u{0335}', /* ̵ */ '\u{0336}', /* ̶ */
    '\u{034f}', /* ͏ */ '\u{035c}', /* ͜ */ '\u{035d}', /* ͝ */ '\u{035e}', /* ͞ */
    '\u{035f}', /* ͟ */ '\u{0360}', /* ͠ */ '\u{0362}', /* ͢ */ '\u{0338}', /* ̸ */
    '\u{0337}', /* ̷ */ '\u{0361}', /* ͡ */ '\u{0489}', /* ҉_ */ '\u{0315}', /* ̕ */
];
