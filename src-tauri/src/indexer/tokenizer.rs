//! CJK-aware Tantivy tokenizer for Mnemo.
//!
//! Provides character-class segmentation and n-gram (unigram + bigram) tokenization
//! for CJK text (Han, Hiragana, Katakana, Hangul) while preserving Tantivy's
//! SimpleTokenizer behavior for Latin text, code identifiers, numbers, and paths.

use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, TextAnalyzer, Token, TokenStream, Tokenizer,
};

/// Returns true if `ch` falls within recognized CJK Unicode blocks:
/// - Han ideographs (Unified, Extension A, Compatibility, Extensions B-F)
/// - Hiragana and Katakana (including phonetic extensions and halfwidth)
/// - Hangul (syllables, jamo, halfwidth)
/// - Bopomofo
#[inline]
pub fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x11FF       // Hangul Jamo
            | 0x3040..=0x309F // Hiragana
            | 0x30A0..=0x30FF // Katakana
            | 0x3100..=0x312F // Bopomofo
            | 0x31A0..=0x31BF // Bopomofo Extended
            | 0x31F0..=0x31FF // Katakana Phonetic Extensions
            | 0x3200..=0x32FF // Enclosed CJK Letters and Months
            | 0x3300..=0x33FF // CJK Compatibility
            | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
            | 0x4E00..=0x9FFF // CJK Unified Ideographs
            | 0xAC00..=0xD7AF // Hangul Syllables
            | 0xF900..=0xFAFF // CJK Compatibility Ideographs
            | 0xFF65..=0xFF9F // Halfwidth Katakana
            | 0xFFA0..=0xFFDC // Halfwidth Hangul
            | 0x20000..=0x2FA1F // CJK Extensions B-F & Compatibility Supplement
    )
}

/// Tokenizer that segments non-CJK text on non-alphanumeric boundaries
/// and decomposes CJK text into unigrams and bigrams.
#[derive(Clone, Default)]
pub struct MnemoTokenizer {
    token: Token,
}

pub struct MnemoTokenStream<'a> {
    text: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    token: &'a mut Token,
    cjk_run: Vec<(usize, usize)>,
    cjk_idx: usize,
    cjk_emit_bigram: bool,
}

impl Tokenizer for MnemoTokenizer {
    type TokenStream<'a> = MnemoTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> MnemoTokenStream<'a> {
        self.token.reset();
        MnemoTokenStream {
            text,
            chars: text.char_indices().peekable(),
            token: &mut self.token,
            cjk_run: Vec::new(),
            cjk_idx: 0,
            cjk_emit_bigram: false,
        }
    }
}

impl<'a> TokenStream for MnemoTokenStream<'a> {
    fn advance(&mut self) -> bool {
        // First, exhaust any active CJK run in interleaved unigram -> bigram order
        if !self.cjk_run.is_empty() {
            while self.cjk_idx < self.cjk_run.len() {
                if !self.cjk_emit_bigram {
                    let (from, to) = self.cjk_run[self.cjk_idx];
                    self.cjk_emit_bigram = true;
                    self.emit_token(from, to);
                    return true;
                } else if self.cjk_idx + 1 < self.cjk_run.len() {
                    let from = self.cjk_run[self.cjk_idx].0;
                    let to = self.cjk_run[self.cjk_idx + 1].1;
                    self.cjk_idx += 1;
                    self.cjk_emit_bigram = false;
                    self.emit_token(from, to);
                    return true;
                } else {
                    self.cjk_idx += 1;
                    self.cjk_emit_bigram = false;
                }
            }
            self.cjk_run.clear();
            self.cjk_idx = 0;
            self.cjk_emit_bigram = false;
        }

        // Process next characters from stream
        while let Some((offset, ch)) = self.chars.next() {
            if is_cjk(ch) {
                // Collect entire contiguous CJK run
                self.cjk_run.clear();
                self.cjk_run.push((offset, offset + ch.len_utf8()));
                while let Some(&(next_offset, next_ch)) = self.chars.peek() {
                    if is_cjk(next_ch) {
                        self.chars.next();
                        self.cjk_run
                            .push((next_offset, next_offset + next_ch.len_utf8()));
                    } else {
                        break;
                    }
                }
                // Emit first unigram
                let (from, to) = self.cjk_run[0];
                self.cjk_idx = 0;
                self.cjk_emit_bigram = true;
                self.emit_token(from, to);
                return true;
            } else if ch.is_alphanumeric() {
                // Non-CJK alphanumeric run (split on non-alphanumeric or CJK)
                let start_offset = offset;
                let mut end_offset = offset + ch.len_utf8();
                while let Some(&(next_offset, next_ch)) = self.chars.peek() {
                    if !is_cjk(next_ch) && next_ch.is_alphanumeric() {
                        self.chars.next();
                        end_offset = next_offset + next_ch.len_utf8();
                    } else {
                        break;
                    }
                }
                self.emit_token(start_offset, end_offset);
                return true;
            }
            // Delimiter/whitespace/punctuation - continue
        }

        false
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

impl<'a> MnemoTokenStream<'a> {
    fn emit_token(&mut self, from: usize, to: usize) {
        self.token.text.clear();
        self.token.position = self.token.position.wrapping_add(1);
        self.token.offset_from = from;
        self.token.offset_to = to;
        self.token.text.push_str(&self.text[from..to]);
        self.token.position_length = 1;
    }
}

/// Builds the default text analyzer for Mnemo fields using the CJK-aware tokenizer,
/// length filter, and lowercaser.
pub fn mnemo_analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(MnemoTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyze(text: &str) -> Vec<Token> {
        let mut analyzer = mnemo_analyzer();
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().clone());
        }
        tokens
    }

