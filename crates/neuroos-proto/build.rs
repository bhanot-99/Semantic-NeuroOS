// prost codegen from proto/neuroos/v1
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    let proto_dir = root.join("neuroos/v1");
    let protos: Vec<_> = std::fs::read_dir(&proto_dir)
        .expect("read proto/neuroos/v1")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "proto"))
        .collect();

    for p in &protos {
        println!("cargo:rerun-if-changed={}", p.display());
    }

    prost_build::compile_protos(&protos, &[root]).expect("compile neuroos.v1 protos with prost");
}
