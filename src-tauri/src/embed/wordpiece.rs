//! WordPiece tokenizer for BERT-family models (Potion, BGE).

use serde_json::Value;
use std::collections::HashMap;

pub const MAX_TOKENS: usize = 512;

#[derive(Debug, Clone)]
pub struct WordPieceTokenizer {
    pub vocab: HashMap<String, u32>,
    pub unknown_id: u32,
    pub cls_id: u32,
    pub sep_id: u32,
    pub lowercase: bool,
    pub continuing_prefix: String,
    pub max_input_chars: usize,
}

impl WordPieceTokenizer {
    pub fn parse(value: &Value, vocab_size_opt: Option<usize>) -> Option<Self> {
        let model = value.get("model")?;
        let vocab_object = model.get("vocab")?.as_object()?;
        if let Some(expected_size) = vocab_size_opt
            && vocab_object.len() != expected_size
        {
            return None;
        }
        let mut vocab = HashMap::with_capacity(vocab_object.len());
        for (k, v) in vocab_object {
            let id = v.as_u64()? as u32;
            vocab.insert(k.clone(), id);
        }

        let unknown_id = vocab.get("[UNK]").copied().unwrap_or(100);
        let cls_id = vocab.get("[CLS]").copied().unwrap_or(101);
        let sep_id = vocab.get("[SEP]").copied().unwrap_or(102);

        let continuing_prefix = model
            .get("continuing_subword_prefix")
            .and_then(|v| v.as_str())
            .unwrap_or("##")
            .to_string();

        let max_input_chars = model
            .get("max_input_chars_per_word")
            .and_then(|v| v.as_u64())
            .unwrap_or(100) as usize;

        let lowercase = value
            .get("normalizer")
            .and_then(|n| n.get("lowercase"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        Some(Self {
            vocab,
            unknown_id,
            cls_id,
            sep_id,
            lowercase,
            continuing_prefix,
            max_input_chars,
        })
    }

    pub fn pre_tokenize(&self, text: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut current = String::new();

        for ch in text.chars() {
            if ch.is_whitespace() {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            } else if ch.is_control() {
                continue;
            } else if is_punctuation(ch) || is_cjk(ch) {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push(lowercase_char(ch, self.lowercase));
            } else if self.lowercase {
                current.extend(ch.to_lowercase());
            } else {
                current.push(ch);
            }
        }
        if !current.is_empty() {
            words.push(current);
        }
        words
    }

    pub fn tokenize_word(&self, word: &str, ids: &mut Vec<u32>) {
        if word.is_empty() {
            return;
        }

        if let Some(&id) = self.vocab.get(word) {
            ids.push(id);
            return;
        }

        let chars: Vec<char> = word.chars().collect();
        if chars.len() > self.max_input_chars {
            ids.push(self.unknown_id);
            return;
        }

        let mut start = 0usize;
        let mut pieces = Vec::new();
        while start < chars.len() {
            let mut end = chars.len();
            let mut matched: Option<u32> = None;
            while end > start {
                let mut piece: String = chars[start..end].iter().collect();
                if start > 0 {
                    piece.insert_str(0, &self.continuing_prefix);
                }
                if let Some(&id) = self.vocab.get(&piece) {
                    matched = Some(id);
                    break;
                }
                end -= 1;
            }

            match matched {
                Some(id) => {
                    pieces.push(id);
                    start = end;
                }
                None => {
                    ids.push(self.unknown_id);
                    return;
                }
            }
        }
        ids.extend(pieces);
    }

    pub fn token_ids(&self, text: &str) -> Vec<u32> {
        let mut ids = Vec::new();
        for word in self.pre_tokenize(text) {
            self.tokenize_word(&word, &mut ids);
            if ids.len() >= MAX_TOKENS {
                ids.truncate(MAX_TOKENS);
                break;
            }
        }
        ids
    }

    pub fn encode_bert(&self, text: &str, max_len: usize) -> (Vec<i64>, Vec<i64>, Vec<i64>) {
        let inner_cap = max_len.saturating_sub(2);
        let mut ids = Vec::new();
        for word in self.pre_tokenize(text) {
            self.tokenize_word(&word, &mut ids);
            if ids.len() >= inner_cap {
                ids.truncate(inner_cap);
                break;
            }
        }

        let mut input_ids = Vec::with_capacity(ids.len() + 2);
        input_ids.push(self.cls_id as i64);
        for id in ids {
            input_ids.push(id as i64);
        }
        input_ids.push(self.sep_id as i64);

        let seq_len = input_ids.len();
        let attention_mask = vec![1i64; seq_len];
        let token_type_ids = vec![0i64; seq_len];

        (input_ids, attention_mask, token_type_ids)
    }
}

pub fn lowercase_char(ch: char, lowercase: bool) -> String {
    if lowercase {
        ch.to_lowercase().collect()
    } else {
        ch.to_string()
    }
}

pub fn is_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
}

pub fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF       // hiragana + katakana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xAC00..=0xD7AF // hangul syllables
            | 0xF900..=0xFAFF // CJK compatibility ideographs
    )
}

