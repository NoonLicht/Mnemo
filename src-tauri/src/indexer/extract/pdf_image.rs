//! Decodes the images embedded in a PDF so scanned pages can be OCR'd.
//!
//! A PDF image XObject is not an image file. Its stream holds either a
//! complete JPEG (`DCTDecode`), or raw pixel rows behind lossless filters
//! (`FlateDecode`, `LZWDecode`, ...) whose layout is described only by the
//! dictionary: `Width`, `Height`, `BitsPerComponent`, `ColorSpace`, `Decode`.
//! `image::load_from_memory` on `decompressed_content()` therefore fails for
//! both: lopdf refuses to "decompress" a JPEG, and raw rows have no header.
//! This module handles both shapes, which together cover almost every
//! scanner and phone-scan app.
//!
//! Not supported (returned as `Err` with a reason so callers can log it):
//! `JPXDecode` (JPEG 2000), `CCITTFaxDecode`, `JBIG2Decode`, `Indexed` and
//! other exotic colour spaces, and bit depths other than 1 and 8.

use image::{DynamicImage, GrayImage, RgbImage};
use lopdf::{Document, Object, Stream};

/// Images above this many pixels are skipped; matches the image decoder's
/// 50 MP budget so a hostile PDF cannot allocate an unbounded buffer.
pub const MAX_PDF_IMAGE_PIXELS: u64 = 50_000_000;

/// Images smaller than this on either side are logos, bullets or icons, not
/// scanned pages. Skipping them saves OCR time and noise.
pub const MIN_PDF_IMAGE_SIDE: u32 = 64;

/// Every embedded raster image of every page, in page order, decoded.
///
/// Images are deduplicated by object id (a logo repeated on every page is
/// decoded once) and capped at `max_images`. Undecodable images are reported
/// through `on_skip` with a reason instead of being dropped silently.
pub fn page_images(
    doc: &Document,
    max_images: usize,
    mut on_skip: impl FnMut(String),
) -> Vec<DynamicImage> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (page_no, page_id) in doc.get_pages() {
        let Ok(images) = doc.get_page_images(page_id) else {
            continue;
        };
        for pdf_image in images {
            if out.len() >= max_images {
                return out;
            }
            if !seen.insert(pdf_image.id) {
                continue;
            }
            let Ok(stream) = doc.get_object(pdf_image.id).and_then(Object::as_stream) else {
                continue;
            };
            match decode_image_stream(doc, stream) {
                Ok(img) => out.push(img),
                Err(reason) => on_skip(format!("page {page_no}: {reason}")),
            }
        }
    }
    out
}

/// Decodes one image XObject stream into pixels.
pub fn decode_image_stream(doc: &Document, stream: &Stream) -> Result<DynamicImage, String> {
    let dict = &stream.dict;
    let width = dict_u32(doc, dict, b"Width").ok_or("missing Width")?;
    let height = dict_u32(doc, dict, b"Height").ok_or("missing Height")?;
    if width < MIN_PDF_IMAGE_SIDE || height < MIN_PDF_IMAGE_SIDE {
        return Err(format!("{width}x{height} is too small to be a page"));
    }
    if u64::from(width) * u64::from(height) > MAX_PDF_IMAGE_PIXELS {
        return Err(format!("{width}x{height} exceeds the pixel budget"));
    }
    if dict
        .get(b"ImageMask")
        .and_then(Object::as_bool)
        .unwrap_or(false)
    {
        return Err("stencil mask, not an image".into());
    }

    let filters: Vec<Vec<u8>> = stream
        .filters()
        .map(|f| f.into_iter().map(<[u8]>::to_vec).collect())
        .unwrap_or_default();

    // Terminal image codecs. Anything before them is a lossless wrapper.
    let terminal = filters.last().map(Vec::as_slice);
    match terminal {
        Some(b"DCTDecode") | Some(b"DCT") => {
            let jpeg = unwrap_prefix_filters(stream, &filters)?;
            let img = image::load_from_memory_with_format(&jpeg, image::ImageFormat::Jpeg)
                .map_err(|e| format!("JPEG decode failed: {e}"))?;
            return Ok(img);
        }
        Some(b"JPXDecode") => return Err("JPEG 2000 images are not supported".into()),
        Some(b"CCITTFaxDecode") | Some(b"CCF") => {
            return Err("CCITT fax images are not supported".into());
        }
        Some(b"JBIG2Decode") => return Err("JBIG2 images are not supported".into()),
        _ => {}
    }

    // Raw pixel rows.
    let max_bytes = (u64::from(width) * u64::from(height) * 4) as usize + 1024;
    let data = stream
        .decompressed_content_with_limit(max_bytes)
        .or_else(|e| {
            if filters.is_empty() {
                Ok(stream.content.clone())
            } else {
                Err(e)
            }
        })
        .map_err(|e| format!("could not decompress image data: {e}"))?;

    let bpc = dict_u32(doc, dict, b"BitsPerComponent").unwrap_or(8);
    let components = color_components(doc, dict.get(b"ColorSpace").ok())?;
    let inverted = decode_is_inverted(doc, dict);
    raw_to_image(&data, width, height, bpc, components, inverted)
}

