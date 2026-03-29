# WQL — Wire Query Language
## Product Requirements Document

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.4 |
| **Date** | 2026-03-29 |
| **Authors** | Platform Engineering |

---

## 1. Overview

WQL (Wire Query Language) is a language and runtime toolkit for evaluating predicates and applying projections directly on protobuf wire-format bytes, without deserializing the message into a language struct or object.

The goal is to make WQL programs evaluable conveniently in any language or runtime — Rust, Go, JVM, and WebAssembly — from a single compiled representation, without re-implementing semantics per platform. This is achieved through a single authoritative interpreter written in Rust and distributed to other runtimes via a stable C FFI boundary. No per-platform interpreter rewrite is required; each non-Rust runtime embeds the same native library.

Two operations are supported, both expressed in WQL source and compiled through a shared IR:

- **Predicate filtering** — evaluate a boolean expression over a raw message; skip the message entirely if the predicate is false, avoiding any downstream deserialization cost.
- **Projection** — rewrite the wire bytes to include only a declared subset of fields, reducing the payload that downstream consumers must parse.
- **Combined** — evaluate the predicate and apply the projection in a single forward scan; the output is only committed if the predicate is true.

---

## 2. Problem Statement

Any system that processes protobuf messages at scale faces the same three compounding costs, regardless of whether the runtime is a message broker, a stream processor, or a microservice:

| Cost | Description |
|---|---|
| Unnecessary CPU | Every consumer pays full deserialization cost per message, regardless of whether it uses the message. |
| Unnecessary I/O | Full wire payloads are transferred to consumers that need only 2–3 fields from a 40-field message. |
| Schema coupling | Consumers must import and maintain proto schemas for messages they only partially care about. |

These costs are typically addressed at the application layer — each team writes their own deserialization-and-discard logic. This means the cost is paid once per consumer, not once per pipeline. In a system with many consumers fanned off a single topic or stream, the same bytes are deserialized and filtered independently for each.

WQL moves filter-and-project work as early as possible in the data path — ideally before the bytes leave the broker, but also within stream processors and application-layer consumers — so the work is paid once and the savings compound across all downstream readers.

---

## 3. Goals and Non-Goals

### 3.1 Goals

- Operate on protobuf wire bytes with zero deserialization for pure field-copy projections.
- Expose a stable, platform-agnostic interface across all runtime targets.
- Support multiple deployment targets from a single WQL source expression: native Rust, native Go, JVM, and WebAssembly — without per-platform interpreter reimplementation.
- The interpreter is written once in Rust and distributed to Go and JVM runtimes as a native shared library via C FFI. WebAssembly is a compilation target for WQL *programs* (broker-side transforms), not a distribution mechanism for the interpreter.
- Design the compiler as an embeddable library — not just a CLI tool — so that Flink jobs, Go services, and broker plugins can all compile WQL expressions in-process using the same compilation path.
- Support schema-bound (named) and schema-free (`#N` field number) field references.
- Projection must preserve unknown fields by default to be safe under schema evolution.
- Support ergonomic deep field exclusion using the `..` recursive operator combined with `-field` exclusion syntax (e.g. `{ .. -payload }` strips a field at any nesting depth without enumerating the schema).
- Predicate subset scoped to proto-validate-tier expressiveness: scalar comparisons, field presence, string predicates, logical operators, one level of nested field access.
- A single scan pass over the wire bytes must be sufficient to evaluate both a predicate and apply a projection.

### 3.2 Non-Goals (v1)

- Full CEL evaluation semantics — comprehensions, cross-field arithmetic, and map-key filtering are deferred to v2.
- Message mutation (rewriting field values). Projection copies or drops fields verbatim; it does not transform their content.
- Proto2 extensions and groups.
- Aggregation across a stream of messages (`COUNT`, `SUM`, etc.).
- A production-quality JIT compiler. The WVM interpreter is the v1 target; Cranelift/LLVM codegen is a v2 concern.
- Runtime compilation / hot-reload of WQL expressions (startup-time compilation is in scope; live swap and UDF registration are v2).

---

## 4. Canonical Function Interface

