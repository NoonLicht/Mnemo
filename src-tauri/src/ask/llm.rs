//! Minimal client for OpenAI-compatible chat endpoints (llama.cpp server, Ollama, LM Studio),
//! with token streaming (SPEC §7.3).
//!
//! It speaks plain HTTP only, deliberately: there is no TLS stack in the build, so a request can
//! never reach a cloud service by accident. Loopback endpoints work out of the box; a non-loopback
//! host additionally needs `allow_remote`, because the question and the retrieved passages leave
//! the machine in that case.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::mnemo_err::{Error, Result};
use serde::{Deserialize, Serialize};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(180);
/// Upper bound for a non-streaming error body we are willing to read.
const MAX_ERROR_BODY: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// e.g. `http://127.0.0.1:11434/v1`
    pub endpoint: String,
    pub model: String,
    pub api_key: Option<String>,
    pub allow_remote: bool,
}

struct Target {
    host: String,
    port: u16,
    path: String,
}

fn cfg_err(msg: impl Into<String>) -> Error {
    Error::Config(msg.into())
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn parse_endpoint(endpoint: &str) -> Result<Target> {
    let rest = endpoint.strip_prefix("http://").ok_or_else(|| {
        cfg_err("llm_endpoint must start with http:// (HTTPS is not built in; use a local server)")
    })?;
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse().map_err(|_| cfg_err("bad port in llm_endpoint"))?,
        ),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err(cfg_err("llm_endpoint has no host"));
    }
    Ok(Target {
        host: host.to_owned(),
        port,
        path: format!("/{}", path.trim_end_matches('/')),
    })
}

/// Decodes `Transfer-Encoding: chunked` on the fly.
struct Chunked<R: BufRead> {
    inner: R,
    left: usize,
    done: bool,
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            self.inner.read_line(&mut line)?;
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "bad chunk size")
                })?;
            if size == 0 {
                self.done = true;
                return Ok(0);
            }
            self.left = size;
        }
        let n = buf.len().min(self.left);
        let got = self.inner.read(&mut buf[..n])?;
        self.left -= got;
        if self.left == 0 {
            let mut crlf = [0u8; 2];
            self.inner.read_exact(&mut crlf)?;
        }
        Ok(got)
    }
}

#[derive(Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}
#[derive(Deserialize)]
struct StreamChoice {
    delta: Option<Delta>,
}
#[derive(Deserialize)]
struct Delta {
    content: Option<String>,
}

