# WQL — Architecture Document

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Date** | 2026-03-29 |

---

## 1. Design Goals

- **Single interpreter, multiple distributions.** The WVM interpreter is written once in Rust. Go and JVM runtimes link against the same native binary via C FFI; there is no per-language reimplementation.
- **No allocation on the hot path.** The register file and frame stack are stack-allocated. The output buffer is caller-provided. No heap allocation occurs per field or per message during scanning.
- **`wql-runtime` is `#![no_std]` + `alloc`.** This keeps the WASM program binary small and eliminates hidden OS dependencies from the interpreter.
- **Compiler and runtime are independent crates.** The compiler can be embedded in build tools and schema registries without pulling in the interpreter. The runtime can be embedded in constrained environments without pulling in the compiler.
- **WASM is a program deployment target, not an interpreter distribution mechanism.** A compiled WASM module contains the interpreter and the WVM bytecode for one program. The interpreter is not hosted inside a WASM runtime on the Go or JVM side.

---

## 2. Repository Layout

```
wql/
├── Cargo.toml                   # workspace root
├── crates/
│   ├── wql-ir/                  # shared IR types (no_std + alloc)
│   ├── wql-runtime/             # interpreter (no_std + alloc)
│   ├── wql-compiler/            # parser, type checker, IR emitter (std)
│   ├── wql-capi/                # C FFI → libwql (std, cdylib)
│   └── wqlc/                    # CLI: compile, eval, inspect
├── bindings/
│   └── include/                 # C header (stable ABI)
└── doc/
```

**Planned but not yet implemented:**
- `crates/wql-wasm/` — WASM program shell (no_std, wasm32)
- `bindings/go/` — CGO wrapper + Go API
- `bindings/java/` — JNI wrapper + Java API

---

## 3. Crate Dependency Graph

```
wql-ir  (no_std + alloc)
    │
    ├──▶ wql-runtime  (no_std + alloc)
    │
    ├──▶ wql-compiler (std)
    │         │
    │         └── prost-types (FileDescriptorSet for schema binding)
    │
    ├──▶ wql-capi     (std, cdylib)   depends on: wql-runtime + wql-compiler
    └──▶ wqlc         (std, binary)   depends on: wql-compiler + wql-runtime + wql-ir
```

`wql-runtime` does not depend on `wql-compiler`. The runtime is a pure consumer of bytecode bytes. The compiler is a pure producer. Neither knows about the other.

---

## 4. Crates

### 4.1 `wql-ir` — Shared IR types

`#![no_std]` + `extern crate alloc`. Both the compiler and the runtime need these types, and the runtime must be `no_std`. A shared crate with the `no_std` constraint is the only layout that avoids a layering inversion.

**Key types:**

- `Instruction` — structured enum with one variant per opcode; carries typed operands. Used by the compiler to build programs; not used directly by the runtime (which decodes from bytes).
- `Program<'a>` — a validated view over `&'a [u8]`. Construction is via `Program::decode`. Used by `wql-ir` for zero-copy access to raw bytecode.
- `ProgramHeader` — fixed 14-byte prefix of every encoded program:

  ```
  magic:          [u8; 4]  = b"WQL\x00"
  version:        u16 LE   — bytecode format version; incremented on encoding changes
  register_count: u8       — max register index + 1; runtime validates before executing
  max_frame_depth: u8      — max FRAME nesting depth; runtime validates against MAX_DEPTH
  flags:          u16      — bit 0: uses_regex (runtime rejects if built without `regex`)
  bytecode_len:   u32 LE
  ```

- `RegisterValue` — `Copy` tagged union for decoded scalar values: `I64(i64)`, `U64(u64)`, `F32(f32)`, `F64(f64)`, `Bytes { ptr, len }` (borrows from input, zero-copy), `Unset`.
- `DecodeError`, `EncodeError` — `no_std`-compatible error enums.

**Methods:**

- `Program::decode(bytes: &[u8]) -> Result<Program<'_>, DecodeError>` — validates magic, version, and structural integrity. Does not fully validate bytecode. Available always.
- `Program::encode(instrs: &[Instruction]) -> Vec<u8>` — available only under `feature = "alloc"`. Runs a two-pass linker to resolve forward label references to relative byte offsets.

**Bytecode operand encoding:**
- Field numbers: unsigned LEB128
- Register indices: single `u8`
- Integer immediates: signed LEB128 (zigzag)
- Bytes/string immediates: LEB128 length prefix + raw bytes
- Label references: unsigned LEB128 byte offset from the start of the bytecode. Forward references point to a later LABEL.