pub fn l2_normalize(values: &mut [f32]) {
    let norm_sq: f32 = values.iter().map(|v| v * v).sum();
    if norm_sq > 0.0 {
        let norm = norm_sq.sqrt();
        for value in values {
            *value /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A tiny WordPiece tokenizer.json equivalent. Vocab deliberately chosen
    /// so greedy longest-match matters: "unbelievable" splits, "un ##broken"
    /// hits whole-word pieces.
    fn tokenizer() -> WordPieceTokenizer {
        let value = json!({
            "model": {
                "vocab": {
                    "[UNK]": 0, "[CLS]": 1, "[SEP]": 2,
                    "un": 10, "believable": 11, "##believable": 12,
                    "dog": 13, "house": 14, "##house": 15, "##cat": 16,
                    "a": 17, "b": 18
                },
                "continuing_subword_prefix": "##",
                "max_input_chars_per_word": 20
            },
            "normalizer": { "lowercase": true }
        });
        WordPieceTokenizer::parse(&value, Some(12)).unwrap()
    }

    #[test]
    fn parse_rejects_bad_or_mismatched_configs() {
        // Missing model / vocab must be None, never panic.
        assert!(WordPieceTokenizer::parse(&json!({}), None).is_none());
        assert!(WordPieceTokenizer::parse(&json!({ "model": {} }), None).is_none());
        assert!(
            WordPieceTokenizer::parse(&json!({ "model": { "vocab": "" } }), None).is_none(),
            "vocab as string is not a map"
        );

        // Vocab size mismatch is a hard error (guards truncated downloads).
        let value = json!({ "model": { "vocab": { "a": 0 } } });
        assert_eq!(
            WordPieceTokenizer::parse(&value, Some(1)).map(|t| t.vocab.len()),
            Some(1)
        );
        assert!(WordPieceTokenizer::parse(&value, Some(99)).is_none());
    }

    #[test]
    fn parse_applies_documented_defaults() {
        let value = json!({ "model": { "vocab": { "dog": 5 } } });
        let t = WordPieceTokenizer::parse(&value, None).unwrap();
        // Special-token ids fall back to the standard BERT positions.
        assert_eq!(t.unknown_id, 100);
        assert_eq!(t.cls_id, 101);
        assert_eq!(t.sep_id, 102);
        assert_eq!(t.continuing_prefix, "##");
        assert_eq!(t.max_input_chars, 100);
        assert!(t.lowercase);
    }

    #[test]
    fn pre_tokenize_splits_on_punctuation_and_cjk_per_char() {
        let t = tokenizer();
        // ASCII punctuation is its own token.
        assert_eq!(t.pre_tokenize("end.start"), vec!["end", ".", "start"]);
        // CJK chars split one-by-one, even glued to latin words.
        assert_eq!(t.pre_tokenize("世界一dogs"), vec!["世", "界", "一", "dogs"]);
        // Hiragana and Hangul ranges too.
        assert_eq!(
            t.pre_tokenize("かなハンabc"),
            vec!["か", "な", "ハ", "ン", "abc"]
        );
        // Control characters are dropped, not split points producing "".
        assert_eq!(t.pre_tokenize("a\u{1}b"), t.pre_tokenize("ab"));
    }

    #[test]
    fn pre_tokenize_honors_lowercase_flag() {
        let t = tokenizer();
        assert_eq!(t.pre_tokenize("CamelCase!"), vec!["camelcase", "!"]);

        let value = json!({
            "model": { "vocab": { "dog": 13 } },
            "normalizer": { "lowercase": false }
        });
        let cased = WordPieceTokenizer::parse(&value, None).unwrap();
        assert_eq!(cased.pre_tokenize("Dog Dog"), vec!["Dog", "Dog"]);
    }

    #[test]
    fn pre_tokenize_collapses_whitespace_runs() {
        let t = tokenizer();
        // Tabs, newlines and long space runs never produce empty words.
        assert_eq!(t.pre_tokenize("  a\t\tb\n\nc  "), vec!["a", "b", "c"]);
        // Empty/whitespace-only input produces nothing.
        assert!(t.pre_tokenize("").is_empty());
        assert!(t.pre_tokenize(" \t\r\n").is_empty());
    }

    #[test]
    fn tokenize_word_uses_whole_word_before_splitting() {
        let t = tokenizer();
        let mut ids = Vec::new();
        t.tokenize_word("dog", &mut ids);
        assert_eq!(ids, vec![13]);
        // Empty word contributes nothing.
        t.tokenize_word("", &mut ids);
        assert_eq!(ids, vec![13]);
    }

    #[test]
    fn tokenize_word_greedily_matches_longest_pieces() {
        let t = tokenizer();
        let mut ids = Vec::new();
        // "unbelievable" is not in vocab; greedy left-to-right finds
        // "un" + "##believable" (not just "un" + fragments).
        t.tokenize_word("unbelievable", &mut ids);
        assert_eq!(ids, vec![10, 12]);
        // Continuation piece must carry the ## prefix.
        let mut ids = Vec::new();
        t.tokenize_word("doghouse", &mut ids);
        assert_eq!(ids, vec![13, 15]);
    }

    #[test]
    fn tokenize_word_unknown_when_no_piece_fits_or_word_too_long() {
        let t = tokenizer();
        // No prefix in vocab at all -> [UNK].
        let mut ids = Vec::new();
        t.tokenize_word("zzz", &mut ids);
        assert_eq!(ids, vec![0]);

        // Longer than max_input_chars_per_word (20) -> [UNK] even though
        // every piece individually exists.
        let mut ids = Vec::new();
        t.tokenize_word("unbelievablex!", &mut ids);
        assert_eq!(ids, vec![0]);
    }

    #[test]
    fn token_ids_of_empty_text_are_empty_and_list_partial_words() {
        let t = tokenizer();
        assert!(t.token_ids("").is_empty());
        // Blend of known word and unknown word per word.
        assert_eq!(t.token_ids("dog zzz"), vec![13, 0]);
    }

    #[test]
    fn token_ids_truncate_to_max_tokens() {
        // One huge run of split CJK chars easily exceeds 512 tokens.
        let text = "世".repeat(10_000);
        let t = tokenizer();
        let ids = t.token_ids(&text);
        assert_eq!(ids.len(), MAX_TOKENS);
    }

    #[test]
    fn encode_bert_wraps_with_cls_and_sep() {
        let t = tokenizer();
        let (ids, mask, types) = t.encode_bert("dog", 512);
        assert_eq!(ids, vec![1, 13, 2], "[CLS] word [SEP]");
        assert_eq!(mask, vec![1, 1, 1]);
        assert_eq!(types, vec![0, 0, 0]);
    }

    #[test]
    fn encode_bert_respects_max_len_including_specials() {
        let t = tokenizer();
        let text = "doghouse ".repeat(200);
        let max_len = 6;
        let (ids, mask, types) = t.encode_bert(&text, max_len);
        assert_eq!(ids.len(), max_len);
        assert_eq!(mask.len(), max_len);
        assert_eq!(types.len(), max_len);
        // Content is capped at max_len - 2, specials included.
        assert_eq!(ids.first(), Some(&1));
        assert_eq!(ids.last(), Some(&2));
    }

    #[test]
    fn encode_bert_empty_text_is_just_specials() {
        let t = tokenizer();
        let (ids, mask, _types) = t.encode_bert("   ", 16);
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(mask, vec![1, 1]);
    }

    #[test]
    fn l2_normalize_unitizes_and_never_divides_by_zero() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);

        // All-zero vector stays all-zero (norm 0 branch).
        let mut zero = vec![0.0f32; 4];
        l2_normalize(&mut zero);
        assert_eq!(zero, vec![0.0; 4]);

        // Empty slice is fine.
        let mut empty: Vec<f32> = Vec::new();
        l2_normalize(&mut empty);
        assert!(empty.is_empty());
    }

    #[test]
    fn cjk_classifier_covers_the_documented_ranges() {
        assert!(is_cjk('世'));
        assert!(is_cjk('か'));
        assert!(is_cjk('하'));
        assert!(!is_cjk('e'));
        assert!(!is_cjk('Ω'));
        assert!(!is_cjk('😀'));
    }
}
