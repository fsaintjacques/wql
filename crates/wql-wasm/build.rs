use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=WQL_PROGRAM");

    let src = env::var("WQL_PROGRAM").unwrap_or_else(|_| {
        panic!(
            "WQL_PROGRAM environment variable not set.\n\
             Usage: WQL_PROGRAM=path/to/program.wqlbc cargo wasm"
        )
    });

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dst = out_dir.join("program.wqlbc");
    fs::copy(&src, &dst).unwrap_or_else(|e| {
        panic!("failed to copy WQL_PROGRAM '{src}' to '{dst:?}': {e}")
    });
}