The three functions below are the stable public contract of WQL across all platforms. Platform-specific naming conventions apply (e.g. `Filter([]byte) bool` in Go, `fn filter(input: &[u8]) -> bool` in Rust, `boolean filter(byte[] input)` in Java), but the semantics are identical.

All three public functions are thin wrappers over a single internal execution model: `execute(program, input, output) -> (bool, usize)`. The WVM interpreter always performs one forward scan, always writes to the output buffer during that scan, and always returns both a predicate result and a byte count.

```rust
/// Evaluates a WQL predicate over raw proto wire bytes.
/// Returns true if the message matches; false if it should be discarded.
/// No deserialization occurs for pure scalar predicates.
/// Pure filter programs never write to the output buffer.
fn filter(program: &Program, input: &[u8]) -> bool;

/// Applies a WQL projection to raw proto wire bytes.
/// Writes the projected wire bytes into the caller-provided output buffer.
/// Returns the number of bytes written. The output is always a valid proto3 encoding.
/// The caller must provide an output buffer of at least input.len() bytes; projection
/// output is guaranteed never to exceed input size (fields are only dropped, never added
/// or expanded).
fn project(program: &Program, input: &[u8], output: &mut [u8]) -> usize;

/// Evaluates the predicate and applies the projection in a single forward scan.
/// The output buffer is written during the scan regardless of the predicate result.
/// Returns Some(n) if the predicate is true: output[..n] contains the projected bytes.
/// Returns None if the predicate is false: output contents are undefined and must not be read.
/// This is the preferred form for latency-critical pipelines.
fn project_and_filter(program: &Program, input: &[u8], output: &mut [u8]) -> Option<usize>;
```

These signatures carry no schema, no reflection, and no allocations. The output buffer is caller-provided; the caller is responsible for sizing it to at least `input.len()`. A caller embeds a compiled WQL program into their binary and calls one of these functions per message. The program is opaque bytecode; the caller does not need to understand WQL or the WVM to use it.

The equal-length output bound holds for all v1 programs. It breaks if field-value rewriting is introduced (v2+); that extension would require a separate, explicitly sized API.

---

## 5. Use Cases

### 5.1 Flink / JVM stream processor

A WQL predicate or projection is compiled to a JVM-native runtime library and embedded in a Flink `ProcessFunction` or Kafka Streams `Processor`. The function receives raw `byte[]` from the Kafka consumer record before any deserialization and calls `filter` or `projectAndFilter` directly.

```java
// Flink ProcessFunction — no proto schema import needed on the hot path
public void processElement(byte[] record, Context ctx, Collector<byte[]> out) {
    int n = wql.projectAndFilter(record, outputBuffer);
    if (n >= 0) {
        out.collect(Arrays.copyOf(outputBuffer, n));
    }
}
```

This is the highest-priority deployment scenario. Flink jobs processing high-volume topics pay full deserialization cost today even when discarding 90% of messages.

### 5.2 Go service consumer

A Go service consuming from Kafka or Redpanda embeds a compiled WQL program as a Go library. The `filter` function is called on the raw `[]byte` from the consumer before passing to any unmarshalling code.

```go
// Inline filter before proto.Unmarshal — no schema needed
for msg := range consumer.Messages() {
    n := wql.ProjectAndFilter(msg.Value, outputBuf)
    if n < 0 {
        continue
    }
    var event MyEvent
    proto.Unmarshal(outputBuf[:n], &event)
    // event only has fields the consumer declared it needs
}
```

### 5.3 Rust consumer / sidecar

A Rust service or sidecar proxy embeds the WQL runtime as a crate. Because the WVM runtime is written in Rust, this is a zero-FFI path — the compiled program runs directly as native code with no overhead beyond the scan itself.

```rust
if let Some(n) = wql.project_and_filter(&raw_bytes, &mut output_buf) {
    // output_buf[..n] contains the projected wire bytes
}
```

### 5.4 Broker-side WASM transform (Redpanda, WasmEdge)

A WQL *program* is compiled to a WASM module and attached to a broker topic as a transform. The broker calls the WASM function per message in-flight. This offloads filter-and-project work entirely from consumers — downstream subscribers receive pre-filtered, pre-projected payloads.

