// The 273-byte ONNX graph edit that makes Kokoro return the per-token durations it was
// already computing. Applied in memory to the bytes of the stock `model.onnx` at session
// build time (native_synth::build_session) — there is no patched file on disk and nothing
// extra to download.
//
// WHY IN MEMORY. The edit was originally shipped as a 326 MB sidecar (`kokoro-claude-
// variant.onnx`) that had to be produced by a Python script and copied in by hand, so every
// released install fell back to estimated word timing. The three alternatives all cost
// something real: hosting a 326 MB derivative that nobody can reproduce, a second 326 MB
// download, or a second 326 MB on disk. Patching the bytes on the way into the session costs
// none of them, and the shipped code *is* the derivation, so it is reproducible by
// inspection rather than on trust.
//
// WHY IT IS ONLY 273 BYTES. `graph` is field 7 of `ModelProto`, and protobuf MERGES a
// repeated appearance of a singular message field: a second `graph` carrying only `node` and
// `output` entries is appended to the lists already there. So the edit is a pure append —
// not one byte of the 325 MB of weights before it moves, and no length prefix has to be
// rewritten. That is the whole reason this can be a `Vec::extend_from_slice` instead of a
// protobuf writer.
//
// WHAT IT ADDS. Two `Identity` nodes exporting tensors the stock length regulator already
// produces, plus their `graph.output` declarations:
//
//   durations_frames  <- /encoder/Clip_output_0    float32 [batch, tokens]
//   duration_cumsum   <- /encoder/CumSum_output_0  int64   [tokens]
//
// No computation is added and no weight is touched. See `native_synth`'s DURATIONS_OUTPUT
// for what the host does with the first; the second is an offline audit witness and is never
// fetched, which costs nothing because ORT is asked for outputs by name.
//
// Three traps, each of which cost something to find:
//
//   1. Export via `Identity`, never by renaming the producer's output. Renaming
//      `/encoder/Clip_output_0` in place would rewire every downstream consumer of it.
//   2. The two tensors are NOT the same element type — float32 and int64. Declaring both
//      float passes `onnx.checker` and then fails at ORT session creation.
//   3. Both a type and a shape are required on each declaration; a bare name is rejected.
//
// The appended nodes land at the end of `graph.node`, which keeps ONNX's topological-order
// requirement satisfied: their inputs are produced far upstream.

/// ONNX `TensorProto.DataType` values for the two exported tensors.
const ELEM_FLOAT: u64 = 1;
const ELEM_INT64: u64 = 7;

/// The tensors to export, and the public name each is given.
const EXPORTS: [(&str, &str, u64, &[&str]); 2] = [
    ("/encoder/Clip_output_0", "durations_frames", ELEM_FLOAT, &["batch", "tokens"]),
    ("/encoder/CumSum_output_0", "duration_cumsum", ELEM_INT64, &["tokens"]),
];

// Protobuf field numbers, from onnx.proto. Named rather than inlined because a wrong one
// here produces a file that still parses — into a different graph.
const MODEL_GRAPH: u32 = 7; // ModelProto.graph
const GRAPH_NODE: u32 = 1; // GraphProto.node
const GRAPH_OUTPUT: u32 = 12; // GraphProto.output
const NODE_INPUT: u32 = 1; // NodeProto.input
const NODE_OUTPUT: u32 = 2; // NodeProto.output
const NODE_NAME: u32 = 3; // NodeProto.name
const NODE_OP_TYPE: u32 = 4; // NodeProto.op_type
const VALUE_INFO_NAME: u32 = 1; // ValueInfoProto.name
const VALUE_INFO_TYPE: u32 = 2; // ValueInfoProto.type
const TYPE_TENSOR_TYPE: u32 = 1; // TypeProto.tensor_type
const TENSOR_ELEM_TYPE: u32 = 1; // TypeProto.Tensor.elem_type
const TENSOR_SHAPE: u32 = 2; // TypeProto.Tensor.shape
const SHAPE_DIM: u32 = 1; // TensorShapeProto.dim
const DIM_PARAM: u32 = 2; // TensorShapeProto.Dimension.dim_param

const WIRE_VARINT: u64 = 0;
const WIRE_LEN: u64 = 2;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_tag(out: &mut Vec<u8>, field: u32, wire: u64) {
    put_varint(out, ((field as u64) << 3) | wire);
}

fn put_varint_field(out: &mut Vec<u8>, field: u32, v: u64) {
    put_tag(out, field, WIRE_VARINT);
    put_varint(out, v);
}

/// A length-delimited field: tag, byte length, body. Used for both strings and nested
/// messages, which is why every nested message here is built into its own `Vec` first —
/// the length has to be known before the tag can be written.
fn put_len_field(out: &mut Vec<u8>, field: u32, body: &[u8]) {
    put_tag(out, field, WIRE_LEN);
    put_varint(out, body.len() as u64);
    out.extend_from_slice(body);
}

fn put_str_field(out: &mut Vec<u8>, field: u32, s: &str) {
    put_len_field(out, field, s.as_bytes());
}

