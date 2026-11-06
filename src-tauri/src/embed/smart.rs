//! Optional ONNX smart embedding pack using `ort`.
//!
//! Provides a transformer-based text embedder (e.g., `bge-small-en-v1.5` int8)
//! behind the `smart-embeddings` Cargo feature. When the feature is disabled
//! or model files are missing, callers cleanly fall back to `StaticTable`.

use std::path::Path;

#[cfg(not(feature = "smart-embeddings"))]
#[derive(Debug, Clone)]
pub struct SmartEmbedder;

#[cfg(not(feature = "smart-embeddings"))]
impl SmartEmbedder {
    pub fn load(_dir: &Path) -> Option<Self> {
        None
    }

    pub fn embed(&self, _text: &str) -> Vec<i8> {
        Vec::new()
    }

    pub fn embed_f32(&self, _text: &str) -> Vec<f32> {
        Vec::new()
    }
}

#[cfg(feature = "smart-embeddings")]
use crate::embed::wordpiece::{WordPieceTokenizer, l2_normalize};
#[cfg(feature = "smart-embeddings")]
use ort::session::Session;
#[cfg(feature = "smart-embeddings")]
use parking_lot::Mutex;
#[cfg(feature = "smart-embeddings")]
use std::sync::Arc;

#[cfg(feature = "smart-embeddings")]
pub struct SmartEmbedder {
    session: Arc<Mutex<Session>>,
    tokenizer: WordPieceTokenizer,
    pub dim: usize,
    has_token_types: bool,
    output_name: String,
}

#[cfg(feature = "smart-embeddings")]
impl std::fmt::Debug for SmartEmbedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmartEmbedder")
            .field("dim", &self.dim)
            .field("has_token_types", &self.has_token_types)
            .field("output_name", &self.output_name)
            .finish()
    }
}

#[cfg(feature = "smart-embeddings")]
impl SmartEmbedder {
    pub fn load(dir: &Path) -> Option<Self> {
        let model_path = dir.join("model.onnx");
        let tokenizer_path = dir.join("tokenizer.json");
        let config_path = dir.join("config.json");

        if !model_path.is_file() || !tokenizer_path.is_file() {
            return None;
        }

        let tokenizer_bytes = std::fs::read(&tokenizer_path).ok()?;
        let tokenizer_json: serde_json::Value = serde_json::from_slice(&tokenizer_bytes).ok()?;
        let tokenizer = WordPieceTokenizer::parse(&tokenizer_json, None)?;

        let dim = match std::fs::read(&config_path) {
            Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                Ok(cfg) => cfg
                    .get("hidden_size")
                    .or_else(|| cfg.get("hidden_dim"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize)
                    .unwrap_or(384),
                Err(_) => 384,
            },
            Err(_) => 384,
        };

        static ENVIRONMENT: Mutex<Option<ort::environment::Environment>> = Mutex::new(None);
        let session = {
            let mut env = ENVIRONMENT.lock();
            if env.is_none() {
                *env = Some(ort::init().build().ok()?);
            }
            let builder = ort::session::Session::builder(env.as_ref().unwrap()).ok()?;
            // Bound intra-op threads to 1 to stay within background budget
            let mut builder = builder.with_intra_threads(1).ok()?;
            builder.commit_from_file(&model_path).ok()?
        };

        let has_token_types = session
            .inputs()
            .iter()
            .any(|inp| inp.name() == "token_type_ids");

        let output_name = session
            .outputs()
            .iter()
            .find(|o| o.name() == "last_hidden_state")
            .or_else(|| session.outputs().first())
            .map(|o| o.name().to_string())
            .unwrap_or_else(|| "last_hidden_state".to_string());

        Some(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer,
            dim,
            has_token_types,
            output_name,
        })
    }

    pub fn embed_f32(&self, text: &str) -> Vec<f32> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return vec![0.0f32; self.dim];
        }

        let (input_ids, attention_mask, token_type_ids) = self.tokenizer.encode_bert(trimmed, 512);

        let seq_len = input_ids.len();
        if seq_len == 0 {
            return vec![0.0f32; self.dim];
        }

        let shape = [1i64, seq_len as i64];
        let t_input_ids = match ort::value::Tensor::from_array((shape, input_ids)) {
            Ok(t) => t,
            Err(_) => return vec![0.0f32; self.dim],
        };
        let t_attention_mask = match ort::value::Tensor::from_array((shape, attention_mask)) {
            Ok(t) => t,
            Err(_) => return vec![0.0f32; self.dim],
        };

        let mut session = self.session.lock();
        let run_result = if self.has_token_types {
            let t_token_type = match ort::value::Tensor::from_array((shape, token_type_ids)) {
                Ok(t) => t,
                Err(_) => return vec![0.0f32; self.dim],
            };
            session.run(ort::inputs![
                "input_ids" => t_input_ids,
                "attention_mask" => t_attention_mask,
                "token_type_ids" => t_token_type,
            ])
        } else {
            session.run(ort::inputs![
                "input_ids" => t_input_ids,
                "attention_mask" => t_attention_mask,
            ])
        };

        let outputs = match run_result {
            Ok(o) => o,
            Err(_) => return vec![0.0f32; self.dim],
        };

        let (shape, slice) = match outputs[self.output_name.as_str()].try_extract_tensor::<f32>() {
            Ok(res) => res,
            Err(_) => return vec![0.0f32; self.dim],
        };

        let hidden_dim = if shape.len() >= 3 {
            shape[2] as usize
        } else {
            self.dim
        };

        let mut pooled = vec![0.0f32; hidden_dim];
        for step in 0..seq_len {
            let base = step * hidden_dim;
            if base + hidden_dim <= slice.len() {
                for (d, slot) in pooled.iter_mut().enumerate() {
                    *slot += slice[base + d];
                }
            }
        }

        let scale = 1.0 / seq_len as f32;
        for val in &mut pooled {
            *val *= scale;
        }

        l2_normalize(&mut pooled);
        pooled
    }

    pub fn embed(&self, text: &str) -> Vec<i8> {
        let f32_vec = self.embed_f32(text);
        f32_vec
            .iter()
            .map(|&v| (v * 127.0).round().clamp(-127.0, 127.0) as i8)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // These tests exercise the stub that is compiled when the `smart-embeddings`
    // feature is OFF (the default in `cargo test`). The stub must never panic
    // and must return sentinels that callers can detect.

    #[test]
    fn load_always_returns_none_without_feature() {
        // A non-existent directory is fine — the stub returns None regardless.
        let result = SmartEmbedder::load(Path::new("/tmp/no-such-model-dir"));
        assert!(
            result.is_none(),
            "SmartEmbedder::load must be None without smart-embeddings feature"
        );
    }

    #[test]
    fn embed_returns_empty_without_feature() {
        // We can only call embed on a value we construct. With the feature off
        // SmartEmbedder is a unit struct, so just build one directly.
        let e = SmartEmbedder;
        assert!(e.embed("hello world").is_empty());
    }

    #[test]
    fn embed_f32_returns_empty_without_feature() {
        let e = SmartEmbedder;
        assert!(e.embed_f32("hello world").is_empty());
    }

    #[test]
    fn embed_empty_text_is_empty_without_feature() {
        let e = SmartEmbedder;
        assert!(e.embed("").is_empty());
        assert!(e.embed_f32("").is_empty());
    }
}
