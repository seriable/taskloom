//! Conservative, dependency-free input token estimate used for the pre-send
//! budget check. It deliberately over-estimates: refusing a request that
//! would have fit is cheap (the caller compresses once more), sending one
//! that does not fit wastes a full provider round trip.

/// Approximate token cost of one image at `detail=high/auto`
/// (OpenAI-compatible tiling of a ~1024px frame: 85 + 4 × 170).
pub const DEFAULT_TOKENS_PER_IMAGE: usize = 765;
/// Approximate token cost of one image at `detail=low`.
pub const LOW_DETAIL_TOKENS_PER_IMAGE: usize = 85;
/// Per-message protocol overhead (role markers, separators).
const MESSAGE_OVERHEAD_TOKENS: usize = 8;
const SAFETY_FACTOR: f64 = 1.15;
const NON_CJK_CHARS_PER_TOKEN: f64 = 3.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimator {
    pub tokens_per_image: usize,
}

impl Default for TokenEstimator {
    fn default() -> Self {
        Self {
            tokens_per_image: DEFAULT_TOKENS_PER_IMAGE,
        }
    }
}

impl TokenEstimator {
    pub fn with_tokens_per_image(tokens_per_image: usize) -> Self {
        Self { tokens_per_image }
    }

    /// Estimator matching the configured vision detail level.
    pub fn for_vision_detail(detail: &str) -> Self {
        if detail.trim().eq_ignore_ascii_case("low") {
            Self::with_tokens_per_image(LOW_DETAIL_TOKENS_PER_IMAGE)
        } else {
            Self::default()
        }
    }

    /// Estimated tokens for a text: CJK characters ≈ 1 token each, other
    /// characters ≈ 1 token per 3.5, multiplied by a 1.15 safety factor.
    pub fn estimate(text: &str) -> usize {
        let mut cjk = 0usize;
        let mut other = 0usize;
        for character in text.chars() {
            if is_cjk_token_char(character) {
                cjk += 1;
            } else {
                other += 1;
            }
        }
        if cjk == 0 && other == 0 {
            return 0;
        }
        let raw = cjk as f64 + other as f64 / NON_CJK_CHARS_PER_TOKEN;
        (raw * SAFETY_FACTOR).ceil() as usize
    }

    /// Estimated input tokens for a system + user prompt pair with images.
    pub fn estimate_request(&self, system: &str, user: &str, image_count: usize) -> usize {
        Self::estimate(system)
            + Self::estimate(user)
            + 2 * MESSAGE_OVERHEAD_TOKENS
            + image_count.saturating_mul(self.tokens_per_image)
    }
}

/// Characters a tokenizer usually encodes one per token: CJK ideographs and
/// punctuation, kana, Hangul and full-width forms.
pub fn is_cjk_token_char(character: char) -> bool {
    matches!(
        character as u32,
        0x3000..=0x303F // CJK symbols and punctuation
            | 0x3040..=0x30FF // Hiragana, Katakana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xAC00..=0xD7AF // Hangul syllables
            | 0xF900..=0xFAFF // CJK compatibility ideographs
            | 0xFF00..=0xFFEF // Full-width forms
            | 0x20000..=0x2FA1F // CJK extensions B–F, compatibility supplement
    )
}
