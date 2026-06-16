fn main() {
    // Generate Rust types for the ONNX protobuf (proto/onnx.proto) so the
    // backend can patch the downloaded Kokoro model for the DirectML EP
    // (onnx_patch.rs). Point prost-build at a vendored protoc so the build
    // needs no system protoc install.
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc binary");
    std::env::set_var("PROTOC", protoc);
    prost_build::compile_protos(&["proto/onnx.proto"], &["proto"])
        .expect("compile proto/onnx.proto");
    println!("cargo:rerun-if-changed=proto/onnx.proto");

    tauri_build::build()
}