/// `NodeProto` for `output = Identity(input)`.
fn identity_node(input: &str, output: &str) -> Vec<u8> {
    let mut n = Vec::new();
    put_str_field(&mut n, NODE_INPUT, input);
    put_str_field(&mut n, NODE_OUTPUT, output);
    put_str_field(&mut n, NODE_NAME, &format!("kokoro_claude_export_{output}"));
    put_str_field(&mut n, NODE_OP_TYPE, "Identity");
    n
}

/// `ValueInfoProto` declaring `name` as a tensor of `elem_type` with one symbolic
/// dimension per entry of `dims`.
fn value_info(name: &str, elem_type: u64, dims: &[&str]) -> Vec<u8> {
    let mut shape = Vec::new();
    for d in dims {
        let mut dim = Vec::new();
        put_str_field(&mut dim, DIM_PARAM, d);
        put_len_field(&mut shape, SHAPE_DIM, &dim);
    }
    let mut tensor = Vec::new();
    put_varint_field(&mut tensor, TENSOR_ELEM_TYPE, elem_type);
    put_len_field(&mut tensor, TENSOR_SHAPE, &shape);

    let mut ty = Vec::new();
    put_len_field(&mut ty, TYPE_TENSOR_TYPE, &tensor);

    let mut vi = Vec::new();
    put_str_field(&mut vi, VALUE_INFO_NAME, name);
    put_len_field(&mut vi, VALUE_INFO_TYPE, &ty);
    vi
}

/// The bytes to append to a stock `model.onnx` so the loaded graph also returns
/// [`EXPORTS`]. Appending is the entire edit; see the module comment for why that is
/// sufficient.
pub fn duration_outputs() -> Vec<u8> {
    let mut graph = Vec::new();
    // Nodes first, then the output declarations. Wire order between different fields is
    // free in protobuf; this just mirrors how onnx itself serializes the graph.
    for (src, name, _, _) in EXPORTS {
        put_len_field(&mut graph, GRAPH_NODE, &identity_node(src, name));
    }
    for (_, name, elem_type, dims) in EXPORTS {
        put_len_field(&mut graph, GRAPH_OUTPUT, &value_info(name, elem_type, dims));
    }
    let mut out = Vec::new();
    put_len_field(&mut out, MODEL_GRAPH, &graph);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `graph.node` and `graph.output` bytes lifted verbatim out of
    /// `kokoro-claude-variant.onnx` — the artifact `onnx` itself serialized, which was
    /// verified to produce a bit-identical waveform to stock on both execution providers.
    /// They are the oracle for the encoder above: this crate must emit exactly what the
    /// Python library emitted, or the in-memory graph is not the graph that was tested.
    ///
    /// 186 + 84 = 270, which is exactly how much larger that file is than `model.onnx`.
    const ONNX_NODES: &str = "0a5b0a162f656e636f6465722f436c69705f6f75747075745f30121064757261\
        74696f6e735f6672616d65731a256b6f6b6f726f5f636c617564655f6578706f72745f6475726174696f6e\
        735f6672616d657322084964656e746974790a5b0a182f656e636f6465722f43756d53756d5f6f75747075\
        745f30120f6475726174696f6e5f63756d73756d1a246b6f6b6f726f5f636c617564655f6578706f72745f\
        6475726174696f6e5f63756d73756d22084964656e74697479";
    const ONNX_OUTPUTS: &str = "622d0a106475726174696f6e735f6672616d657312190a17080112130a0712\
        0562617463680a081206746f6b656e7362230a0f6475726174696f6e5f63756d73756d12100a0e0807120a\
        0a081206746f6b656e73";

    fn unhex(s: &str) -> Vec<u8> {
        let h: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        h.chunks(2)
            .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn matches_the_bytes_onnx_wrote() {
        let mut want = unhex(ONNX_NODES);
        want.extend_from_slice(&unhex(ONNX_OUTPUTS));
        assert_eq!(
            want.len(),
            270,
            "the oracle is the +270 bytes, or it is the wrong oracle"
        );

        let got = duration_outputs();
        // Strip the wrapping `graph` tag + length this module adds; onnx rewrote the
        // existing graph's own length instead, which is the difference between an append
        // and a re-serialization.
        assert_eq!(got[0], (MODEL_GRAPH << 3) as u8 | WIRE_LEN as u8);
        let mut len = Vec::new();
        put_varint(&mut len, want.len() as u64);
        assert_eq!(&got[1..1 + len.len()], &len[..], "graph length prefix");
        assert_eq!(&got[1 + len.len()..], &want[..], "graph body");
    }

    #[test]
    fn varints_are_little_endian_base_128() {
        let mut v = Vec::new();
        put_varint(&mut v, 0);
        put_varint(&mut v, 127);
        put_varint(&mut v, 128);
        put_varint(&mut v, 270);
        assert_eq!(v, vec![0x00, 0x7f, 0x80, 0x01, 0x8e, 0x02]);
    }

    /// The whole append, including the wrapper (1-byte tag + 2-byte length + 270-byte
    /// body). Worth pinning: the number appears in the module comment and in the docs, and
    /// a change to it means the graph edit changed.
    #[test]
    fn total_append_is_273_bytes() {
        assert_eq!(duration_outputs().len(), 273);
    }
}
