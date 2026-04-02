# WVM — Wire Virtual Machine
## Intermediate Representation Specification

| | |
|---|---|
| **Status** | Draft |
| **Version** | 0.2 |
| **Date** | 2026-03-29 |
| **Scope** | Instruction set, dispatch model, compilation examples |

---

## 1. Purpose and Scope

This document specifies the Wire VM (WVM) intermediate representation — the instruction set that all WQL programs compile to. WVM is the boundary between the front-end language (CEL subset for predicates; a struct-expression DSL for projections) and the back-end execution environment (WASM interpreter, future JIT).

The IR is intentionally small: 19 instructions. It is designed to express exactly two operations over a protobuf wire byte sequence — predicate evaluation and field projection — while maintaining the invariant that both execute in a single forward pass with no deserialization.

---

## 2. Design Principles

- **Single-pass.** A WVM program makes exactly one forward scan over the input byte sequence. There are no seek, rewind, or random-access instructions.
- **Zero-copy for pure projections.** When a field is copied verbatim, the tag and value bytes are `memcpy`'d to the output buffer. No decoding occurs.
- **Composable default actions.** The `DISPATCH` instruction carries a default action (`SKIP`, `COPY`, or `RECURSE`) that determines what happens to any field not explicitly matched. This single knob controls strict projection and unknown-field preservation.
- **Unified IR.** Predicates and projections share the same instruction set and the same execution model. Every program produces both a bool result and an output byte count. The caller decides which to use.
- **Always write, caller decides.** The output buffer is written during the scan unconditionally. If the bool result is false, the buffer contents are undefined and the caller must not read them. This avoids any rollback or conditional write path.
- **Explicit nesting.** Sub-message scope is entered and exited explicitly via `FRAME`. Length prefixes are recomputed automatically on `FRAME` exit. Nesting can be arbitrarily deep.
- **Frontend-agnostic.** The IR does not encode any front-end syntax. It is a valid compilation target for CEL AST, a hand-written DSL, or a query planner.

---

## 3. Protobuf Wire Format Primer

A protobuf message is a flat sequence of `(tag, value)` pairs in wire order. Each tag encodes two values: a field number and a wire type. The wire type determines the byte layout of the following value.

| Wire type | ID | Encoding | Used for |
|---|---|---|---|
| `VARINT` | 0 | Variable-length integer (1–10 bytes) | int32, int64, uint32, uint64, sint32, sint64, bool, enum |
| `I64` | 1 | Fixed 8 bytes, little-endian | double, fixed64, sfixed64 |
| `LEN` | 2 | Length varint, then N bytes | string, bytes, sub-message, packed repeated |
| `I32` | 5 | Fixed 4 bytes, little-endian | float, fixed32, sfixed32 |

The critical property for WVM: any field value can be skipped without decoding by reading only its length from the wire type. `VARINT`: scan until a byte with the high bit clear. `LEN`: read the length prefix, skip N bytes. `I32`/`I64`: skip exactly 4 or 8 bytes. This is the foundation of the `SKIP` action and the zero-copy `COPY` action.

---

## 4. Execution Model

A WVM program runs against a **scan window**: a contiguous slice of bytes with a read cursor. The initial scan window is the entire input message. `FRAME` pushes a new scan window (a length-delimited sub-slice); `FRAME` exit pops it.

A **register file** (R0–R15 in v1) holds decoded field values for predicate evaluation. Registers are typed at write time by the `DECODE` instruction.

An **output buffer** (caller-provided, length ≥ input length) accumulates `COPY` and `FRAME` bytes. An **output cursor** tracks the number of bytes written; it is the second component of the return value. On `FRAME` exit the buffer is reframed: a new length varint is prepended and the sub-buffer is appended to the parent buffer.

A **bool stack** accumulates predicate results. On `RETURN`, the top of the bool stack is the predicate result; an empty stack is treated as `true` (pure projection programs push nothing).

The execution result is always `(bool, usize)` — the predicate result and the number of bytes written to the output buffer. The three public API functions (`filter`, `project`, `project_and_filter`) are wrappers that consume this pair differently:

| API function | Uses bool | Uses usize | Output buffer contract |
|---|---|---|---|
| `filter` | yes | ignored | never written (no COPY in filter programs) |
| `project` | ignored | yes | valid for `output[..n]` |
| `project_and_filter` | yes | yes | valid for `output[..n]` only if bool is true; undefined otherwise |

The output buffer for `project_and_filter` is written during the scan regardless of the predicate result. The caller must not read the buffer if the return value is `None`. No rollback occurs.

---

## 5. Instruction Set

### 5.1 Core construct — `DISPATCH`

`DISPATCH` is the only looping instruction. It iterates over every `(field_num, wire_type)` pair in the current scan window and routes each to the first matching action. At end-of-window it halts.

```
DISPATCH  default: SKIP | COPY | RECURSE(label)
  | Field(n)                → action+
  | Field(m), WireType(LEN) → action+   -- wire type guard (optional)
  ...
```