---

### 4.2 `wql-runtime` — Interpreter

`#![no_std]` + `extern crate alloc`. No panics in release builds on the hot path. No heap allocation per message.

**Public API:**

```rust
pub struct EvalResult {
    pub output_len: usize,  // bytes written (0 when no projection)
    pub matched: bool,       // predicate result (true when no predicate)
}

impl LoadedProgram {
    pub fn eval(&self, input: &[u8], output: &mut [u8]) -> Result<EvalResult, RuntimeError>;
}
```

A single entry point replaces the former `filter`/`project`/`project_and_filter` functions. The program header determines what happens — callers don't dispatch manually. Filter-only callers pass `&mut []`; the `depth == 0` path runs with zero allocation. Programs with `max_frame_depth > 0` and an undersized buffer allocate scratch internally (projected output is discarded).

Internally, `eval` delegates to:

```rust
fn execute(&mut self, pc: usize, input: &[u8], output: &mut [u8], out_cursor: usize)
    -> Result<(bool, usize), RuntimeError>
```

The output buffer is written during the scan unconditionally. If `matched` is false, the buffer contents are undefined; the caller must not read them.

**Internal types:**

- `Vm` — holds references to `program`, `input`, `output`; owns a stack-allocated register file `[RegisterValue; 16]` and frame stack `[Frame; MAX_DEPTH]` where `MAX_DEPTH = 32`.
- `Frame` — `{ scan_start: usize, scan_end: usize, out_start: usize }`. Tracks the scan window and output cursor offset at FRAME entry. On FRAME exit, the sub-buffer `output[out_start..out_cursor]` is reframed: the length varint is prepended in-place and the result appended to the parent buffer.
- Wire scanner module — reads `(field_num: u32, wire_type: WireType)` tag varints and provides wire-type-specific skip logic. This is the only place raw varint decoding occurs.

**Execution model:**
1. `Vm::new` validates `program.header().register_count ≤ 16` and `program.header().max_frame_depth ≤ MAX_DEPTH`. Returns `Err` if violated.
2. `Vm::execute` runs the bytecode byte stream from offset 0. The opcode dispatch is a `match` on the current byte; operands are read inline from the stream.
3. `DISPATCH` is the only loop: iterate `(field_num, wire_type)` pairs, binary-search the arm list for a match, execute actions, else apply the default action.
4. `FRAME` pushes a new `Frame`, recurses into the sub-slice, then reframes the sub-output on exit.
5. `RETURN` reads the top of the bool stack (or `true` if empty), reads the output cursor, returns `(bool, usize)`.

**Feature flags:**
- `regex` (default: off) — enables `BYTES_MATCHES`. Uses a `no_std`-compatible regex engine (`regex-automata` with `alloc`-only features). When off, a program with `uses_regex` set in its header returns `Err(ExecuteError::UnsupportedInstruction)` at load time.
- `std` (default: off) — if enabled, re-exports `std::alloc` so callers in `std` environments do not need to provide an allocator.

---

### 4.3 `wql-compiler` — Parser, Type Checker, IR Emitter

`std` allowed. Runs at startup time or build time. Not on the message hot path.

**Public API:**

```rust
pub struct CompileOptions<'a> {
    /// Serialized `FileDescriptorSet` (full transitive closure, as produced by
    /// `protoc --include_imports` or `buf build`). None = schema-free mode;
    /// field references must use `#N` field-number syntax.
    pub schema: Option<&'a [u8]>,

    /// Fully-qualified root message type, e.g. `"acme.events.OrderPlaced"`.
    /// Required when `schema` is `Some`; ignored when `schema` is `None`.
    pub root_message: Option<&'a str>,

    /// Target bytecode format version. `None` = current (latest) version.
    /// Set to an older version to produce bytecode compatible with runtimes
    /// that have not yet been upgraded. The compiler returns
    /// `CompileError::UnsupportedTargetVersion` if the requested version
    /// cannot be targeted by this compiler.
    pub target_version: Option<u16>,
}

impl Default for CompileOptions<'_> {
    fn default() -> Self {
        Self { schema: None, root_message: None, target_version: None }
    }
}

fn compile(source: &str, options: &CompileOptions) -> Result<Vec<u8>, CompileError>
```

`CompileOptions::default()` is schema-free, targeting the current bytecode version. The return value is the encoded bytecode (`Vec<u8>`); callers construct a `Program<'_>` by calling `Program::decode` on it. This keeps the compiler output self-contained and serializable.

