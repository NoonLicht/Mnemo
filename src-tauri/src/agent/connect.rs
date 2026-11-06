//! One-click agent connect: generate and install Mnemo MCP configs for the
//! common clients.
//!
//! Mnemo is one binary with two surfaces (`mnemo-cli mcp` over stdio, plus
//! the loopback HTTP transport); every client config points at the same
//! read-only server. Installation means a **merge** into the client's own
//! config file — never a rewrite of the whole file — with a `.bak` backup
//! left next to it. The manual JSON fallback stays available in the UI and
//! via `mnemo-cli connect <client> --print`.

use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeDesktop,
    ClaudeCode,
    Cursor,
}

impl Client {
    pub const ALL: [Self; 3] = [Self::ClaudeDesktop, Self::ClaudeCode, Self::Cursor];

    pub fn id(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "claude-desktop",
            Self::ClaudeCode => "claude-code",
            Self::Cursor => "cursor",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
            Self::Cursor => "Cursor",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|client| client.id() == id)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientStatus {
    pub id: &'static str,
    pub name: &'static str,
    /// Absolute config path when the client's config dir is locatable.
    pub config_path: Option<String>,
    /// True when the config file already exists on disk.
    pub installed: bool,
    /// True when the file exists and already wires `mnemo`.
    pub configured: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstallOutcome {
    pub status: ClientStatus,
    pub wrote: bool,
    pub backup: Option<String>,
    pub message: String,
}

/// Candidate config paths for a client on this OS. The first existing file
/// wins for `installed`/`configured` and for the install target; otherwise the
/// first locatable path is used.
fn config_paths(client: Client) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    match client {
        Client::ClaudeDesktop => {
            // macOS keeps app data in ~/Library/Application Support; the Linux
            // and Windows builds read from the platform config dir
            // (~/.config and %APPDATA% respectively).
            #[cfg(target_os = "macos")]
            let base = dirs::data_dir();
            #[cfg(not(target_os = "macos"))]
            let base = dirs::config_dir();
            base.map(|base| vec![base.join("Claude/claude_desktop_config.json")])
                .unwrap_or_default()
        }
        Client::ClaudeCode => vec![home.join(".claude.json")],
        Client::Cursor => vec![home.join(".cursor/mcp.json")],
    }
}

fn primary_path(client: Client) -> Option<PathBuf> {
    let paths = config_paths(client);
    paths
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .or_else(|| paths.into_iter().next())
}

/// The MCP server entry for Mnemo, with the best absolute path we can find
/// for `mnemo-cli`.
pub fn server_entry() -> Value {
    json!({
        "command": cli_command(),
        "args": ["mcp"],
    })
}

/// Best-effort absolute path to the CLI binary shipped next to this process.
pub fn cli_command() -> String {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("mnemo-cli"));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".cargo/bin/mnemo-cli"));
        candidates.push(home.join(".local/bin/mnemo-cli"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            candidates.push(dir.join("mnemo-cli"));
        }
    }
    candidates.push(PathBuf::from("mnemo-cli"));
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mnemo-cli".to_string())
}

/// Status for every supported client (locatable config path, installed,
/// configured). Never fails: unlocatable clients report `None`.
pub fn detect_clients() -> Vec<ClientStatus> {
    Client::ALL
        .iter()
        .map(|client| {
            let path = primary_path(*client);
            let installed = path.as_ref().map(|path| path.is_file()).unwrap_or(false);
            let configured = installed
                && path
                    .as_ref()
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .map(|raw| is_configured(&raw))
                    .unwrap_or(false);
            ClientStatus {
                id: client.id(),
                name: client.name(),
                config_path: path.map(|path| path.to_string_lossy().into_owned()),
                installed,
                configured,
            }
        })
        .collect()
}

fn is_configured(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    if has_mnemo(value.get("mcpServers")) {
        return true;
    }
    // Claude Code also stores local-scope servers per project.
    value
        .get("projects")
        .and_then(Value::as_object)
        .map(|projects| {
            projects
                .values()
                .any(|project| has_mnemo(project.get("mcpServers")))
        })
        .unwrap_or(false)
}

fn has_mnemo(servers: Option<&Value>) -> bool {
    servers
        .and_then(Value::as_object)
        .and_then(|servers| servers.get("mnemo"))
        .map(|entry| entry.get("command").is_some())
        .unwrap_or(false)
}

/// The ready-to-paste JSON snippet for a client's config file.
pub fn config_snippet(_client: Client) -> Result<String, String> {
    let pretty = serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "mnemo": server_entry(),
        }
    }))
    .map_err(|e| e.to_string())?;
    Ok(pretty)
}

/// `<path>.bak`, preserving the full original file name (e.g.
/// `~/.claude.json.bak`, not `~/.claude.bak`).
fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Writes `contents` through a sibling temp file and renames it into place, so
/// an interrupted write cannot truncate the user's config. New files are 0600;
/// an existing file keeps its mode.
fn write_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("create dir {}: {}", parent.display(), e))?;

    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        ".{}.tmp-{}-{}",
        file_name,
        std::process::id(),
        stamp
    ));

    std::fs::write(&tmp, contents).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|metadata| metadata.permissions().mode())
            .unwrap_or(0o600);
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("replace {}: {}", path.display(), e)
    })
}

