use crate::test_utils::*;
use crate::{LoadedProgram, RuntimeError};
use alloc::vec;
use alloc::vec::Vec;
use wql_ir::{ArmAction, ArmMatch, DefaultAction, DispatchArm, Encoding, Instruction};

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
    let result = program.eval(input, &mut output)?;
    output.truncate(result.output_len);
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
fn eval_output_too_small_errors() {
    // Flat projection (depth=0) with undersized buffer → OutputBufferTooSmall.
    let program = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let input = encode_varint_field(1, 42);
    let mut output = [0u8; 1];
    let result = program.eval(&input, &mut output);
    assert_eq!(result, Err(RuntimeError::OutputBufferTooSmall));
}

#[test]
fn eval_filter_only_zero_allocation() {
    // Filter-only with &mut [] should succeed without allocation.
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpGt { reg: 0, imm: 10 });
    let input = make_person(25, b"Alice", 1);

    let result = prog.eval(&input, &mut []).unwrap();
    assert!(result.matched);
    assert_eq!(result.output_len, 0);
}

#[test]
fn eval_empty_input_with_depth_runs_vm() {
    // Empty input on a nested-predicate program should run the VM
    // and return the VM's predicate (true for empty input, no predicates pushed).
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(4),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::CmpLenEq {
            reg: 0,
            bytes: b"NYC".to_vec(),
        },
        Instruction::Return,
        Instruction::Label,
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::Len,
                }],
            }],
        },
        Instruction::Return,
    ]);

    // Empty input with undersized buffer — should NOT short-circuit to true.
    // The VM runs, no fields match, CmpLenEq sees unset register → false.
    let result = prog.eval(&[], &mut []).unwrap();
    assert!(!result.matched);
}

// ── Nested projection tests (FRAME) ──

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
fn frame_depth_exceeded() {
    // L0's sub-program re-enters L0 on field 2, so nesting deeper than
    // max_frame_depth (= 1 distinct label) triggers the error.
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
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(2),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::Return,
    ]);

    // 4 levels of nesting to exceed max_frame_depth of 1.
    let d3 = encode_varint_field(2, 1);
    let d2 = encode_len_field(2, &d3);
    let d1 = encode_len_field(2, &d2);
    let outer = encode_len_field(2, &d1);

    let mut output = vec![0u8; outer.len() + 64];
    let result = program.eval(&outer, &mut output);
    assert_eq!(result, Err(RuntimeError::FrameDepthExceeded));
}

// ── Filter / predicate tests ──
//
// Proto structure: message { age: uint32 = 1, name: string = 2, status: uint32 = 3 }

/// Build a filter program: DISPATCH(SKIP, [field→DECODE(R0, enc)]) + predicate + RETURN
fn make_filter_program(
    field_num: u32,
    encoding: Encoding,
    predicate: Instruction,
) -> LoadedProgram {
    make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(field_num),
                actions: vec![ArmAction::Decode { reg: 0, encoding }],
            }],
        },
        predicate,
        Instruction::Return,
    ])
}

fn make_person(age: u64, name: &[u8], status: u64) -> Vec<u8> {
    let mut msg = encode_varint_field(1, age);
    msg.extend_from_slice(&encode_len_field(2, name));
    msg.extend_from_slice(&encode_varint_field(3, status));
    msg
}

fn run_filter(program: &LoadedProgram, input: &[u8]) -> bool {
    program.eval(input, &mut []).unwrap().matched
}

#[test]
fn filter_cmp_eq_true() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpEq { reg: 0, imm: 25 });
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn filter_cmp_eq_false() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpEq { reg: 0, imm: 25 });
    assert!(!run_filter(&prog, &make_person(30, b"Alice", 1)));
}

#[test]
fn filter_cmp_gt() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpGt { reg: 0, imm: 18 });
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(10, b"Bob", 1)));
}

#[test]
fn filter_cmp_lt() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpLt { reg: 0, imm: 18 });
    assert!(run_filter(&prog, &make_person(10, b"Alice", 1)));
}

#[test]
fn filter_cmp_lte() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpLte { reg: 0, imm: 18 });
    assert!(run_filter(&prog, &make_person(18, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(19, b"Alice", 1)));
}

#[test]
fn filter_cmp_gte() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpGte { reg: 0, imm: 18 });
    assert!(run_filter(&prog, &make_person(18, b"Alice", 1)));
}

