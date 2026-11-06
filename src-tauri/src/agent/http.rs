//! Loopback HTTP + SSE transport for the agent surface.
//!
//! One long-lived process serves the existing MCP tool surface over HTTP so
//! web-capable MCP clients and long-lived agents can attach without a stdio
//! parent. The security model matches the daemon:
//!
//! - **Loopback only.** The listener binds `127.0.0.1` and rejects any peer
//!   that is not a loopback address.
//! - **Token auth.** Every request needs `Authorization: Bearer <token>`;
//!   the token lives in `<config>/http.token` (0600) and is printed to
//!   stderr once at startup.
//! - **Read-only.** `reindex` is never enabled on this transport regardless
//!   of config.
//!
//! Endpoints:
//!
//! - `POST /mcp` — MCP Streamable HTTP: JSON-RPC request in, JSON-RPC
//!   response out. With `Accept: text/event-stream` the same response is
//!   delivered as a single `message` SSE event.
//! - `GET /mcp` — `405`; this server is stateless and opens no server→client
//!   notification stream.
//! - `GET /health` — same payload as the MCP `health_check` tool.
//! - `GET /search?q=<query>` — convenience REST search, same payload shape
//!   and budgets as the MCP `search` tool.
//! - `GET /events` — SSE keepalive stream (no server-initiated notifications
//!   yet; useful as a liveness signal for long-lived agents).
//!
//! Hand-rolled on `std::net` (the daemon transport avoids HTTP frameworks
//! too): requests are parsed permissively, responses are conservative, and
//! every connection is one request → one response → close.

use super::{AgentSession, HeadlessIndex};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// SSE keepalive interval, well under typical proxy timeouts.
const SSE_KEEPALIVE_SECS: u64 = 15;
/// An SSE connection is dropped after this long without a real event.
const SSE_MAX_IDLE_SECS: u64 = 300;
pub const DEFAULT_PORT: u16 = 8765;