**Internal pipeline:**

```
source: &str
    │
    ▼
Parser  →  SyntaxTree (AST)
    │
    ▼
SchemaBinder(SyntaxTree, schema)  →  BoundAst
    │  (resolves field names to field numbers,
    │   validates literal types against field types)
    ▼
IrEmitter(BoundAst)  →  Vec<Instruction>  +  LabelMap
    │  (allocates registers greedily,
    │   emits DISPATCH/FRAME nesting,
    │   tracks max_frame_depth and register_count)
    ▼
Linker(Vec<Instruction>, LabelMap)  →  Vec<u8>
    │  (two-pass: collect label byte offsets,
    │   patch FRAME references,
    │   write ProgramHeader, serialize instructions)
    ▼
bytecode: Vec<u8>
```

**Register allocation:** greedy linear scan. Registers are assigned to `DECODE` instructions in walk order. Because registers are globally scoped across FRAME nesting (Invariant I-04 in the IR spec), the emitter counts all registers together across all FRAME scopes. If more than 16 are required simultaneously, `CompileError::TooManyRegisters` is returned.

**`CompileError`** is a `std` error enum with span information: `ParseError { span, message }`, `TypeError { field, expected, actual }`, `TooManyRegisters`, `UnsupportedConstruct { description }`.

**Feature flags:**
- `regex` (default: on) — enables compilation of `BYTES_MATCHES` instructions and sets `flags::uses_regex` in the program header. When off, `BYTES_MATCHES` in the source returns `CompileError::UnsupportedConstruct`.

---

### 4.4 `wql-capi` — C FFI Layer

`std` required (global allocator, `catch_unwind`). Produces a `cdylib`/`staticlib`: `libwql_capi.so` / `libwql_capi.dylib` / `libwql_capi.a`.

**Ownership model:**

`wql_program_t` is an opaque struct wrapping `wql_runtime::LoadedProgram`. It is heap-allocated via `Box::into_raw` and returned to C as an opaque pointer. `Box::from_raw` in `wql_program_free` reclaims it.

**C API:**

```c
typedef struct wql_program_t wql_program_t;

// Owned byte buffer returned by wql_compile*. Caller frees with wql_bytes_free.
typedef struct { uint8_t *data; size_t len; } wql_bytes_t;

// Result of wql_eval. Zero-initialize before calling.
typedef struct {
    uintptr_t output_len;   // bytes written (0 when no projection)
    bool      matched;      // predicate result (true when no predicate)
    uint8_t   _reserved[7];
} wql_eval_result_t;

// Compile (schema-free). Returns bytecode; data is NULL on error.
wql_bytes_t wql_compile(const char *query, char **errmsg);

// Compile (with schema).
wql_bytes_t wql_compile_with_schema(
    const char *query,
    const uint8_t *schema, size_t schema_len,
    const char *root_message,
    char **errmsg);

// Load bytecode into a reusable program handle.
wql_program_t *wql_program_load(const uint8_t *bytecode, size_t len, char **errmsg);
void wql_program_free(wql_program_t *prog);

// Query program metadata.
void wql_program_info(const wql_program_t *prog, wql_program_info_t *out);

// Evaluate — single entry point for filter, project, or both.
// Returns 0 on success, -1 on error. result must be non-NULL.
// For filter-only, pass output=NULL / output_len=0.
int32_t wql_eval(
    const wql_program_t *prog,
    const uint8_t *input, size_t input_len,
    uint8_t *output, size_t output_len,
    wql_eval_result_t *result,
    char **errmsg);

// Cleanup
void wql_bytes_free(wql_bytes_t bytes);
void wql_errmsg_free(char *msg);
```

**Error model:** functions that can fail accept an optional `char **errmsg` out parameter. On error, `*errmsg` is set to a heap-allocated string the caller must free with `wql_errmsg_free`. Passing `NULL` for `errmsg` is safe (error message is discarded).

**Thread safety:** `wql_program_t` is immutable after construction. `wql_eval` takes `const wql_program_t*` and can be called concurrently from multiple threads with the same handle.

**Panic safety:** all public functions are wrapped in `std::panic::catch_unwind`. A caught panic returns an error code and sets `*errmsg` to `"internal panic during ..."`.

The header file is maintained manually at `crates/wql-capi/include/wql.h`.

**Feature flags:**
- `regex` (default: on) — passed through to `wql-runtime` and `wql-compiler`.

---

### 4.5 `wql-wasm` — WASM Program Shell