#[test]
fn filter_cmp_neq() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::CmpNeq { reg: 0, imm: 0 });
    assert!(run_filter(&prog, &make_person(5, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(0, b"Alice", 1)));
}

#[test]
fn filter_string_eq() {
    let prog = make_filter_program(
        2,
        Encoding::Len,
        Instruction::CmpLenEq {
            reg: 0,
            bytes: b"Alice".to_vec(),
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(25, b"Bob", 1)));
}

#[test]
fn filter_bytes_starts() {
    let prog = make_filter_program(
        2,
        Encoding::Len,
        Instruction::BytesStarts {
            reg: 0,
            bytes: b"Al".to_vec(),
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(25, b"Bob", 1)));
}

#[test]
fn filter_bytes_ends() {
    let prog = make_filter_program(
        2,
        Encoding::Len,
        Instruction::BytesEnds {
            reg: 0,
            bytes: b"ce".to_vec(),
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(25, b"Bob", 1)));
}

#[test]
fn filter_bytes_contains() {
    let prog = make_filter_program(
        2,
        Encoding::Len,
        Instruction::BytesContains {
            reg: 0,
            bytes: b"lic".to_vec(),
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(25, b"Bob", 1)));
}

#[test]
fn filter_in_set_hit() {
    let prog = make_filter_program(
        3,
        Encoding::Varint,
        Instruction::InSet {
            reg: 0,
            values: vec![1, 2, 3],
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 2)));
}

#[test]
fn filter_in_set_miss() {
    let prog = make_filter_program(
        3,
        Encoding::Varint,
        Instruction::InSet {
            reg: 0,
            values: vec![1, 2, 3],
        },
    );
    assert!(!run_filter(&prog, &make_person(25, b"Alice", 5)));
}

#[test]
fn filter_in_set_empty() {
    let prog = make_filter_program(
        3,
        Encoding::Varint,
        Instruction::InSet {
            reg: 0,
            values: vec![],
        },
    );
    assert!(!run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn filter_is_set_true() {
    let prog = make_filter_program(1, Encoding::Varint, Instruction::IsSet { reg: 0 });
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn filter_is_set_false() {
    // Field 5 not present → R0 never set.
    let prog = make_filter_program(5, Encoding::Varint, Instruction::IsSet { reg: 0 });
    assert!(!run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn filter_and() {
    // age > 18 AND name == "Alice"
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![
                DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint,
                    }],
                },
                DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Decode {
                        reg: 1,
                        encoding: Encoding::Len,
                    }],
                },
            ],
        },
        Instruction::CmpGt { reg: 0, imm: 18 },
        Instruction::CmpLenEq {
            reg: 1,
            bytes: b"Alice".to_vec(),
        },
        Instruction::And,
        Instruction::Return,
    ]);

    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(25, b"Bob", 1)));
    assert!(!run_filter(&prog, &make_person(10, b"Alice", 1)));
}

#[test]
fn filter_or() {
    // age > 65 OR status == 1
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![
                DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint,
                    }],
                },
                DispatchArm {
                    match_: ArmMatch::Field(3),
                    actions: vec![ArmAction::Decode {
                        reg: 1,
                        encoding: Encoding::Varint,
                    }],
                },
            ],
        },
        Instruction::CmpGt { reg: 0, imm: 65 },
        Instruction::CmpEq { reg: 1, imm: 1 },
        Instruction::Or,
        Instruction::Return,
    ]);

    // age=30, status=1 → true (second clause)
    assert!(run_filter(&prog, &make_person(30, b"Alice", 1)));
    // age=70, status=0 → true (first clause)
    assert!(run_filter(&prog, &make_person(70, b"Alice", 0)));
    // age=30, status=0 → false
    assert!(!run_filter(&prog, &make_person(30, b"Alice", 0)));
}

#[test]
fn filter_not() {
    // NOT age == 0
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::Varint,
                }],
            }],
        },
        Instruction::CmpEq { reg: 0, imm: 0 },
        Instruction::Not,
        Instruction::Return,
    ]);

    assert!(run_filter(&prog, &make_person(5, b"Alice", 1)));
    assert!(!run_filter(&prog, &make_person(0, b"Alice", 1)));
}

