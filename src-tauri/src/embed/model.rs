use crate::config::MnemoConfig;
use crate::embed::provider::EmbeddingProviderKind;
use crate::embed::table::StaticTable;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, OnceLock};

pub const EMBEDDING_DIM: usize = 256;
/// Id of the hashing estimator used when the real model files are unavailable.
pub const DEFAULT_MODEL_ID: u32 = 1;
/// Id of the real `potion-base-8M` static embedding table.
pub const REAL_MODEL_ID: u32 = 2;
/// Id of the optional ONNX smart embedding pack (`bge-small-en-v1.5`).
pub const SMART_MODEL_ID: u32 = 3;

static GLOBAL: OnceLock<ModelRegistry> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRegistry {
    pub provider: EmbeddingProviderKind,
    pub model_id: u32,
    pub name: String,
    pub dim: usize,
    pub normalize: bool,
    pub fallback_reason: Option<String>,
    /// Real embedding table when the model files were loaded; `None` means the
    /// hashing estimator is in use.
    #[serde(skip)]
    table: Option<Arc<StaticTable>>,
    #[serde(skip)]
    smart: Option<Arc<crate::embed::smart::SmartEmbedder>>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new_static()
    }
}

impl ModelRegistry {
    /// Deterministic hashing estimator. Used when the downloaded model files
    /// are absent or unusable, and kept as the `Default` implementation.
    pub fn new_static() -> Self {
        Self {
            provider: EmbeddingProviderKind::Static,
            model_id: DEFAULT_MODEL_ID,
            name: "static-embed-256".to_string(),
            dim: EMBEDDING_DIM,
            normalize: true,
            fallback_reason: None,
            table: None,
            smart: None,
        }
    }

    /// Disabled / lexical-only provider.
    pub fn new_none() -> Self {
        Self {
            provider: EmbeddingProviderKind::None,
            model_id: 0,
            name: "none".to_string(),
            dim: 0,
            normalize: false,
            fallback_reason: None,
            table: None,
            smart: None,
        }
    }

    /// Loads the real static table from `dir` (the downloaded model directory).
    ///
    /// Returns `None` when the artifacts are missing/corrupt or the embedding
    /// width is not [`EMBEDDING_DIM`].
    pub fn load(dir: &Path) -> Option<Self> {
        Self::load_with_f32(dir, false)
    }

    /// Loads the static table, optionally keeping the f32 matrix.
    pub fn load_with_f32(dir: &Path, use_f32: bool) -> Option<Self> {
        let table = StaticTable::load_with_f32(dir, use_f32)?;
        Some(Self {
            provider: EmbeddingProviderKind::Static,
            model_id: table.model_id,
            name: "potion-base-8M".to_string(),
            dim: table.dim,
            normalize: table.normalize,
            fallback_reason: None,
            table: Some(Arc::new(table)),
            smart: None,
        })
    }

    /// Chooses the provider according to `config` and artifact presence in `dir`.
    pub fn from_config_and_dir(config: &MnemoConfig, dir: &Path) -> Self {
        if !config.semantic_enabled {
            return Self::new_none();
        }

        let model_str = config.semantic_model.trim().to_lowercase();
        match model_str.as_str() {
            "none" | "off" | "disabled" => Self::new_none(),
            "static" | "potion" => {
                if let Some(registry) = Self::load_with_f32(dir, config.semantic_f32_table) {
                    registry
                } else {
                    let mut reg = Self::new_static();
                    reg.fallback_reason = Some(format!(
                        "Model artifacts not found in {}; using hashing fallback",
                        dir.display()
                    ));
                    reg
                }
            }
            "smart" => {
                #[cfg(feature = "smart-embeddings")]
                {
                    let smart_dir = std::env::var_os("MNEMO_SMART_MODEL_DIR")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_else(crate::models::smart_dir);
                    if let Some(smart) = crate::embed::smart::SmartEmbedder::load(&smart_dir) {
                        Self {
                            provider: EmbeddingProviderKind::Smart,
                            model_id: SMART_MODEL_ID,
                            name: "bge-small-en-v1.5".to_string(),
                            dim: smart.dim,
                            normalize: true,
                            fallback_reason: None,
                            table: None,
                            smart: Some(Arc::new(smart)),
                        }
                    } else {
                        let fallback_reason = format!(
                            "Smart ONNX artifacts missing or invalid in {}; falling back to static provider",
                            smart_dir.display()
                        );
                        let mut reg =
                            Self::load_with_f32(dir, config.semantic_f32_table).unwrap_or_default();
                        reg.fallback_reason = Some(fallback_reason);
                        reg
                    }
                }
                #[cfg(not(feature = "smart-embeddings"))]
                {
                    let fallback_reason =
                        "Smart ONNX pack is not compiled in this build; falling back to static provider".to_string();
                    let mut reg =
                        Self::load_with_f32(dir, config.semantic_f32_table).unwrap_or_default();
                    reg.fallback_reason = Some(fallback_reason);
                    reg
                }
            }
            other => {
                let fallback_reason = format!(
                    "Unknown semantic_model '{}'; falling back to static provider",
                    other
                );
                if let Some(mut registry) = Self::load_with_f32(dir, config.semantic_f32_table) {
                    registry.fallback_reason = Some(fallback_reason);
                    registry
                } else {
                    let mut reg = Self::new_static();
                    reg.fallback_reason = Some(format!(
                        "{}; artifacts missing in {}, using hashing fallback",
                        fallback_reason,
                        dir.display()
                    ));
                    reg
                }
            }
        }
    }

