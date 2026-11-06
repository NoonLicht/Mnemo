//! `mnemo-cli`: script-friendly access to the local Mnemo index.
//!
//! The binary in `src/bin/cli.rs` is a thin wrapper around [`run`]. All output
//! for the machine-readable modes goes to stdout; diagnostics go to stderr.

use crate::agent::{HeadlessIndex, NO_INDEX_MESSAGE, compose_query, connect, http, mcp};

pub const EXIT_OK: i32 = 0;
/// Runtime failure (unreadable config, I/O error, ...).
pub const EXIT_FAILURE: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_NO_INDEX: i32 = 3;

const DEFAULT_LIMIT: usize = 10;

const USAGE: &str = "\
mnemo-cli — search the local Mnemo index

USAGE:
    mnemo-cli search \"<query>\" [--json] [--limit N] [--exhaustive] [--path P] [--ext E] [--since 7d|30d|1h]
    mnemo-cli state [--json]
    mnemo-cli mcp
    mnemo-cli serve [--port N]
    mnemo-cli connect <claude-desktop|claude-code|cursor> [--write]
    mnemo-cli --help | -h | help

EXIT CODES:
    0  success
    2  usage error
    3  no Mnemo index found (launch Mnemo once to build it)";

/// Runs the CLI with `args` (including the program name as the first item) and
/// returns the process exit code.
pub fn run<I>(args: I) -> i32
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let _program = args.next();
    let args: Vec<String> = args.collect();

    let Some(command) = args.first().map(String::as_str) else {
        println!("{}", USAGE);
        return EXIT_OK;
    };

    match command {
        "search" => run_search(&args[1..]),
        "state" => run_state(&args[1..]),
        "mcp" => match mcp::serve() {
            Ok(()) => EXIT_OK,
            Err(e) => {
                eprintln!("mnemo-cli: {}", e);
                EXIT_FAILURE
            }
        },
        "serve" => run_serve(&args[1..]),
        "connect" => connect::run_cli(&args[1..]),
        "-h" | "--help" | "help" => {
            println!("{}", USAGE);
            EXIT_OK
        }
        other => {
            eprintln!("mnemo-cli: unknown command '{}'", other);
            eprintln!();
            eprintln!("{}", USAGE);
            EXIT_USAGE
        }
    }
}

#[derive(Debug)]
struct SearchOptions {
    query: String,
    json: bool,
    limit: usize,
    exhaustive: bool,
    path: Option<String>,
    ext: Option<String>,
    since: Option<String>,
}

impl SearchOptions {
    fn composed_query(&self) -> String {
        compose_query(
            &self.query,
            self.path.as_deref(),
            self.ext.as_deref(),
            self.since.as_deref(),
            self.exhaustive,
        )
    }
}

fn run_search(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", USAGE);
        return EXIT_OK;
    }

    let options = match parse_search_args(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("mnemo-cli: {}", message);
            eprintln!();
            eprintln!("{}", USAGE);
            return EXIT_USAGE;
        }
    };

    if options.query.trim().is_empty() {
        eprintln!("mnemo-cli: search requires a query");
        eprintln!();
        eprintln!("{}", USAGE);
        return EXIT_USAGE;
    }

    let index = match open_index() {
        Ok(index) => index,
        Err(code) => return code,
    };

    let response = match index.search(&options.composed_query(), options.limit) {
        Ok(response) => response,
        Err(e) => {
            eprintln!("mnemo-cli: search failed: {}", e);
            return EXIT_FAILURE;
        }
    };

    if options.json {
        match serde_json::to_string_pretty(&response) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("mnemo-cli: could not encode results: {}", e);
                return EXIT_FAILURE;
            }
        }
        return EXIT_OK;
    }

    for result in &response.results {
        println!(
            "{}\t{:.4}\t{}",
            result.path,
            result.score,
            result.snippet.as_deref().unwrap_or("")
        );
    }

    EXIT_OK
}

fn parse_search_args(args: &[String]) -> Result<SearchOptions, String> {
    let mut query_parts: Vec<String> = Vec::new();
    let mut json = false;
    let mut limit: Option<usize> = None;
    let mut exhaustive = false;
    let mut path = None;
    let mut ext = None;
    let mut since = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let (flag, inline_value) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };

        match flag {
            "--json" => {
                json = true;
                i += 1;
            }
            "--exhaustive" => {
                exhaustive = true;
                i += 1;
            }
            "--limit" | "--path" | "--ext" | "--since" => {
                let value = match inline_value {
                    Some(value) => value,
                    None => {
                        i += 1;
                        args.get(i)
                            .cloned()
                            .ok_or_else(|| format!("missing value for {}", flag))?
                    }
                };
                match flag {
                    "--limit" => {
                        let parsed = value
                            .parse::<usize>()
                            .ok()
                            .filter(|limit| *limit > 0)
                            .ok_or_else(|| {
                                format!("--limit expects a positive integer, got '{}'", value)
                            })?;
                        limit = Some(parsed);
                    }
                    "--path" => path = Some(value),
                    "--ext" => ext = Some(value),
                    "--since" => since = Some(value),
                    _ => unreachable!(),
                }
                i += 1;
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown option '{}'", arg));
            }
            _ => {
                query_parts.push(arg.clone());
                i += 1;
            }
        }
    }

    Ok(SearchOptions {
        query: query_parts.join(" "),
        json,
        limit: limit.unwrap_or(DEFAULT_LIMIT),
        exhaustive,
        path,
        ext,
        since,
    })
}

