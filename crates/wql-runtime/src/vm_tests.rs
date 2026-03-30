use crate::test_utils::*;
use crate::{project, LoadedProgram, RuntimeError};
use alloc::vec;
use alloc::vec::Vec;
use wql_ir::{ArmAction, ArmMatch, DefaultAction, DispatchArm, Instruction};

/// Helper: encode instructions into a `LoadedProgram`.
fn make_program(instrs: &[Instruction]) -> LoadedProgram {
    let bytes = wql_ir::encode(instrs);
    LoadedProgram::from_bytes(&bytes).unwrap()
}

/// Helper: run project and return the output bytes.
/// Adds padding beyond `input.len()` to accommodate FRAME's 5-byte
/// gap-and-shift strategy which needs temporary extra space.
fn run_project(program: &LoadedProgram, input: &[u8]) -> Result<Vec<u8>, RuntimeError> {
    let mut output = vec![0u8; input.len() + 64];
    let written = project(program, input, &mut output)?;
    output.truncate(written);
    Ok(output)
}

// ── Flat projection tests ──

#[test]
fn project_flat_strict() {
    // DISPATCH(SKIP, [1→COPY, 2→COPY]) on {1:varint, 2:LEN, 3:varint}
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![
                DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                },
                DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Copy],
                },
            ],
        },
        Instruction::Return,
    ]);

    let mut input = encode_varint_field(1, 42);
    let field2 = encode_len_field(2, b"hello");
    let field3 = encode_varint_field(3, 99);
    input.extend_from_slice(&field2);
    input.extend_from_slice(&field3);

    let output = run_project(&program, &input).unwrap();

    // Output should have fields 1 and 2 only.
    let mut expected = encode_varint_field(1, 42);
    expected.extend_from_slice(&field2);
    assert_eq!(output, expected);
}

#[test]
fn project_flat_preserve() {
    // DISPATCH(COPY, [3→SKIP]) on {1:varint, 2:LEN, 3:varint}
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(3),
                actions: vec![ArmAction::Skip],
            }],
        },
        Instruction::Return,
    ]);

    let field1 = encode_varint_field(1, 42);
    let field2 = encode_len_field(2, b"hello");
    let field3 = encode_varint_field(3, 99);
    let mut input = field1.clone();
    input.extend_from_slice(&field2);
    input.extend_from_slice(&field3);

    let output = run_project(&program, &input).unwrap();

    let mut expected = field1;
    expected.extend_from_slice(&field2);
    assert_eq!(output, expected);
}

#[test]
fn project_identity() {
    // DISPATCH(COPY, []) — output equals input.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let mut input = encode_varint_field(1, 42);
    input.extend_from_slice(&encode_len_field(2, b"world"));
    input.extend_from_slice(&encode_fixed32_field(3, 0xDEAD));

    let output = run_project(&program, &input).unwrap();
    assert_eq!(output, input);
}

#[test]
fn project_drop_all() {
    // DISPATCH(SKIP, []) — output is empty.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let mut input = encode_varint_field(1, 42);
    input.extend_from_slice(&encode_len_field(2, b"world"));

    let output = run_project(&program, &input).unwrap();
    assert!(output.is_empty());
}

#[test]
fn project_repeated_field() {
    // Field 1 appears 3 times. DISPATCH(SKIP, [1→COPY]). All 3 copied.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    let mut input = encode_varint_field(1, 10);
    input.extend_from_slice(&encode_varint_field(1, 20));
    input.extend_from_slice(&encode_varint_field(1, 30));

    let output = run_project(&program, &input).unwrap();
    assert_eq!(output, input);
}

#[test]
fn project_empty_input() {
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let output = run_project(&program, &[]).unwrap();
    assert!(output.is_empty());
}

#[test]
fn project_output_too_small() {
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let input = encode_varint_field(1, 42);
    let mut output = [0u8; 1]; // too small
    let result = project(&program, &input, &mut output);
    assert_eq!(result, Err(RuntimeError::OutputBufferTooSmall));
}

// ── Nested projection tests (FRAME / RECURSE) ──

/// Build an Outer { id: u32 = 1, inner: Inner = 2 }
/// Inner { name: string = 1, value: u32 = 2 }
fn make_outer_inner_input(id: u64, name: &[u8], value: u64) -> Vec<u8> {
    let mut inner = encode_len_field(1, name);
    inner.extend_from_slice(&encode_varint_field(2, value));
    let mut outer = encode_varint_field(1, id);
    outer.extend_from_slice(&encode_len_field(2, &inner));
    outer
}

