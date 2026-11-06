//! Settings for the Mnemo additions (Ask and Activity), stored next to the upstream config
//! in `mnemo.json`. Everything here is opt-in and local. The LLM API key is never stored:
//! it is read from `MNEMO_LLM_API_KEY` only.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MnemoSettings {
    /// Window-focus tracking (Windows). Off by default.
    pub activity_enabled: bool,
    /// Periodic import of Chromium/Firefox history. Off by default.
    pub browser_history_enabled: bool,
    pub activity_exclude_apps: Vec<String>,
    pub activity_exclude_urls: Vec<String>,
    /// OpenAI-compatible endpoint, plain HTTP, e.g. `http://127.0.0.1:11434/v1`.
    pub llm_endpoint: String,
    pub llm_model: String,
    /// Allow a non-loopback endpoint (questions and passages leave the device).
    pub llm_allow_remote: bool,
    /// Encrypt the full-text index (extracted document text) on disk. Switching rebuilds it.
    pub encrypt_at_rest: bool,
}

impl MnemoSettings {
    fn path() -> PathBuf {
        crate::config::MnemoConfig::config_dir().join("mnemo.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if let Some(parsed) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            return parsed;
        }
        // First run of this build: encryption is on by default for a fresh install, but an
        // existing plaintext index is left alone until the user opts in (it would have to be
        // rebuilt).
        let fresh = !crate::config::MnemoConfig::config_dir()
            .join("index")
            .exists();
        let settings = Self {
            encrypt_at_rest: fresh,
            ..Self::default()
        };
        let _ = settings.save();
        settings
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
    }
}