    /// Process-wide registry. Loads the active embedding provider once.
    /// Hot paths must use this instead of `ModelRegistry::default()`.
    pub fn global() -> &'static ModelRegistry {
        GLOBAL.get_or_init(|| {
            let dir = MnemoConfig::config_dir().join("models");
            let config = MnemoConfig::load();
            let registry = Self::from_config_and_dir(&config, &dir);
            log::info!(
                "[Mnemo] Semantic provider: {} ({}, model_id: {}, dim: {})",
                registry.provider,
                registry.name,
                registry.model_id,
                registry.dim
            );
            if let Some(ref reason) = registry.fallback_reason {
                log::info!("[Mnemo] Semantic model fallback: {}", reason);
            }
            registry
        })
    }

    /// True when semantic indexing and querying are active.
    pub fn is_enabled(&self) -> bool {
        self.provider != EmbeddingProviderKind::None
    }

    /// Provider identifier for status and Doctor reporting.
    pub fn provider_name(&self) -> &'static str {
        match self.provider {
            EmbeddingProviderKind::Static => "static",
            EmbeddingProviderKind::Smart => "smart",
            EmbeddingProviderKind::None => "none",
        }
    }

    /// True when real model weights back this registry.
    pub fn has_static_table(&self) -> bool {
        self.table.is_some()
    }

    /// Tokenizes input text into normalized terms and character 3-grams for subword coverage.
    /// Only used by the hashing estimator; the static table has its own tokenizer.
    pub fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let cleaned = text.to_lowercase();

        for word in cleaned.split(|c: char| !c.is_alphanumeric()) {
            let trimmed = word.trim();
            if trimmed.len() < 2 {
                continue;
            }
            tokens.push(trimmed.to_string());

            // Add character trigrams for words longer than 4 chars
            let chars: Vec<char> = trimmed.chars().collect();
            if chars.len() >= 4 {
                for window in chars.windows(3) {
                    tokens.push(window.iter().collect::<String>());
                }
            }
        }
        tokens
    }

    /// Embeds text into a unit-normalized float vector. Returns empty if disabled.
    pub fn embed(&self, text: &str) -> Vec<f32> {
        if !self.is_enabled() {
            return Vec::new();
        }
        if let Some(smart) = &self.smart {
            return smart.embed_f32(text);
        }
        if let Some(table) = &self.table {
            return table.embed(text);
        }
        self.embed_hashing(text)
    }

    fn embed_hashing(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dim];
        let tokens = self.tokenize(text);
        if tokens.is_empty() {
            return vec;
        }

        for token in &tokens {
            let (h1, h2) = hash_token(token);
            let idx1 = (h1 as usize) % self.dim;
            let idx2 = (h2 as usize) % self.dim;

            let sign1 = if (h1 >> 32) & 1 == 0 { 1.0f32 } else { -1.0f32 };
            let sign2 = if (h2 >> 32) & 1 == 0 { 1.0f32 } else { -1.0f32 };

            vec[idx1] += sign1;
            vec[idx2] += sign2 * 0.5;
        }

        if self.normalize {
            let norm_sq: f32 = vec.iter().map(|v| v * v).sum();
            if norm_sq > 0.0 {
                let norm = norm_sq.sqrt();
                for v in &mut vec {
                    *v /= norm;
                }
            }
        }

        vec
    }

    /// Embeds text directly to signed 8-bit quantized vector (-127..=127).
    pub fn embed_to_i8(&self, text: &str) -> Vec<i8> {
        if !self.is_enabled() {
            return Vec::new();
        }
        let floats = self.embed(text);
        floats
            .into_iter()
            .map(|f| (f * 127.0).round().clamp(-127.0, 127.0) as i8)
            .collect()
    }

    /// Computes cosine similarity between two unit-normalized i8 vectors in [-1.0, 1.0].
    pub fn cosine_similarity_i8(a: &[i8], b: &[i8]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }

        let mut dot: i32 = 0;
        for i in 0..a.len() {
            dot += (a[i] as i32) * (b[i] as i32);
        }

        // 127.0 * 127.0 = 16129.0
        (dot as f32) / 16129.0
    }
}

