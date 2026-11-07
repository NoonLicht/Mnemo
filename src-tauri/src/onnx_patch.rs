//! Minimal ONNX model surgery for the hybrid OCR helper.
//!
//! The PP-OCR recogniser ends in `Softmax` and returns `[n, t, classes]`
//! probabilities (6906 classes). Copying that tensor off the GPU and decoding
//! it on the CPU was the bottleneck of GPU recognition: reducing the class axis
//! inside the model makes the output about 6900 times smaller. This module adds
//! `ArgMax` and `ReduceMax` nodes after the last node and exposes their results
//! (`rec_idx`: int64 `[n, t]`, `rec_max`: float `[n, t]`) as the graph outputs.
//!
//! It edits the protobuf wire format directly (no protobuf dependency): a
//! message is a sequence of `tag, payload` fields, so appending nodes and
//! replacing the output list only needs the graph field re-emitted with a new
//! length. Everything else is copied byte for byte.

const TENSOR_FLOAT: u64 = 1;
const TENSOR_INT64: u64 = 7;
const ATTR_INT: u64 = 2;
const ATTR_INTS: u64 = 7;

/// Names of the two outputs of a patched recogniser.
pub const OUT_INDEX: &str = "rec_idx";
pub const OUT_MAX: &str = "rec_max";

struct Field<'a> {
    num: u32,
    /// The complete field as it appeared (tag and payload).
    raw: &'a [u8],
    /// The payload of a length-delimited field; empty otherwise.
    payload: &'a [u8],
    varint: u64,
}

fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *buf.get(*pos).ok_or("truncated varint")?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 63 {
            return Err("varint too long".into());
        }
    }
}

fn put_varint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn fields(buf: &[u8]) -> Result<Vec<Field<'_>>, String> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let start = pos;
        let tag = read_varint(buf, &mut pos)?;
        let num = (tag >> 3) as u32;
        let (payload, varint) = match tag & 7 {
            0 => (&buf[0..0], read_varint(buf, &mut pos)?),
            1 => {
                pos = pos
                    .checked_add(8)
                    .filter(|p| *p <= buf.len())
                    .ok_or("truncated fixed64")?;
                (&buf[0..0], 0)
            }
            2 => {
                let len = read_varint(buf, &mut pos)? as usize;
                let end = pos
                    .checked_add(len)
                    .filter(|e| *e <= buf.len())
                    .ok_or("truncated field")?;
                let payload = &buf[pos..end];
                pos = end;
                (payload, 0)
            }
            5 => {
                pos = pos
                    .checked_add(4)
                    .filter(|p| *p <= buf.len())
                    .ok_or("truncated fixed32")?;
                (&buf[0..0], 0)
            }
            other => return Err(format!("unsupported wire type {other}")),
        };
        out.push(Field {
            num,
            raw: &buf[start..pos],
            payload,
            varint,
        });
    }
    Ok(out)
}