`#![no_std]` + `extern crate alloc`. Target: `wasm32-unknown-unknown`. This crate is a template that produces a self-contained WASM module containing one compiled WQL program.

**What this is not:** this is not a WASM interpreter for the WVM. It is the WVM interpreter compiled to WASM, with one specific WQL program's bytecode embedded as a static byte array.

**Bytecode embedding:**

```rust
static BYTECODE: &[u8] = include_bytes!(env!("WQL_BYTECODE_PATH"));
```

`WQL_BYTECODE_PATH` is an environment variable set by the build tooling pointing to a `.wqlbc` file produced by `wqlc`.

**Program initialization:**

```rust
static PROGRAM: OnceLock<Program<'static>> = OnceLock::new();

fn program() -> &'static Program<'static> {
    PROGRAM.get_or_init(|| {
        Program::decode(BYTECODE).expect("embedded bytecode is valid")
    })
}
```

`OnceLock` (or an equivalent `no_std` atomic once-init) ensures the `Program` is constructed once and reused. `Program::decode` is O(1) (header check + slice construction), so first-call overhead is negligible.

**Static output buffer:**

```rust
const MAX_MESSAGE_SIZE: usize = 1024 * 1024;  // 1 MB, configurable at build time
static mut OUTPUT_BUF: [u8; MAX_MESSAGE_SIZE] = [0u8; MAX_MESSAGE_SIZE];
```

WASM linear memory is single-threaded per module instance; the broker calls one message at a time. The static buffer is safe.

**Exported WASM functions:**

```rust
#[no_mangle]
pub extern "C" fn wql_eval(
    input_ptr: i32, input_len: i32,
    output_ptr: i32, output_len: i32,
    result_ptr: i32) -> i32;  // 0=ok, -1=error
```

Input and output are pointers into WASM linear memory. The broker writes input bytes into memory before calling, and reads the result struct (output_len + matched) from memory after. A single exported function handles filter, project, or both.

**Feature flags:** `regex` is off by default to meet the <100 KB WASM binary size target.

**Build:**

```toml
# wql-wasm/Cargo.toml
[lib]
crate-type = ["cdylib"]

[profile.release]
opt-level = "s"
lto = "fat"
strip = true
```

---

## 5. Program Lifecycle

### 5.1 Compile path (Rust)

```
compile(source, schema) → Vec<u8>           (wql-compiler)
LoadedProgram::from_bytes(&bytes)           (wql-runtime)
program.eval(input, &mut output) → EvalResult  (wql-runtime)
```

The caller owns the `Vec<u8>` (or a `Box<[u8]>`, or a `&'static [u8]` for a compile-time embedded program). `LoadedProgram` decodes and owns the instructions for the duration of use.

### 5.2 Serialized bytecode path

The same `Vec<u8>` can be written to disk, stored in a database, or distributed over the network. Any runtime that receives the bytes constructs a `Program` via `Program::decode` and executes it. Bytecode produced on any platform executes identically on all runtimes.

### 5.3 Go via CGO

```
wql.Compile(source, schema)
  → cgo → wql_compile → wql_program_load → *wql_program_t on heap
  Go: *Program struct with runtime.SetFinalizer(p, (*Program).free)

program.Eval(input, output)
  → cgo → wql_eval(prog, input, len, output, len, &result, &err)
  → result.matched, result.output_len returned to Go
```

### 5.4 JVM via JNI

Same as Go. Key difference: use `ByteBuffer.allocateDirect` for the output buffer to avoid a Java-heap-to-native copy on the output side. `GetByteArrayElements` / `ReleaseByteArrayElements` pins the input `byte[]`.

### 5.5 WASM broker transform

```
Build time:
  wqlc compile program.wql → program.wqlbc
  WQL_BYTECODE_PATH=program.wqlbc cargo build \
    --target wasm32-unknown-unknown -p wql-wasm \
    --release
  → program.wasm  (~<100 KB)

Deploy:
  operator uploads program.wasm to broker topic transform slot

Per message (inside broker WASM sandbox):
  broker writes input bytes into WASM linear memory
  broker calls wql_eval(input_ptr, input_len, output_ptr, output_len, result_ptr)
    → PROGRAM.get_or_init(decode BYTECODE)
    → program.eval(input_slice, output_slice) → EvalResult
    → write result to result_ptr, return 0
  broker reads result.matched and output[..result.output_len]
```

---

## 6. Go Binding (`bindings/go/`)

```
bindings/go/
├── wql.go          # CGO shim + Go API
└── wql_test.go     # integration tests
```