/// Strips the lossless filters in front of a terminal codec, returning the
/// codec's input bytes (e.g. the JPEG file inside `[/FlateDecode /DCTDecode]`).
fn unwrap_prefix_filters(stream: &Stream, filters: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    if filters.len() <= 1 {
        return Ok(stream.content.clone());
    }
    let mut inner = stream.clone();
    let prefix: Vec<Object> = filters[..filters.len() - 1]
        .iter()
        .map(|f| Object::Name(f.clone()))
        .collect();
    inner.dict.set("Filter", Object::Array(prefix));
    inner
        .decompressed_content_with_limit(256 * 1024 * 1024)
        .map_err(|e| format!("could not unwrap image filters: {e}"))
}

fn resolve<'a>(doc: &'a Document, obj: &'a Object) -> &'a Object {
    match obj {
        Object::Reference(id) => doc.get_object(*id).unwrap_or(obj),
        _ => obj,
    }
}

fn dict_u32(doc: &Document, dict: &lopdf::Dictionary, key: &[u8]) -> Option<u32> {
    let value = resolve(doc, dict.get(key).ok()?).as_i64().ok()?;
    u32::try_from(value).ok()
}

/// Number of colour components for the supported colour spaces.
fn color_components(doc: &Document, space: Option<&Object>) -> Result<u32, String> {
    let Some(space) = space else {
        // Required for non-mask images, but be lenient: assume grey.
        return Ok(1);
    };
    match resolve(doc, space) {
        Object::Name(name) => match name.as_slice() {
            b"DeviceGray" | b"CalGray" | b"G" => Ok(1),
            b"DeviceRGB" | b"CalRGB" | b"RGB" => Ok(3),
            b"DeviceCMYK" | b"CMYK" => Ok(4),
            other => Err(format!(
                "colour space {} is not supported",
                String::from_utf8_lossy(other)
            )),
        },
        Object::Array(items) => {
            let family = items
                .first()
                .map(|o| resolve(doc, o))
                .and_then(|o| o.as_name().ok())
                .unwrap_or(b"");
            match family {
                b"ICCBased" => {
                    let n = items
                        .get(1)
                        .map(|o| resolve(doc, o))
                        .and_then(|o| o.as_stream().ok())
                        .and_then(|s| s.dict.get(b"N").ok())
                        .and_then(|n| n.as_i64().ok())
                        .unwrap_or(3);
                    match n {
                        1 | 3 | 4 => Ok(n as u32),
                        _ => Err(format!("ICC profile with {n} components")),
                    }
                }
                b"CalGray" => Ok(1),
                b"CalRGB" | b"Lab" => Ok(3),
                other => Err(format!(
                    "colour space {} is not supported",
                    String::from_utf8_lossy(other)
                )),
            }
        }
        _ => Err("unreadable colour space".into()),
    }
}