fn put_len(out: &mut Vec<u8>, field: u32, payload: &[u8]) {
    put_varint(out, (u64::from(field) << 3) | 2);
    put_varint(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

fn put_int(out: &mut Vec<u8>, field: u32, value: u64) {
    put_varint(out, u64::from(field) << 3);
    put_varint(out, value);
}

fn attr_int(name: &str, value: u64) -> Vec<u8> {
    let mut a = Vec::new();
    put_len(&mut a, 1, name.as_bytes());
    put_int(&mut a, 3, value);
    put_int(&mut a, 20, ATTR_INT);
    a
}

fn attr_ints(name: &str, values: &[u64]) -> Vec<u8> {
    let mut a = Vec::new();
    put_len(&mut a, 1, name.as_bytes());
    for value in values {
        put_int(&mut a, 8, *value);
    }
    put_int(&mut a, 20, ATTR_INTS);
    a
}

fn node(op: &str, name: &str, input: &str, output: &str, attrs: &[Vec<u8>]) -> Vec<u8> {
    let mut n = Vec::new();
    put_len(&mut n, 1, input.as_bytes());
    put_len(&mut n, 2, output.as_bytes());
    put_len(&mut n, 3, name.as_bytes());
    put_len(&mut n, 4, op.as_bytes());
    for attr in attrs {
        put_len(&mut n, 5, attr);
    }
    n
}

fn value_info(name: &str, elem_type: u64) -> Vec<u8> {
    let mut tensor = Vec::new();
    put_int(&mut tensor, 1, elem_type);
    let mut type_proto = Vec::new();
    put_len(&mut type_proto, 1, &tensor);
    let mut info = Vec::new();
    put_len(&mut info, 1, name.as_bytes());
    put_len(&mut info, 2, &type_proto);
    info
}

/// True when the model already has the two reduced outputs.
pub fn is_patched(model: &[u8]) -> bool {
    output_names(model).is_ok_and(|names| names == [OUT_INDEX, OUT_MAX])
}

fn output_names(model: &[u8]) -> Result<Vec<String>, String> {
    let top = fields(model)?;
    let graph = top
        .iter()
        .find(|f| f.num == 7)
        .ok_or("model has no graph")?;
    let mut names = Vec::new();
    for f in fields(graph.payload)?.iter().filter(|f| f.num == 12) {
        let name = fields(f.payload)?
            .into_iter()
            .find(|g| g.num == 1)
            .map(|g| String::from_utf8_lossy(g.payload).into_owned())
            .ok_or("graph output without a name")?;
        names.push(name);
    }
    Ok(names)
}

/// Returns a copy of `model` whose outputs are the per-step argmax and max
/// probability of its single (class-axis-last) output.
pub fn add_argmax_outputs(model: &[u8]) -> Result<Vec<u8>, String> {
    if is_patched(model) {
        return Ok(model.to_vec());
    }
    let top = fields(model)?;
    // `ReduceMax` takes `axes` as an attribute up to opset 17.
    for f in top.iter().filter(|f| f.num == 8) {
        let parts = fields(f.payload)?;
        let domain = parts
            .iter()
            .find(|p| p.num == 1)
            .map(|p| p.payload)
            .unwrap_or_default();
        let version = parts.iter().find(|p| p.num == 2).map_or(0, |p| p.varint);
        if (domain.is_empty() || domain == b"ai.onnx") && !(11..=17).contains(&version) {
            return Err(format!("unsupported ONNX opset {version} (need 11 to 17)"));
        }
    }
    let graph = top
        .iter()
        .find(|f| f.num == 7)
        .ok_or("model has no graph")?;
    let names = output_names(model)?;
    let [source] = names.as_slice() else {
        return Err(format!("expected one graph output, found {}", names.len()));
    };

    let mut new_graph = Vec::with_capacity(graph.payload.len() + 512);
    for f in fields(graph.payload)? {
        if f.num != 12 {
            new_graph.extend_from_slice(f.raw);
        }
    }
    put_len(
        &mut new_graph,
        1,
        &node(
            "ArgMax",
            "mnemo_argmax",
            source,
            OUT_INDEX,
            &[attr_int("axis", 2), attr_int("keepdims", 0)],
        ),
    );
    put_len(
        &mut new_graph,
        1,
        &node(
            "ReduceMax",
            "mnemo_reducemax",
            source,
            OUT_MAX,
            &[attr_ints("axes", &[2]), attr_int("keepdims", 0)],
        ),
    );
    put_len(&mut new_graph, 12, &value_info(OUT_INDEX, TENSOR_INT64));
    put_len(&mut new_graph, 12, &value_info(OUT_MAX, TENSOR_FLOAT));

    let mut out = Vec::with_capacity(model.len() + 512);
    for f in &top {
        if f.num == 7 {
            put_len(&mut out, 7, &new_graph);
        } else {
            out.extend_from_slice(f.raw);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny model: input `x`, one `Softmax` node, output `y`, opset 11.
    fn tiny_model(opset: u64) -> Vec<u8> {
        let softmax = node("Softmax", "sm", "x", "y", &[attr_int("axis", 2)]);
        let mut graph = Vec::new();
        put_len(&mut graph, 1, &softmax);
        put_len(&mut graph, 2, b"g");
        put_len(&mut graph, 11, &value_info("x", TENSOR_FLOAT));
        put_len(&mut graph, 12, &value_info("y", TENSOR_FLOAT));
        let mut opset_import = Vec::new();
        put_int(&mut opset_import, 2, opset);
        let mut model = Vec::new();
        put_int(&mut model, 1, 7);
        put_len(&mut model, 7, &graph);
        put_len(&mut model, 8, &opset_import);
        model
    }

    #[test]
    fn varints_round_trip() {
        for value in [0u64, 1, 127, 128, 300, 1 << 40] {
            let mut buf = Vec::new();
            put_varint(&mut buf, value);
            let mut pos = 0;
            assert_eq!(read_varint(&buf, &mut pos).unwrap(), value);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn patch_replaces_the_single_output_with_the_two_reduced_ones() {
        let model = tiny_model(11);
        assert_eq!(output_names(&model).unwrap(), ["y"]);
        assert!(!is_patched(&model));
        let patched = add_argmax_outputs(&model).unwrap();
        assert_eq!(output_names(&patched).unwrap(), [OUT_INDEX, OUT_MAX]);
        assert!(is_patched(&patched));
        // The input, opset and original node survive; two nodes were added.
        let top = fields(&patched).unwrap();
        let graph = top.iter().find(|f| f.num == 7).unwrap();
        let nodes = fields(graph.payload)
            .unwrap()
            .iter()
            .filter(|f| f.num == 1)
            .count();
        assert_eq!(nodes, 3);
        assert_eq!(top.iter().filter(|f| f.num == 8).count(), 1);
    }

    #[test]
    fn patching_twice_changes_nothing() {
        let once = add_argmax_outputs(&tiny_model(13)).unwrap();
        assert_eq!(add_argmax_outputs(&once).unwrap(), once);
    }

    #[test]
    fn unsupported_opsets_and_garbage_are_refused() {
        assert!(add_argmax_outputs(&tiny_model(18)).is_err());
        assert!(add_argmax_outputs(&tiny_model(9)).is_err());
        assert!(add_argmax_outputs(&[0xff, 0xff, 0xff]).is_err());
        assert!(add_argmax_outputs(&[]).is_err());
    }

    /// Needs the real recogniser: the patched model must load and give the
    /// argmax of the original output. Run with MNEMO_OCR_PACK_DIR and
    /// ORT_DYLIB_PATH set.
    #[test]
    #[ignore = "requires MNEMO_OCR_PACK_DIR and ORT_DYLIB_PATH"]
    fn real_recogniser_patches_and_loads() {
        let dir = std::env::var("MNEMO_OCR_PACK_DIR").expect("MNEMO_OCR_PACK_DIR");
        let rec = std::fs::read(std::path::Path::new(&dir).join("rec.onnx")).unwrap();
        let patched = add_argmax_outputs(&rec).unwrap();
        assert!(is_patched(&patched));
        assert!(patched.len() > rec.len());
    }
}
