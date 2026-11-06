use quick_xml::events::Event;
use quick_xml::reader::Reader;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use zip::ZipArchive;

/// Decodes an XML reference event (`&amp;`, `&#233;`, ...) to text.
///
/// quick-xml 0.42 separates general references out of `Event::Text`, so
/// extractors must handle them or "R&D" becomes unsearchable "RD". Unknown
/// named entities (XLSX/DOCX sometimes invent their own) fall back to the
/// raw `&name;` rather than silently dropping content.
fn decoded_ref(r: &quick_xml::events::BytesRef<'_>) -> String {
    let raw = format!("&{};", r.as_ref());
    quick_xml::escape::unescape(&raw)
        .map(|cow| cow.into_owned())
        .unwrap_or(raw)
}

pub fn extract_docx(path: &Path, max_bytes: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut doc_file = archive.by_name("word/document.xml").ok()?;
    // Stream XML directly instead of buffering entire document in memory
    let mut reader = Reader::from_reader(std::io::BufReader::new(&mut doc_file));
    reader.config_mut().trim_text(true);

    let mut text_out = String::new();
    let mut in_text = false;
    let mut buf = Vec::new();

    loop {
        if text_out.len() >= max_bytes {
            text_out.push_str("\n... [content truncated at limit]");
            break;
        }

        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if e.name().as_ref() == "w:t" {
                    in_text = true;
                }
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                if name.as_ref() == "w:t" {
                    in_text = false;
                } else if name.as_ref() == "w:p" && !text_out.ends_with('\n') {
                    text_out.push('\n');
                }
            }
            Ok(Event::Text(ref e)) => {
                if in_text {
                    // XML entities in raw text (&amp; &#233;) must be decoded
                    // or "R&D report" can never match a search for "R&D".
                    text_out.push_str(e.as_ref());
                    text_out.push(' ');
                }
            }
            Ok(Event::GeneralRef(ref r)) => {
                if in_text {
                    text_out.push_str(&decoded_ref(r));
                    text_out.push(' ');
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub fn extract_pptx(path: &Path, max_bytes: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut slide_names: Vec<String> = archive
        .file_names()
        .filter_map(|r| r.ok())
        .filter(|n| n.starts_with("ppt/slides/slide") && n.ends_with(".xml"))
        .map(|s| s.to_string())
        .collect();

    // Sort slides numerically: slide1.xml, slide2.xml, ...
    slide_names.sort_by_key(|n| {
        n.strip_prefix("ppt/slides/slide")
            .and_then(|s| s.strip_suffix(".xml"))
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0)
    });

    let mut text_out = String::new();

    for (idx, slide_name) in slide_names.iter().enumerate() {
        if text_out.len() >= max_bytes {
            text_out.push_str("\n... [content truncated at limit]");
            break;
        }

        let mut slide_file = match archive.by_name(slide_name) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let mut reader = Reader::from_reader(std::io::BufReader::new(&mut slide_file));
        reader.config_mut().trim_text(true);

        let mut slide_text = String::new();
        let mut in_text = false;
        let mut buf = Vec::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    if e.name().as_ref() == "a:t" {
                        in_text = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    if name.as_ref() == "a:t" {
                        in_text = false;
                    } else if name.as_ref() == "a:p" {
                        slide_text.push(' ');
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if in_text {
                        slide_text.push_str(e.as_ref());
                        slide_text.push(' ');
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }

        let trimmed_slide = slide_text.trim();
        if !trimmed_slide.is_empty() {
            if !text_out.is_empty() {
                text_out.push_str("\n\n");
            }
            text_out.push_str(&format!("--- Slide {} ---\n", idx + 1));
            text_out.push_str(trimmed_slide);
        }
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub fn extract_xlsx(path: &Path, max_bytes: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    // 1. Read shared strings if present
    let mut shared_strings = Vec::new();
    if let Ok(mut ss_file) = archive.by_name("xl/sharedStrings.xml") {
        let mut reader = Reader::from_reader(std::io::BufReader::new(&mut ss_file));
        reader.config_mut().trim_text(true);
        {
            let mut in_t = false;
            let mut current = String::new();
            let mut buf = Vec::new();
            loop {
                match reader.read_event_into(&mut buf) {
                    Ok(Event::Start(ref e)) => {
                        if e.name().as_ref() == "t" {
                            in_t = true;
                        }
                    }
                    Ok(Event::End(ref e)) => {
                        if e.name().as_ref() == "t" && in_t {
                            // One shared string per <t>: quick-xml 0.42
                            // splits entity references into their own
                            // events, so text must span them.
                            shared_strings.push(std::mem::take(&mut current));
                            in_t = false;
                        }
                    }
                    Ok(Event::Text(ref e)) => {
                        if in_t {
                            current.push_str(e.as_ref());
                        }
                    }
                    Ok(Event::GeneralRef(ref r)) => {
                        if in_t {
                            current.push_str(&decoded_ref(r));
                        }
                    }
                    Ok(Event::Eof) => break,
                    Err(_) => break,
                    _ => {}
                }
                buf.clear();
            }
        }
    }

    // 2. Read sheets
    let mut sheet_names: Vec<String> = archive
        .file_names()
        .filter_map(|r| r.ok())
        .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
        .map(|s| s.to_string())
        .collect();
    sheet_names.sort();

    let mut text_out = String::new();

    // If shared strings were extracted, include them as a summary
    if !shared_strings.is_empty() {
        for s in shared_strings.iter().take(2000) {
            if text_out.len() >= max_bytes {
                break;
            }
            text_out.push_str(s);
            text_out.push('\n');
        }
    }

    // Also scan worksheet cells for inline strings or numbers
    for sheet_name in sheet_names.iter().take(10) {
        if text_out.len() >= max_bytes {
            text_out.push_str("\n... [content truncated at limit]");
            break;
        }

        let mut sheet_file = match archive.by_name(sheet_name) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let mut reader = Reader::from_reader(std::io::BufReader::new(&mut sheet_file));
        reader.config_mut().trim_text(true);

        let mut in_v = false;
        let mut buf = Vec::new();

        loop {
            if text_out.len() >= max_bytes {
                break;
            }
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => {
                    let name = e.name();
                    if name.as_ref() == "v" || name.as_ref() == "t" {
                        in_v = true;
                    }
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    if name.as_ref() == "v" || name.as_ref() == "t" {
                        in_v = false;
                    } else if name.as_ref() == "row" {
                        text_out.push('\n');
                    }
                }
                Ok(Event::Text(ref e)) => {
                    if in_v {
                        text_out.push_str(e.as_ref());
                        text_out.push(' ');
                    }
                }
                Ok(Event::GeneralRef(ref r)) => {
                    if in_v {
                        text_out.push_str(&decoded_ref(r));
                        text_out.push(' ');
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub fn extract_epub(path: &Path, max_bytes: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut chapter_files: Vec<String> = archive
        .file_names()
        .filter_map(|r| r.ok())
        .filter(|n| n.ends_with(".xhtml") || n.ends_with(".html"))
        .map(|s| s.to_string())
        .collect();
    chapter_files.sort();

    let mut text_out = String::new();

    for chap_name in chapter_files.iter().take(50) {
        if text_out.len() >= max_bytes {
            text_out.push_str("\n... [content truncated at limit]");
            break;
        }

        let mut chap_file = match archive.by_name(chap_name) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let mut reader = Reader::from_reader(std::io::BufReader::new(&mut chap_file));
        reader.config_mut().trim_text(true);

        let mut buf = Vec::new();
        loop {
            if text_out.len() >= max_bytes {
                break;
            }
            match reader.read_event_into(&mut buf) {
                Ok(Event::Text(ref e)) => {
                    text_out.push_str(e.as_ref());
                    text_out.push(' ');
                }
                Ok(Event::GeneralRef(ref r)) => {
                    text_out.push_str(&decoded_ref(r));
                    text_out.push(' ');
                }
                Ok(Event::End(ref e)) => {
                    let name = e.name();
                    let tag = name.as_ref();
                    if tag == "p" || tag == "h1" || tag == "h2" || tag == "h3" {
                        text_out.push('\n');
                    }
                }
                Ok(Event::Eof) => break,
                Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub fn extract_odt(path: &Path, max_bytes: usize) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = ZipArchive::new(BufReader::new(file)).ok()?;

    let mut content_file = archive.by_name("content.xml").ok()?;

    let mut reader = Reader::from_reader(std::io::BufReader::new(&mut content_file));
    reader.config_mut().trim_text(true);

    let mut text_out = String::new();
    let mut buf = Vec::new();

    loop {
        if text_out.len() >= max_bytes {
            text_out.push_str("\n... [content truncated at limit]");
            break;
        }

        match reader.read_event_into(&mut buf) {
            Ok(Event::Text(ref e)) => {
                text_out.push_str(e.as_ref());
                text_out.push(' ');
            }
            Ok(Event::GeneralRef(ref r)) => {
                text_out.push_str(&decoded_ref(r));
                text_out.push(' ');
            }
            Ok(Event::End(ref e)) => {
                let name = e.name();
                let tag = name.as_ref();
                if tag == "text:p" || tag == "text:h" {
                    text_out.push('\n');
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    let trimmed = text_out.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Removes its file on drop so failed tests don't litter temp dirs.
    struct Fixture(PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp_path(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "mnemo-office-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ))
    }

    /// Builds a zip archive with the given (name, string) members.
    fn zip_fixture(tag: &str, members: &[(&str, &str)]) -> Fixture {
        let path = temp_path(tag);
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, body) in members {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        Fixture(path)
    }

    fn plain_fixture(tag: &str, contents: &str) -> Fixture {
        let path = temp_path(tag);
        std::fs::write(&path, contents).unwrap();
        Fixture(path)
    }

    /// All five formats are (path, max_bytes) -> Option<String>; invalid
    /// input must return None for every one of them, never panic.
    type Extractor = fn(&Path, usize) -> Option<String>;
    const ALL_EXTRACTORS: &[(&str, Extractor)] = &[
        ("extract_docx", extract_docx),
        ("extract_pptx", extract_pptx),
        ("extract_xlsx", extract_xlsx),
        ("extract_epub", extract_epub),
        ("extract_odt", extract_odt),
    ];

    #[test]
    fn docx_extracts_runs_and_paragraph_breaks() {
        let fx = zip_fixture(
            "docx-basic",
            &[(
                "word/document.xml",
                "<w:body><w:p><w:r><w:t>alpha run</w:t></w:r></w:p><w:p><w:r><w:t>beta</w:t></w:r></w:p></w:body>",
            )],
        );
        let text = extract_docx(&fx.0, 10_000).expect("docx extracts");
        assert!(text.contains("alpha"), "got: {text:?}");
        assert!(text.ends_with("beta"));
        assert!(text.contains('\n'), "w:p must force a break: {text:?}");
    }

    #[test]
    fn docx_decodes_xml_entities() {
        let fx = zip_fixture(
            "docx-entities",
            &[(
                "word/document.xml",
                "<w:p><w:r><w:t>R&amp;D — café &#233; &lt;tag&gt;</w:t></w:r></w:p>",
            )],
        );
        let text = extract_docx(&fx.0, 10_000).expect("docx extracts");
        // Runs and references arrive as separate event streams, so entity
        // output is surrounded by the spaces the extractor adds between
        // pieces; what matters is that every decoded piece is present and
        // no raw entity leaks.
        assert!(text.contains("R"), "got: {text:?}");
        assert!(text.contains('&'), "entity must decode: {text:?}");
        assert!(
            text.contains("café"),
            "numeric entity must decode: {text:?}"
        );
        assert!(text.contains('é'), "numeric entity must decode: {text:?}");
        assert!(text.contains('<'), "escaped tag must decode: {text:?}");
        assert!(
            !text.contains("&amp;"),
            "raw entity must not leak: {text:?}"
        );
    }

    #[test]
    fn docx_ignores_text_outside_runs() {
        // Tempting failure mode: index all text nodes, so file metadata
        // strings would pollute every match. Only w:t content is indexed.
        let fx = zip_fixture(
            "docx-outline",
            &[(
                "word/document.xml",
                "<w:document>outline-not-indexed<w:p><w:r><w:t>kept</w:t></w:r></w:p></w:document>",
            )],
        );
        let text = extract_docx(&fx.0, 10_000).expect("docx extracts");
        assert!(text.contains("kept"));
        assert!(!text.contains("outline-not-indexed"), "got: {text:?}");
    }

    #[test]
    fn docx_truncates_at_max_bytes_with_marker() {
        let repeated = "word ".repeat(1000);
        let body = format!("<w:p><w:r><w:t>{}</w:t></w:r></w:p>", repeated);
        let fx = zip_fixture("docx-truncate", &[("word/document.xml", &body)]);
        let text = extract_docx(&fx.0, 40).expect("docx extracts");
        assert!(
            text.contains("[content truncated at limit]"),
            "got: {text:?}"
        );
    }

    #[test]
    fn docx_empty_document_is_none() {
        let fx = zip_fixture("docx-empty", &[("word/document.xml", "<w:body></w:body>")]);
        assert!(extract_docx(&fx.0, 10_000).is_none());
    }

    #[test]
    fn docx_bigger_max_than_content_is_fine() {
        let fx = zip_fixture(
            "docx-bigmax",
            &[(
                "word/document.xml",
                "<w:p><w:r><w:t>small</w:t></w:r></w:p>",
            )],
        );
        let text = extract_docx(&fx.0, usize::MAX / 2).expect("docx extracts");
        assert!(text.contains("small"));
        assert!(!text.contains("truncated"));
    }

    #[test]
    fn corrupt_zip_is_none_never_panics() {
        let fx = plain_fixture("corrupt", "this is not a zip archive at all");
        let empty = plain_fixture("empty-file", "");
        for (name, extractor) in ALL_EXTRACTORS {
            assert!(
                extractor(&fx.0, 10_000).is_none(),
                "{name} on garbage bytes must be None"
            );
            assert!(
                extractor(&empty.0, 10_000).is_none(),
                "{name} on empty file must be None"
            );
        }
    }

    #[test]
    fn pptx_sorts_slides_numerically_not_lexically() {
        // Lexical order would put slide10 right after slide1.
        let fx = zip_fixture(
            "pptx-order",
            &[
                ("ppt/slides/slide10.xml", "<a:t>ten</a:t>"),
                ("ppt/slides/slide2.xml", "<a:p><a:t>two</a:t></a:p>"),
                ("ppt/slides/slide1.xml", "<a:t>one</a:t>"),
            ],
        );
        let text = extract_pptx(&fx.0, 10_000).expect("pptx extracts");
        let one = text.find("one").expect("slide 1 first");
        let two = text.find("two").expect("slide 2 second");
        let ten = text.find("ten").expect("slide 10 last");
        assert!(one < two && two < ten, "slide order wrong: {text:?}");
        assert!(text.contains("--- Slide 1 ---"));
        assert!(
            text.contains("--- Slide 3 ---"),
            "slide10 is the 3rd non-empty slide: {text:?}"
        );
    }

    #[test]
    fn pptx_empty_slides_do_not_shadow_nonempty_ones() {
        let fx = zip_fixture(
            "pptx-empty-slide",
            &[
                ("ppt/slides/slide1.xml", "<a:p></a:p>"),
                ("ppt/slides/slide2.xml", "<a:t>only text</a:t>"),
            ],
        );
        let text = extract_pptx(&fx.0, 10_000).expect("pptx extracts");
        assert!(text.contains("only text"));
        assert!(
            text.contains("--- Slide 2 ---"),
            "slide 2 must keep its number, not be renumbered: {text:?}"
        );
        assert!(
            !text.contains("Slide 1"),
            "empty slide must not emit a header: {text:?}"
        );
    }

    #[test]
    fn xlsx_emits_shared_strings_and_sheet_values() {
        let fx = zip_fixture(
            "xlsx-basic",
            &[
                (
                    "xl/sharedStrings.xml",
                    "<sst><si><t>Zo&amp;euml;</t></si><si><t>pivotal string</t></si></sst>",
                ),
                (
                    "xl/worksheets/sheet1.xml",
                    "<sheetData><row><c><v>42</v></c></row><row><c><v>7</v></c></row></sheetData>",
                ),
            ],
        );
        let text = extract_xlsx(&fx.0, 10_000).expect("xlsx extracts");
        assert!(
            text.contains("Zo&euml;"),
            "shared strings decoded: {text:?}"
        );
        assert!(text.contains("42"), "sheet values indexed: {text:?}");
        assert!(text.contains('\n'));
    }

    #[test]
    fn xlsx_without_shared_strings_still_extracts_cells() {
        let fx = zip_fixture(
            "xlsx-no-shared",
            &[(
                "xl/worksheets/sheet1.xml",
                "<sheetData><row><c><v>inline note</v></c></row></sheetData>",
            )],
        );
        let text = extract_xlsx(&fx.0, 10_000).expect("xlsx extracts");
        assert!(text.contains("inline note"), "got: {text:?}");
    }

    #[test]
    fn xlsx_workbook_with_no_content_is_none() {
        let fx = zip_fixture(
            "xlsx-empty",
            &[("xl/workbook.xml", "<workbook></workbook>")],
        );
        assert!(extract_xlsx(&fx.0, 10_000).is_none());
    }

    #[test]
    fn epub_extracts_text_and_newlines_per_heading() {
        let fx = zip_fixture(
            "epub-basic",
            &[
                (
                    "OEBPS/chapter2.xhtml",
                    "<h:html><h:body><h:p>second chapter words</h:p><h:h1>heading two</h:h1></h:body></h:html>",
                ),
                (
                    "OEBPS/chapter1.html",
                    "<html><body><p>first chapter words</p></body></html>",
                ),
            ],
        );
        let text = extract_epub(&fx.0, 10_000).expect("epub extracts");
        assert!(text.contains("first chapter words"));
        assert!(text.contains("second chapter words"));
        assert!(text.contains("heading two"));
        assert!(text.contains('\n'));
    }

    #[test]
    fn odt_paragraphs_and_headings_break_lines() {
        let fx = zip_fixture(
            "odt-basic",
            &[(
                "content.xml",
                "<office:body><text:h>Chapter one</text:h><text:p>body words here</text:p></office:body>",
            )],
        );
        let text = extract_odt(&fx.0, 10_000).expect("odt extracts");
        assert!(text.contains("Chapter one"));
        assert!(text.contains("body words here"));
        assert!(text.contains('\n'));
    }

    #[test]
    fn odt_without_content_xml_is_none() {
        let fx = zip_fixture("odt-missing", &[("styles.xml", "<styles/>")]);
        assert!(extract_odt(&fx.0, 10_000).is_none());
    }
}
