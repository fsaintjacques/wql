//! End-to-end integration tests: compile WQL source → execute bytecode → verify results.
//!
//! These tests exercise the full pipeline: `wql_compiler::compile` produces bytecode,
//! `wql_runtime` executes it against hand-built protobuf messages, and we verify
//! the output bytes and predicate results.

use wql_compiler::{compile, CompileOptions};

// ═══════════════════════════════════════════════════════════════════════
// Protobuf encoding helpers
// ═══════════════════════════════════════════════════════════════════════

fn encode_varint(val: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut v = val;
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if v == 0 {
            break;
        }
    }
    buf
}

fn encode_tag(field: u32, wire_type: u8) -> Vec<u8> {
    encode_varint(u64::from(field) << 3 | u64::from(wire_type))
}

/// Encode a varint field (wire type 0).
fn proto_varint(field: u32, val: u64) -> Vec<u8> {
    let mut buf = encode_tag(field, 0);
    buf.extend(encode_varint(val));
    buf
}

/// Encode a length-delimited field (wire type 2).
fn proto_len(field: u32, val: &[u8]) -> Vec<u8> {
    let mut buf = encode_tag(field, 2);
    buf.extend(encode_varint(val.len() as u64));
    buf.extend(val);
    buf
}

/// Encode a fixed32 field (wire type 5).
fn proto_fixed32(field: u32, val: u32) -> Vec<u8> {
    let mut buf = encode_tag(field, 5);
    buf.extend(val.to_le_bytes());
    buf
}

/// Encode a fixed64 field (wire type 1).
fn proto_fixed64(field: u32, val: u64) -> Vec<u8> {
    let mut buf = encode_tag(field, 1);
    buf.extend(val.to_le_bytes());
    buf
}

/// Build a protobuf message from a list of encoded fields.
fn build_message(fields: &[Vec<u8>]) -> Vec<u8> {
    fields.iter().flat_map(|f| f.iter().copied()).collect()
}

/// Compile and load a schema-free program.
fn load_program(source: &str) -> wql_runtime::LoadedProgram {
    let bytecode = compile(source, &CompileOptions::default()).unwrap();
    wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap()
}

/// Run eval() for a projection and return the output bytes.
fn run_project(source: &str, input: &[u8]) -> Vec<u8> {
    let program = load_program(source);
    let mut output = vec![0u8; input.len() * 2 + 256];
    let result = program
        .eval(input, &mut output)
        .unwrap_or_else(|e| panic!("eval({source:?}) failed: {e:?}"));
    output[..result.output_len].to_vec()
}

/// Run eval() for a filter and return the boolean result.
fn run_filter(source: &str, input: &[u8]) -> bool {
    let program = load_program(source);
    program.eval(input, &mut []).unwrap().matched
}

/// Run eval() for a combined filter+project and return Option<output_bytes>.
fn run_project_and_filter(source: &str, input: &[u8]) -> Option<Vec<u8>> {
    let program = load_program(source);
    let mut output = vec![0u8; input.len() * 2 + 256];
    let result = program.eval(input, &mut output).unwrap();
    if result.matched {
        Some(output[..result.output_len].to_vec())
    } else {
        None
    }
}

/// Check that a protobuf field is present in the output.
fn has_field(output: &[u8], field: u32) -> bool {
    // Scan for any tag with the given field number (any wire type)
    let mut pos = 0;
    while pos < output.len() {
        let (tag, consumed) = decode_varint(&output[pos..]);
        if consumed == 0 {
            break;
        }
        let f = (tag >> 3) as u32;
        let wt = (tag & 0x7) as u8;
        pos += consumed;
        if f == field {
            return true;
        }
        // Skip field value
        match wt {
            0 => {
                let (_, c) = decode_varint(&output[pos..]);
                pos += c;
            }
            1 => pos += 8, // fixed64
            2 => {
                let (len, c) = decode_varint(&output[pos..]);
                pos += c + len as usize;
            }
            5 => pos += 4, // fixed32
            _ => break,
        }
    }
    false
}

fn decode_varint(buf: &[u8]) -> (u64, usize) {
    let mut val: u64 = 0;
    let mut shift = 0;
    for (i, &b) in buf.iter().enumerate() {
        val |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return (val, i + 1);
        }
        shift += 7;
    }
    (0, 0)
}

