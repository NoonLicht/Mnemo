//! Dependency-light loader for the Potion static embedding table.
//!
//! The three artifacts downloaded by [`crate::models`] are:
//!
//! * `model.safetensors` - a single `[vocab, dim]` embedding matrix,
//! * `tokenizer.json` - a HuggingFace BERT WordPiece tokenizer,
//! * `config.json` - model hyper-parameters (normalization, hidden size).
//!
//! This module parses all three with only `serde_json` plus the standard
//! library. Anything malformed or unsupported is rejected with `None`, which
//! callers translate into the hashing fallback in [`crate::embed::model`].

use crate::embed::model::EMBEDDING_DIM;
use serde_json::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Hard cap on the number of WordPiece ids returned per text.
pub const MAX_TOKENS: usize = 512;

/// Upper bound for the safetensors JSON header. Real headers are a few KB;
/// this only exists so a corrupt length prefix cannot allocate gigabytes.
const MAX_HEADER_BYTES: u64 = 64 * 1024 * 1024;

/// An in-memory static embedding table plus the WordPiece tokenizer needed to
/// index into it. Both are immutable after construction; hold one per process.
#[derive(Debug)]
pub struct StaticTable {
    /// Row-major `[vocab, dim]` embedding matrix (f32 mode only).
    pub embeddings: Vec<f32>,
    /// Quantized row-major matrix (default). One scale per row.
    pub embeddings_i8: Vec<i8>,
    pub embedding_scales: Vec<f32>,
    pub vocab: HashMap<String, u32>,
    pub dim: usize,
    pub unknown_id: u32,
    pub lowercase: bool,
    pub normalize: bool,
    pub model_id: u32,
    continuing_prefix: String,
    max_input_chars: usize,
}

impl StaticTable {
    /// Loads `model.safetensors`, `tokenizer.json` and `config.json` from `dir`.
    ///
    /// Returns `None` when any required artifact is missing or unusable, when
    /// the embedding matrix is not 2-D, or when its width is not
    /// [`EMBEDDING_DIM`]. Callers should fall back to the hashing embedder.
    pub fn load(dir: &Path) -> Option<Self> {
        Self::load_with_f32(dir, false)
    }

