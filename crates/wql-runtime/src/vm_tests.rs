use crate::test_utils::*;
use crate::{project, LoadedProgram, RuntimeError};
use alloc::vec;
    use alloc::vec::Vec;
    use wql_ir::{
        ArmAction, ArmMatch, DefaultAction, DispatchArm, Instruction,
    };

    /// Helper: encode instructions into a LoadedProgram.
    fn make_program(instrs: &[Instruction]) -> LoadedProgram {
        let bytes = wql_ir::encode(instrs);
        LoadedProgram::from_bytes(&bytes).unwrap()
    }

    /// Helper: run project and return the output bytes.
    fn run_project(program: &LoadedProgram, input: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        let mut output = vec![0u8; input.len()];
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