**Go API:**

```go
type Program struct { ptr *C.wql_program_t }

type EvalResult struct {
    OutputLen int
    Matched   bool
}

type CompileOptions struct {
    Schema      []byte // serialized FileDescriptorSet; nil = schema-free
    RootMessage string // fully-qualified message name; required when Schema is set
}

func Compile(source string, opts *CompileOptions) (*Program, error)
func LoadBytecode(bytecode []byte) (*Program, error)
func (*Program) Eval(input, output []byte) (EvalResult, error)
func (*Program) Close()
```

`runtime.SetFinalizer` calls `wql_program_free` when the GC collects the `Program`. Callers should still call `Close()` explicitly to release native memory promptly.

---

## 7. Java Binding (`bindings/java/`)

```
bindings/java/
├── WqlProgram.java     # AutoCloseable wrapper around native handle
├── WqlException.java
└── WqlNative.java      # static native JNI declarations
```

JNI glue is written in Rust using the `jni` crate, living in a `wql-jni` crate (separate from `wql-capi` because JNI export names encode the Java package). `wql-jni` depends on `wql-capi`.

**Java API:**

```java
public class CompileOptions {
    public byte[] schema;        // serialized FileDescriptorSet; null = schema-free
    public String rootMessage;   // required when schema is non-null
}

public class EvalResult {
    public int     outputLen;    // bytes written (0 when no projection)
    public boolean matched;      // predicate result (true when no predicate)
}

public class WqlProgram implements AutoCloseable {
    public static WqlProgram compile(String source, CompileOptions opts) throws WqlException;
    public static WqlProgram loadBytecode(byte[] bytecode) throws WqlException;

    public EvalResult eval(byte[] input, ByteBuffer output) throws WqlException;

    @Override public void close();
}
```

Use `ByteBuffer.allocateDirect` for `output` to avoid a heap copy on the output path. Input `byte[]` is pinned via `GetByteArrayElements`.

---

## 8. Feature Flags Summary

| Crate | Flag | Default | Effect |
|---|---|---|---|
| `wql-ir` | `alloc` | on | Enables `Program::encode` (requires heap) |
| `wql-ir` | `serde` | off | Derives `Serialize`/`Deserialize` on IR types |
| `wql-runtime` | `regex` | off | Enables `BYTES_MATCHES` execution |
| `wql-runtime` | `std` | off | Re-exports `std::alloc` for `std` callers |
| `wql-compiler` | `regex` | on | Enables compilation of `BYTES_MATCHES` |
| `wql-capi` | `regex` | on | Passes through to runtime + compiler |
| `wql-wasm` | `regex` | off | Keep binary under 100 KB |

When the compiler emits a `BYTES_MATCHES` instruction it sets `flags::uses_regex` in the program header. If `wql-runtime` was built without `regex`, it detects this bit at program load time and returns `Err(ExecuteError::UnsupportedInstruction)` before any bytes are scanned.

---

## 9. Open Questions

| # | Question |
|---|---|
| OQ-A-01 | **`wql-jni` scope.** Separate crate or a feature flag on `wql-capi`? A separate crate is cleaner (JNI export names encode the Java package, which is application-level), but adds a crate to the workspace. Recommendation: separate `wql-jni` crate that depends on `wql-capi`. |
| OQ-A-02 | **`wqlc` CLI scope for v1.** The PRD defers a standalone CLI, but the WASM build path requires a way to compile `.wql` to `.wqlbc`. A minimal `wqlc compile --output program.wqlbc program.wql` is needed to close the WASM build loop. Recommend shipping a narrow `wqlc` in v1 scoped to bytecode-only output. |
| OQ-A-03 | **WASM `MAX_MESSAGE_SIZE`.** The static output buffer in `wql-wasm` must be sized at compile time. 1 MB is a reasonable default. Should this be a build-time constant via an env var, or should the broker's WASM host provide a larger shared memory region? Depends on the broker platform. |
| OQ-A-04 | **Bytecode version compatibility policy.** The `version: u16` in `ProgramHeader` is a bytecode format version. The policy — which version changes are breaking, whether older runtimes must reject newer bytecode or attempt forward compatibility — needs to be defined before the format is stabilised. |
| OQ-A-05 | **Schema distribution.** `wql_compile` in the C API accepts a serialized `FileDescriptorSet`. How does the caller obtain it? Options: bundle the `.proto` files and run `protoc` at build time, use a schema registry API, or embed the descriptor in the service binary. This is an integration concern, not a WQL concern, but should be documented. |
