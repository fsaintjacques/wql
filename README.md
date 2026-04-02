# WQL — Wire Query Language

Filter and project protobuf messages directly on the wire encoding, in a single forward pass, with zero deserialization.

## What is WQL?

WQL compiles a small query language into bytecode for the Wire Virtual Machine (WVM). The WVM scans protobuf wire bytes and can:

- **Filter** — evaluate a predicate (like `grep` for protobuf streams)
- **Project** — select/reshape fields (like `cut`/`jq` for protobuf)
- **Both at once** — filter + project in a single pass

All operations run in O(n) time over the input bytes with no heap allocation on the hot path.

## Install

```bash
cargo install --path crates/wqlc
```

## Quick Start

```bash
# Filter: keep messages where age > 18
wqlc eval -q 'age > 18' -s schema.bin -m pkg.Person < messages.bin

# Project: extract only name and age, output as JSON
wqlc eval -q '{ name, age }' -s schema.bin -m pkg.Person --json < message.bin

# Combined: filter + project in one pass, stream mode (JSONL)
wqlc eval -q 'WHERE age > 18 SELECT { name, address { city } }' \
  -s schema.bin -m pkg.Person --json --delimited < stream.bin

# Schema-free mode: reference fields by number
wqlc eval -q '{ #1, #3 }' < message.bin

# Compile to bytecode for embedding in other runtimes
wqlc compile -q '{ name, age }' -s schema.bin -m pkg.Person -o program.wqlbc
```

## CLI Usage

```
wqlc <command> [options]

Commands:
  compile   Compile a WQL query to bytecode
  eval      Compile and execute a WQL query on stdin
  inspect   Disassemble a compiled WQL program
```

### Common Options

| Flag | Description |
|---|---|
| `-q <query>` | WQL query string (required) |
| `-s <schema.bin>` | `FileDescriptorSet` for schema-bound mode |
| `-m <message>` | Root message type (required with `-s`) |
| `-o <output>` | Output file (compile only; default: stdout) |
| `--delimited` | Varint length-delimited stream mode (eval only) |
| `--json` | Output as JSON (eval only; requires `-s` and `-m`) |

### Modes

**Single message** (default): reads one protobuf message from stdin. Filter exit code: 0 = pass, 1 = filtered out. Projections write the result to stdout.

**Delimited stream** (`--delimited`): reads/writes varint length-prefixed records. Filters pass through matching records (grep semantics). Projections emit one output record per input record.

## Query Language

### Projections

```
{ name, age }                    # strict: keep only these fields
{ name, address { city }, .. }   # copy mode: keep all, reshape address
{ .. -payload -thumbnail }       # copy mode: drop specific fields
{ ..name }                       # deep search: find 'name' at any depth
```

### Predicates

```
age > 18
name == "Alice"
status IN (1, 2, 3)
address.city == "NYC" && age >= 21
name STARTS_WITH "A"
!active
```

**Operators:** `==`, `!=`, `<`, `<=`, `>`, `>=`, `IN`, `STARTS_WITH`, `ENDS_WITH`, `CONTAINS`, `MATCHES` (regex), `&&`, `||`, `!`

### Combined

```
WHERE age > 18 SELECT { name, address { city } }
```

## Library API

### Runtime (no_std)

```rust
use wql_runtime::LoadedProgram;

let program = LoadedProgram::from_bytes(&bytecode)?;
let mut output = vec![0u8; input.len() * 2];

let result = program.eval(&input, &mut output)?;
// result.matched  — true if the record passed the predicate (always true when no predicate)
// result.output_len — bytes written to output (0 when no projection)

if result.matched && result.output_len > 0 {
    let projected = &output[..result.output_len];
}
```

The program header determines what happens — callers don't need to know whether the query is a filter, projection, or both. Pass `&mut []` when you only care about filtering.

### Compiler (std)

```rust
use wql_compiler::{compile, CompileOptions};

let opts = CompileOptions {
    schema: Some(&schema_bytes),
    root_message: Some("pkg.Person"),
    ..Default::default()
};
let bytecode = compile("{ name, age }", &opts)?;
```

## Architecture

WQL is split into independent crates with a strict dependency graph:

```
wql-ir  (no_std + alloc)         — shared IR types + bytecode codec
  ├──▶ wql-runtime  (no_std)     — interpreter (LoadedProgram::eval)
  ├──▶ wql-compiler (std)        — parser → binder → emitter → linker
  ├──▶ wql-capi     (std, cdylib)— C FFI layer
  └──▶ wqlc         (std, bin)   — CLI tool
```

The compiler and runtime are fully independent: the compiler produces bytecode bytes, the runtime consumes them. Neither depends on the other.

### WVM Instruction Set

The WVM has 19 instructions organized around a single looping construct (`DISPATCH`) that iterates over protobuf `(tag, value)` pairs:

| Category | Instructions |
|---|---|
| Control | `DISPATCH`, `LABEL`, `RETURN` |
| Scope | `FRAME` (enter sub-message via arm action) |
| Predicate: int | `CMP_EQ`, `CMP_NEQ`, `CMP_LT`, `CMP_LTE`, `CMP_GT`, `CMP_GTE` |
| Predicate: bytes | `CMP_LEN_EQ`, `BYTES_STARTS`, `BYTES_ENDS`, `BYTES_CONTAINS`, `BYTES_MATCHES` |
| Predicate: set | `IN_SET`, `IS_SET` |
| Logic | `AND`, `OR`, `NOT` |

Field actions (`COPY`, `SKIP`, `DECODE`, `FRAME`) exist as arm actions within `DISPATCH`, not as standalone instructions.

See [doc/IR.md](doc/IR.md) for the full specification and [doc/ARCHITECTURE.md](doc/ARCHITECTURE.md) for crate design details.

## C FFI

The `wql-capi` crate produces `libwql` with a stable C ABI. The workflow is: compile query to bytecode, load bytecode into a program handle, evaluate against input bytes.

```c
// Compile (schema-free)
wql_bytes_t wql_compile(const char* query, char** errmsg);

// Compile (with schema)
wql_bytes_t wql_compile_with_schema(
    const char* query,
    const uint8_t* schema, size_t schema_len,
    const char* root_message,
    char** errmsg);

// Load bytecode into a reusable program handle
wql_program_t* wql_program_load(const uint8_t* bytecode, size_t len, char** errmsg);

// Evaluate — single entry point for filter, project, or both
typedef struct {
    uintptr_t output_len;  // bytes written (0 when no projection)
    bool      matched;     // predicate result (true when no predicate)
} wql_eval_result_t;

int32_t wql_eval(const wql_program_t* prog,
                 const uint8_t* input, size_t input_len,
                 uint8_t* output, size_t output_len,
                 wql_eval_result_t* result,
                 char** errmsg);                  // 0=ok, -1=error

// Cleanup
void wql_program_free(wql_program_t* prog);
void wql_bytes_free(wql_bytes_t bytes);
void wql_errmsg_free(char* msg);
```

Thread-safe: `wql_eval` takes `const wql_program_t*` and can be called concurrently. For filter-only programs, pass `output=NULL` / `output_len=0`.

## License

See [LICENSE](LICENSE).
