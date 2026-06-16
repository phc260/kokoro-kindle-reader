// Lift 1-D ConvTranspose nodes to 2-D for the ONNX Runtime DirectML EP.
//
// A direct port of kokoro-sapi/tools/patch_convtranspose_2d.py. The DML EP
// fails at *execute* time on Kokoro's 1-D ConvTranspose nodes ("the parameter
// is incorrect"); the standard fix is to wrap each in Unsqueeze(axes=[2]) /
// Squeeze(axes=[2]), reshape its weights [C, M/g, k] -> [C, M/g, 1, k], and
// widen its attributes to 2-D. The (large) weight raw data is never touched.
//
// This runs once in the backend after the fp32 `onnx/model.onnx` is downloaded,
// producing `onnx/model_dml.onnx` for kokoro_worker.exe's GPU path.

use std::collections::HashMap;

use prost::Message;

// Types generated from proto/onnx.proto by prost-build (see build.rs).
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/onnx.rs"));
}

use pb::attribute_proto::AttributeType;
use pb::tensor_proto::DataType;
use pb::{AttributeProto, NodeProto, TensorProto};

// Shared axes constant for Unsqueeze/Squeeze (opset >= 13: axes are inputs).
const AXES_NAME: &str = "kokoro_dml_axes_h";

/// Patch `model_bytes` (a serialized ONNX ModelProto) and return the patched
/// bytes plus the number of ConvTranspose nodes rewritten.
pub fn patch_convtranspose_2d(model_bytes: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut model = pb::ModelProto::decode(model_bytes).map_err(|e| format!("decode onnx: {e}"))?;
    let graph = model.graph.as_mut().ok_or("model has no graph")?;

    // Append the shared [2] INT64 axes initializer (Unsqueeze/Squeeze input).
    graph.initializer.push(TensorProto {
        name: Some(AXES_NAME.to_string()),
        data_type: Some(DataType::Int64 as i32),
        dims: vec![1],
        int64_data: vec![2],
        ..Default::default()
    });

    // Index initializers by name so each ConvTranspose's weight can be found.
    let mut init_idx: HashMap<String, usize> = HashMap::new();
    for (i, t) in graph.initializer.iter().enumerate() {
        if let Some(n) = &t.name {
            init_idx.insert(n.clone(), i);
        }
    }

    let nodes = std::mem::take(&mut graph.node);
    let mut new_nodes: Vec<NodeProto> = Vec::with_capacity(nodes.len());
    let mut patched = 0usize;

    for node in nodes {
        if node.op_type.as_deref() != Some("ConvTranspose") {
            new_nodes.push(node);
            continue;
        }
        let ints = int_list_attrs(&node);
        // Only 1-D nodes (kernel_shape has exactly one entry); else already 2-D.
        let kernel = match ints.get("kernel_shape") {
            Some(ks) if ks.len() == 1 => ks[0],
            _ => {
                new_nodes.push(node);
                continue;
            }
        };

        let base = node
            .name
            .clone()
            .unwrap_or_else(|| node.output.first().cloned().unwrap_or_default());

        // Weights: [C, M/g, k] -> [C, M/g, 1, k] (raw data unchanged).
        let wname = node
            .input
            .get(1)
            .ok_or_else(|| format!("{base}: ConvTranspose has no weight input"))?;
        let wi = *init_idx
            .get(wname)
            .ok_or_else(|| format!("{base}: weight initializer '{wname}' not found"))?;
        let wdims = &mut graph.initializer[wi].dims;
        if wdims.len() != 3 {
            return Err(format!("{base}: unexpected weight rank {}", wdims.len()));
        }
        wdims.insert(2, 1);

        let x4 = format!("{base}_x4d");
        let y4 = format!("{base}_y4d");

        // Unsqueeze(data, axes) -> x4
        new_nodes.push(NodeProto {
            op_type: Some("Unsqueeze".into()),
            name: Some(format!("{base}_unsq")),
            input: vec![node.input[0].clone(), AXES_NAME.into()],
            output: vec![x4.clone()],
            ..Default::default()
        });

        // ConvTranspose widened to 2-D.
        let s = first_or(&ints, "strides", 1);
        let d = first_or(&ints, "dilations", 1);
        let (pb_begin, pb_end) = pads_pair(&ints);
        let group = int_attr(&node, "group", 1);
        let mut attrs = vec![
            attr_ints("kernel_shape", vec![1, kernel]),
            attr_ints("strides", vec![1, s]),
            attr_ints("dilations", vec![1, d]),
            attr_ints("pads", vec![0, pb_begin, 0, pb_end]),
            attr_int("group", group),
        ];
        if let Some(op) = ints.get("output_padding").and_then(|v| v.first()).copied() {
            attrs.push(attr_ints("output_padding", vec![0, op]));
        }
        let mut ct_input = vec![x4];
        ct_input.extend(node.input.iter().skip(1).cloned());
        new_nodes.push(NodeProto {
            op_type: Some("ConvTranspose".into()),
            name: node.name.clone(),
            input: ct_input,
            output: vec![y4.clone()],
            attribute: attrs,
            ..Default::default()
        });

        // Squeeze(y4, axes) -> original output(s)
        new_nodes.push(NodeProto {
            op_type: Some("Squeeze".into()),
            name: Some(format!("{base}_sq")),
            input: vec![y4, AXES_NAME.into()],
            output: node.output.clone(),
            ..Default::default()
        });
        patched += 1;
    }

    graph.node = new_nodes;
    Ok((model.encode_to_vec(), patched))
}

// Map of INTS-valued attributes by name (kernel_shape, strides, pads, ...).
fn int_list_attrs(node: &NodeProto) -> HashMap<String, Vec<i64>> {
    let mut m = HashMap::new();
    for a in &node.attribute {
        if let Some(name) = &a.name {
            if !a.ints.is_empty() {
                m.insert(name.clone(), a.ints.clone());
            }
        }
    }
    m
}

fn first_or(m: &HashMap<String, Vec<i64>>, key: &str, default: i64) -> i64 {
    m.get(key).and_then(|v| v.first().copied()).unwrap_or(default)
}

// ONNX 1-D pads are [begin, end]; missing -> [0, 0].
fn pads_pair(m: &HashMap<String, Vec<i64>>) -> (i64, i64) {
    match m.get("pads") {
        Some(p) if p.len() >= 2 => (p[0], p[1]),
        _ => (0, 0),
    }
}

// Single INT-valued attribute (e.g. group).
fn int_attr(node: &NodeProto, key: &str, default: i64) -> i64 {
    node.attribute
        .iter()
        .find(|a| a.name.as_deref() == Some(key))
        .and_then(|a| a.i)
        .unwrap_or(default)
}

fn attr_ints(name: &str, ints: Vec<i64>) -> AttributeProto {
    AttributeProto {
        name: Some(name.into()),
        r#type: Some(AttributeType::Ints as i32),
        ints,
        ..Default::default()
    }
}

fn attr_int(name: &str, i: i64) -> AttributeProto {
    AttributeProto {
        name: Some(name.into()),
        r#type: Some(AttributeType::Int as i32),
        i: Some(i),
        ..Default::default()
    }
}
