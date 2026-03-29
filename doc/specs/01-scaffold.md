# Block 1 — Workspace & Crate Scaffold

| | |
|---|---|
| **Status** | Draft |
| **Date** | 2026-03-29 |
| **Depends on** | — |

---

## Goal

Create the Cargo workspace with all five crates declared, correctly configured, and compiling. No logic beyond what is needed to satisfy each crate's declared constraints (`no_std`, feature flags, dependency edges). The result is a green CI baseline from which all subsequent blocks build.

---

## Deliverables

- All files listed in the file tree below exist and compile cleanly.
- `cargo build --workspace` passes on the host target.
- `cargo build -p wql-runtime --target wasm32-unknown-unknown` passes (validates `no_std` + `alloc` with no std leakage).
- `cargo build -p wql-wasm --target wasm32-unknown-unknown` passes.
- `cargo clippy --workspace` passes with zero warnings.
- `cargo fmt --check` passes.

---

## File Tree

```
wql/
├── Cargo.toml                          # workspace root
├── Cargo.lock
├── .cargo/
│   └── config.toml                     # target aliases, build config
├── crates/
│   ├── wql-ir/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── wql-runtime/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── wql-compiler/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── wql-capi/
│   │   ├── Cargo.toml
│   │   ├── build.rs
│   │   └── src/
│   │       └── lib.rs
│   └── wql-wasm/
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs
├── bindings/
│   └── include/                        # empty; wql.h generated in Block 8
└── tests/
    └── integration/                    # empty; populated in Block 6
```

---

## `Cargo.toml` (workspace root)

```toml
[workspace]
resolver = "2"
members = [
    "crates/wql-ir",
    "crates/wql-runtime",
    "crates/wql-compiler",
    "crates/wql-capi",
    "crates/wql-wasm",
]

[workspace.dependencies]
wql-ir      = { path = "crates/wql-ir" }
wql-runtime = { path = "crates/wql-runtime" }
wql-compiler = { path = "crates/wql-compiler" }

[workspace.lints.rust]
unsafe_code = "warn"

[workspace.lints.clippy]
all  = "warn"
pedantic = "warn"

# WASM-optimised release profile
[profile.release-wasm]
inherits  = "release"
opt-level = "s"
lto       = "fat"
strip     = true
codegen-units = 1
panic     = "abort"
```

---

## `.cargo/config.toml`

```toml
# Convenience alias: `cargo wasm` builds the wql-wasm crate for wasm32
[alias]
wasm = "build -p wql-wasm --target wasm32-unknown-unknown --profile release-wasm"
```

---

## `crates/wql-ir`

### `Cargo.toml`

```toml
[package]
name    = "wql-ir"
version = "0.1.0"
edition = "2021"

[features]
default = ["alloc"]
alloc   = []          # enables encode (Vec<u8> output); always on except in
                      # hypothetical bare-metal embeddings
serde   = ["dep:serde"]

[dependencies]
serde = { version = "1", default-features = false, features = ["derive"], optional = true }

[lints]
workspace = true
```

### `src/lib.rs`

```rust
#![no_std]
#![cfg_attr(feature = "alloc", allow(unused_imports))]

#[cfg(feature = "alloc")]
extern crate alloc;

// Types defined in subsequent blocks.
```

---

## `crates/wql-runtime`

### `Cargo.toml`

```toml
[package]
name    = "wql-runtime"
version = "0.1.0"
edition = "2021"

[features]
default = []
std     = []      # re-export std::alloc; simplifies use from std contexts
regex   = []      # enables BYTES_MATCHES; pulls in a no_std regex engine (Block 3)

[dependencies]
wql-ir = { workspace = true }

[lints]
workspace = true
```

### `src/lib.rs`

```rust
#![no_std]

extern crate alloc;

// Public API defined in Block 3.
```

---

## `crates/wql-compiler`

### `Cargo.toml`

```toml
[package]
name    = "wql-compiler"
version = "0.1.0"
edition = "2021"

[features]
default = ["regex"]
regex   = []      # enables compilation of BYTES_MATCHES instructions

[dependencies]
wql-ir      = { workspace = true }
prost-types = "0.13"    # FileDescriptorSet for schema binding

[lints]
workspace = true
```

### `src/lib.rs`

```rust
// std is allowed in the compiler.

// Public API defined in Blocks 4–5.
```

---

## `crates/wql-capi`

The C FFI crate overrides the workspace `unsafe_code` lint — FFI requires unsafe.

### `Cargo.toml`

```toml
[package]
name    = "wql-capi"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "staticlib"]

[features]
default = ["regex"]
regex   = [
    "wql-runtime/regex",
    "wql-compiler/regex",
]

[dependencies]
wql-runtime  = { workspace = true }
wql-compiler = { workspace = true }

[build-dependencies]
cbindgen = "0.27"

[lints]
workspace = true

[lints.rust]
unsafe_code = "allow"
```

### `build.rs`

```rust
fn main() {
    // cbindgen header generation added in Block 8.
    // Placeholder to establish the build script.
    println!("cargo:rerun-if-changed=src/lib.rs");
}
```

### `src/lib.rs`

```rust
// C FFI layer defined in Block 8.
```

---

## `crates/wql-wasm`

### `Cargo.toml`

```toml
[package]
name    = "wql-wasm"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[features]
default = []
regex   = ["wql-runtime/regex"]   # off by default — binary size

[dependencies]
wql-runtime = { workspace = true }

[lints]
workspace = true
```

### `src/lib.rs`

```rust
#![no_std]

extern crate alloc;

// WASM program shell defined in Block 7.
```

---

## Verification

All of the following must pass before this block is considered complete.

```sh
# Host build — all crates
cargo build --workspace

# Host tests — nothing to test yet, but must compile
cargo test --workspace

# no_std validation — wasm32 has no std by default
cargo build -p wql-ir      --target wasm32-unknown-unknown
cargo build -p wql-runtime --target wasm32-unknown-unknown
cargo build -p wql-wasm    --target wasm32-unknown-unknown

# Lints
cargo clippy --workspace
cargo clippy -p wql-ir      --target wasm32-unknown-unknown
cargo clippy -p wql-runtime --target wasm32-unknown-unknown

# Formatting
cargo fmt --check
```

The `wasm32-unknown-unknown` target must be installed:

```sh
rustup target add wasm32-unknown-unknown
```

---

## Constraints & Notes

- `wql-compiler` and `wql-capi` are **not** built for `wasm32-unknown-unknown`. Only `wql-ir`, `wql-runtime`, and `wql-wasm` are validated against that target.
- The workspace `unsafe_code = "warn"` lint applies to all crates. `wql-capi` explicitly overrides it to `"allow"`. No other crate should require unsafe in this block or beyond.
- `prost-types` version should be pinned to whatever version is in use in the broader protobuf toolchain. Check before locking.
- The `release-wasm` profile is defined now so it can be used in WASM builds from Block 7 onward without a profile migration.
- `[lib] crate-type` for `wql-capi` declares both `cdylib` (for dynamic linking from Go/Java) and `staticlib` (for static linking scenarios). Both are generated from the same source.