A field may list multiple actions, executed in sequence left-to-right. The most common combination is `DECODE` followed by `COPY` (load value into register for predicate evaluation, and also copy it to the output buffer for projection).

The **default action** applies to every field that matches no explicit arm:

| Default | Behavior |
|---|---|
| `SKIP` | Consume value bytes, emit nothing. Strict projection — unknown fields are dropped. |
| `COPY` | Emit tag + raw value bytes verbatim to output buffer. Unknown fields are preserved. Safe for schema evolution. |
| `RECURSE(P)` | If `wire_type == LEN`: push new scan window, run program P inside it, emit tag + reframed length + sub-output. If `wire_type != LEN`: `SKIP`. |

### 5.2 Sub-message scope — `FRAME`

| Instruction | Operands | Description |
|---|---|---|
| `FRAME(prog)` | program reference | Current field must have wire type `LEN`. Read length prefix N. Push a new scan window of N bytes and a fresh output buffer. Run `prog` inside the new scope. On exit: prepend new length varint to sub-output; append `tag + length + sub-output` to parent output buffer. Pop scan window. |
| `LABEL(name)` | name string | Declares a named program entry point. Required target for `RECURSE(P)`. Programs are referenced by label, not by bytecode offset, to support self-referential programs. |

### 5.3 Predicate evaluation

These instructions execute after the `DISPATCH` loop has finished loading registers. They operate on the bool stack.

| Instruction | Operands | Description |
|---|---|---|
| `CMP_EQ` | `reg, imm` | Push `reg == imm`. |
| `CMP_NEQ` | `reg, imm` | Push `reg != imm`. |
| `CMP_LT` | `reg, imm` | Push `reg < imm`. |
| `CMP_LTE` | `reg, imm` | Push `reg ≤ imm`. |
| `CMP_GT` | `reg, imm` | Push `reg > imm`. |
| `CMP_GTE` | `reg, imm` | Push `reg ≥ imm`. |
| `CMP_LEN_EQ` | `reg, bytes` | Push `reg == bytes` (exact bytes/string equality). |
| `BYTES_STARTS` | `reg, bytes` | Push true if `reg` starts with `bytes`. |
| `BYTES_ENDS` | `reg, bytes` | Push true if `reg` ends with `bytes`. |
| `BYTES_CONTAINS` | `reg, bytes` | Push true if `reg` contains `bytes` as a substring. |
| `BYTES_MATCHES` | `reg, pattern` | Push true if `reg` matches RE2 `pattern`. **Optional feature**: requires a regex engine; gated behind a `regex` feature flag in the runtime crate to avoid mandatory binary size impact. |
| `IN_SET` | `reg, [imm…]` | Push true if `reg` equals any element of the literal set. |
| `IS_SET` | `reg` | Push true if `reg` was written during this scan (`has()` / `exists()` semantics). |
| `AND` | — | Pop two bools, push their conjunction. |
| `OR` | — | Pop two bools, push their disjunction. |
| `NOT` | — | Pop one bool, push its negation. |

### 5.4 Return

| Instruction | Description |
|---|---|
| `RETURN` | End program execution. Result is `(top_of_bool_stack_or_true, output_cursor)`. An empty bool stack is treated as `true`. |

---

## 6. Compilation Examples

All examples use a `Person` message with the following field numbers:
`name=1`, `age=2`, `address=3` (sub-message: `city=1`, `country=2`), `tags=4` (repeated string), `departments=5` (repeated sub-message: `name=1`, `members=2`).

### 6.1 Flat projection, strict

Source: `{ name, age }`

```
DISPATCH  default:SKIP
  | Field(1)  → COPY          -- name
  | Field(2)  → COPY          -- age
RETURN
-- bool: true (stack empty); output: projected bytes
```

### 6.2 Nested projection, preserve unknowns

Source: `{ name, address { city, ... }, ... }`

```
DISPATCH  default:COPY                -- preserve unknown top-level fields
  | Field(1)  → COPY                 -- name
  | Field(3)  → FRAME(addr_prog)     -- address sub-message

LABEL(addr_prog)
DISPATCH  default:COPY               -- preserve unknown address fields
  | Field(1)  → COPY                 -- city
RETURN
```

### 6.3 Predicate only

Source: `age > 18 && address.city == "NYC"`

```
DISPATCH  default:SKIP
  | Field(2)  → DECODE(R0, VARINT)
  | Field(3)  → FRAME(addr_pred)

LABEL(addr_pred)
DISPATCH  default:SKIP
  | Field(1)  → DECODE(R1, LEN)

CMP_GT      R0, 18
CMP_LEN_EQ  R1, "NYC"
AND
RETURN
-- bool: predicate result; output: empty (no COPY instructions), cursor: 0
```

### 6.4 Combined filter + projection, single pass

Source: `WHERE age > 18 AND address.city == "NYC"  SELECT { name, address { city }, ... }`