#[test]
fn frame_simple() {
    // Outer: DISPATCH(SKIP, [1→COPY, 2→FRAME(L0)])
    // L0: Inner sub-program: DISPATCH(SKIP, [1→COPY])
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![
                DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Copy],
                },
                DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Frame(0)],
                },
            ],
        },
        Instruction::Return,
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    let input = make_outer_inner_input(42, b"Alice", 99);
    let output = run_project(&program, &input).unwrap();

    // Expected: Outer { id=42, inner { name="Alice" } }
    let inner_projected = encode_len_field(1, b"Alice");
    let mut expected = encode_varint_field(1, 42);
    expected.extend_from_slice(&encode_len_field(2, &inner_projected));
    assert_eq!(output, expected);
}

#[test]
fn frame_nested_two() {
    // Three levels: Outer { mid: Middle = 1 }
    // Middle { inner: Inner = 1 }
    // Inner { val: u32 = 1 }
    // Program: keep val through two frames.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::Return,
        Instruction::Label, // L0 — middle
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Frame(1)],
            }],
        },
        Instruction::Return,
        Instruction::Label, // L1 — inner
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    let inner = encode_varint_field(1, 7);
    let middle = encode_len_field(1, &inner);
    let outer = encode_len_field(1, &middle);

    let output = run_project(&program, &outer).unwrap();

    // Rebuild expected: Outer { Middle { Inner { val=7 } } }
    let exp_inner = encode_varint_field(1, 7);
    let exp_middle = encode_len_field(1, &exp_inner);
    let exp_outer = encode_len_field(1, &exp_middle);
    assert_eq!(output, exp_outer);
}

#[test]
fn frame_empty_sub() {
    // FRAME into sub-message where nothing is copied → 0-byte sub-output.
    // Tag + length(0) still emitted.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(2),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::Return,
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let input = make_outer_inner_input(42, b"Alice", 99);
    let output = run_project(&program, &input).unwrap();

    // Expected: just field 2 with empty sub-message.
    let expected = encode_len_field(2, &[]);
    assert_eq!(output, expected);
}

#[test]
fn recurse_deep_search() {
    // LABEL(L0), DISPATCH(RECURSE(L0), [1→COPY])
    // Field 1 exists only at depth 3.
    let program = make_program(&[
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Recurse(0),
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    // depth 0: { 2: { 2: { 1: 42 } } }
    let innermost = encode_varint_field(1, 42);
    let mid = encode_len_field(2, &innermost);
    let outer = encode_len_field(2, &mid);

    let output = run_project(&program, &outer).unwrap();

    // RECURSE re-emits the nesting structure for LEN fields.
    let exp_inner = encode_varint_field(1, 42);
    let exp_mid = encode_len_field(2, &exp_inner);
    let exp_outer = encode_len_field(2, &exp_mid);
    assert_eq!(output, exp_outer);
}

#[test]
fn recurse_no_match() {
    // RECURSE over nested messages with no field 1 anywhere → output empty.
    let program = make_program(&[
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Recurse(0),
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    // Only field 2 and 3, no field 1 anywhere.
    let inner = encode_varint_field(3, 10);
    let outer = encode_len_field(2, &inner);

    let output = run_project(&program, &outer).unwrap();

    // Field 2 recurses into sub-message, finds no field 1 → sub-output is 0 bytes.
    // The enclosing RECURSE emits tag + length(0) for the LEN field.
    let expected = encode_len_field(2, &[]);
    assert_eq!(output, expected);
}

#[test]
fn frame_depth_exceeded() {
    // Program with RECURSE, but input nests deeper than max_frame_depth.
    let program = make_program(&[
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Recurse(0),
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Copy],
            }],
        },
        Instruction::Return,
    ]);

    // 10 levels of nesting — will exceed max_frame_depth.
    let mut msg = encode_varint_field(1, 1);
    for _ in 0..10 {
        msg = encode_len_field(2, &msg);
    }

    let mut output = vec![0u8; msg.len() + 64];
    let result = project(&program, &msg, &mut output);
    assert_eq!(result, Err(RuntimeError::FrameDepthExceeded));
}