/// Merges the Mnemo entry into the client's config file.
///
/// Creates missing parent dirs and the file itself, backs up any previous
/// content to `<path>.bak`, and never removes unrelated entries. The write is
/// atomic. Returns the post-install status.
pub fn install(client: Client) -> Result<InstallOutcome, String> {
    let Some(path) = primary_path(client) else {
        return Err(format!(
            "could not locate the {} config directory on this machine",
            client.name()
        ));
    };

    let previous = if path.is_file() {
        Some(
            std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {}", path.display(), e))?,
        )
    } else {
        None
    };

    let mut root = match &previous {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .map_err(|e| format!("{} is not valid JSON: {}", path.display(), e))?,
        None => json!({}),
    };
    if !root.is_object() {
        return Err(format!("{} is not a JSON object", path.display()));
    }

    let servers = root
        .as_object_mut()
        .expect("checked object")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        return Err(format!(
            "{} has a non-object \"mcpServers\" key; merge manually",
            path.display()
        ));
    }
    servers
        .as_object_mut()
        .expect("checked object")
        .insert("mnemo".to_string(), server_entry());

    let merged = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;

    let mut backup = None;
    if let Some(previous) = &previous {
        let backup_path = backup_path(&path);
        write_atomic(&backup_path, previous)
            .map_err(|e| format!("write backup {}: {}", backup_path.display(), e))?;
        backup = Some(backup_path.to_string_lossy().into_owned());
    }
    write_atomic(&path, &merged)?;

    let status = ClientStatus {
        id: client.id(),
        name: client.name(),
        config_path: Some(path.to_string_lossy().into_owned()),
        installed: true,
        configured: true,
    };
    Ok(InstallOutcome {
        status,
        wrote: true,
        backup,
        message: format!(
            "Mnemo MCP added to {}. Restart {} to pick it up.",
            path.display(),
            client.name()
        ),
    })
}

/// CLI handler: `connect <client> [--write]`. Default prints the snippet.
pub fn run_cli(args: &[String]) -> i32 {
    let write = args.iter().any(|arg| arg == "--write");
    let client = args
        .iter()
        .find(|arg| !arg.starts_with('-'))
        .and_then(|arg| Client::parse(arg));
    let Some(client) = client else {
        eprintln!(
            "mnemo-cli: connect requires one of: {}",
            Client::ALL
                .iter()
                .map(|client| client.id())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return crate::cli::EXIT_USAGE;
    };

    if write {
        match install(client) {
            Ok(outcome) => {
                println!("{}", outcome.message);
                if let Some(backup) = outcome.backup {
                    println!("backup: {}", backup);
                }
                crate::cli::EXIT_OK
            }
            Err(message) => {
                eprintln!("mnemo-cli: {}", message);
                crate::cli::EXIT_FAILURE
            }
        }
    } else {
        match config_snippet(client) {
            Ok(snippet) => {
                println!("{}", snippet);
                println!();
                println!(
                    "# paste into {} (or run with --write to install)",
                    primary_path(client)
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_else(|| client.name().to_string())
                );
                crate::cli::EXIT_OK
            }
            Err(message) => {
                eprintln!("mnemo-cli: {}", message);
                crate::cli::EXIT_FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_are_well_formed_and_local() {
        let statuses = detect_clients();
        assert_eq!(statuses.len(), 3);
        for status in &statuses {
            assert!(!status.name.is_empty());
            if status.configured {
                assert!(status.installed, "configured implies installed");
            }
        }
    }

    #[test]
    fn snippet_is_valid_json_with_mnemo_entry() {
        let snippet = config_snippet(Client::Cursor).unwrap();
        let value: Value = serde_json::from_str(&snippet).unwrap();
        assert_eq!(value["mcpServers"]["mnemo"]["args"][0], "mcp");
        assert!(
            !value["mcpServers"]["mnemo"]["command"]
                .as_str()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn is_configured_matches_only_real_entries() {
        assert!(!is_configured("{}"));
        assert!(!is_configured("not json"));
        assert!(is_configured(
            r#"{"mcpServers": {"mnemo": {"command": "mnemo-cli", "args": ["mcp"]}}}"#
        ));
        assert!(!is_configured(
            r#"{"mcpServers": {"other": {"command": "x"}}}"#
        ));
    }

    #[test]
    fn is_configured_detects_project_scoped_entries() {
        assert!(is_configured(
            r#"{"projects": {"/a": {"mcpServers": {"mnemo": {"command": "mnemo-cli"}}}}}"#
        ));
        assert!(!is_configured(
            r#"{"projects": {"/a": {"mcpServers": {"other": {"command": "x"}}}}}"#
        ));
    }

    #[test]
    fn backup_path_keeps_the_full_file_name() {
        assert_eq!(
            backup_path(std::path::Path::new("/home/u/.claude.json")),
            PathBuf::from("/home/u/.claude.json.bak")
        );
        assert_eq!(
            backup_path(std::path::Path::new("/home/u/.cursor/mcp.json")),
            PathBuf::from("/home/u/.cursor/mcp.json.bak")
        );
    }

    #[test]
    fn atomic_write_replaces_and_creates() {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-connect-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let path = dir.join("nested/config.json");
        write_atomic(&path, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        write_atomic(&path, "{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        // No temp files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_connect_rejects_unknown_clients() {
        assert_eq!(run_cli(&["nope".to_string()]), crate::cli::EXIT_USAGE);
        assert_eq!(run_cli(&[]), crate::cli::EXIT_USAGE);
    }
}
