//! Compiles and runs tests/smoke.c against libwql_capi to verify the C API.

use std::process::Command;

#[test]
fn c_smoke_test() {
    // Build the static library in debug mode
    let status = Command::new("cargo")
        .args(["build", "-p", "wql-capi"])
        .status()
        .expect("cargo build failed");
    assert!(status.success(), "cargo build -p wql-capi failed");

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let workspace_root = std::path::Path::new(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let target_dir = workspace_root.join("target/debug");
    let c_source = format!("{manifest_dir}/tests/smoke.c");
    let binary = target_dir.join("wql_c_smoke");

    // Compile C test against the static library
    let mut cc_args = vec![
        c_source.clone(),
        "-o".into(),
        binary.to_str().unwrap().into(),
        "-I".into(),
        format!("{manifest_dir}/include"),
        "-L".into(),
        target_dir.to_str().unwrap().into(),
        "-lwql_capi".into(),
        "-lm".into(),
    ];

    // Platform-specific linker flags
    if cfg!(target_os = "macos") {
        cc_args.extend([
            "-framework".into(),
            "Security".into(),
            "-framework".into(),
            "CoreFoundation".into(),
        ]);
    } else if cfg!(target_os = "linux") {
        cc_args.extend(["-lpthread".into(), "-ldl".into()]);
    }

    let compile = Command::new("cc")
        .args(&cc_args)
        .output()
        .expect("cc failed to start");

    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        panic!("C compilation failed:\n{stderr}");
    }

    // Run the test
    let run = Command::new(&binary)
        .env("DYLD_LIBRARY_PATH", target_dir.to_str().unwrap())
        .output()
        .expect("failed to run C smoke test");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    if !run.status.success() {
        panic!(
            "C smoke test failed (exit {}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            run.status
        );
    }

    print!("{stdout}");
}