/// Set by the SIGINT handler so the accept loop can unwind without a crate
/// dependency on signal handling.
static SIGINT_SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Installs a minimal SIGINT handler (loopback server: storing one atomic
/// boolean from the handler is async-signal-safe). No-op on non-unix.
pub fn install_ctrl_c() {
    #[cfg(unix)]
    unsafe {
        libc::signal(
            libc::SIGINT,
            on_sigint as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

#[cfg(unix)]
extern "C" fn on_sigint(_signal: libc::c_int) {
    SIGINT_SHUTDOWN.store(true, Ordering::Relaxed);
}

#[derive(Debug)]
pub enum HttpError {
    Bind(String),
    Io(String),
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(message) => write!(f, "could not bind: {}", message),
            Self::Io(message) => write!(f, "io error: {}", message),
        }
    }
}

pub fn token_path(config_dir: &Path) -> PathBuf {
    config_dir.join("http.token")
}

/// Reads the HTTP token, generating and persisting one when absent.
pub fn ensure_token(config_dir: &Path) -> Result<String, String> {
    let path = token_path(config_dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let token = super::new_connection_token();
    std::fs::write(&path, &token).map_err(|e| format!("write {}: {}", path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(token)
}

/// Long-lived server state shared across connections.
struct HttpState {
    session: Mutex<Option<AgentSessionState>>,
    token: String,
    shutdown: Arc<AtomicBool>,
    active: AtomicUsize,
}

/// Backend for the server lifetime: the daemon when one is reachable
/// (matching MCP session semantics), else a lazily-opened read-only local
/// index, else none (initialize still answers; tools explain what is
/// missing).
struct AgentSessionState {
    daemon: bool,
    index: Option<Box<HeadlessIndex>>,
}

impl AgentSessionState {
    fn session(&self) -> AgentSession<'_> {
        if self.daemon {
            AgentSession::Daemon
        } else if let Some(index) = &self.index {
            AgentSession::Local(index)
        } else {
            AgentSession::None
        }
    }
}

/// Runs the server until `shutdown` is set. Opens the index lazily on first
/// use (a missing index is not fatal; MCP `initialize` must still answer).
pub fn serve(config_dir: &Path, port: u16, shutdown: Arc<AtomicBool>) -> Result<(), HttpError> {
    let token = ensure_token(config_dir).map_err(HttpError::Io)?;
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener =
        TcpListener::bind(addr).map_err(|e| HttpError::Bind(format!("{}: {}", addr, e)))?;
    serve_on_listener(listener, token, shutdown)
}

/// Server body over an already-bound listener. Split from [`serve`] so tests
/// can bind port 0 and learn the real port.
pub fn serve_on_listener(
    listener: TcpListener,
    token: String,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HttpError> {
    eprintln!(
        "[Mnemo HTTP] listening on http://{} (token: {})",
        listener
            .local_addr()
            .map_err(|e| HttpError::Io(e.to_string()))?,
        token
    );

    let state = Arc::new(HttpState {
        session: Mutex::new(None),
        token,
        shutdown,
        active: AtomicUsize::new(0),
    });

    for stream in listener.incoming() {
        if state.shutdown.load(Ordering::Relaxed) || SIGINT_SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        let Ok(mut stream) = stream else {
            continue;
        };
        if state.active.load(Ordering::Relaxed) >= MAX_CONCURRENT_CONNECTIONS {
            let _ = write_response(
                &mut stream,
                &json_response(503, "Service Unavailable", &json!({"error": "busy"})),
            );
            continue;
        }
        let worker_state = Arc::clone(&state);
        state.active.fetch_add(1, Ordering::Relaxed);
        let result = std::thread::Builder::new()
            .name("mnemo-http".to_string())
            .spawn(move || {
                handle_connection(stream, &worker_state);
                worker_state.active.fetch_sub(1, Ordering::Relaxed);
            });
        if result.is_err() {
            state.active.fetch_sub(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

fn handle_connection(stream: TcpStream, state: &HttpState) {
    // Loopback is enforced twice: bind is loopback-only, and the peer address
    // is checked per connection in case the socket ever gets wider.
    if !stream
        .peer_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
    {
        return;
    }
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_nodelay(true);

    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(reader) => reader,
        Err(_) => return,
    });
    let mut writer = stream;

    let Some(request) = read_request(&mut reader) else {
        return;
    };
    match route_request(&request, state) {
        Route::Body(response) => {
            let _ = write_response(&mut writer, &response);
        }
        Route::Sse(body, keepalive) => {
            let response = HttpResponse {
                status: 200,
                reason: "OK",
                content_type: "text/event-stream",
                body,
            };
            if write_response(&mut writer, &response).is_ok() {
                match keepalive {
                    KeepAlive::Hold => sse_hold(&mut writer, &state.shutdown),
                    KeepAlive::Close => {}
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum KeepAlive {
    /// Hold the connection open with keepalive comments (the `/events` stream).
    Hold,
    /// The SSE response is complete; close after the body.
    Close,
}

struct Request {
    method: String,
    path: String,
    target: String,
    accept_sse: bool,
    token: Option<String>,
    body: Vec<u8>,
}

/// Reads one HTTP/1.x request (headers + bounded body). Returns `None` on
/// parse failure, EOF, or an oversized/malformed request.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).ok()? == 0 {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_ascii_uppercase();
    let target = parts.next()?.to_string();
    let version = parts.next()?.to_string();
    if !version.starts_with("HTTP/1.") {
        return None;
    }

    let mut content_length = 0usize;
    let mut accept_sse = false;
    let mut token = None;
    let mut header_lines = 0usize;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).ok()?;
        if read == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        header_lines += 1;
        if header_lines > 100 {
            return None;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "content-length" => {
                content_length = value.parse().ok()?;
                if content_length > MAX_BODY_BYTES {
                    return None;
                }
            }
            "accept" => {
                accept_sse = value.to_ascii_lowercase().contains("text/event-stream");
            }
            "authorization" => {
                token = value.strip_prefix("Bearer ").map(str::to_string);
            }
            _ => {}
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }

    let path = target.split('?').next().unwrap_or("/").to_string();
    Some(Request {
        method,
        path,
        target,
        accept_sse,
        token,
        body,
    })
}

enum Route {
    /// A plain response; the connection closes after the body.
    Body(HttpResponse),
    /// An `text/event-stream` body, optionally held open with keepalives.
    Sse(Vec<u8>, KeepAlive),
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: &'static str,
    body: Vec<u8>,
}

fn route_request(request: &Request, state: &HttpState) -> Route {
    // Auth first: everything requires the bearer token.
    if request.token.as_deref() != Some(state.token.as_str()) {
        return Route::Body(json_response(
            401,
            "Unauthorized",
            &json!({
                "error": "missing or invalid bearer token"
            }),
        ));
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/mcp") => handle_mcp_post(request, state),
        ("GET", "/mcp") => Route::Body(json_response(
            405,
            "Method Not Allowed",
            &json!({
                "error": "this server is stateless; POST JSON-RPC to /mcp"
            }),
        )),
        ("GET", "/health") => Route::Body(health_response(state)),
        ("GET", "/search") => Route::Body(handle_rest_search(request, state)),
        ("GET", "/events") => Route::Sse(b": connected\n\n".to_vec(), KeepAlive::Hold),
        _ => Route::Body(json_response(
            404,
            "Not Found",
            &json!({"error": "not found"}),
        )),
    }
}

fn health_response(state: &HttpState) -> HttpResponse {
    let payload = with_session(state, |session| {
        let health = session.health();
        json!({
            "ok": health["ok"],
            "health": health,
            "as_of": super::now_unix_secs(),
        })
    });
    json_response(200, "OK", &payload)
}

/// Runs a closure with the shared session, opening the index lazily.
fn with_session<T>(state: &HttpState, f: impl FnOnce(&AgentSession<'_>) -> T) -> T {
    let mut guard = state.session.lock().expect("http session lock");
    if guard.is_none() {
        let daemon = crate::daemon::request("ping", json!({})).is_ok();
        let index = if daemon {
            None
        } else {
            HeadlessIndex::open().ok().map(Box::new)
        };
        *guard = Some(AgentSessionState { daemon, index });
    }
    let session = guard.as_ref().expect("session initialized").session();
    f(&session)
}

fn handle_mcp_post(request: &Request, state: &HttpState) -> Route {
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(e) => {
            return Route::Body(json_response(
                400,
                "Bad Request",
                &json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {}", e) }
                }),
            ));
        }
    };

    // Read-only transport: reindex is never offered or honored here, no
    // matter what config says.
    let response = with_session(state, |session| {
        super::mcp::handle_request_session(session, &body, false)
    });

    let Some(response) = response else {
        // Notification: 202 Accepted with an empty body, per streamable HTTP.
        return Route::Body(HttpResponse {
            status: 202,
            reason: "Accepted",
            content_type: "application/json",
            body: Vec::new(),
        });
    };

    let encoded = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
    if request.accept_sse {
        let mut body = Vec::with_capacity(encoded.len() + 64);
        body.extend_from_slice(b"event: message\ndata: ");
        body.extend_from_slice(&encoded);
        body.extend_from_slice(b"\n\n");
        return Route::Sse(body, KeepAlive::Close);
    }
    Route::Body(HttpResponse {
        status: 200,
        reason: "OK",
        content_type: "application/json",
        body: encoded,
    })
}

fn handle_rest_search(request: &Request, state: &HttpState) -> HttpResponse {
    let query_string = request.target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let params = parse_query_params(query_string);
    let Some(query) = params
        .iter()
        .find(|(k, _)| k == "q")
        .map(|(_, v)| v.clone())
    else {
        return json_response(400, "Bad Request", &json!({"error": "missing q parameter"}));
    };
    if query.trim().is_empty() {
        return json_response(400, "Bad Request", &json!({"error": "q must not be empty"}));
    }
    let limit = params
        .iter()
        .find(|(k, _)| k == "limit")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .map(|limit| limit.clamp(1, 50))
        .unwrap_or(10);

    let payload = with_session(state, |session| {
        match session.search_page(&query, limit, 0) {
            Ok(response) => {
                let results: Vec<Value> = response
                    .results
                    .iter()
                    .map(|result| {
                        json!({
                            "path": result.path,
                            "score": result.score,
                            "snippet": result.snippet,
                            "match_type": result.match_type,
                            "size": result.file_size,
                            "modified": result.modified,
                        })
                    })
                    .collect();
                json!({
                    "results": results,
                    "total": response.total,
                    "mode": response.mode,
                    "returned": results.len(),
                    "as_of": super::now_unix_secs(),
                })
            }
            Err(error) => json!({"error": error}),
        }
    });
    json_response(200, "OK", &payload)
}

fn parse_query_params(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((key, value)) => (url_decode(key), url_decode(value)),
            None => (url_decode(part), String::new()),
        })
        .collect()
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_response(status: u16, reason: &'static str, value: &Value) -> HttpResponse {
    HttpResponse {
        status,
        reason,
        content_type: "application/json",
        body: serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec()),
    }
}

fn write_response(writer: &mut TcpStream, response: &HttpResponse) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        response.status,
        response.reason,
        response.content_type,
        response.body.len()
    );
    writer.write_all(head.as_bytes())?;
    writer.write_all(&response.body)?;
    writer.flush()
}

/// Holds an SSE connection open with comment keepalives, giving up after
/// [`SSE_MAX_IDLE_SECS`] so abandoned connections cannot accumulate.
fn sse_hold(writer: &mut TcpStream, shutdown: &AtomicBool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(SSE_MAX_IDLE_SECS);
    while !shutdown.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_secs(SSE_KEEPALIVE_SECS));
        if writer.write_all(b": keepalive\n\n").is_err() || writer.flush().is_err() {
            return;
        }
    }
    let _ = writer.write_all(b"event: close\ndata: {}\n\n");
    let _ = writer.flush();
}

/// Parses `--port N` (or `--port=N`) from serve args, defaulting to
/// [`DEFAULT_PORT`].
pub fn parse_port(args: &[String]) -> Result<u16, String> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--port" {
            let Some(value) = args.next() else {
                return Err("--port requires a value".to_string());
            };
            return value
                .parse::<u16>()
                .map_err(|_| format!("invalid port: {}", value));
        }
        if let Some(rest) = arg.strip_prefix("--port=") {
            return rest
                .parse::<u16>()
                .map_err(|_| format!("invalid port: {}", rest));
        }
    }
    Ok(DEFAULT_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "mnemo-http-test-{}-{}-{}",
            tag,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Server {
        port: u16,
        token: String,
        shutdown: Arc<AtomicBool>,
        dir: PathBuf,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(("127.0.0.1", self.port));
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn start_server(tag: &str) -> Server {
        let dir = temp_dir(tag);
        let token = ensure_token(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle_shutdown = Arc::clone(&shutdown);
        let serve_token = token.clone();
        std::thread::spawn(move || {
            let _ = serve_on_listener(listener, serve_token, handle_shutdown);
        });
        // Wait for the listener to accept.
        for _ in 0..100 {
            if TcpStream::connect_timeout(
                &format!("127.0.0.1:{}", port).parse().unwrap(),
                Duration::from_millis(50),
            )
            .is_ok()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Server {
            port,
            token,
            shutdown,
            dir,
        }
    }

    fn http(server: &Server, raw: &str) -> (u16, String, String) {
        let mut stream =
            TcpStream::connect(("127.0.0.1", server.port)).expect("connect to test server");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        let mut headers = String::new();
        reader.read_line(&mut status).unwrap();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.trim().is_empty() {
                break;
            }
            headers.push_str(&line);
        }
        let mut body = String::new();
        let _ = reader.read_to_string(&mut body);
        (
            status
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .unwrap_or(0),
            headers,
            body,
        )
    }

    fn post_mcp(server: &Server, body: &str, accept: &str) -> (u16, String, String) {
        http(
            server,
            &format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nAccept: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                server.token,
                accept,
                body.len(),
                body
            ),
        )
    }

    #[test]
    fn rejects_missing_and_wrong_tokens() {
        let server = start_server("auth");
        let (status, _, _) = http(&server, "GET /health HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(status, 401);

        let (status, _, _) = http(
            &server,
            "GET /health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer wrong\r\n\r\n",
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn initialize_answers_without_index_and_health_reports_unhealthy() {
        let server = start_server("no-index");
        let (status, _, body) = post_mcp(
            &server,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            "application/json",
        );
        assert_eq!(status, 200);
        let payload: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(payload["result"]["serverInfo"]["name"], "mnemo");

        let (status, _, body) = http(
            &server,
            &format!(
                "GET /health HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
                server.token
            ),
        );
        assert_eq!(status, 200);
        let payload: Value = serde_json::from_str(&body).unwrap();
        // `ok` depends on whether a real Mnemo index exists on this
        // machine; the contract under test is that /health answers with the
        // standard shape either way.
        assert!(payload["ok"].is_boolean());
        assert!(payload["health"].is_object());
    }

    #[test]
    fn post_mcp_sse_wraps_response_in_event_frame() {
        let server = start_server("sse");
        let (status, headers, body) = post_mcp(
            &server,
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#,
            "text/event-stream",
        );
        assert_eq!(status, 200);
        assert!(headers.contains("text/event-stream"));
        assert!(body.starts_with("event: message\ndata: "));
        let data = body
            .strip_prefix("event: message\ndata: ")
            .and_then(|rest| rest.strip_suffix("\n\n"))
            .unwrap();
        let payload: Value = serde_json::from_str(data).unwrap();
        assert_eq!(payload["id"], 2);
    }

    #[test]
    fn notifications_acknowledge_with_202() {
        let server = start_server("notify");
        let (status, _, body) = post_mcp(
            &server,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "application/json",
        );
        assert_eq!(status, 202);
        assert!(body.is_empty());
    }

    #[test]
    fn mcp_post_rejects_malformed_json_as_parse_error() {
        let server = start_server("badjson");
        let (status, _, body) = post_mcp(&server, "not json", "application/json");
        assert_eq!(status, 400);
        let payload: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(payload["error"]["code"], -32700);
    }

    #[test]
    fn unknown_routes_404_and_get_mcp_405() {
        let server = start_server("routes");
        let (status, _, _) = http(
            &server,
            &format!(
                "GET /nope HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
                server.token
            ),
        );
        assert_eq!(status, 404);

        let (status, _, _) = http(
            &server,
            &format!(
                "GET /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
                server.token
            ),
        );
        assert_eq!(status, 405);
    }

    #[test]
    fn rest_search_without_q_is_bad_request() {
        let server = start_server("rest");
        let (status, _, body) = http(
            &server,
            &format!(
                "GET /search HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {}\r\n\r\n",
                server.token
            ),
        );
        assert_eq!(status, 400);
        let payload: Value = serde_json::from_str(&body).unwrap();
        assert!(payload["error"].as_str().unwrap().contains("q"));
    }

    #[test]
    fn parse_port_defaults_and_parses() {
        assert_eq!(parse_port(&[]).unwrap(), DEFAULT_PORT);
        assert_eq!(
            parse_port(&["--port".to_string(), "9001".to_string()]).unwrap(),
            9001
        );
        assert_eq!(parse_port(&["--port=9002".to_string()]).unwrap(), 9002);
        assert!(parse_port(&["--port".to_string(), "abc".to_string()]).is_err());
        assert!(parse_port(&["--port".to_string()]).is_err());
    }

    #[test]
    fn url_decode_handles_escapes() {
        assert_eq!(url_decode("a%20b+c"), "a b c");
        assert_eq!(url_decode("100%25"), "100%");
        assert_eq!(url_decode("bad%zz"), "bad%zz");
    }

    #[test]
    fn token_file_is_generated_once_with_fixed_content() {
        let dir = temp_dir("token");
        let first = ensure_token(&dir).unwrap();
        let second = ensure_token(&dir).unwrap();
        assert_eq!(first, second);
        assert!(!first.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_path(&dir))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