```
-- WQL source compiled to WASM module (the program, not the interpreter)
WHERE age > 18 AND address.city == "NYC"
SELECT { name, address { city }, .. -embedding }
```

This is not the first deployment target, but it is the highest-leverage one: cost is paid once at the broker, savings multiply across all consumers.

### 5.5 Application-layer field stripping

Even without broker integration, a consumer can use WQL to strip heavy fields (embeddings, blobs, audit payloads) from messages before passing them to business logic, without importing the full proto schema:

```go
// Schema-free: strip field #7 (embedding) anywhere in the message hierarchy
n := wql.Project(raw, outputBuf, `{ .. -#7 }`)
```

---

## 6. Toolchain Architecture

WQL is a toolkit, not a single binary. The components are designed to be embedded, extended, and replaced independently.

### 6.1 Component overview

```
WQL source expression
        │
        ▼
┌───────────────────┐
│   Parser / AST    │  Language-agnostic; consumes WQL surface syntax
└───────────────────┘
        │
        ▼
┌───────────────────┐
│  Type checker /   │  Optional: resolves field names to field numbers
│  Schema binder    │  using a bound .proto descriptor. Schema-free
└───────────────────┘  programs skip this step.
        │
        ▼
┌───────────────────┐
│  WVM IR emitter   │  Produces WVM bytecode: the portable, platform-
│  (compiler core)  │  agnostic representation of the program.
└───────────────────┘
        │
    ┌───┴────────────────────────────────────────┐
    │                                            │
    ▼                                            ▼
┌──────────────┐                      ┌──────────────────────┐
│ WVM bytecode │                      │  Native code backend │
│  (portable)  │                      │  (future: Cranelift) │
└──────────────┘                      └──────────────────────┘
    │
    ├──► Rust runtime (crate)         → zero-FFI, direct crate dependency
    ├──► C shared library             → Go (CGO), JVM (JNI / Panama)
    └──► WASM module (program only)   → Redpanda transforms, WasmEdge, browser
```

### 6.2 Interpreter: single implementation, multiple distributions

The WVM interpreter is written once in Rust and distributed in three forms:

| Distribution | Consumers | Notes |
|---|---|---|
| Rust crate (`wql-runtime`) | Rust services, sidecar proxies | Zero FFI overhead |
| C shared library (`libwql`) | Go (CGO), JVM (JNI / Panama) | Single native binary, all non-Rust runtimes |
| WASM module (program) | Redpanda, WasmEdge, browser | The compiled WQL *program* runs in WASM; the interpreter is not involved |

There is no separate Go or JVM interpreter implementation. Go and JVM runtimes link against `libwql`; they call the same machine code as the Rust crate. Behavioral correctness is guaranteed by construction.

**WASM is not used as an interpreter distribution mechanism.** Embedding a WASM runtime inside Go or JVM hosts to run the interpreter would add a WASM runtime dependency on the hot path, and available WASM runtimes on the JVM (chicory, GraalVM Truffle) cannot meet the throughput targets without GraalVM — which cannot be assumed for standard Flink deployments.

### 6.3 Runtime crate design constraints

The `wql-runtime` crate is `#![no_std]` with `extern crate alloc`. This constraint:

- Keeps the WASM program binary well under the 100 KB target (std adds significant WASM runtime glue).
- Ensures the shared library has minimal symbol surface and no hidden runtime initialization.
- Enforces that the hot path has no implicit I/O, threading, or OS dependencies.

The three canonical functions are allocation-free on the hot path. The output buffer is caller-provided; the caller sizes it to `input.len()`. No heap allocation occurs per field or per message during scanning. The register file (R0–R15) and frame stack are stack-allocated.

### 6.4 Compiler core

The parser, type checker, and WVM IR emitter are implemented as a library. Any host application — a Flink job builder, a Redpanda plugin, a service startup path — can embed the compiler and compile WQL expressions at startup or build time.

The compiler core (`wql-compiler`) may use `std`. It runs at startup time or build time, not on the message hot path, and correctness and ergonomics matter more than minimal dependencies there.

The compiler's public entry point must be I/O-free:

```rust
fn compile(source: &str, schema: Option<&FileDescriptorSet>) -> Result<Program, CompileError>;
```

This keeps the path to `no_std` + `alloc` for the compiler open for v2 (runtime UDF registration, in-process hot compilation), without requiring it now.

### 6.5 Compilation modes

| Mode | When | Output |
|---|---|---|
| AOT (ahead-of-time) | Build pipeline, CLI tool | Bytecode binary or native shared library |
| Startup-time | Service init, Flink job setup | In-process bytecode, no disk I/O |
| Dynamic (v2) | Admin API, UDF registration | Compile and swap program at runtime |

All modes use the same `compile()` entry point and produce the same WVM bytecode. The runtime does not distinguish between them.

---

## 7. Functional Requirements

### 7.1 Predicate language

| ID | Requirement |
|---|---|
| F-P-01 | Support field access by field number (`#N`) without a schema. |
| F-P-02 | Support field access by name when a `.proto` schema is bound. |
| F-P-03 | Support scalar comparisons: `==`, `!=`, `<`, `<=`, `>`, `>=` against integer, float, string, bool, and bytes literals. |
| F-P-04 | Support field presence check: `exists(field)` / `has(field)`. |
| F-P-05 | Support string predicates: `starts_with`, `ends_with`, `contains`, `matches` (RE2). |
| F-P-06 | Support set membership: `field in [v1, v2, ...]`. |
| F-P-07 | Support logical operators: `&&`, `\|\|`, `!`. |
| F-P-08 | Support one level of nested field access: `msg.address.city` / `#3.#1`. |
| F-P-09 | A missing field evaluates to its proto3 zero value unless the `exists()` operator is used. |

### 7.2 Projection language

| ID | Requirement |
|---|---|
| F-J-01 | Support flat field inclusion: `{ name, age }` — copy only the listed fields, drop all others. |
| F-J-02 | Support nested sub-message projection: `{ address { city } }`. |
| F-J-03 | Support unknown field preservation at each nesting level via `...` trailer: `{ name, ... }`. |
| F-J-04 | Default behavior (no `...` trailer) must drop unknown fields. |
| F-J-05 | Support deep copy-all via `..`: `{ .. }` recursively copies the entire message including all sub-messages, preserving wire bytes verbatim. |
| F-J-06 | Support deep field exclusion via `-field` suffix on `..`: `{ .. -payload }` copies the entire message except any field named `payload` at any nesting depth. Multiple exclusions are supported: `{ .. -payload -thumbnail }`. |
| F-J-07 | Support scoped deep exclusion: `{ departments { .. -payload } }` enters the `departments` repeated field, then strips `payload` at any depth inside each entry, forwarding everything else. |
| F-J-08 | Projection output must be a valid proto wire encoding with correct length prefixes at every nesting level. |
| F-J-09 | Repeated fields that are projected wholesale must be copied verbatim (no re-encoding of packed values). |

### 7.3 Runtime and interface requirements

| ID | Requirement |
|---|---|
| F-R-01 | WVM programs must execute in a single forward pass over the input wire bytes. |
| F-R-02 | No heap allocation per field or per message during scanning. The register file and frame stack are stack-allocated; the output buffer is caller-provided. |
| F-R-03 | The output buffer for `project` and `project_and_filter` is caller-provided and must be at least `input.len()` bytes. Projection output is guaranteed never to exceed input size. |
| F-R-04 | The three canonical functions (`filter`, `project`, `project_and_filter`) must be the sole public interface of every runtime distribution. Callers do not need to understand WQL, WVM, or protobuf wire format. |
| F-R-05 | The same WVM bytecode must be interpretable by all runtime distributions without modification. |
| F-R-06 | The `wql-runtime` crate must be `#![no_std]` + `extern crate alloc`. No `std` dependency is permitted in the runtime. |
| F-R-07 | The runtime must be distributable as a C shared library (`libwql`) exposing the three canonical functions via `extern "C"`. This is the integration path for Go (CGO) and JVM (JNI / Panama). |
| F-R-08 | WVM bytecode must also be compilable to a WASM module exposing the three canonical functions, for broker-side program deployment. |
| F-R-09 | The compiler entry point must be I/O-free: `compile(source: &str, schema: Option<&FileDescriptorSet>) -> Result<Program, CompileError>`. |

---

## 8. Non-Functional Requirements

| Attribute | Target |
|---|---|
| Throughput | WVM interpreter: ≥10 GB/s of wire bytes scanned for pure copy projections on a single core, across all native runtime targets. |
| Latency | Added latency per message call: <5 µs P99 for typical programs (< 64 fields) on all native runtimes. |
| WASM binary size | Compiled WASM module per program: <100 KB. |
| Compiler latency | Startup-time compilation of a typical WQL expression: <1 ms, suitable for service init paths. |
| Correctness | Projection output must round-trip through any proto3 decoder without error, including for messages with unknown fields. |
| Safety | WASM runtime: programs are sandboxed and cannot access host memory or perform I/O. Native runtimes: programs are pure functions with no side effects. |
| Portability | WVM bytecode produced by the compiler on any platform must execute identically on all runtime targets. |

---

## 9. Schema Evolution Considerations

Unknown field preservation is the critical safety property for production deployments. A projection compiled against schema version N must produce valid output when run against a message written by schema version N+k, where N+k adds new fields.

The `...` trailer maps to `default: COPY` in the WVM `DISPATCH` instruction — unknown fields at that nesting level are forwarded verbatim. Without it, `default: SKIP` is used and new fields are silently dropped.

The `{ .. }` and `{ .. -field }` forms are inherently schema-evolution safe: they use `default: RECURSE` which enters and re-frames any unknown sub-message, preserving its contents (minus the excluded fields). A `{ .. -embedding }` projection compiled today will correctly strip `embedding` from any future schema version that restructures or renames surrounding fields, because it operates by field number at every level rather than against a fixed path.

**Recommendation:** any program that re-publishes projected messages downstream (broker transforms, stream processors) should use either the `...` clause on inclusion projections or the `{ .. -field }` exclusion form. Consumers projecting for their own deserialization may safely use strict inclusion mode.

---

## 10. Open Questions

| # | Question |
|---|---|
| OQ-01 | Frontend language: adopt CEL for predicates with a custom projection DSL, or design a unified surface syntax? Decision deferred; IR is frontend-agnostic. |
| OQ-02 | Packed repeated fields: require schema annotation to distinguish packed `int32` from a sub-message `len` field, or add a heuristic probe? |
| OQ-03 | Map fields: should map-entry filtering (`{ labels["env"] }`) be in scope for v1? |
| OQ-04 | Float disambiguation: schema-free programs cannot distinguish `float` (i32 wire) from `fixed32`. Require explicit cast syntax or require schema for float comparisons? |
| OQ-05 | WASM host interface: do broker runtimes (Redpanda, WasmEdge) provide raw wire bytes directly, or only after stripping a framing header? Does this differ per platform? |
| OQ-06 | Mixed inclusion/exclusion: should `{ name, .. -payload }` be valid (include `name` explicitly, deep-copy everything else except `payload`)? The IR supports this but the surface grammar needs to define it clearly. |
| OQ-07 | FFI strategy for the compiler core: C FFI with a header, or expose as a WASM module that all host languages can embed? The latter avoids per-language binding maintenance but adds a WASM runtime dependency to the compiler path. |
| OQ-08 | Bytecode versioning: how does a runtime detect and reject bytecode compiled by an incompatible compiler version? A program header magic + version field is the minimal answer; a full capability negotiation may be needed for long-lived deployments. |

---

## 11. Out of Scope for v1

- CEL comprehensions (`exists_one`, `all`, `filter` over repeated fields).
- Field value transformation (`SET`, `truncate`, `mask`).
- Stream aggregation (`COUNT`, `SUM`, `GROUP BY`).
- JIT / native code generation (Cranelift, LLVM).
- Proto2 extensions, groups, and required fields.
- gRPC streaming intercept.
- Dynamic hot-reload and runtime UDF registration (startup-time compilation is in scope; live swap is not).
- A standalone CLI tool (the compiler is a library first; a CLI wrapper is a thin layer on top and is not a v1 deliverable).
