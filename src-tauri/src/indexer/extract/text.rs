use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Decodes UTF-16 (LE or BE) when the buffer starts with its byte-order mark.
/// Returns None if there is no UTF-16 BOM or the body is not valid UTF-16, so
/// the caller can fall back to the other encodings.
pub(crate) fn decode_utf16_bom(buffer: &[u8]) -> Option<String> {
    if buffer.len() < 2 {
        return None;
    }
    let units: Vec<u16> = if buffer[0] == 0xFF && buffer[1] == 0xFE {
        buffer[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    } else if buffer[0] == 0xFE && buffer[1] == 0xFF {
        buffer[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect()
    } else {
        return None;
    };
    String::from_utf16(&units).ok()
}

pub fn extract_text(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buffer = Vec::new();
    let mut handle = (&mut file).take(max_bytes as u64);
    handle.read_to_end(&mut buffer).ok()?;

    if buffer.is_empty() {
        return None;
    }

    // 1. UTF-16 with a byte-order mark (BOM)
    if let Some(s) = decode_utf16_bom(&buffer) {
        return Some(clean_text(&s));
    }

    // 2. Reject binary files (null bytes in first 1024 bytes)
    if buffer.iter().take(1024).any(|&b| b == 0) {
        return None;
    }

    // 3. UTF-8 decoding
    let raw_text = match String::from_utf8(buffer.clone()) {
        Ok(s) => s,
        Err(_) => {
            // Lossy fallback if mostly text
            String::from_utf8_lossy(&buffer).to_string()
        }
    };

    // 4. Markdown frontmatter stripping
    let cleaned = strip_markdown_frontmatter(&raw_text);

    Some(clean_text(&cleaned))
}

pub fn extract_rtf(path: &Path, max_bytes: usize) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let mut buffer = Vec::new();
    let mut handle = (&mut file).take(max_bytes as u64);
    handle.read_to_end(&mut buffer).ok()?;

    let raw = String::from_utf8_lossy(&buffer);
    if !raw.starts_with("{\\rtf") {
        return None;
    }

    // Simple RTF text stripper: remove \{ ... \} control groups and \control words
    let mut text_out = String::new();
    let mut in_control = false;
    let mut depth = 0;

    for ch in raw.chars() {
        if text_out.len() >= max_bytes {
            break;
        }
        match ch {
            '{' => depth += 1,
            '}' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            '\\' => {
                in_control = true;
            }
            ' ' | '\t' | '\n' | '\r' => {
                if in_control {
                    in_control = false;
                } else if depth <= 1 {
                    text_out.push(ch);
                }
            }
            _ => {
                if in_control {
                    if !ch.is_alphanumeric() {
                        in_control = false;
                    }
                } else if depth <= 1 {
                    text_out.push(ch);
                }
            }
        }
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn strip_markdown_frontmatter(text: &str) -> String {
    let trimmed = text.trim_start();
    if let Some(stripped) = trimmed.strip_prefix("---")
        && let Some(rest) = stripped
            .strip_prefix('\n')
            .or_else(|| stripped.strip_prefix("\r\n"))
        && let Some(end_pos) = rest.find("\n---")
    {
        let after = &rest[end_pos + 4..];
        return after.trim_start_matches(['\r', '\n']).to_string();
    }
    text.to_string()
}

fn clean_text(s: &str) -> String {
    // Strip BOM and null characters
    s.trim_start_matches('\u{feff}')
        .replace('\0', "")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A temp file that removes itself on drop.
    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn bytes_fixture(tag: &str, bytes: &[u8]) -> Fixture {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mnemo-text-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ));
        fs::write(&path, bytes).unwrap();
        Fixture(path)
    }

    #[test]
    fn plain_utf8_extracts_trimmed() {
        let fx = bytes_fixture("plain", b"  hello world \n");
        assert_eq!(extract_text(&fx.0, 1000).as_deref(), Some("hello world"));
    }

    #[test]
    fn empty_file_and_whitespace_only_differ() {
        let empty = bytes_fixture("empty", b"");
        assert!(extract_text(&empty.0, 1000).is_none());

        let blank = bytes_fixture("blank", b"   \n\n ");
        assert_eq!(extract_text(&blank.0, 1000).as_deref(), Some(""));
    }

    #[test]
    fn utf16_le_and_be_bom_decode() {
        // "héllo" in UTF-16, little-endian with BOM.
        let le: Vec<u8> = [0xFF, 0xFE]
            .iter()
            .copied()
            .chain("héllo".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let fx = bytes_fixture("utf16le", &le);
        assert_eq!(extract_text(&fx.0, 1000).as_deref(), Some("héllo"));

        // Same text big-endian with its own BOM.
        let be: Vec<u8> = [0xFE, 0xFF]
            .iter()
            .copied()
            .chain("héllo".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let fx = bytes_fixture("utf16be", &be);
        assert_eq!(extract_text(&fx.0, 1000).as_deref(), Some("héllo"));
    }

    #[test]
    fn utf16_bom_with_odd_trailing_byte_does_not_panic() {
        // 3-byte body after the BOM: truncated UTF-16 unit.
        let truncated: Vec<u8> = [0xFF, 0xFE, b'a', b'\0', b'b'].to_vec();
        let fx = bytes_fixture("utf16odd", &truncated);
        // Either decodes what it can of the full units or gives up; must not
        // panic and must not index the raw BOM bytes as text.
        let _ = extract_text(&fx.0, 1000);
    }

    #[test]
    fn decode_utf16_bom_ignores_text_without_bom() {
        assert_eq!(decode_utf16_bom(b"plain ascii"), None);
        assert_eq!(decode_utf16_bom(b"x"), None);
        assert_eq!(decode_utf16_bom(&[0xFF, 0xFE]).as_deref(), Some(""));
    }

    #[test]
    fn null_bytes_in_first_1k_reject_binary() {
        let mut bytes = vec![b'>'; 10];
        bytes[5] = 0;
        let fx = bytes_fixture("binary-early", &bytes);
        assert!(
            extract_text(&fx.0, 1000).is_none(),
            "null inside the binary-probe window"
        );
    }

    #[test]
    fn null_bytes_after_1k_are_text_provided_they_are_stripped() {
        // A large text file with a null byte far beyond the probe window.
        let padding = "x".repeat(1024);
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(padding.as_bytes()); // fills the probe window
        bytes.push(0);
        bytes.extend_from_slice(b"tail content");
        let fx = bytes_fixture("binary-late", &bytes);
        let text = extract_text(&fx.0, 10_000).expect("late nulls must not reject");
        assert!(text.contains("tail content"), "got: {text:?}");
    }

    #[test]
    fn markdown_frontmatter_is_stripped() {
        let fx = bytes_fixture(
            "front",
            b"---\ntitle: Zettel\ndate: 2026-01-01\n---\n\nbody text\n",
        );
        assert_eq!(extract_text(&fx.0, 1000).as_deref(), Some("body text"));
    }

    #[test]
    fn markdown_frontmatter_with_crlf_is_stripped() {
        let fx = bytes_fixture(
            "front-crlf",
            b"---\r\ntitle: Zettel\r\n---\r\nbody after crlf\r\n",
        );
        assert_eq!(
            extract_text(&fx.0, 1000).as_deref(),
            Some("body after crlf")
        );
    }

    #[test]
    fn markdown_without_closing_frontmatter_is_text() {
        // A document that starts with --- but never closes it is NOT
        // frontmatter; dropping its content would lose real words.
        let fx = bytes_fixture("front-open", b"---\ntitle: no closing marker here\nmore\n");
        let text = extract_text(&fx.0, 1000).expect("kept as body");
        assert!(text.contains("no closing marker"), "got: {text:?}");
    }

    #[test]
    fn max_bytes_caps_the_read() {
        let big = "word ".repeat(10_000); // ~50 KB
        let fx = bytes_fixture("cap", big.as_bytes());
        let text = extract_text(&fx.0, 100).expect("capped read still yields text");
        assert!(text.len() <= 101, "capped at 100 bytes: {}", text.len());
    }

    #[test]
    fn invalid_utf8_decodes_lossily_around_ascii() {
        let fx = bytes_fixture("lossy", b"keep this \xF0\x9F");
        let text = extract_text(&fx.0, 1000).expect("lossy fallback");
        assert!(text.contains("keep this"), "got: {text:?}");
    }

    #[test]
    fn rtf_basic_stripping_and_rejection() {
        let fx = bytes_fixture(
            "rtf",
            b"{\\rtf1\\ansi Hello {\\b bold} world \\highlight7 highlighted}",
        );
        let text = extract_rtf(&fx.0, 1000).expect("rtf extracts");
        assert!(text.contains("Hello"), "got: {text:?}");
        assert!(text.contains("world"));

        let not_rtf = bytes_fixture("not-rtf", b"<html>body</html>");
        assert!(extract_rtf(&not_rtf.0, 1000).is_none());

        let empty_rtf = bytes_fixture("rtf-empty", b"{\\rtf1\\ansi}");
        assert!(extract_rtf(&empty_rtf.0, 1000).is_none());
    }

    #[test]
    fn missing_file_is_none_never_panic() {
        let path = std::env::temp_dir().join("mnemo-text-absent-file");
        let _ = fs::remove_file(&path);
        assert!(extract_text(&path, 1000).is_none());
        assert!(extract_rtf(&path, 1000).is_none());
    }
}