fn run_state(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", USAGE);
        return EXIT_OK;
    }

    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            other => {
                eprintln!("mnemo-cli: unknown option '{}'", other);
                eprintln!();
                eprintln!("{}", USAGE);
                return EXIT_USAGE;
            }
        }
    }

    let index = match open_index() {
        Ok(index) => index,
        Err(code) => return code,
    };

    let state = match index.state() {
        Ok(state) => state,
        Err(e) => {
            eprintln!("mnemo-cli: could not read index state: {}", e);
            return EXIT_FAILURE;
        }
    };

    if json {
        match serde_json::to_string_pretty(&state) {
            Ok(encoded) => println!("{}", encoded),
            Err(e) => {
                eprintln!("mnemo-cli: could not encode state: {}", e);
                return EXIT_FAILURE;
            }
        }
        return EXIT_OK;
    }

    for key in [
        "files",
        "dirs",
        "content_docs",
        "semantic_units",
        "index_bytes",
    ] {
        println!("{}: {}", key, state[key]);
    }

    match state["model_id"].as_u64() {
        Some(id) => println!("units model: {}", id),
        None => println!("units model: none"),
    }
    println!(
        "active model: {} ({})",
        state["active_model_id"],
        state["model_name"].as_str().unwrap_or("unknown")
    );

    let roots = state["roots"]
        .as_array()
        .map(|roots| {
            roots
                .iter()
                .map(|root| root.as_str().unwrap_or(""))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!(
        "roots: {}",
        if roots.is_empty() { "(none)" } else { &roots }
    );
    match state["last_reconcile"].as_u64() {
        Some(secs) => println!("last_reconcile: {}", secs),
        None => println!("last_reconcile: never"),
    }

    EXIT_OK
}

/// `mnemo-cli serve`: loopback HTTP/SSE transport for MCP-style clients.
fn run_serve(args: &[String]) -> i32 {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", USAGE);
        return EXIT_OK;
    }

    let port = match http::parse_port(args) {
        Ok(port) => port,
        Err(message) => {
            eprintln!("mnemo-cli: {}", message);
            return EXIT_USAGE;
        }
    };

    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    http::install_ctrl_c();

    match http::serve(&crate::config::MnemoConfig::config_dir(), port, shutdown) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("mnemo-cli: {}", e);
            EXIT_FAILURE
        }
    }
}

fn open_index() -> Result<HeadlessIndex, i32> {
    match HeadlessIndex::open() {
        Ok(index) => Ok(index),
        Err(e) => {
            eprintln!("mnemo-cli: {}", e);
            if e == NO_INDEX_MESSAGE {
                Err(EXIT_NO_INDEX)
            } else {
                Err(EXIT_FAILURE)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn composes_path_with_spaces_ext_since_and_exhaustive() {
        let options = parse_search_args(&args(&[
            "hello world",
            "--path",
            "/tmp/My Documents",
            "--ext",
            ".rs",
            "--since",
            "7d",
            "--exhaustive",
        ]))
        .expect("parse");

        assert_eq!(
            options.composed_query(),
            "hello world in:\"/tmp/My Documents\" ext:rs modified:7d mode:exhaustive"
        );
    }

    #[test]
    fn escapes_quotes_and_backslashes_in_path() {
        let options =
            parse_search_args(&args(&["needle", "--path", "/tmp/a\"b\\c"])).expect("parse");

        assert_eq!(options.composed_query(), "needle in:\"/tmp/a\\\"b\\\\c\"");
    }

    #[test]
    fn supports_equals_style_flags_and_plain_query() {
        let options =
            parse_search_args(&args(&["alpha beta", "--limit=25", "--ext=md"])).expect("parse");

        assert_eq!(options.limit, 25);
        assert_eq!(options.composed_query(), "alpha beta ext:md");
    }

    #[test]
    fn rejects_unknown_flags_and_bad_limits() {
        assert!(parse_search_args(&args(&["x", "--nope"])).is_err());
        assert!(parse_search_args(&args(&["x", "--limit", "0"])).is_err());
        assert!(parse_search_args(&args(&["x", "--limit", "many"])).is_err());
        assert!(parse_search_args(&args(&["x", "--limit"])).is_err());
    }

    #[test]
    fn help_and_usage_commands_do_not_touch_the_index() {
        assert_eq!(run(args(&["mnemo-cli", "--help"])), EXIT_OK);
        assert_eq!(run(args(&["mnemo-cli", "help"])), EXIT_OK);
        assert_eq!(run(args(&["mnemo-cli", "not-a-command"])), EXIT_USAGE);
    }
}