    /// Loads the table, optionally retaining the f32 matrix for one release.
    ///
    /// The default quantization path keeps `Vec<i8>` plus one scale per row.
    /// Set `MNEMO_F32_TABLE=1` as a developer escape hatch, or use
    /// [`crate::config::MnemoConfig::semantic_f32_table`] for a persisted
    /// switch.
    pub fn load_with_f32(dir: &Path, use_f32: bool) -> Option<Self> {
        let use_f32 = use_f32 || std::env::var_os("MNEMO_F32_TABLE").is_some();
        let mut file = File::open(dir.join("model.safetensors")).ok()?;
        let (embeddings, vocab_size, dim) = parse_safetensors(&mut file)?;
        if dim != EMBEDDING_DIM {
            return None;
        }

        let tokenizer_bytes = std::fs::read(dir.join("tokenizer.json")).ok()?;
        let tokenizer: Value = serde_json::from_slice(&tokenizer_bytes).ok()?;
        let tokenizer_parts = parse_tokenizer(&tokenizer, vocab_size)?;

        let normalize = match std::fs::read(dir.join("config.json")) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(config) => {
                    // `hidden_size` is the transformers name; model2vec uses
                    // `hidden_dim`. Accept either, and reject a mismatch.
                    if let Some(size) = config
                        .get("hidden_size")
                        .or_else(|| config.get("hidden_dim"))
                        .and_then(|v| v.as_u64())
                        && size as usize != dim
                    {
                        return None;
                    }
                    config
                        .get("normalize")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true)
                }
                // A corrupt config is not fatal: defaults are safe.
                Err(_) => true,
            },
            Err(_) => true,
        };

        let (embeddings, embeddings_i8, embedding_scales) = if use_f32 {
            (embeddings, Vec::new(), Vec::new())
        } else {
            // Quantize first, then drop the f32 matrix before returning so the
            // startup peak does not retain both representations.
            let (quantized, scales) = quantize(&embeddings, dim);
            drop(embeddings);
            (Vec::new(), quantized, scales)
        };

        Some(Self {
            embeddings,
            embeddings_i8,
            embedding_scales,
            vocab: tokenizer_parts.vocab,
            dim,
            unknown_id: tokenizer_parts.unknown_id,
            lowercase: tokenizer_parts.lowercase,
            normalize,
            model_id: crate::embed::model::REAL_MODEL_ID,
            continuing_prefix: tokenizer_parts.continuing_prefix,
            max_input_chars: tokenizer_parts.max_input_chars,
        })
    }

    /// Heap bytes held by the embedding matrix and its scales.
    pub fn table_bytes(&self) -> usize {
        self.embeddings.len() * std::mem::size_of::<f32>()
            + self.embeddings_i8.len()
            + self.embedding_scales.len() * std::mem::size_of::<f32>()
    }

    /// True when the compact i8 representation is active.
    pub fn is_quantized(&self) -> bool {
        !self.embeddings_i8.is_empty()
    }

    /// Encodes `text` to WordPiece ids, without any special tokens.
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

    /// Mean-pools token embedding rows, then L2-normalizes when configured.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut out = vec![0.0f32; self.dim];
        let ids = self.token_ids(text);
        if ids.is_empty() {
            return out;
        }

        let mut used = 0usize;
        for id in ids {
            let start = id as usize * self.dim;
            let end = start + self.dim;
            if !self.embeddings.is_empty() {
                if end > self.embeddings.len() {
                    continue;
                }
                for (slot, value) in out.iter_mut().zip(&self.embeddings[start..end]) {
                    *slot += *value;
                }
            } else {
                if end > self.embeddings_i8.len() {
                    continue;
                }
                let scale = self.embedding_scales[id as usize];
                for (slot, value) in out.iter_mut().zip(&self.embeddings_i8[start..end]) {
                    *slot += *value as f32 * scale;
                }
            }
            used += 1;
        }

        if used == 0 {
            return out;
        }
        let scale = 1.0 / used as f32;
        for value in &mut out {
            *value *= scale;
        }

        if self.normalize {
            l2_normalize(&mut out);
        }
        out
    }

    /// BERT-ish normalization + pre-tokenization: lowercase, drop control
    /// characters, collapse whitespace, and isolate punctuation and CJK.
    fn pre_tokenize(&self, text: &str) -> Vec<String> {
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

    /// Greedy longest-match WordPiece with the `##` continuation prefix.
    fn tokenize_word(&self, word: &str, ids: &mut Vec<u32>) {
        if word.is_empty() {
            return;
        }

        // Fast path: the whole word is a token.
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
                    // A single unmatched character invalidates the whole word.
                    ids.push(self.unknown_id);
                    return;
                }
            }
        }
        ids.extend(pieces);
    }

    #[cfg(test)]
    fn test_table(embeddings: Vec<f32>, vocab: &[(&str, u32)], dim: usize) -> Self {
        let (embeddings_i8, embedding_scales) = quantize(&embeddings, dim);
        Self {
            embeddings: Vec::new(),
            embeddings_i8,
            embedding_scales,
            vocab: vocab.iter().map(|(t, id)| (t.to_string(), *id)).collect(),
            dim,
            unknown_id: 1,
            lowercase: true,
            normalize: true,
            model_id: crate::embed::model::REAL_MODEL_ID,
            continuing_prefix: "##".to_string(),
            max_input_chars: 100,
        }
    }

    #[cfg(test)]
    fn test_table_f32(embeddings: Vec<f32>, vocab: &[(&str, u32)], dim: usize) -> Self {
        Self {
            embeddings,
            embeddings_i8: Vec::new(),
            embedding_scales: Vec::new(),
            vocab: vocab.iter().map(|(t, id)| (t.to_string(), *id)).collect(),
            dim,
            unknown_id: 1,
            lowercase: true,
            normalize: true,
            model_id: crate::embed::model::REAL_MODEL_ID,
            continuing_prefix: "##".to_string(),
            max_input_chars: 100,
        }
    }
}

fn lowercase_char(ch: char, lowercase: bool) -> String {
    if lowercase {
        ch.to_lowercase().collect()
    } else {
        ch.to_string()
    }
}

/// BERT's punctuation definition (ASCII ranges), which is what
/// `BertPreTokenizer` splits on.
fn is_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
}