/// Extract a varint field value from a protobuf message.
fn extract_varint(output: &[u8], target_field: u32) -> Option<u64> {
    let mut pos = 0;
    while pos < output.len() {
        let (tag, consumed) = decode_varint(&output[pos..]);
        if consumed == 0 {
            break;
        }
        let f = (tag >> 3) as u32;
        let wt = (tag & 0x7) as u8;
        pos += consumed;
        match wt {
            0 => {
                let (val, c) = decode_varint(&output[pos..]);
                if f == target_field {
                    return Some(val);
                }
                pos += c;
            }
            1 => pos += 8,
            2 => {
                let (len, c) = decode_varint(&output[pos..]);
                pos += c;
                if f == target_field {
                    return Some(len); // return the length for LEN fields
                }
                pos += len as usize;
            }
            5 => pos += 4,
            _ => break,
        }
    }
    None
}

/// Extract a length-delimited field value from a protobuf message.
fn extract_bytes(output: &[u8], target_field: u32) -> Option<Vec<u8>> {
    let mut pos = 0;
    while pos < output.len() {
        let (tag, consumed) = decode_varint(&output[pos..]);
        if consumed == 0 {
            break;
        }
        let f = (tag >> 3) as u32;
        let wt = (tag & 0x7) as u8;
        pos += consumed;
        match wt {
            0 => {
                let (_, c) = decode_varint(&output[pos..]);
                pos += c;
            }
            1 => pos += 8,
            2 => {
                let (len, c) = decode_varint(&output[pos..]);
                pos += c;
                if f == target_field {
                    return Some(output[pos..pos + len as usize].to_vec());
                }
                pos += len as usize;
            }
            5 => pos += 4,
            _ => break,
        }
    }
    None
}

// ═══════════════════════════════════════════════════════════════════════
// Projection tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn project_flat_include() {
    let input = build_message(&[
        proto_varint(1, 42),
        proto_len(2, b"hello"),
        proto_varint(3, 99),
    ]);
    let output = run_project("{ #1, #2 }", &input);

    assert!(has_field(&output, 1));
    assert!(has_field(&output, 2));
    assert!(!has_field(&output, 3));
    assert_eq!(extract_varint(&output, 1), Some(42));
    assert_eq!(extract_bytes(&output, 2), Some(b"hello".to_vec()));
}

#[test]
fn project_flat_preserve_unknowns() {
    let input = build_message(&[
        proto_varint(1, 10),
        proto_varint(2, 20),
        proto_varint(3, 30),
    ]);
    let output = run_project("{ #1, .. }", &input);

    // All fields should be present (unknowns preserved)
    assert!(has_field(&output, 1));
    assert!(has_field(&output, 2));
    assert!(has_field(&output, 3));
}

#[test]
fn project_identity() {
    let input = build_message(&[
        proto_varint(1, 10),
        proto_len(2, b"data"),
        proto_varint(3, 30),
    ]);
    let output = run_project("{ .. }", &input);

    // Identity projection: output == input
    assert_eq!(output, input);
}

#[test]
fn project_empty_drops_all() {
    let input = build_message(&[proto_varint(1, 10), proto_varint(2, 20)]);
    let output = run_project("{ }", &input);
    assert!(output.is_empty());
}

#[test]
fn project_nested_message() {
    // Inner message: { 1: "NYC", 2: "US" }
    let inner = build_message(&[proto_len(1, b"NYC"), proto_len(2, b"US")]);
    let input = build_message(&[proto_len(1, b"Alice"), proto_len(3, &inner)]);

    let output = run_project("{ #1, #3 { #1 } }", &input);

    assert!(has_field(&output, 1));
    assert!(has_field(&output, 3));
    // The nested message should contain field 1 (city) but not field 2 (country)
    let nested = extract_bytes(&output, 3).unwrap();
    assert!(has_field(&nested, 1));
    assert!(!has_field(&nested, 2));
    assert_eq!(extract_bytes(&nested, 1), Some(b"NYC".to_vec()));
}

