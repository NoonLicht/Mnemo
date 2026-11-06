use lopdf::Document;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct PdfExtractResult {
    pub text: String,
    pub page_count: usize,
    pub is_encrypted: bool,
    pub is_scanned: bool,
    /// The PDF looked scanned but no OCR engine could run, so the text is a
    /// placeholder that should be replaced once one can.
    pub ocr_unavailable: bool,
}

pub fn extract_pdf(path: &Path, max_bytes: usize) -> Option<PdfExtractResult> {
    let path_buf = path.to_path_buf();
    let res = std::panic::catch_unwind(move || {
        let doc = match Document::load(&path_buf) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[Mnemo PDF] Failed to load {}: {}", path_buf.display(), e);
                return None;
            }
        };

        if doc.is_encrypted() {
            return Some(PdfExtractResult {
                text: "[Encrypted PDF: Password Protected]".to_string(),
                page_count: 0,
                is_encrypted: true,
                is_scanned: false,
                ocr_unavailable: false,
            });
        }

        let pages = doc.get_pages();
        let page_count = pages.len();
        let mut full_text = String::new();

        for page_num in pages.keys() {
            if full_text.len() >= max_bytes {
                full_text.push_str("\n... [content truncated at limit]");
                break;
            }
            if let Ok(page_text) = doc.extract_text(&[*page_num]) {
                let trimmed = page_text.trim();
                if !trimmed.is_empty() {
                    if !full_text.is_empty() {
                        full_text.push_str(&format!("\n\n--- Page {} ---\n", page_num));
                    }
                    full_text.push_str(trimmed);
                }
            }
        }

        // Fewer than 50 characters of real text across the whole document
        // means scanned pages (often with a stamped page number or a scanner
        // banner, which is why this is not "exactly empty").
        let is_scanned = page_count > 0 && full_text.trim().len() < 50;
        let mut ocr_unavailable = false;
        // A single huge page can exceed the cap on its own (the per-page
        // check only fires between pages); the assembled text is truncated
        // here so the content budget holds for every PDF.
        let mut text = if is_scanned && crate::indexer::extract::ocr_enabled() {
            let (ocr_text, unavailable) = extract_scanned_pdf_ocr(&doc, max_bytes);
            ocr_unavailable = unavailable;
            match ocr_text {
                Some(ocr_text) if full_text.trim().is_empty() => ocr_text,
                Some(ocr_text) => format!("{}\n\n{}", full_text.trim(), ocr_text),
                None if full_text.trim().is_empty() => "[Scanned PDF: Image only]".to_string(),
                None => full_text,
            }
        } else if is_scanned && full_text.trim().is_empty() {
            "[Scanned PDF: Image only]".to_string()
        } else {
            full_text
        };

        if text.len() > max_bytes {
            let mut cut = max_bytes;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n... [content truncated at limit]");
        }

        Some(PdfExtractResult {
            text,
            page_count,
            is_encrypted: false,
            is_scanned,
            ocr_unavailable,
        })
    });

    match res {
        Ok(opt) => opt,
        Err(_) => {
            eprintln!(
                "[Mnemo PDF] Panic caught while parsing PDF {}",
                path.display()
            );
            None
        }
    }
}

/// Most pages of a scanned PDF that get OCR'd. PP-OCR reads a page in about
/// 0.15 s, so 20 pages fit comfortably in the extraction time budget; Indra
/// takes up to about 0.75 s on a dense page, so 10; Tesseract takes about 1 s
/// a page, so it stays at 6 to avoid timing out (a timed-out file is
/// retried, so too many pages would retry forever).
fn max_ocr_pages() -> usize {
    match crate::ocr::usable_engine() {
        crate::ocr::ACCURATE => 20,
        crate::ocr::INDRA => 10,
        _ => 6,
    }
}