/// `BertNormalizer` surrounds CJK ideographs/kana/hangul with spaces so each
/// becomes its own pre-token. Roughly mirrors `handle_chinese_chars`.
fn is_cjk(ch: char) -> bool {
    matches!(
        ch as u32,
        0x3040..=0x30FF       // hiragana + katakana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified ideographs
            | 0xAC00..=0xD7AF // hangul syllables
            | 0xF900..=0xFAFF // CJK compatibility ideographs
    )
}

/// Quantizes a row-major f32 matrix to i8 with one symmetric scale per row.
///
/// Returns `(values, scales)` where each row's dequantized value is
/// `value as f32 * scales[row]`.
fn quantize(embeddings: &[f32], dim: usize) -> (Vec<i8>, Vec<f32>) {
    let rows = embeddings.len() / dim;
    let mut out = Vec::with_capacity(embeddings.len());
    let mut scales = Vec::with_capacity(rows);
    for row in embeddings.chunks(dim) {
        let max_abs = row.iter().fold(0.0f32, |acc, v| acc.max(v.abs()));
        let scale = if max_abs > 0.0 { max_abs / 127.0 } else { 0.0 };
        scales.push(scale);
        for &value in row {
            let q = if scale > 0.0 {
                (value / scale).round().clamp(-127.0, 127.0) as i8
            } else {
                0
            };
            out.push(q);
        }
    }
    (out, scales)
}

fn l2_normalize(values: &mut [f32]) {
    let norm_sq: f32 = values.iter().map(|v| v * v).sum();
    if norm_sq > 0.0 {
        let norm = norm_sq.sqrt();
        for value in values {
            *value /= norm;
        }
    }
}

struct TokenizerParts {
    vocab: HashMap<String, u32>,
    unknown_id: u32,
    lowercase: bool,
    continuing_prefix: String,
    max_input_chars: usize,
}

fn parse_tokenizer(value: &Value, vocab_size: usize) -> Option<TokenizerParts> {
    let model = value.get("model")?;
    let vocab_object = model.get("vocab")?.as_object()?;

    let mut vocab = HashMap::with_capacity(vocab_object.len());
    for (token, id) in vocab_object {
        let Some(id) = id.as_u64() else { continue };
        if (id as usize) < vocab_size {
            vocab.insert(token.clone(), id as u32);
        }
    }
    if vocab.is_empty() {
        return None;
    }

    let unk_token = model
        .get("unk_token")
        .and_then(|v| v.as_str())
        .unwrap_or("[UNK]");
    let unknown_id = vocab.get(unk_token).copied().unwrap_or(0);
    if unknown_id as usize >= vocab_size {
        return None;
    }

    let continuing_prefix = model
        .get("continuing_subword_prefix")
        .and_then(|v| v.as_str())
        .unwrap_or("##")
        .to_string();
    let max_input_chars = model
        .get("max_input_chars_per_word")
        .and_then(|v| v.as_u64())
        .unwrap_or(100) as usize;

    let normalizer = value.get("normalizer");
    let lowercase = normalizer
        .and_then(|n| n.get("lowercase"))
        .and_then(|v| v.as_bool())
        .or_else(|| {
            normalizer
                .and_then(|n| n.get("do_lower_case"))
                .and_then(|v| v.as_bool())
        })
        .unwrap_or(true);

    Some(TokenizerParts {
        vocab,
        unknown_id,
        lowercase,
        continuing_prefix,
        max_input_chars,
    })
}

