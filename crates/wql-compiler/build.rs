use std::path::PathBuf;

fn main() {
    let proto_dir = PathBuf::from("proto");
    let proto_file = proto_dir.join("testdata.proto");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let descriptor_path = out_dir.join("testdata.bin");

    // Generate Rust types and FileDescriptorSet for tests.
    prost_build::Config::new()
        .file_descriptor_set_path(&descriptor_path)
        .out_dir(&out_dir)
        .compile_protos(&[&proto_file], &[&proto_dir])
        .expect("failed to compile proto");

    println!("cargo:rerun-if-changed=proto/testdata.proto");
}
