// Dev harness for the DirectML ONNX patch (onnx_patch.rs): apply it to a file
// so the output can be validated with kokoro-sapi's kokoro_test / worker.
//
//   cargo run --example dml_patch -- <in.onnx> <out.onnx>
use std::{env, fs};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: dml_patch <in.onnx> <out.onnx>");
        std::process::exit(2);
    }
    let bytes = fs::read(&args[1]).expect("read input");
    let (out, n) =
        kokoro_reader_lib::onnx_patch::patch_convtranspose_2d(&bytes).expect("patch failed");
    fs::write(&args[2], &out).expect("write output");
    println!("patched {n} ConvTranspose node(s): {} -> {}", args[1], args[2]);
}
