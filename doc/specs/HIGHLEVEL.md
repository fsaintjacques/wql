# WQL — High-Level Implementation Plan

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.1 |
| **Date** | 2026-03-29 |

Each block below maps to a dedicated spec document. Blocks are ordered by dependency: each block may assume all prior blocks are complete and tested. Blocks within the Rust phase are sequential; the C phase begins after the Rust phase is stable.

---

## Phase 1 — Rust

### Block 1 — Workspace & Crate Scaffold

Set up the Cargo workspace with all crates declared, correct `no_std` configurations, feature flag declarations, and inter-crate dependency edges. No logic beyond confirming each crate compiles against its declared constraints.

**Produces:** a compiling workspace; CI baseline; `no_std` + `alloc` verified for `wql-ir`, `wql-runtime`, `wql-wasm`.
**Spec:** `doc/specs/01-scaffold.md`

---

### Block 2 — `wql-ir`: IR Types & Bytecode Codec

Define all IR types (`Instruction`, `Program`, `ProgramHeader`, `RegisterValue`, `DispatchArm`, encoding enums) and implement the bytecode encode/decode round-trip. This is the shared contract between the compiler and the runtime; everything downstream depends on it being correct and stable.

**Produces:** `wql-ir` crate with encode/decode tested against hand-crafted bytecode for all 22 instructions.
**Spec:** `doc/specs/02-ir.md`

---

### Block 3 — `wql-runtime`: WVM Interpreter

Implement the interpreter: `Vm`, wire scanner, `DISPATCH` loop, `FRAME`/`LABEL`/`RECURSE`, all leaf actions, predicate instructions, and `RETURN`. The three public functions (`filter`, `project`, `project_and_filter`) wrap the internal `execute() -> (bool, usize)`. Tested directly against hand-assembled bytecode programs.

**Produces:** `wql-runtime` crate, fully tested against manually encoded programs covering all instructions and nesting depths.
**Spec:** `doc/specs/03-runtime.md`

---

### Block 4 — `wql-compiler` Part 1: Parser & AST

Implement the WQL surface syntax parser. Produces a typed AST for predicate expressions and projection expressions. No IR knowledge; no schema. Tested against a broad corpus of valid and invalid WQL source strings.

**Produces:** parser module inside `wql-compiler`; AST type definitions; parse error reporting with span information.
**Spec:** `doc/specs/04-parser.md`

---

### Block 5 — `wql-compiler` Part 2: Schema Binder, IR Emitter & Linker

Three tightly-coupled compiler stages operating on the AST produced by Block 4:

- **Schema binder** — resolves named field references to field numbers using a `FileDescriptorSet`; validates literal types; produces a `BoundAst`. Schema-free programs skip this stage.
- **IR emitter** — walks the `BoundAst`, allocates registers, emits `Vec<Instruction>` with DISPATCH/FRAME nesting and RECURSE back-references.
- **Linker** — resolves label references to relative byte offsets and serializes to the `Program` bytecode format defined in Block 2.

**Produces:** `wql-compiler` crate; `CompileOptions`; `compile()` entry point end-to-end.
**Spec:** `doc/specs/05-compiler.md`

---

### Block 6 — End-to-End Test Suite

Cross-crate correctness tests: compile WQL source through `wql-compiler`, execute the resulting bytecode through `wql-runtime`, assert output bytes and predicate results. Covers schema-free and schema-bound programs, all projection forms, all predicate operators, nested messages, schema evolution (unknown fields), and edge cases.

**Produces:** `tests/integration/` suite; any bugs found in Blocks 3–5 fixed here before proceeding.
**Spec:** `doc/specs/06-e2e-tests.md`

---

### Block 7 — `wql-wasm`: WASM Program Shell

Implement the `wasm32-unknown-unknown` program shell: embedded bytecode via `include_bytes!`, once-init `Program`, static output buffer, and the three exported WASM functions. Validate binary size against the <100 KB target. Smoke-test with a real WASM runtime (Wasmtime) driven from an integration test.

**Produces:** `wql-wasm` crate; a working `.wasm` artifact for a sample program; size regression test.
**Spec:** `doc/specs/07-wasm.md`

---

## Phase 2 — C Bindings

### Block 8 — `wql-capi`: C FFI Layer & Generated Header

Implement `OwnedProgram`, the five C API functions (`wql_compile`, `wql_program_load`, `wql_program_free`, `wql_filter`, `wql_project`, `wql_project_and_filter`), panic safety via `catch_unwind`, and `cbindgen`-based generation of `bindings/include/wql.h`. Validate the header and shared library against a minimal C consumer in the integration test suite.

**Produces:** `wql-capi` crate; `libwql`; `bindings/include/wql.h`; C smoke test.
**Spec:** `doc/specs/08-capi.md`

---

## What Comes After

The following are not scheduled yet. They follow naturally from the blocks above and will be planned once Phase 2 is stable:

- Go binding (`bindings/go/`) — CGO wrapper over `libwql`
- Java/JNI binding (`bindings/java/`, `wql-jni` crate)
- `tools/wqlc` CLI — thin wrapper over `wql-compiler` for AOT and WASM build flows
- `regex` feature (`BYTES_MATCHES`) — depends on a `no_std`-compatible regex engine decision