#[test]
fn filter_nested_predicate() {
    // Predicate on address.city: DISPATCH + FRAME + sub-DISPATCH with DECODE.
    // message { address: { city: string = 1 } = 4 }
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(4),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::CmpLenEq {
            reg: 0,
            bytes: b"NYC".to_vec(),
        },
        Instruction::Return,
        Instruction::Label, // L0 — address sub-program
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::Len,
                }],
            }],
        },
        Instruction::Return,
    ]);

    let addr_nyc = encode_len_field(1, b"NYC");
    let mut input = encode_len_field(4, &addr_nyc);
    assert!(run_filter(&prog, &input));

    let addr_la = encode_len_field(1, b"LA");
    input = encode_len_field(4, &addr_la);
    assert!(!run_filter(&prog, &input));
}

#[test]
fn filter_unset_register() {
    // CmpEq on unset reg → false. CmpNeq on unset → true. IsSet on unset → false.
    let prog_eq = make_filter_program(5, Encoding::Varint, Instruction::CmpEq { reg: 0, imm: 0 });
    let prog_neq = make_filter_program(5, Encoding::Varint, Instruction::CmpNeq { reg: 0, imm: 0 });
    let prog_isset = make_filter_program(5, Encoding::Varint, Instruction::IsSet { reg: 0 });

    let input = make_person(25, b"Alice", 1);
    assert!(!run_filter(&prog_eq, &input));
    assert!(run_filter(&prog_neq, &input));
    assert!(!run_filter(&prog_isset, &input));
}

#[test]
fn filter_type_mismatch() {
    // CmpEq (int comparison) on a Bytes register → false.
    let prog_int_on_bytes =
        make_filter_program(2, Encoding::Len, Instruction::CmpEq { reg: 0, imm: 42 });
    assert!(!run_filter(
        &prog_int_on_bytes,
        &make_person(25, b"Alice", 1)
    ));

    // CmpLenEq on an Int register → false.
    let prog_bytes_on_int = make_filter_program(
        1,
        Encoding::Varint,
        Instruction::CmpLenEq {
            reg: 0,
            bytes: b"test".to_vec(),
        },
    );
    assert!(!run_filter(
        &prog_bytes_on_int,
        &make_person(25, b"Alice", 1)
    ));
}

// ── Combined tests ──

#[test]
fn project_and_filter_pass() {
    // age > 18 → predicate true, project copies field 2 (name).
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![
                DispatchArm {
                    match_: ArmMatch::Field(1),
                    actions: vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint,
                    }],
                },
                DispatchArm {
                    match_: ArmMatch::Field(2),
                    actions: vec![ArmAction::Copy],
                },
            ],
        },
        Instruction::CmpGt { reg: 0, imm: 18 },
        Instruction::Return,
    ]);

    let input = make_person(25, b"Alice", 1);
    let mut output = vec![0u8; input.len() + 64];
    let result = prog.eval(&input, &mut output).unwrap();

    let expected = encode_len_field(2, b"Alice");
    assert!(result.matched);
    assert_eq!(result.output_len, expected.len());
    assert_eq!(&output[..expected.len()], expected.as_slice());
}

#[test]
fn project_and_filter_fail() {
    // age > 18 → predicate false.
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::Varint,
                }],
            }],
        },
        Instruction::CmpGt { reg: 0, imm: 18 },
        Instruction::Return,
    ]);

    let input = make_person(10, b"Alice", 1);
    let mut output = vec![0u8; input.len() + 64];
    let result = prog.eval(&input, &mut output).unwrap();
    assert!(!result.matched);
}

// ── Integration / edge cases ──

#[test]
fn frame_preserves_registers() {
    // DECODE inside FRAME, CMP outside FRAME uses the register.
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(2),
                actions: vec![ArmAction::Frame(0)],
            }],
        },
        Instruction::CmpLenEq {
            reg: 0,
            bytes: b"Alice".to_vec(),
        },
        Instruction::Return,
        Instruction::Label, // L0
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::Len,
                }],
            }],
        },
        Instruction::Return,
    ]);

    let input = make_outer_inner_input(42, b"Alice", 99);
    assert!(run_filter(&prog, &input));
}

#[test]
fn empty_input_filter() {
    // Empty input → filter returns true (no predicates pushed).
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![],
        },
        Instruction::Return,
    ]);
    assert!(run_filter(&prog, &[]));
}