```
DISPATCH  default:COPY                        -- preserve unknown top-level fields
  | Field(1)  → COPY                         -- name → output buffer
  | Field(2)  → DECODE(R0, VARINT), COPY     -- age: decode for predicate + copy
  | Field(3)  → FRAME(combined_addr)

LABEL(combined_addr)
DISPATCH  default:COPY
  | Field(1)  → DECODE(R1, LEN), COPY        -- city: decode + copy

CMP_GT      R0, 18
IS_SET      R1
CMP_LEN_EQ  R1, "NYC"
AND
AND
RETURN
-- bool: predicate result; output: written unconditionally during scan
-- caller discards output[..n] if bool is false
```

---

## 7. Invariants and Guarantees

| Invariant | Statement |
|---|---|
| **I-01 Single pass** | A WVM program advances the read cursor monotonically. No instruction moves the cursor backward. |
| **I-02 Valid output** | The output buffer produced by any projection program is a valid proto3 wire encoding. Length prefixes are exact (recomputed on `FRAME` exit). |
| **I-03 Unknown safety** | With `default:COPY` at every `DISPATCH` level, a program compiled against schema version N produces correct output for messages written by any schema version N+k (k ≥ 0). |
| **I-04 Register scope** | Registers loaded inside a `FRAME` scope remain valid after the `FRAME` exits (they are in the flat register file). |
| **I-05 RECURSE termination** | `RECURSE(P)` terminates because each recursive invocation consumes a strictly bounded sub-slice of bytes (the `LEN` field's declared length). Proto wire format forbids cycles. |
| **I-06 Code size** | A `RECURSE(P)` back-reference is a constant-size pointer. Deep projection programs do not grow in code size with nesting depth. |
| **I-07 Output cursor** | The output cursor is monotonically non-decreasing. It equals the number of bytes appended to the output buffer. For pure filter programs, the cursor is 0 at `RETURN`. |
| **I-08 Bool default** | An empty bool stack at `RETURN` is treated as `true`. Pure projection programs never push to the bool stack. |

---

## 8. Bytecode Encoding (Informative)

The binary encoding of WVM bytecode is not yet finalised. The following conventions are recommended for the first implementation:

- Instructions are one-byte opcodes followed by variable-length operands.
- Field numbers are encoded as unsigned varints, supporting the full proto field number range (1–29999) without fixed-width overhead.
- Immediate integer values are encoded as signed varints (zigzag for negative constants).
- Bytes/string immediates are length-prefixed: a varint length followed by raw bytes.
- `LABEL` references in `FRAME` and `RECURSE` operands are relative byte offsets from the start of the program, or named string labels resolved by the linker.
- A program header declares the register count required, enabling the runtime to pre-allocate the register file without scanning the bytecode.

---

## 9. Open Questions

| # | Question |
|---|---|
| OQ-01 | **Packed repeated fields.** A packed `repeated int32` is a single `LEN` field whose body is concatenated varints. `RECURSE` will misparse it as a sub-message. Resolution: require schema annotation (`WireType` guard on `DISPATCH` arm) or add a `DECODE_PACKED` instruction family. |
| OQ-02 | **Map fields.** Wire-encoded as `repeated LEN` containing `{key=1, value=2}` sub-messages. Map-entry filtering needs a `FRAME` that decodes the key register and gates `COPY` on a comparison. Expressible today but verbose; a `MAP_FRAME` sugar instruction may be warranted. |
| OQ-03 | **Accumulator instructions** (`ACCUM`, `LOAD_ACCUM`) for repeated field quantifiers (`exists`, `all`). Deferred to v2. |
| OQ-04 | **Register file size.** 16 registers covers all v1 programs. Overflow strategy (error vs. spill to heap map) to be decided before finalising the binary encoding. |
| OQ-05 | **Float vs. fixed32 disambiguation** in schema-free mode. Both are `I32` wire type. Require schema binding or an explicit `WireType` guard on the `DISPATCH` arm. |

---

## Appendix A — Complete Instruction Summary

| Instruction | Category | Operands |
|---|---|---|
| `DISPATCH` | Control | `default: SKIP\|COPY\|RECURSE(label)`; arm list |
| `FRAME` | Scope | program reference (label or inline) |
| `LABEL` | Scope | name |
| `CMP_EQ` | Predicate | `reg, imm` |
| `CMP_NEQ` | Predicate | `reg, imm` |
| `CMP_LT` | Predicate | `reg, imm` |
| `CMP_LTE` | Predicate | `reg, imm` |
| `CMP_GT` | Predicate | `reg, imm` |
| `CMP_GTE` | Predicate | `reg, imm` |
| `CMP_LEN_EQ` | Predicate | `reg, bytes` |
| `BYTES_STARTS` | Predicate | `reg, bytes` |
| `BYTES_ENDS` | Predicate | `reg, bytes` |
| `BYTES_CONTAINS` | Predicate | `reg, bytes` |
| `BYTES_MATCHES` | Predicate (optional) | `reg, pattern` |
| `IN_SET` | Predicate | `reg, [imm…]` |
| `IS_SET` | Predicate | `reg` |
| `AND` | Logic | — |
| `OR` | Logic | — |
| `NOT` | Logic | — |
| `RETURN` | Return | — |
