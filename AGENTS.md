# Agent Instructions

## Build & Test

```bash
make check              # format + clippy + all tests — the merge gate
cargo test -p <crate>   # test a single crate
cargo test -- <name>    # run a single test by name
```

Regenerate test proto schema after editing `proto/testdata.proto`:

```bash
protoc --descriptor_set_out=crates/wql-compiler/tests/testdata/testdata.bin \
       --include_imports crates/wql-compiler/proto/testdata.proto
```

Then manually update `tests/testdata/testdata.rs` to match (prost-generated structs).

## Architecture Invariants

- **Compiler and runtime are independent.** Neither depends on the other. The compiler produces bytecode bytes, the runtime consumes them. Do not add cross-dependencies.
- **`wql-runtime` is `no_std`.** No `std` imports, no heap allocation on the hot path. The register file and frame stack are stack-allocated.
- **`Recurse` exists in the IR/runtime but the compiler does not emit it.** Deep search was removed; the instruction is kept for potential future use by external bytecode producers.
- **Deep exclusion (`..-field`) expands at bind time.** The binder walks the schema tree and produces regular Copy projections with exclusions. The emitter sees no special construct — no new IR instructions were needed.
- **Schema-free mode rejects features that require schema traversal** (deep exclusion, named fields). This is intentional, not a TODO.

## Compiler Pipeline

```
Source → Parser (ast.rs) → Binder (bind.rs) → Emitter (emit.rs) → Linker (codec.rs)
```

- **Parser** produces an untyped AST. Field references are names or numbers.
- **Binder** resolves names to field numbers using the proto schema, infers encodings, expands syntactic sugar (deep exclusions). Produces a `BoundQuery`.
- **Emitter** lowers `BoundQuery` to IR instructions. Handles merging of predicate and projection arms into a single DISPATCH.
- **Linker** (in `wql-ir/codec.rs`) encodes instructions to bytecode, resolves label references, computes the program header.

## Test Tiers

1. **Unit tests** — in-module (`#[cfg(test)]`). Parser, binder, emitter each have their own.
2. **Wire-level e2e** — `tests/e2e.rs`. Schema-free, builds raw protobuf bytes, asserts on output bytes.
3. **Schema-bound e2e** — `tests/e2e_schema.rs`. Uses generated Rust types and real `FileDescriptorSet`.
4. **Data-driven** — `tests/testdata/e2e.txt`. JSON input/output, one test case per block. Format:
   ```
   # message: testdata.Person
   { name, age }
   {"name": "Alice", "age": 30}
   ----
   {"name": "Alice", "age": 30}
   ```

## Commit Conventions

- Use conventional commits: `feat:`, `fix:`, `test:`, `refactor:`, `style:`, `docs:`
- Update README.md and doc/IR.md when changing user-facing syntax or IR behavior
- Each commit should pass `make check` independently