#[test]
fn empty_program() {
    // [RETURN] → true, 0 bytes.
    let prog = make_program(&[Instruction::Return]);
    assert!(run_filter(&prog, &[]));
}

#[test]
fn bool_stack_empty() {
    // Pure projection (no predicates) → predicate is true.
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Copy,
            arms: vec![],
        },
        Instruction::Return,
    ]);

    let input = encode_varint_field(1, 42);
    let mut output = vec![0u8; input.len() + 64];
    let result = prog.eval(&input, &mut output).unwrap();
    assert!(result.matched);
    assert_eq!(result.output_len, input.len());
}

#[test]
fn stack_underflow() {
    // AND with empty stack → StackUnderflow.
    let prog = make_program(&[Instruction::And, Instruction::Return]);
    let result = prog.eval(&[], &mut []);
    assert_eq!(result, Err(RuntimeError::StackUnderflow));
}

// ── Decode encoding coverage ──

#[test]
fn filter_decode_sint() {
    // Sint zigzag: -1 encodes as zigzag varint 1.
    let prog = make_filter_program(1, Encoding::Sint, Instruction::CmpEq { reg: 0, imm: -1 });
    let input = encode_sint_field(1, -1);
    assert!(run_filter(&prog, &input));

    // Positive: 25 encodes as zigzag varint 50.
    let prog2 = make_filter_program(1, Encoding::Sint, Instruction::CmpEq { reg: 0, imm: 25 });
    let input2 = encode_sint_field(1, 25);
    assert!(run_filter(&prog2, &input2));
}

#[test]
fn filter_decode_i32() {
    // Fixed32 field, sign-extend to i64.
    use wql_ir::WireType;
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::FieldAndWireType(1, WireType::I32),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::I32,
                }],
            }],
        },
        Instruction::CmpEq { reg: 0, imm: -1 },
        Instruction::Return,
    ]);
    // 0xFFFFFFFF as i32 = -1, sign-extended to i64 = -1.
    let input = encode_fixed32_field(1, 0xFFFF_FFFF);
    assert!(run_filter(&prog, &input));
}

#[test]
fn filter_decode_i64() {
    use wql_ir::WireType;
    let prog = make_program(&[
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::FieldAndWireType(1, WireType::I64),
                actions: vec![ArmAction::Decode {
                    reg: 0,
                    encoding: Encoding::I64,
                }],
            }],
        },
        Instruction::CmpEq {
            reg: 0,
            imm: 0x0123_4567_89AB_CDEF,
        },
        Instruction::Return,
    ]);
    let input = encode_fixed64_field(1, 0x0123_4567_89AB_CDEF);
    assert!(run_filter(&prog, &input));
}

#[test]
fn filter_cmp_neq_type_mismatch() {
    // CmpNeq on Bytes register → true (type mismatch means not equal).
    let prog = make_filter_program(2, Encoding::Len, Instruction::CmpNeq { reg: 0, imm: 0 });
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn filter_bytes_contains_empty_pattern() {
    // BytesContains with empty pattern → true if register is Bytes.
    let prog = make_filter_program(
        2,
        Encoding::Len,
        Instruction::BytesContains {
            reg: 0,
            bytes: vec![],
        },
    );
    assert!(run_filter(&prog, &make_person(25, b"Alice", 1)));
}

#[test]
fn decode_reg_out_of_bounds() {
    // reg >= 16 should not panic — construct Vm directly since
    // wql_ir::encode rejects reg >= 16 at compile time.
    use crate::vm::Vm;

    let instructions = vec![
        Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms: vec![DispatchArm {
                match_: ArmMatch::Field(1),
                actions: vec![ArmAction::Decode {
                    reg: 99,
                    encoding: Encoding::Varint,
                }],
            }],
        },
        // CmpEq on reg 99 should also not panic (treated as unset → false).
        Instruction::CmpEq { reg: 99, imm: 42 },
        Instruction::Return,
    ];
    let label_table = vec![];
    let mut vm = Vm::new(&instructions, &label_table, 0);

    let input = encode_varint_field(1, 42);
    let mut output = vec![0u8; input.len() + 64];
    let (predicate, _) = vm.execute(0, &input, &mut output, 0).unwrap();
    // Decode silently skipped (reg out of bounds), CmpEq on unset → false.
    assert!(!predicate);
}