/// `/Decode [1 0]` on a 1-component image means 0 is white, not black.
fn decode_is_inverted(doc: &Document, dict: &lopdf::Dictionary) -> bool {
    let Ok(decode) = dict.get(b"Decode") else {
        return false;
    };
    let Ok(values) = resolve(doc, decode).as_array() else {
        return false;
    };
    let num = |o: &Object| {
        o.as_float()
            .ok()
            .or_else(|| o.as_i64().ok().map(|v| v as f32))
    };
    matches!(
        (values.first().and_then(num), values.get(1).and_then(num)),
        (Some(a), Some(b)) if a > b
    )
}

fn raw_to_image(
    data: &[u8],
    width: u32,
    height: u32,
    bpc: u32,
    components: u32,
    inverted: bool,
) -> Result<DynamicImage, String> {
    let (w, h) = (width as usize, height as usize);
    match (bpc, components) {
        (8, 1) => {
            let mut pixels = take(data, w * h)?;
            if inverted {
                pixels.iter_mut().for_each(|p| *p = 255 - *p);
            }
            GrayImage::from_raw(width, height, pixels)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| "grey buffer size mismatch".into())
        }
        (8, 3) => RgbImage::from_raw(width, height, take(data, w * h * 3)?)
            .map(DynamicImage::ImageRgb8)
            .ok_or_else(|| "RGB buffer size mismatch".into()),
        (8, 4) => {
            let cmyk = take(data, w * h * 4)?;
            let rgb: Vec<u8> = cmyk
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let k = 255 - u32::from(p[3]);
                    [p[0], p[1], p[2]].map(|c| ((255 - u32::from(c)) * k / 255) as u8)
                })
                .collect();
            RgbImage::from_raw(width, height, rgb)
                .map(DynamicImage::ImageRgb8)
                .ok_or_else(|| "CMYK buffer size mismatch".into())
        }
        (1, 1) => {
            // Rows are padded to whole bytes. 1 = white unless inverted.
            let stride = w.div_ceil(8);
            let packed = take(data, stride * h)?;
            let mut pixels = Vec::with_capacity(w * h);
            for row in packed.chunks_exact(stride) {
                for x in 0..w {
                    let bit = (row[x / 8] >> (7 - (x % 8))) & 1;
                    let white = (bit == 1) != inverted;
                    pixels.push(if white { 255 } else { 0 });
                }
            }
            GrayImage::from_raw(width, height, pixels)
                .map(DynamicImage::ImageLuma8)
                .ok_or_else(|| "bitmap buffer size mismatch".into())
        }
        _ => Err(format!(
            "{bpc}-bit images with {components} components are not supported"
        )),
    }
}