/// Parses one safetensors file and returns `(embeddings, vocab_size, dim)`.
fn parse_safetensors<R: Read + Seek>(reader: &mut R) -> Option<(Vec<f32>, usize, usize)> {
    let mut length_bytes = [0u8; 8];
    reader.read_exact(&mut length_bytes).ok()?;
    let header_len = u64::from_le_bytes(length_bytes);
    if header_len == 0 || header_len > MAX_HEADER_BYTES {
        return None;
    }
    let header_len = header_len as usize;

    let mut header_bytes = vec![0u8; header_len];
    reader.read_exact(&mut header_bytes).ok()?;
    let header: Value = serde_json::from_slice(&header_bytes).ok()?;
    let tensors = header.as_object()?;

    let selected = if let Some(tensor) = tensors.get("embeddings") {
        tensor_info(tensor)?
    } else {
        // No `embeddings` key: accept only an unambiguous 2-D tensor.
        let mut found = None;
        for (name, tensor) in tensors {
            if name == "__metadata__" {
                continue;
            }
            if let Some(info) = tensor_info(tensor)
                && info.1.len() == 2
            {
                if found.is_some() {
                    return None;
                }
                found = Some(info);
            }
        }
        found?
    };

    let (dtype, shape, begin, end) = selected;
    if shape.len() != 2 {
        return None;
    }
    let vocab_size = shape[0];
    let dim = shape[1];
    if dim != EMBEDDING_DIM {
        return None;
    }
    let item_size: u64 = match dtype.as_str() {
        "F32" => 4,
        "F16" => 2,
        _ => return None,
    };
    if end < begin {
        return None;
    }
    let expected = (vocab_size as u64)
        .checked_mul(dim as u64)?
        .checked_mul(item_size)?;
    if end - begin != expected {
        return None;
    }

    let data_start = 8u64.checked_add(header_len as u64)?;
    reader
        .seek(SeekFrom::Start(data_start.checked_add(begin)?))
        .ok()?;

    let mut raw = vec![0u8; expected as usize];
    reader.read_exact(&mut raw).ok()?;

    let embeddings = if dtype == "F32" {
        raw.as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect()
    } else {
        raw.as_chunks::<2>()
            .0
            .iter()
            .map(|c| f16_to_f32(u16::from_le_bytes(*c)))
            .collect()
    };

    Some((embeddings, vocab_size, dim))
}

fn tensor_info(value: &Value) -> Option<(String, Vec<usize>, u64, u64)> {
    let dtype = value.get("dtype")?.as_str()?.to_string();
    let shape = value
        .get("shape")?
        .as_array()?
        .iter()
        .map(|v| v.as_u64().map(|n| n as usize))
        .collect::<Option<Vec<usize>>>()?;
    let offsets = value.get("data_offsets")?.as_array()?;
    if offsets.len() != 2 {
        return None;
    }
    Some((dtype, shape, offsets[0].as_u64()?, offsets[1].as_u64()?))
}

