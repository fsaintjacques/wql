# Plan: Unified `eval` API

Replace the three-function execution API (`filter`, `project`, `project_and_filter`)
with a single `eval` entry point. The program header already encodes which
capabilities the program has (predicate, projection, or both); users should not
need to match on this themselves.

## Rust API

```rust
/// Result of evaluating a WQL program against an input record.
pub struct EvalResult {
    /// Bytes written to the output buffer (0 when the program has no projection).
    pub output_len: usize,
    /// Whether the record passed the predicate (true when the program has no predicate).
    pub matched: bool,
}

impl LoadedProgram {
    /// Evaluate this program against `input`.
    ///
    /// - **Filter-only**: `output` is unused; pass `&mut []`.
    ///   Returns `EvalResult { output_len: 0, matched }`.
    /// - **Project-only**: `matched` is always `true`.
    ///   Returns `EvalResult { output_len: n, matched: true }`.
    /// - **Filter+project**: both fields populated.
    ///
    /// # Buffer sizing
    ///
    /// `output` must be at least `input.len() + 5 * max_frame_depth` bytes
    /// when the program has projection. For filter-only programs, an empty
    /// slice is sufficient (an internal buffer is allocated when
    /// `max_frame_depth > 0`).
    pub fn eval(&self, input: &[u8], output: &mut [u8])
        -> Result<EvalResult, RuntimeError>;
}
```

`Result<EvalResult, RuntimeError>` is stack-allocated (2 words + discriminant);
Rust performs NRVO so the caller receives it in-place — zero heap allocation on
the hot path.

## C API

```c
typedef struct {
    uintptr_t output_len;  /* bytes written to output (0 when no projection) */
    bool      matched;     /* predicate result (true when no predicate)      */
} wql_eval_result_t;

/// Evaluate a WQL program against input bytes.
///
/// Returns:
///   0  — success; *result is populated.
///  -1  — error;   *errmsg (if non-null) is set.
///
/// For filter-only programs, pass output=NULL / output_len=0.
/// For project-only programs, result->matched is always true.
///
/// Buffer sizing: output_len >= input_len is always sufficient.
int wql_eval(
    const wql_program_t   *program,
    const uint8_t *input,  uintptr_t input_len,
    uint8_t       *output, uintptr_t output_len,
    wql_eval_result_t     *result,
    char                 **errmsg
);
```

Return code is `int` (0 = ok, -1 = error). Output goes through `result`
struct, mirroring the Rust `EvalResult`. `errmsg` follows existing convention
(caller frees with `wql_errmsg_free`).

## Migration steps

1. **wql-runtime** — add `EvalResult` struct and `eval()` method on
   `LoadedProgram`. Rewrite `filter`, `project`, `project_and_filter` as thin
   wrappers around `eval` (keeps them compiling during transition, tests stay
   green).

2. **wql-capi** — add `wql_eval_result_t` and `wql_eval()`. Mark old
   execution functions `#[deprecated]`. Update the C header.

3. **Callers** — migrate `wqlc` and C tests (`smoke.c`) to `eval` /
   `wql_eval`.

4. **Cleanup** — remove the three old functions from runtime and capi.
   Remove deprecated markers. Final pass on docs.

## Design notes

- `EvalResult` fields are always populated (no `Option`). The method inspects
  the program header flags to decide what to execute, filling in sensible
  defaults for the absent half (`matched=true` when no predicate,
  `output_len=0` when no projection).

- `eval` is a method on `LoadedProgram` — idiomatic Rust, discoverable via
  autocomplete, and avoids the free-function dispatch problem.

- The C API groups outputs into `wql_eval_result_t` to keep the parameter list
  manageable (7 params instead of 8) and mirror the Rust struct.

- The C API returns `int` error code rather than encoding multiple meanings
  into a single `int64_t` (the current `project_and_filter` uses -1 = filtered,
  -2 = error). Cleaner, no sentinel values.

- Filter-only programs with `max_frame_depth > 0` still allocate an internal
  scratch buffer (same as today). This is the only allocation on the hot path
  and only applies to nested-predicate programs when `output` is empty.