fn take(data: &[u8], needed: usize) -> Result<Vec<u8>, String> {
    data.get(..needed)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| format!("image data is short: {} of {needed} bytes", data.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{Dictionary, Stream, dictionary};

    /// Builds a one-page PDF whose page draws a single image XObject.
    fn pdf_with_image(image_dict: Dictionary, content: Vec<u8>) -> Document {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Stream::new(image_dict, content).with_compression(false));
        let resources = dictionary! { "XObject" => dictionary! { "Im0" => image_id } };
        let page_content = doc.add_object(Stream::new(
            Dictionary::new(),
            b"q 100 0 0 100 0 0 cm /Im0 Do Q".to_vec(),
        ));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => page_content,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        doc
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn base(width: i64, height: i64) -> Dictionary {
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
        }
    }

    fn decode_single(doc: &Document) -> Result<DynamicImage, String> {
        let mut reasons = Vec::new();
        let mut images = page_images(doc, 5, |r| reasons.push(r));
        match images.pop() {
            Some(image) => Ok(image),
            None => Err(reasons.join("; ")),
        }
    }

    #[test]
    fn jpeg_scans_are_decoded_not_skipped() {
        // lopdf refuses to "decompress" DCTDecode; the old path skipped every
        // JPEG scan, which is most scanners and phone-scan apps.
        let source = image::RgbImage::from_fn(120, 80, |x, _| image::Rgb([x as u8, 40, 200]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(source)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let mut dict = base(120, 80);
        dict.set("ColorSpace", "DeviceRGB");
        dict.set("BitsPerComponent", 8);
        dict.set("Filter", "DCTDecode");
        let img = decode_single(&pdf_with_image(dict, jpeg)).unwrap();
        assert_eq!((img.width(), img.height()), (120, 80));
    }

    #[test]
    fn flate_rgb_and_gray_rows_are_rebuilt_from_the_dictionary() {
        // The old path fed raw rows to `load_from_memory`, which needs a file
        // header, so every Flate scan was skipped too.
        let (w, h) = (100usize, 70usize);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 0, 255]).collect();
        let mut dict = base(w as i64, h as i64);
        dict.set("ColorSpace", "DeviceRGB");
        dict.set("BitsPerComponent", 8);
        dict.set("Filter", "FlateDecode");
        let img = decode_single(&pdf_with_image(dict, zlib(&rgb))).unwrap();
        assert_eq!(img.to_rgb8().get_pixel(1, 0).0, [1, 0, 255]);

        let gray: Vec<u8> = (0..w * h).map(|i| (i % 251) as u8).collect();
        let mut dict = base(w as i64, h as i64);
        dict.set("ColorSpace", "DeviceGray");
        dict.set("BitsPerComponent", 8);
        dict.set("Filter", "FlateDecode");
        let img = decode_single(&pdf_with_image(dict, zlib(&gray))).unwrap();
        assert_eq!(img.to_luma8().get_pixel(5, 0).0, [5]);
    }

    #[test]
    fn one_bit_scans_unpack_with_row_padding_and_decode_inversion() {
        // 70 px wide = 9 bytes per row (padded). First pixel black (bit 0).
        let (w, h) = (70usize, 64usize);
        let stride = w.div_ceil(8);
        let mut rows = vec![0xFFu8; stride * h];
        rows[0] = 0x7F;
        let mut dict = base(w as i64, h as i64);
        dict.set("ColorSpace", "DeviceGray");
        dict.set("BitsPerComponent", 1);
        dict.set("Filter", "FlateDecode");
        let img = decode_single(&pdf_with_image(dict.clone(), zlib(&rows))).unwrap();
        let luma = img.to_luma8();
        assert_eq!(luma.get_pixel(0, 0).0, [0]);
        assert_eq!(luma.get_pixel(1, 0).0, [255]);
        assert_eq!(luma.get_pixel(69, 63).0, [255]);

        dict.set("Decode", vec![1.into(), 0.into()]);
        let inverted = decode_single(&pdf_with_image(dict, zlib(&rows))).unwrap();
        assert_eq!(inverted.to_luma8().get_pixel(0, 0).0, [255]);
    }

    #[test]
    fn unsupported_codecs_and_icons_are_reported_not_silently_dropped() {
        let mut dict = base(200, 200);
        dict.set("Filter", "JBIG2Decode");
        let err = decode_single(&pdf_with_image(dict, vec![0; 16])).unwrap_err();
        assert!(err.contains("JBIG2"), "{err}");

        let mut dict = base(16, 16);
        dict.set("ColorSpace", "DeviceGray");
        dict.set("BitsPerComponent", 8);
        let err = decode_single(&pdf_with_image(dict, vec![0; 256])).unwrap_err();
        assert!(err.contains("too small"), "{err}");
    }

    #[test]
    fn short_image_data_is_an_error_not_a_panic() {
        let mut dict = base(100, 100);
        dict.set("ColorSpace", "DeviceRGB");
        dict.set("BitsPerComponent", 8);
        dict.set("Filter", "FlateDecode");
        let err = decode_single(&pdf_with_image(dict, zlib(&[1, 2, 3]))).unwrap_err();
        assert!(err.contains("short"), "{err}");
    }
}