/// IEEE 754 half-precision to single-precision conversion (no dependency).
fn f16_to_f32(bits: u16) -> f32 {
    let sign = (bits >> 15) & 1;
    let exponent = (bits >> 10) & 0x1f;
    let fraction = bits & 0x03ff;

    let magnitude = if exponent == 0 {
        // Subnormal (or zero).
        if fraction == 0 {
            0.0
        } else {
            (fraction as f32) * (2.0f32).powi(-24)
        }
    } else if exponent == 0x1f {
        // Infinity / NaN.
        if fraction == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + fraction as f32 / 1024.0) * (2.0f32).powi(exponent as i32 - 15)
    };

    if sign == 1 { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn safetensors_bytes(dtype: &str, shape: &[usize], data: &[u8]) -> Vec<u8> {
        let header = serde_json::json!({
            "embeddings": {
                "dtype": dtype,
                "shape": shape,
                "data_offsets": [0, data.len()],
            }
        });
        let header = serde_json::to_vec(&header).unwrap();
        let mut bytes = Vec::with_capacity(8 + header.len() + data.len());
        bytes.extend_from_slice(&(header.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(data);
        bytes
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mnemo-table-test-{}-{}-{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    #[test]
    fn parses_f32_safetensors_header() {
        let mut data = vec![0u8; EMBEDDING_DIM * 4];
        for (index, value) in [1.0f32, 2.0, 3.5, -4.0].into_iter().enumerate() {
            data[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        let bytes = safetensors_bytes("F32", &[1, EMBEDDING_DIM], &data);
        let mut reader = Cursor::new(bytes);
        let (embeddings, vocab_size, dim) = parse_safetensors(&mut reader).expect("parse");

        assert_eq!(vocab_size, 1);
        assert_eq!(dim, EMBEDDING_DIM);
        assert_eq!(embeddings.len(), EMBEDDING_DIM);
        assert_eq!(&embeddings[..4], &[1.0, 2.0, 3.5, -4.0]);
    }

    #[test]
    fn parses_f16_safetensors_header() {
        // 1.0, -2.0, 0.5, 0.0 in IEEE 754 half precision.
        let values: [u16; 4] = [0x3c00, 0xc000, 0x3800, 0x0000];
        let mut data = Vec::new();
        for value in values {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let mut header_data = data.clone();
        header_data.resize(EMBEDDING_DIM * 2, 0);
        let bytes = safetensors_bytes("F16", &[1, EMBEDDING_DIM], &header_data);
        let mut reader = Cursor::new(bytes);
        let (embeddings, vocab_size, dim) = parse_safetensors(&mut reader).expect("parse");

        assert_eq!(vocab_size, 1);
        assert_eq!(dim, EMBEDDING_DIM);
        assert_eq!(&embeddings[..4], &[1.0, -2.0, 0.5, 0.0]);
    }

    #[test]
    fn rejects_wrong_embedding_width() {
        let data = vec![0u8; 3 * 4];
        let bytes = safetensors_bytes("F32", &[1, 3], &data);
        let mut reader = Cursor::new(bytes);
        assert!(parse_safetensors(&mut reader).is_none());
    }

    #[test]
    fn wordpiece_tokenization() {
        // 128 rows is more than enough for this tiny vocab.
        let table = StaticTable::test_table(
            vec![0.0; 8 * EMBEDDING_DIM],
            &[
                ("[PAD]", 0),
                ("[UNK]", 1),
                ("hello", 2),
                ("world", 3),
                ("##ing", 4),
                ("play", 5),
                ("##ed", 6),
            ],
            EMBEDDING_DIM,
        );

        assert_eq!(table.token_ids("hello world"), vec![2, 3]);
        assert_eq!(table.token_ids("playing"), vec![5, 4]);
        assert_eq!(table.token_ids("played"), vec![5, 6]);
        assert_eq!(table.token_ids("zzz"), vec![1]);
        assert_eq!(table.token_ids("hello zzz world"), vec![2, 1, 3]);
        // Lowercased before lookup, punctuation isolated, whitespace collapsed.
        assert_eq!(table.token_ids("  Hello,\tWORLD!  "), vec![2, 1, 3, 1]);
    }

    #[test]
    fn static_embedding_is_mean_pooled_and_normalized() {
        let mut embeddings = vec![0.0f32; 8 * EMBEDDING_DIM];
        // Row 2 = all ones, row 3 = all twos.
        for v in &mut embeddings[2 * EMBEDDING_DIM..3 * EMBEDDING_DIM] {
            *v = 1.0;
        }
        for v in &mut embeddings[3 * EMBEDDING_DIM..4 * EMBEDDING_DIM] {
            *v = 2.0;
        }
        let table = StaticTable::test_table(
            embeddings,
            &[("[PAD]", 0), ("[UNK]", 1), ("hello", 2), ("world", 3)],
            EMBEDDING_DIM,
        );

        let vector = table.embed("hello world");
        assert_eq!(vector.len(), EMBEDDING_DIM);
        // Mean of 1s and 2s is 1.5 in every dimension; normalized => 1/sqrt(256).
        let expected = 1.0 / (EMBEDDING_DIM as f32).sqrt();
        for value in &vector {
            assert!((value - expected).abs() < 1e-6, "unexpected value {value}");
        }
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    fn synthetic_embeddings(rows: usize, dim: usize) -> Vec<f32> {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut values = Vec::with_capacity(rows * dim);
        for _ in 0..rows * dim {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((state >> 40) as f32) / (1u64 << 24) as f32;
            values.push(unit * 2.0 - 1.0);
        }
        values
    }

    fn ranking(table: &StaticTable, query: &str, words: &[String]) -> Vec<usize> {
        let query = table.embed(query);
        let mut scored: Vec<(usize, f32)> = words
            .iter()
            .enumerate()
            .map(|(index, word)| {
                let vector = table.embed(word);
                let score = query.iter().zip(&vector).map(|(a, b)| a * b).sum::<f32>();
                (index, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.into_iter().map(|(index, _)| index).collect()
    }

    #[test]
    fn quantized_table_reports_budget_and_keeps_recall() {
        // Quality gate on a synthetic golden set: the real model artifacts are
        // not part of the repository, so ranking is compared row-for-row with
        // the f32 matrix. This catches quantization-induced rank inversions.
        let rows = 96;
        let dim = 64;
        let embeddings = synthetic_embeddings(rows, dim);
        let words: Vec<String> = (0..rows).map(|i| format!("w{i}")).collect();
        let vocab: Vec<(&str, u32)> = words
            .iter()
            .enumerate()
            .map(|(i, word)| (word.as_str(), i as u32))
            .collect();

        let f32_table = StaticTable::test_table_f32(embeddings.clone(), &vocab, dim);
        let i8_table = StaticTable::test_table(embeddings, &vocab, dim);

        let mut hits = 0usize;
        let mut f32_rr = 0.0f32;
        let mut i8_rr = 0.0f32;
        for word in &words {
            let expected = words.iter().position(|w| w == word).unwrap();
            let f32_rank = ranking(&f32_table, word, &words);
            let i8_rank = ranking(&i8_table, word, &words);

            if i8_rank.iter().take(5).any(|&index| index == expected) {
                hits += 1;
            }
            let f32_position = f32_rank.iter().position(|&i| i == expected).unwrap() + 1;
            let i8_position = i8_rank.iter().position(|&i| i == expected).unwrap() + 1;
            f32_rr += 1.0 / f32_position as f32;
            i8_rr += 1.0 / i8_position as f32;
        }

        let recall = hits as f32 / rows as f32;
        let f32_mrr = f32_rr / rows as f32;
        let i8_mrr = i8_rr / rows as f32;
        println!("K14 synthetic recall@5={recall:.3} mrr(f32)={f32_mrr:.3} mrr(i8)={i8_mrr:.3}");
        assert!(recall >= 0.99, "i8 recall@5 regressed: {recall}");
        assert!(
            i8_mrr >= f32_mrr - 0.01,
            "i8 MRR regressed: {i8_mrr} vs {f32_mrr}"
        );

        // Production shape: potion-base-8M is 29,528 x 256.
        let vocab_rows = 29_528usize;
        let f32_bytes = vocab_rows * EMBEDDING_DIM * std::mem::size_of::<f32>();
        let i8_bytes = vocab_rows * EMBEDDING_DIM + vocab_rows * std::mem::size_of::<f32>();
        println!("K14 table bytes f32={f32_bytes} i8={i8_bytes}");
        assert!(i8_bytes <= 8_000_000, "i8 table exceeds 8 MB: {i8_bytes}");
        assert!(
            f32_bytes >= 29_000_000,
            "unexpected f32 baseline: {f32_bytes}"
        );

        let empty: Vec<(&str, u32)> = Vec::new();
        let dense = StaticTable::test_table_f32(
            vec![0.0; vocab_rows * EMBEDDING_DIM],
            &empty,
            EMBEDDING_DIM,
        );
        let compact =
            StaticTable::test_table(vec![0.0; vocab_rows * EMBEDDING_DIM], &empty, EMBEDDING_DIM);
        assert_eq!(dense.table_bytes(), f32_bytes);
        assert_eq!(compact.table_bytes(), i8_bytes);
    }

    #[test]
    #[ignore = "reports p50; run manually with --ignored --nocapture"]
    fn embed_latency_f32_vs_i8() {
        let rows = 512;
        let dim = EMBEDDING_DIM;
        let embeddings = synthetic_embeddings(rows, dim);
        let words: Vec<String> = (0..rows).map(|i| format!("token{i}")).collect();
        let vocab: Vec<(&str, u32)> = words
            .iter()
            .enumerate()
            .map(|(i, word)| (word.as_str(), i as u32))
            .collect();
        let f32_table = StaticTable::test_table_f32(embeddings.clone(), &vocab, dim);
        let i8_table = StaticTable::test_table(embeddings, &vocab, dim);
        let text = words.join(" ");
        let iterations = 2_000;

        let measure = |table: &StaticTable| {
            let mut samples = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let start = std::time::Instant::now();
                std::hint::black_box(table.embed(&text));
                samples.push(start.elapsed().as_nanos() as u64);
            }
            samples.sort_unstable();
            samples[samples.len() / 2]
        };

        let f32_p50 = measure(&f32_table);
        let i8_p50 = measure(&i8_table);
        println!("K14 embed p50 f32={f32_p50}ns i8={i8_p50}ns");
        assert!(
            i8_p50 <= f32_p50.saturating_mul(2),
            "i8 p50 more than 2x f32: {i8_p50} vs {f32_p50}"
        );
    }

    #[test]
    fn load_returns_none_for_missing_directory() {
        let dir = temp_dir("missing");
        assert!(StaticTable::load(&dir).is_none());
    }

    #[test]
    fn load_returns_none_for_corrupt_artifacts() {
        let dir = temp_dir("corrupt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("model.safetensors"), b"not a safetensors file").unwrap();
        std::fs::write(dir.join("tokenizer.json"), b"{ not json").unwrap();
        std::fs::write(dir.join("config.json"), b"{}").unwrap();

        assert!(StaticTable::load(&dir).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod real_model_validation {
    use super::StaticTable;
    use std::path::Path;

    #[test]
    #[ignore = "requires MNEMO_MODEL_DIR pointing at Potion artifacts"]
    fn real_model_table_bytes_and_load() {
        let dir = std::env::var("MNEMO_MODEL_DIR")
            .expect("MNEMO_MODEL_DIR must point at a model directory");
        let path = Path::new(&dir);
        let f32_table = StaticTable::load_with_f32(path, true).expect("f32 table");
        let i8_table = StaticTable::load_with_f32(path, false).expect("i8 table");
        println!(
            "K14 real model: vocab={} dim={} f32_matrix_bytes={} i8_matrix_bytes={} scales_bytes={}",
            f32_table.vocab.len(),
            f32_table.dim,
            f32_table.embeddings.len() * std::mem::size_of::<f32>(),
            i8_table.embeddings_i8.len(),
            i8_table.embedding_scales.len() * std::mem::size_of::<f32>()
        );
        assert!(i8_table.embeddings_i8.len() <= 8 * 1024 * 1024);
    }

    fn cosine(a: &[f32], b: &[f32]) -> f32 {
        let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
        let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
        if na == 0.0 || nb == 0.0 {
            0.0
        } else {
            dot / (na * nb)
        }
    }

    fn mrr(table: &StaticTable, queries: &[(&str, &[&str])]) -> f32 {
        let mut total = 0.0f32;
        for (query, docs) in queries {
            let query_vec = table.embed(query);
            let mut scored: Vec<(f32, usize)> = docs
                .iter()
                .enumerate()
                .map(|(index, doc)| (cosine(&query_vec, &table.embed(doc)), index))
                .collect();
            scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            if let Some(rank) = scored.iter().position(|(_, index)| *index == 0) {
                total += 1.0 / (rank as f32 + 1.0);
            }
        }
        total / queries.len() as f32
    }

    #[test]
    #[ignore = "requires MNEMO_MODEL_DIR pointing at Potion artifacts"]
    fn real_model_i8_relevance_stays_within_one_percent() {
        let dir = std::env::var("MNEMO_MODEL_DIR")
            .expect("MNEMO_MODEL_DIR must point at a model directory");
        let path = Path::new(&dir);
        let f32_table = StaticTable::load_with_f32(path, true).expect("f32 table");
        let i8_table = StaticTable::load_with_f32(path, false).expect("i8 table");

        // A small labelled set: the relevant document is always index 0.
        let queries: &[(&str, &[&str])] = &[
            (
                "invoice payment",
                &["billing receipt total due", "cat photo", "server logs"],
            ),
            (
                "deploy docker",
                &["container image build deploy", "resume cv", "invoice"],
            ),
            (
                "vacation beach",
                &["summer holiday ocean sand", "database migration", "invoice"],
            ),
            (
                "resume cv",
                &[
                    "curriculum vitae job experience",
                    "invoice payment",
                    "docker deploy",
                ],
            ),
            (
                "error timeout",
                &["connection timed out retry", "holiday photo", "invoice"],
            ),
            (
                "screenshot design",
                &["ui mockup wireframe design", "invoice", "server logs"],
            ),
            (
                "meeting notes",
                &["minutes agenda discussion", "photo", "invoice"],
            ),
            (
                "database query",
                &["sql index table query", "holiday", "invoice"],
            ),
            (
                "photo sunset",
                &["beach ocean sky sunset", "database", "invoice"],
            ),
            (
                "security password",
                &["login credential secret", "invoice", "photo"],
            ),
            (
                "machine learning",
                &["neural network training model", "invoice", "photo"],
            ),
            (
                "budget report",
                &["finance spreadsheet cost", "holiday", "docker"],
            ),
        ];

        let f32_mrr = mrr(&f32_table, queries);
        let i8_mrr = mrr(&i8_table, queries);
        println!("K14 real model MRR: f32={f32_mrr:.4} i8={i8_mrr:.4}");
        assert!(
            i8_mrr + 0.01 >= f32_mrr,
            "i8 MRR {i8_mrr:.4} is more than 0.01 below f32 {f32_mrr:.4}"
        );
    }
}