/// Computes two independent 64-bit FNV-1a hashes with distinct seeds.
fn hash_token(s: &str) -> (u64, u64) {
    let mut h1: u64 = 0xcbf29ce484222325;
    let mut h2: u64 = 0x84222325cbf29ce4;

    for b in s.as_bytes() {
        h1 ^= *b as u64;
        h1 = h1.wrapping_mul(0x100000001b3);

        h2 ^= (*b as u64).rotate_left(4);
        h2 = h2.wrapping_mul(0x100000001b3);
    }

    (h1, h2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_fallback_has_expected_dim_and_unit_norm() {
        let model = ModelRegistry::new_static();
        assert_eq!(model.model_id, DEFAULT_MODEL_ID);
        assert!(!model.has_static_table());

        let vector = model.embed("hello world, this is Mnemo");
        assert_eq!(vector.len(), EMBEDDING_DIM);
        let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");

        let quantized = model.embed_to_i8("hello world");
        assert_eq!(quantized.len(), EMBEDDING_DIM);
    }

    #[test]
    fn default_is_the_hashing_fallback() {
        let model = ModelRegistry::default();
        assert_eq!(model.model_id, DEFAULT_MODEL_ID);
        assert!(!model.has_static_table());
    }

    #[test]
    fn config_round_trip_for_static_none_unknown_fallback() {
        let empty_dir = std::env::temp_dir().join("mnemo-empty-model-test");
        let _ = std::fs::create_dir_all(&empty_dir);

        // 1. static with missing artifacts -> falls back to hashing with recorded reason
        let mut cfg = MnemoConfig {
            semantic_enabled: true,
            semantic_model: "static".to_string(),
            ..Default::default()
        };
        let reg = ModelRegistry::from_config_and_dir(&cfg, &empty_dir);
        assert_eq!(reg.provider, EmbeddingProviderKind::Static);
        assert_eq!(reg.model_id, DEFAULT_MODEL_ID);
        assert!(reg.fallback_reason.is_some());
        assert!(reg.is_enabled());

        // 2. semantic_enabled = false -> provider is None
        cfg.semantic_enabled = false;
        let reg = ModelRegistry::from_config_and_dir(&cfg, &empty_dir);
        assert_eq!(reg.provider, EmbeddingProviderKind::None);
        assert_eq!(reg.model_id, 0);
        assert_eq!(reg.dim, 0);
        assert!(!reg.is_enabled());
        assert!(reg.embed("hello").is_empty());
        assert!(reg.embed_to_i8("hello").is_empty());

        // 3. semantic_model = "none" -> provider is None even if semantic_enabled is true
        cfg.semantic_enabled = true;
        cfg.semantic_model = "none".to_string();
        let reg = ModelRegistry::from_config_and_dir(&cfg, &empty_dir);
        assert_eq!(reg.provider, EmbeddingProviderKind::None);
        assert!(!reg.is_enabled());

        // 4. semantic_model = "unknown-model-xyz" -> falls back to static with reason
        cfg.semantic_model = "unknown-model-xyz".to_string();
        let reg = ModelRegistry::from_config_and_dir(&cfg, &empty_dir);
        assert_eq!(reg.provider, EmbeddingProviderKind::Static);
        assert!(
            reg.fallback_reason
                .as_ref()
                .unwrap()
                .contains("unknown-model-xyz")
        );

        // 5. semantic_model = "smart" -> falls back to static in K16/K17 when files missing
        cfg.semantic_model = "smart".to_string();
        let reg = ModelRegistry::from_config_and_dir(&cfg, &empty_dir);
        assert_eq!(reg.provider, EmbeddingProviderKind::Static);
        assert!(
            reg.fallback_reason.as_ref().unwrap().contains("Smart")
                || reg.fallback_reason.as_ref().unwrap().contains("smart")
        );

        let _ = std::fs::remove_dir_all(&empty_dir);
    }

    #[test]
    #[ignore = "requires real smart model artifacts; set MNEMO_SMART_MODEL_DIR"]
    fn real_smart_model_validation() {
        let smart_dir = match std::env::var("MNEMO_SMART_MODEL_DIR") {
            Ok(d) => std::path::PathBuf::from(d),
            Err(_) => return,
        };
        #[cfg(feature = "smart-embeddings")]
        {
            let embedder = crate::embed::smart::SmartEmbedder::load(&smart_dir)
                .expect("failed to load real smart model");
            let vec = embedder.embed("search query");
            assert_eq!(vec.len(), 384);
            assert!(vec.iter().any(|&v| v != 0));
        }
        #[cfg(not(feature = "smart-embeddings"))]
        {
            let _ = smart_dir;
        }
    }
}