/// OCRs the page images of a scanned PDF, in page order.
///
/// Returns the text (if any) and whether OCR was unavailable, so the caller
/// can retry the file once an engine can run. Undecodable images (JPEG 2000,
/// fax, JBIG2) are logged once per file rather than skipped silently.
fn extract_scanned_pdf_ocr(doc: &Document, max_bytes: usize) -> (Option<String>, bool) {
    use crate::ocr::OcrOutcome;

    let mut skipped: Vec<String> = Vec::new();
    let images = crate::indexer::extract::pdf_image::page_images(doc, max_ocr_pages(), |reason| {
        if skipped.len() < 3 {
            skipped.push(reason);
        }
    });
    if images.is_empty() && !skipped.is_empty() {
        eprintln!(
            "[Mnemo PDF] scanned PDF has no image Mnemo can decode: {}",
            skipped.join("; ")
        );
    }

    let mut collected = String::new();
    let mut unavailable = false;
    for image in &images {
        if collected.len() >= max_bytes {
            break;
        }
        match crate::indexer::extract::image::ocr_decoded_image(image) {
            OcrOutcome::Text(text) => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    if !collected.is_empty() {
                        collected.push_str("\n\n");
                    }
                    collected.push_str(trimmed);
                }
            }
            OcrOutcome::NoText => {}
            OcrOutcome::Unavailable => {
                unavailable = true;
                break;
            }
        }
    }

    let text = (!collected.trim().is_empty()).then_some(collected);
    (text, unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Object, Stream, dictionary};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// A temp file that removes itself on drop.
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
            "mnemo-pdf-{}-{}-{}",
            tag,
            std::process::id(),
            nanos
        ))
    }

    /// Builds and saves a PDF with one page per text. Empty page text
    /// produces a page with no text operators (viewed as "scanned").
    fn pdf_fixture(tag: &str, page_texts: &[&str]) -> Fixture {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        // lopdf's text collection only runs when /F1 Tf resolves to a font
        // in the page resources, so every page carries the same minimal
        // Helvetica resource.
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
            "Encoding" => "StandardEncoding",
        });
        let resources = dictionary! { "Font" => dictionary! { "F1" => font_id } };
        let mut kids = Vec::new();
        for page_text in page_texts {
            let ops = if page_text.is_empty() {
                Vec::new()
            } else {
                format!("BT /F1 12 Tf ({}) Tj ET", page_text).into_bytes()
            };
            let content_id = doc.add_object(Stream::new(Dictionary::new(), ops));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources.clone(),
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            });
            kids.push(page_id.into());
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => kids.clone(),
                "Count" => i64::try_from(kids.len()).expect("page count"),
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let path = temp_path(tag);
        doc.save(&path).expect("write pdf");
        Fixture(path)
    }

    #[test]
    fn pdf_extracts_page_text_and_count() {
        let long = "mnemo full text extraction test document with plenty of words on the page"; // > 50 chars: not scanned
        let fx = pdf_fixture("pdf-multi", &[long, "second page marker"]);
        let result = extract_pdf(&fx.0, 1_000_000).expect("pdf extracts");
        assert_eq!(result.page_count, 2);
        assert!(!result.is_encrypted);
        assert!(!result.is_scanned);
        assert!(result.text.contains(long), "got: {:?}", result.text);
        assert!(
            result.text.contains("second page marker"),
            "all pages indexed: {:?}",
            result.text
        );
    }

    #[test]
    fn pdf_pages_get_their_own_marker() {
        let fx = pdf_fixture(
            "pdf-pages-marker",
            &[
                "alpha words to make the page real",
                "beta words that follow",
            ],
        );
        let result = extract_pdf(&fx.0, 1_000_000).expect("pdf extracts");
        assert!(result.text.contains("--- Page"), "{:?}", result.text);
    }

    #[test]
    fn pdf_with_no_extractable_text_counts_as_scanned() {
        let fx = pdf_fixture("pdf-scanned", &[""]);
        let result = extract_pdf(&fx.0, 1_000_000).expect("scanned pdf still returns a result");
        assert!(result.is_scanned);
        assert_eq!(result.page_count, 1);
        // With no OCR engine available on this host the placeholder stands in;
        // the real replacement happens when an engine can run.
        assert!(!result.text.is_empty());
    }

    #[test]
    fn pdf_short_text_also_counts_as_scanned() {
        // Below the 50-char threshold: scanner banner + page number class of
        // content must not promise a "pure text" extraction.
        let fx = pdf_fixture("pdf-banner", &["Page 3"]);
        let result = extract_pdf(&fx.0, 1_000_000).expect("pdf extracts");
        assert!(result.is_scanned);
    }

    #[test]
    fn pdf_truncates_at_max_bytes() {
        let fx = pdf_fixture("pdf-cap", &["word ".repeat(400).as_str()]);
        let result = extract_pdf(&fx.0, 40).expect("pdf extracts");
        assert!(
            result.text.contains("[content truncated at limit]"),
            "{:?}",
            result.text
        );
    }

    #[test]
    fn pdf_garbage_and_missing_input_is_none_never_panics() {
        let path = temp_path("pdf-garbage");
        std::fs::write(&path, b"definitely not a pdf file").unwrap();
        let fx = Fixture(path);
        assert!(extract_pdf(&fx.0, 1000).is_none());

        let missing = temp_path("pdf-absent");
        let _ = std::fs::remove_file(&missing);
        assert!(extract_pdf(&missing, 1000).is_none());
    }
}