/// Sends the conversation and streams the answer. `on_token` is called for every piece of text;
/// the full answer is returned.
pub fn chat_stream(
    cfg: &LlmConfig,
    messages: &[ChatMessage],
    mut on_token: impl FnMut(&str),
) -> Result<String> {
    let target = parse_endpoint(&cfg.endpoint)?;
    if !is_loopback(&target.host) && !cfg.allow_remote {
        return Err(cfg_err(format!(
            "llm_endpoint host {:?} is not on this machine: your questions and document passages would leave the device. Set llm_allow_remote = true to accept that.",
            target.host
        )));
    }
    let addr = (target.host.as_str(), target.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| cfg_err("cannot resolve llm_endpoint host"))?;
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;

    let body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "stream": true,
        "temperature": 0.2,
    })
    .to_string();
    let mut req = format!(
        "POST {}/chat/completions HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n",
        target.path,
        target.host,
        target.port,
        body.len()
    );
    if let Some(key) = &cfg.api_key {
        req.push_str(&format!("Authorization: Bearer {key}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes())?;
    stream.write_all(body.as_bytes())?;

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut chunked = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let l = line.to_ascii_lowercase();
        if l.starts_with("transfer-encoding:") && l.contains("chunked") {
            chunked = true;
        }
    }
    let body: Box<dyn Read> = if chunked {
        Box::new(Chunked {
            inner: reader,
            left: 0,
            done: false,
        })
    } else {
        Box::new(reader)
    };
    if status != 200 {
        let mut text = String::new();
        let _ = body.take(MAX_ERROR_BODY).read_to_string(&mut text);
        return Err(Error::Protocol(format!(
            "language model returned HTTP {status}: {}",
            text.trim()
        )));
    }

    let mut answer = String::new();
    for line in BufReader::new(body).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) else {
            continue;
        };
        for choice in chunk.choices {
            if let Some(text) = choice
                .delta
                .and_then(|d| d.content)
                .filter(|t| !t.is_empty())
            {
                on_token(&text);
                answer.push_str(&text);
            }
        }
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves one canned HTTP response and returns the request it received.
    fn serve_once(response: Vec<u8>) -> (u16, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let h = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0usize;
            loop {
                let mut l = String::new();
                reader.read_line(&mut l).unwrap();
                if l.trim().is_empty() {
                    break;
                }
                if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                head.push_str(&l);
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            s.write_all(&response).unwrap();
            format!("{head}\n{}", String::from_utf8(body).unwrap())
        });
        (port, h)
    }

    fn chunked(parts: &[&str]) -> Vec<u8> {
        let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        for p in parts {
            out.extend(format!("{:x}\r\n{p}\r\n", p.len()).into_bytes());
        }
        out.extend(b"0\r\n\r\n");
        out
    }

    fn cfg(port: u16) -> LlmConfig {
        LlmConfig {
            endpoint: format!("http://127.0.0.1:{port}/v1"),
            model: "m".into(),
            api_key: Some("k".into()),
            allow_remote: false,
        }
    }

    #[test]
    fn streams_tokens_across_chunk_boundaries() {
        let ev1 = "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n";
        let ev2 = "data: {\"choices\":[{\"delta\":{\"content\":\"lo \"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"[1]\"}}]}\n\n";
        // Split the first event in the middle of the JSON to prove reassembly works.
        let (a, b) = ev1.split_at(20);
        let (port, server) = serve_once(chunked(&[a, b, ev2, "data: [DONE]\n\n"]));
        let mut tokens = Vec::new();
        let answer = chat_stream(&cfg(port), &[ChatMessage::user("hi")], |t| {
            tokens.push(t.to_owned())
        })
        .unwrap();
        assert_eq!(answer, "Hello [1]");
        assert_eq!(tokens, ["Hel", "lo ", "[1]"]);
        let request = server.join().unwrap();
        assert!(
            request.starts_with("POST /v1/chat/completions HTTP/1.1"),
            "{request}"
        );
        assert!(request.contains("Authorization: Bearer k"));
        assert!(request.contains("\"stream\":true") && request.contains("\"content\":\"hi\""));
    }

    #[test]
    fn http_errors_surface_the_body() {
        let body = "{\"error\":\"model not found\"}";
        let resp = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let (port, server) = serve_once(resp.into_bytes());
        let err = chat_stream(&cfg(port), &[ChatMessage::user("x")], |_| {})
            .unwrap_err()
            .to_string();
        server.join().unwrap();
        assert!(
            err.contains("404") && err.contains("model not found"),
            "{err}"
        );
    }

    #[test]
    fn refuses_remote_hosts_and_https_without_opt_in() {
        let remote = LlmConfig {
            endpoint: "http://203.0.113.5:8080/v1".into(),
            model: "m".into(),
            api_key: None,
            allow_remote: false,
        };
        assert!(
            chat_stream(&remote, &[], |_| {})
                .unwrap_err()
                .to_string()
                .contains("leave the device")
        );
        let https = LlmConfig {
            endpoint: "https://api.example.com/v1".into(),
            ..remote.clone()
        };
        assert!(
            chat_stream(&https, &[], |_| {})
                .unwrap_err()
                .to_string()
                .contains("http://")
        );
        assert!(is_loopback("localhost") && is_loopback("127.0.0.1") && is_loopback("::1"));
        assert!(!is_loopback("192.168.1.10"));
    }

    #[test]
    fn endpoint_parsing() {
        let t = parse_endpoint("http://localhost:11434/v1/").unwrap();
        assert_eq!(
            (t.host.as_str(), t.port, t.path.as_str()),
            ("localhost", 11434, "/v1")
        );
        let t = parse_endpoint("http://127.0.0.1").unwrap();
        assert_eq!((t.port, t.path.as_str()), (80, "/"));
        assert!(parse_endpoint("http://:80").is_err());
    }
}