#[test]
fn project_nested_preserve_unknowns() {
    let inner = build_message(&[proto_len(1, b"NYC"), proto_len(2, b"US")]);
    let input = build_message(&[
        proto_len(1, b"Alice"),
        proto_len(3, &inner),
        proto_varint(4, 1),
    ]);

    let output = run_project("{ #1, #3 { #1, .. }, .. }", &input);

    assert!(has_field(&output, 1));
    assert!(has_field(&output, 3));
    assert!(has_field(&output, 4)); // unknown preserved
    let nested = extract_bytes(&output, 3).unwrap();
    assert!(has_field(&nested, 1));
    assert!(has_field(&nested, 2)); // inner unknown preserved
}

#[test]
fn project_copy_all() {
    // Copy mode copies all fields verbatim at current level.
    let inner = build_message(&[proto_varint(1, 10), proto_varint(2, 20)]);
    let input = build_message(&[proto_varint(1, 42), proto_len(5, &inner)]);

    let output = run_project("{ .. }", &input);
    // All fields copied verbatim (including sub-message as opaque bytes)
    assert_eq!(output, input);
}

#[test]
fn project_copy_with_exclusion() {
    // Exclusions with copy mode: field 3 is excluded at top level.
    let input = build_message(&[
        proto_varint(1, 10),
        proto_varint(3, 30),
        proto_varint(5, 50),
    ]);

    let output = run_project("{ -#3, .. }", &input);
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 3));
    assert!(has_field(&output, 5));
}

#[test]
fn project_copy_exclusion_preserves_nested() {
    // { -#2, .. } strips field 2 at top level, but preserves everything else.
    let inner = build_message(&[proto_varint(1, 10), proto_varint(2, 20)]);
    let input = build_message(&[
        proto_varint(2, 99),  // excluded at top level
        proto_len(3, &inner), // kept (opaque)
    ]);

    let output = run_project("{ -#2, .. }", &input);
    assert!(!has_field(&output, 2));
    assert!(has_field(&output, 3));
    // The nested message is preserved as-is (opaque copy)
    let nested = extract_bytes(&output, 3).unwrap();
    assert!(has_field(&nested, 1));
    assert!(has_field(&nested, 2)); // inner field 2 is NOT excluded (copy is shallow)
}

#[test]
fn project_multiple_exclusions() {
    // Multiple exclusions at top level.
    let input = build_message(&[
        proto_varint(1, 10),
        proto_varint(2, 20), // exclude
        proto_varint(3, 30),
        proto_varint(4, 40), // exclude
        proto_varint(5, 50),
    ]);

    let output = run_project("{ -#2, -#4, .. }", &input);
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
    assert!(has_field(&output, 3));
    assert!(!has_field(&output, 4));
    assert!(has_field(&output, 5));
}

#[test]
fn project_empty_input() {
    let output = run_project("{ #1 }", &[]);
    assert!(output.is_empty());
}

#[test]
fn project_field_not_present() {
    // Input has field 2, projection asks for field 1
    let input = build_message(&[proto_varint(2, 42)]);
    let output = run_project("{ #1 }", &input);
    assert!(output.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════
// Deep search tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn project_deep_search_finds_nested() {
    // { ..#1 } — find field 1 at any depth via Recurse.
    // Outer message has field 3 (LEN sub-message) containing field 1 (varint).
    let inner = build_message(&[proto_varint(1, 42), proto_varint(2, 99)]);
    let input = build_message(&[proto_len(3, &inner)]);

    let output = run_project("{ ..#1 }", &input);
    // Recurse enters field 3 (LEN), finds field 1, copies it.
    // The output should have field 3 reframed with only field 1 inside.
    assert!(has_field(&output, 3));
    let nested = extract_bytes(&output, 3).unwrap();
    assert!(has_field(&nested, 1));
    assert!(!has_field(&nested, 2));
}

#[test]
fn project_deep_search_top_level() {
    // { ..#1 } — field 1 is at the top level (varint), directly matched by the arm.
    let input = build_message(&[proto_varint(1, 42), proto_varint(2, 99)]);

    let output = run_project("{ ..#1 }", &input);
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
}

#[test]
fn project_deep_search_not_found() {
    // { ..#9 } — field 9 doesn't exist anywhere.
    let inner = build_message(&[proto_varint(1, 10)]);
    let input = build_message(&[proto_len(3, &inner)]);

    let output = run_project("{ ..#9 }", &input);
    // Nothing matched — output should be the reframed sub-messages with no content
    // (Recurse enters LEN fields but finds no #9).
    assert!(!has_field(&output, 9));
}

// ═══════════════════════════════════════════════════════════════════════
// Filter (predicate-only) tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn filter_eq_true() {
    let input = build_message(&[proto_varint(1, 42)]);
    assert!(run_filter("#1 == 42", &input));
}