    #[test]
    fn latin_and_code_match_simple_behavior() {
        let tokens = analyze("fn calculate_total_tax() -> f64 {");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["fn", "calculate", "total", "tax", "f64"]);
        assert_eq!(tokens[0].position, 0);
        assert_eq!(tokens[1].position, 1);
        assert_eq!(tokens[2].position, 2);
    }

    #[test]
    fn cjk_unigrams_and_bigrams_emitted_with_correct_offsets() {
        let tokens = analyze("最終報告書");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        // Interleaved: 最, 最終, 終, 終報, 報, 報告, 告, 告書, 書
        assert_eq!(
            texts,
            vec!["最", "最終", "終", "終報", "報", "報告", "告", "告書", "書"]
        );

        // Check unigram offsets
        assert_eq!(&tokens[0].text, "最");
        assert_eq!(tokens[0].offset_from, 0);
        assert_eq!(tokens[0].offset_to, 3);

        // Check bigram offsets
        assert_eq!(&tokens[1].text, "最終");
        assert_eq!(tokens[1].offset_from, 0);
        assert_eq!(tokens[1].offset_to, 6);

        assert_eq!(&tokens[5].text, "報告");
        assert_eq!(tokens[5].offset_from, 6);
        assert_eq!(tokens[5].offset_to, 12);

        // Sequential positions
        for (i, t) in tokens.iter().enumerate() {
            assert_eq!(t.position, i);
        }
    }

    #[test]
    fn mixed_script_boundaries_are_flushed() {
        let tokens = analyze("README最終.md");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["readme", "最", "最終", "終", "md"]);
        assert_eq!(tokens[0].offset_from, 0);
        assert_eq!(tokens[0].offset_to, 6);
        assert_eq!(tokens[1].offset_from, 6);
        assert_eq!(tokens[1].offset_to, 9);
        assert_eq!(tokens[2].offset_from, 6);
        assert_eq!(tokens[2].offset_to, 12);
        assert_eq!(tokens[4].offset_from, 13);
        assert_eq!(tokens[4].offset_to, 15);
    }

    #[test]
    fn path_mixed_script_splits_correctly() {
        let tokens = analyze("src/组件/index.ts");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["src", "组", "组件", "件", "index", "ts"]);
    }

    #[test]
    fn single_cjk_character_emits_unigram_only() {
        let tokens = analyze("東");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["東"]);
        assert_eq!(tokens[0].offset_from, 0);
        assert_eq!(tokens[0].offset_to, 3);
    }

    // ── Additional Tokenizer & CJK Coverage ───────────────────────────────────

    #[test]
    fn is_cjk_boundary_character_blocks() {
        // Hangul Jamo
        assert!(is_cjk('\u{1100}'));
        assert!(is_cjk('\u{11FF}'));
        // Hiragana
        assert!(is_cjk('\u{3041}'));
        assert!(is_cjk('\u{3096}'));
        // Katakana
        assert!(is_cjk('\u{30A1}'));
        assert!(is_cjk('\u{30FA}'));
        // Bopomofo
        assert!(is_cjk('\u{3105}'));
        assert!(is_cjk('\u{3129}'));
        // CJK Unified Ideographs
        assert!(is_cjk('\u{4E00}'));
        assert!(is_cjk('\u{9FFF}'));
        // Hangul Syllables
        assert!(is_cjk('\u{AC00}'));
        assert!(is_cjk('\u{D7AF}'));

        // Non-CJK must return false
        assert!(!is_cjk('a'));
        assert!(!is_cjk('Z'));
        assert!(!is_cjk('9'));
        assert!(!is_cjk(' '));
        assert!(!is_cjk('\n'));
        assert!(!is_cjk('.'));
        assert!(!is_cjk('α')); // Greek
        assert!(!is_cjk('ж')); // Cyrillic
        assert!(!is_cjk('é')); // Latin-1
        assert!(!is_cjk('🦀')); // Emoji
    }

    #[test]
    fn empty_and_whitespace_only_return_no_tokens() {
        assert!(analyze("").is_empty());
        assert!(analyze("   \t\r\n  ").is_empty());
        assert!(analyze("!@#$%^&*()_+-=[]{}|;':\",.<>/?`~").is_empty());
    }

    #[test]
    fn two_cjk_characters_emit_three_tokens() {
        let tokens = analyze("東京");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        // Unigram 1, Bigram, Unigram 2
        assert_eq!(texts, vec!["東", "東京", "京"]);
        assert_eq!(tokens[0].offset_from, 0);
        assert_eq!(tokens[0].offset_to, 3);
        assert_eq!(tokens[1].offset_from, 0);
        assert_eq!(tokens[1].offset_to, 6);
        assert_eq!(tokens[2].offset_from, 3);
        assert_eq!(tokens[2].offset_to, 6);
    }

    #[test]
    fn cjk_interleaved_with_latin_and_numbers() {
        let tokens = analyze("第12回 Rust勉強会 2026年");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        // "第", "12", "回", "rust", "勉", "勉強", "強", "強会", "会", "2026", "年"
        assert_eq!(
            texts,
            vec![
                "第", "12", "回", "rust", "勉", "勉強", "強", "強会", "会", "2026", "年"
            ]
        );
    }

    #[test]
    fn tokens_exceeding_40_chars_are_filtered_by_analyzer() {
        let short_word = "short";
        let long_word = "a".repeat(45);
        let text = format!("{} {}", short_word, long_word);
        let tokens = analyze(&text);
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["short"]);
    }

    #[test]
    fn latin_lowercasing_and_code_identifiers() {
        let tokens = analyze("MNEMO_SEARCH_ENGINE_V2");
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["mnemo", "search", "engine", "v2"]);
    }
}
