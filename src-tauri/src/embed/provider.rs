use serde::{Deserialize, Serialize};

/// Identifies the embedding engine provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbeddingProviderKind {
    /// Potion-base-8M static lookup table (or deterministic hashing fallback).
    Static,
    /// Downloaded ONNX model pack (e.g. bge-small or SigLIP).
    Smart,
    /// Lexical only; no semantic vectors.
    None,
}

impl std::fmt::Display for EmbeddingProviderKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static => write!(f, "static"),
            Self::Smart => write!(f, "smart"),
            Self::None => write!(f, "none"),
        }
    }
}