#[test]
fn filter_eq_false() {
    let input = build_message(&[proto_varint(1, 99)]);
    assert!(!run_filter("#1 == 42", &input));
}

#[test]
fn filter_neq() {
    let input = build_message(&[proto_varint(1, 5)]);
    assert!(run_filter("#1 != 0", &input));
    assert!(!run_filter("#1 != 5", &input));
}

#[test]
fn filter_gt() {
    let input = build_message(&[proto_varint(1, 25)]);
    assert!(run_filter("#1 > 18", &input));
    assert!(!run_filter("#1 > 25", &input));
    assert!(!run_filter("#1 > 100", &input));
}

#[test]
fn filter_gte() {
    let input = build_message(&[proto_varint(1, 18)]);
    assert!(run_filter("#1 >= 18", &input));
    assert!(!run_filter("#1 >= 19", &input));
}

#[test]
fn filter_lt() {
    let input = build_message(&[proto_varint(1, 5)]);
    assert!(run_filter("#1 < 10", &input));
    assert!(!run_filter("#1 < 5", &input));
}

#[test]
fn filter_lte() {
    let input = build_message(&[proto_varint(1, 5)]);
    assert!(run_filter("#1 <= 5", &input));
    assert!(!run_filter("#1 <= 4", &input));
}

#[test]
fn filter_string_eq() {
    let input = build_message(&[proto_len(1, b"hello")]);
    assert!(run_filter(r#"#1 == "hello""#, &input));
    assert!(!run_filter(r#"#1 == "world""#, &input));
}

#[test]
fn filter_string_neq() {
    let input = build_message(&[proto_len(1, b"hello")]);
    assert!(run_filter(r#"#1 != "world""#, &input));
    assert!(!run_filter(r#"#1 != "hello""#, &input));
}

#[test]
fn filter_bool_eq() {
    let input = build_message(&[proto_varint(1, 1)]);
    assert!(run_filter("#1 == true", &input));
    assert!(!run_filter("#1 == false", &input));
}

#[test]
fn filter_and() {
    let input = build_message(&[proto_varint(1, 10), proto_varint(2, 20)]);
    assert!(run_filter("#1 > 5 && #2 > 15", &input));
    assert!(!run_filter("#1 > 5 && #2 > 25", &input));
    assert!(!run_filter("#1 > 15 && #2 > 15", &input));
}

#[test]
fn filter_or() {
    let input = build_message(&[proto_varint(1, 10), proto_varint(2, 20)]);
    assert!(run_filter("#1 > 5 || #2 > 25", &input));
    assert!(run_filter("#1 > 15 || #2 > 15", &input));
    assert!(!run_filter("#1 > 15 || #2 > 25", &input));
}

#[test]
fn filter_not() {
    let input = build_message(&[proto_varint(1, 0)]);
    assert!(run_filter("!#1 == 1", &input));
    assert!(!run_filter("!#1 == 0", &input));
}

#[test]
fn filter_complex_logic() {
    let input = build_message(&[
        proto_varint(1, 10),
        proto_varint(2, 20),
        proto_varint(3, 30),
    ]);
    // (a > 5 && b > 15) || c < 10
    assert!(run_filter("(#1 > 5 && #2 > 15) || #3 < 10", &input));
    // a > 15 || (b > 15 && c > 25)
    assert!(run_filter("#1 > 15 || (#2 > 15 && #3 > 25)", &input));
    // !(a > 15)
    assert!(run_filter("!#1 > 15", &input));
}

#[test]
fn filter_in_set() {
    let input = build_message(&[proto_varint(1, 2)]);
    assert!(run_filter("#1 in [1, 2, 3]", &input));
    assert!(!run_filter("#1 in [4, 5, 6]", &input));
}

#[test]
fn filter_exists_present() {
    let input = build_message(&[proto_varint(1, 42)]);
    assert!(run_filter("exists(#1)", &input));
}

#[test]
fn filter_exists_absent() {
    let input = build_message(&[proto_varint(2, 42)]);
    assert!(!run_filter("exists(#1)", &input));
}

#[test]
fn filter_starts_with() {
    let input = build_message(&[proto_len(1, b"hello world")]);
    assert!(run_filter(r#"#1 starts_with "hello""#, &input));
    assert!(!run_filter(r#"#1 starts_with "world""#, &input));
}

#[test]
fn filter_ends_with() {
    let input = build_message(&[proto_len(1, b"hello world")]);
    assert!(run_filter(r#"#1 ends_with "world""#, &input));
    assert!(!run_filter(r#"#1 ends_with "hello""#, &input));
}

#[test]
fn filter_contains() {
    let input = build_message(&[proto_len(1, b"hello world")]);
    assert!(run_filter(r#"#1 contains "lo wo""#, &input));
    assert!(!run_filter(r#"#1 contains "xyz""#, &input));
}

#[test]
fn filter_nested_field() {
    let inner = build_message(&[proto_varint(1, 42)]);
    let input = build_message(&[proto_len(3, &inner)]);
    assert!(run_filter("#3.#1 > 10", &input));
    assert!(!run_filter("#3.#1 > 50", &input));
}

#[test]
fn filter_empty_input() {
    // No fields present — comparisons should fail (register not set)
    assert!(!run_filter("#1 > 0", &[]));
}

// ═══════════════════════════════════════════════════════════════════════
// Combined (WHERE ... SELECT) tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn combined_filter_pass_project() {
    let input = build_message(&[
        proto_len(1, b"Alice"),
        proto_varint(2, 25),
        proto_varint(3, 99),
    ]);

    let result = run_project_and_filter("WHERE #2 > 18 SELECT { #1 }", &input);
    assert!(result.is_some());
    let output = result.unwrap();
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
    assert!(!has_field(&output, 3));
    assert_eq!(extract_bytes(&output, 1), Some(b"Alice".to_vec()));
}

#[test]
fn combined_filter_fail() {
    let input = build_message(&[proto_len(1, b"Alice"), proto_varint(2, 10)]);

    let result = run_project_and_filter("WHERE #2 > 18 SELECT { #1 }", &input);
    assert!(result.is_none());
}

#[test]
fn combined_shared_field() {
    // Field 1 is in both predicate and projection
    let input = build_message(&[proto_varint(1, 42), proto_varint(2, 99)]);

    let result = run_project_and_filter("WHERE #1 > 10 SELECT { #1 }", &input);
    assert!(result.is_some());
    let output = result.unwrap();
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
    assert_eq!(extract_varint(&output, 1), Some(42));
}

#[test]
fn combined_preserve_unknowns() {
    let input = build_message(&[
        proto_varint(1, 42),
        proto_varint(2, 99),
        proto_varint(3, 77),
    ]);

    let result = run_project_and_filter("WHERE #1 > 10 SELECT { #1, .. }", &input);
    assert!(result.is_some());
    let output = result.unwrap();
    assert!(has_field(&output, 1));
    assert!(has_field(&output, 2)); // preserved
    assert!(has_field(&output, 3)); // preserved
}

#[test]
fn combined_nested_shared_parent() {
    // Predicate on #3.#1, projection on #3 { #1 }
    let inner = build_message(&[proto_varint(1, 42), proto_varint(2, 99)]);
    let input = build_message(&[proto_len(1, b"Alice"), proto_len(3, &inner)]);

    let result = run_project_and_filter("WHERE #3.#1 > 10 SELECT { #1, #3 { #1 } }", &input);
    assert!(result.is_some());
    let output = result.unwrap();
    assert!(has_field(&output, 1));
    assert!(has_field(&output, 3));
}

#[test]
fn combined_copy_with_predicate_and_exclusion() {
    // Combined: predicate on varint field, copy projection with exclusion at top level.
    let input = build_message(&[
        proto_varint(2, 20),
        proto_varint(3, 30),
        proto_varint(5, 50),
    ]);

    let result = run_project_and_filter("WHERE #2 > 15 SELECT { -#3, .. }", &input);
    assert!(result.is_some());
    let output = result.unwrap();
    assert!(has_field(&output, 2));
    assert!(!has_field(&output, 3));
    assert!(has_field(&output, 5));
}

// ═══════════════════════════════════════════════════════════════════════
// Wire type / encoding edge cases
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn project_mixed_wire_types() {
    let input = build_message(&[
        proto_varint(1, 100),
        proto_len(2, b"string"),
        proto_fixed32(3, 0xDEAD_BEEF),
        proto_fixed64(4, 0xCAFE_BABE_1234_5678),
    ]);

    let output = run_project("{ #1, #3 }", &input);
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
    assert!(has_field(&output, 3));
    assert!(!has_field(&output, 4));
}

#[test]
fn project_preserves_field_encoding() {
    // Verify that Copy preserves raw bytes exactly
    let input = build_message(&[
        proto_len(1, b"hello"),
        proto_varint(2, 300), // multi-byte varint
    ]);
    let output = run_project("{ #1, #2 }", &input);
    assert_eq!(output, input);
}

#[test]
fn project_preserves_all_wire_types_with_copy() {
    // Identity projection (Copy default) handles all wire types
    let input = build_message(&[
        proto_varint(1, 42),
        proto_fixed32(2, 0x1234),
        proto_fixed64(3, 0x5678),
        proto_len(4, b"data"),
    ]);
    let output = run_project("{ .. }", &input);
    assert_eq!(output, input);
}

#[test]
fn project_repeated_fields() {
    // Protobuf allows repeated fields — both occurrences should be copied
    let input = build_message(&[
        proto_varint(1, 10),
        proto_varint(1, 20),
        proto_varint(2, 99),
    ]);

    let output = run_project("{ #1 }", &input);
    // Both field 1 occurrences should be in output
    assert!(has_field(&output, 1));
    assert!(!has_field(&output, 2));
    // Output should have more than one field 1
    assert!(output.len() > proto_varint(1, 10).len());
}

#[test]
fn project_large_field_number() {
    // Field number 1000 requires multi-byte tag
    let input = build_message(&[proto_varint(1000, 42), proto_varint(1, 99)]);
    let output = run_project("{ #1000 }", &input);
    assert!(has_field(&output, 1000));
    assert!(!has_field(&output, 1));
}

#[test]
fn filter_large_varint() {
    // Large varint: runtime stores as i64, so u64::MAX becomes -1
    let input = build_message(&[proto_varint(1, u64::MAX)]);
    assert!(run_filter("#1 == -1", &input));
}

#[test]
fn filter_zero_varint() {
    let input = build_message(&[proto_varint(1, 0)]);
    assert!(run_filter("#1 == 0", &input));
    assert!(!run_filter("#1 > 0", &input));
}

// ═══════════════════════════════════════════════════════════════════════
// Compile error tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn compile_error_named_field_without_schema() {
    let result = compile("{ name }", &CompileOptions::default());
    assert!(result.is_err());
}

#[test]
fn compile_error_parse_failure() {
    let result = compile("{ unclosed", &CompileOptions::default());
    assert!(result.is_err());
}

#[test]
fn compile_error_bool_ordering() {
    let result = compile("#1 > true", &CompileOptions::default());
    assert!(result.is_err());
}

#[test]
fn compile_error_string_ordering() {
    let result = compile(r#"#1 < "abc""#, &CompileOptions::default());
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Schema-bound compile tests
// ═══════════════════════════════════════════════════════════════════════

mod schema_bound {
    use super::*;
    use prost_types::{
        field_descriptor_proto::Type as ProtoType, DescriptorProto, EnumDescriptorProto,
        EnumValueDescriptorProto, FieldDescriptorProto, FileDescriptorProto, FileDescriptorSet,
    };

    fn make_field(
        name: &str,
        number: i32,
        ty: ProtoType,
        type_name: Option<&str>,
    ) -> FieldDescriptorProto {
        FieldDescriptorProto {
            name: Some(name.to_string()),
            number: Some(number),
            r#type: Some(ty.into()),
            type_name: type_name.map(String::from),
            ..Default::default()
        }
    }

    fn test_schema() -> Vec<u8> {
        let address_msg = DescriptorProto {
            name: Some("Address".to_string()),
            field: vec![
                make_field("city", 1, ProtoType::String, None),
                make_field("zip", 2, ProtoType::Int32, None),
            ],
            ..Default::default()
        };
        let person_msg = DescriptorProto {
            name: Some("Person".to_string()),
            field: vec![
                make_field("name", 1, ProtoType::String, None),
                make_field("age", 2, ProtoType::Int64, None),
                make_field("address", 3, ProtoType::Message, Some(".test.Address")),
                make_field("status", 4, ProtoType::Enum, Some(".test.Status")),
            ],
            ..Default::default()
        };
        let status_enum = EnumDescriptorProto {
            name: Some("Status".to_string()),
            value: vec![
                EnumValueDescriptorProto {
                    name: Some("ACTIVE".to_string()),
                    number: Some(0),
                    ..Default::default()
                },
                EnumValueDescriptorProto {
                    name: Some("INACTIVE".to_string()),
                    number: Some(1),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let fds = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("test.proto".to_string()),
                package: Some("test".to_string()),
                message_type: vec![person_msg, address_msg],
                enum_type: vec![status_enum],
                ..Default::default()
            }],
        };
        prost::Message::encode_to_vec(&fds)
    }

    fn schema_opts(schema: &[u8]) -> CompileOptions<'_> {
        CompileOptions {
            schema: Some(schema),
            root_message: Some("test.Person"),
        }
    }

    #[test]
    fn schema_project_by_name() {
        let schema = test_schema();
        let bytecode = compile("{ name, age }", &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let input = build_message(&[
            proto_len(1, b"Alice"),
            proto_varint(2, 30),
            proto_varint(4, 0),
        ]);
        let mut output = vec![0u8; 256];
        let result = program.eval(&input, &mut output).unwrap();
        let output = &output[..result.output_len];

        assert!(has_field(output, 1));
        assert!(has_field(output, 2));
        assert!(!has_field(output, 4));
    }

    #[test]
    fn schema_project_nested_by_name() {
        let schema = test_schema();
        let bytecode = compile("{ name, address { city } }", &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let inner = build_message(&[proto_len(1, b"NYC"), proto_varint(2, 10001)]);
        let input = build_message(&[proto_len(1, b"Alice"), proto_len(3, &inner)]);
        let mut output = vec![0u8; 256];
        let result = program.eval(&input, &mut output).unwrap();
        let output = &output[..result.output_len];

        assert!(has_field(output, 1));
        assert!(has_field(output, 3));
        let nested = extract_bytes(output, 3).unwrap();
        assert!(has_field(&nested, 1)); // city
        assert!(!has_field(&nested, 2)); // zip excluded
    }

    #[test]
    fn schema_filter_by_name() {
        let schema = test_schema();
        let bytecode = compile("age > 18", &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let input = build_message(&[proto_varint(2, 25)]);
        assert!(program.eval(&input, &mut []).unwrap().matched);

        let input2 = build_message(&[proto_varint(2, 10)]);
        assert!(!program.eval(&input2, &mut []).unwrap().matched);
    }

    #[test]
    fn schema_filter_string() {
        let schema = test_schema();
        let bytecode = compile(r#"name == "Alice""#, &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let input = build_message(&[proto_len(1, b"Alice")]);
        assert!(program.eval(&input, &mut []).unwrap().matched);

        let input2 = build_message(&[proto_len(1, b"Bob")]);
        assert!(!program.eval(&input2, &mut []).unwrap().matched);
    }

    #[test]
    fn schema_filter_nested_path() {
        let schema = test_schema();
        let bytecode = compile(r#"address.city == "NYC""#, &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let inner = build_message(&[proto_len(1, b"NYC")]);
        let input = build_message(&[proto_len(3, &inner)]);
        assert!(program.eval(&input, &mut []).unwrap().matched);
    }

    #[test]
    fn schema_type_error() {
        let schema = test_schema();
        let result = compile(r#"age == "old""#, &schema_opts(&schema));
        assert!(result.is_err());
    }

    #[test]
    fn schema_unresolved_field() {
        let schema = test_schema();
        let result = compile("{ nonexistent }", &schema_opts(&schema));
        assert!(result.is_err());
    }

    #[test]
    fn schema_combined() {
        let schema = test_schema();
        let bytecode = compile(r#"WHERE age > 18 SELECT { name }"#, &schema_opts(&schema)).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let input = build_message(&[
            proto_len(1, b"Alice"),
            proto_varint(2, 25),
            proto_varint(4, 0),
        ]);

        let mut output = vec![0u8; 256];
        let result = program.eval(&input, &mut output).unwrap();
        assert!(result.matched);
        let output = &output[..result.output_len];
        assert!(has_field(output, 1));
        assert!(!has_field(output, 2));
    }
}
