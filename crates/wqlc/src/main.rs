use std::io::{self, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        return usage();
    }

    let result = match args[1].as_str() {
        "compile" => cmd_compile(&args[2..]),
        "eval" => cmd_eval(&args[2..]),
        "inspect" => cmd_inspect(&args[2..]),
        "help" | "--help" | "-h" => return usage(),
        other => {
            eprintln!("wqlc: unknown command '{other}'");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("wqlc: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "\
Usage: wqlc <command> [options]

Commands:
  compile   Compile a WQL query to bytecode
  eval      Compile and execute a WQL query
  inspect   Disassemble a compiled WQL program

Options (compile, eval):
  -q <query>       WQL query string (required)
  -s <schema.bin>  FileDescriptorSet for schema-bound mode
  -m <message>     Root message type (required with -s)
  -o <output>      Output file (compile only; default: stdout)

Examples:
  wqlc compile -q '{{ name, age }}' -o program.wql
  wqlc compile -q '{{ name }}' -s schema.bin -m pkg.Person -o program.wql
  wqlc eval -q 'age > 18' < message.bin
  wqlc eval -q '{{ name }}' -s schema.bin -m pkg.Person < message.bin
  wqlc inspect program.wql"
    );
    ExitCode::from(2)
}

// ═══════════════════════════════════════════════════════════════════════
// compile
// ═══════════════════════════════════════════════════════════════════════

fn cmd_compile(args: &[String]) -> Result<ExitCode, String> {
    let opts = parse_common_opts(args)?;
    let query = opts.query.ok_or("missing -q <query>")?;
    let output_path = opts.output.as_deref();

    let compile_opts = build_compile_opts(opts.schema_bytes.as_deref(), opts.message.as_deref());
    let bytecode =
        wql_compiler::compile(&query, &compile_opts).map_err(|e| format!("compile error: {e}"))?;

    if let Some(path) = output_path {
        std::fs::write(path, &bytecode).map_err(|e| format!("write {path}: {e}"))?;
        eprintln!("wrote {} bytes to {path}", bytecode.len());
    } else {
        io::stdout()
            .write_all(&bytecode)
            .map_err(|e| format!("write stdout: {e}"))?;
    }

    Ok(ExitCode::SUCCESS)
}

// ═══════════════════════════════════════════════════════════════════════
// eval
// ═══════════════════════════════════════════════════════════════════════

fn cmd_eval(args: &[String]) -> Result<ExitCode, String> {
    let opts = parse_common_opts(args)?;
    let query = opts.query.ok_or("missing -q <query>")?;

    let compile_opts = build_compile_opts(opts.schema_bytes.as_deref(), opts.message.as_deref());
    let bytecode =
        wql_compiler::compile(&query, &compile_opts).map_err(|e| format!("compile error: {e}"))?;
    let program = wql_runtime::LoadedProgram::from_bytes(&bytecode)
        .map_err(|e| format!("load error: {e}"))?;

    let input = read_stdin()?;

    let is_filter_only = !query.contains('{');
    let is_combined = query.contains("WHERE") && query.contains("SELECT");

    if is_combined {
        let mut output = vec![0u8; input.len() * 2 + 256];
        let result = wql_runtime::project_and_filter(&program, &input, &mut output)
            .map_err(|e| format!("runtime error: {e}"))?;
        match result {
            Some(len) => {
                io::stdout()
                    .write_all(&output[..len])
                    .map_err(|e| format!("write: {e}"))?;
                Ok(ExitCode::SUCCESS)
            }
            None => {
                // Filtered out
                Ok(ExitCode::FAILURE)
            }
        }
    } else if is_filter_only {
        let result =
            wql_runtime::filter(&program, &input).map_err(|e| format!("runtime error: {e}"))?;
        if result {
            Ok(ExitCode::SUCCESS)
        } else {
            Ok(ExitCode::FAILURE)
        }
    } else {
        let mut output = vec![0u8; input.len() * 2 + 256];
        let len = wql_runtime::project(&program, &input, &mut output)
            .map_err(|e| format!("runtime error: {e}"))?;
        io::stdout()
            .write_all(&output[..len])
            .map_err(|e| format!("write: {e}"))?;
        Ok(ExitCode::SUCCESS)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// inspect
// ═══════════════════════════════════════════════════════════════════════

fn cmd_inspect(args: &[String]) -> Result<ExitCode, String> {
    if args.is_empty() {
        return Err("missing program file".into());
    }
    let path = &args[0];
    let bytecode = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let (header, instructions) =
        wql_ir::decode(&bytecode).map_err(|e| format!("decode error: {e}"))?;

    println!("Program: {path}");
    println!("  size:            {} bytes", bytecode.len());
    println!("  version:         {}", header.version);
    println!("  registers:       {}", header.register_count);
    println!("  max_frame_depth: {}", header.max_frame_depth);
    println!("  flags:           0x{:04X}", header.flags);
    println!("  bytecode_len:    {}", header.bytecode_len);
    println!();
    println!("Instructions ({}):", instructions.len());

    let mut label_idx = 0u32;
    for (i, instr) in instructions.iter().enumerate() {
        let prefix = if matches!(instr, wql_ir::Instruction::Label) {
            let s = format!("L{label_idx}:");
            label_idx += 1;
            format!("{s:<6}")
        } else {
            "      ".to_string()
        };
        println!("  {prefix} [{i:3}] {}", format_instruction(instr));
    }

    Ok(ExitCode::SUCCESS)
}

fn format_instruction(instr: &wql_ir::Instruction) -> String {
    use wql_ir::Instruction;
    match instr {
        Instruction::Dispatch { default, arms } => {
            let def = format_default(default);
            if arms.is_empty() {
                format!("DISPATCH default={def}")
            } else {
                let arms_str: Vec<String> = arms.iter().map(format_arm).collect();
                format!("DISPATCH default={def} [{}]", arms_str.join(", "))
            }
        }
        Instruction::Label => "LABEL".into(),
        Instruction::Copy => "COPY".into(),
        Instruction::Skip => "SKIP".into(),
        Instruction::Decode { reg, encoding } => format!("DECODE R{reg} {encoding:?}"),
        Instruction::CmpEq { reg, imm } => format!("CMP_EQ R{reg} {imm}"),
        Instruction::CmpNeq { reg, imm } => format!("CMP_NEQ R{reg} {imm}"),
        Instruction::CmpLt { reg, imm } => format!("CMP_LT R{reg} {imm}"),
        Instruction::CmpLte { reg, imm } => format!("CMP_LTE R{reg} {imm}"),
        Instruction::CmpGt { reg, imm } => format!("CMP_GT R{reg} {imm}"),
        Instruction::CmpGte { reg, imm } => format!("CMP_GTE R{reg} {imm}"),
        Instruction::CmpLenEq { reg, bytes } => {
            format!("CMP_LEN_EQ R{reg} {:?}", String::from_utf8_lossy(bytes))
        }
        Instruction::BytesStarts { reg, bytes } => {
            format!("BYTES_STARTS R{reg} {:?}", String::from_utf8_lossy(bytes))
        }
        Instruction::BytesEnds { reg, bytes } => {
            format!("BYTES_ENDS R{reg} {:?}", String::from_utf8_lossy(bytes))
        }
        Instruction::BytesContains { reg, bytes } => {
            format!("BYTES_CONTAINS R{reg} {:?}", String::from_utf8_lossy(bytes))
        }
        Instruction::BytesMatches { reg, pattern } => {
            format!(
                "BYTES_MATCHES R{reg} {:?}",
                String::from_utf8_lossy(pattern)
            )
        }
        Instruction::InSet { reg, values } => format!("IN_SET R{reg} {values:?}"),
        Instruction::IsSet { reg } => format!("IS_SET R{reg}"),
        Instruction::And => "AND".into(),
        Instruction::Or => "OR".into(),
        Instruction::Not => "NOT".into(),
        Instruction::Return => "RETURN".into(),
    }
}

fn format_default(d: &wql_ir::DefaultAction) -> String {
    match d {
        wql_ir::DefaultAction::Skip => "Skip".into(),
        wql_ir::DefaultAction::Copy => "Copy".into(),
        wql_ir::DefaultAction::Recurse(idx) => format!("Recurse(L{idx})"),
    }
}

fn format_arm(arm: &wql_ir::DispatchArm) -> String {
    let match_str = match &arm.match_ {
        wql_ir::ArmMatch::Field(n) => format!("#{n}"),
        wql_ir::ArmMatch::FieldAndWireType(n, wt) => format!("#{n}/{wt:?}"),
    };
    let actions: Vec<String> = arm.actions.iter().map(format_action).collect();
    format!("{match_str}->[{}]", actions.join(","))
}

fn format_action(a: &wql_ir::ArmAction) -> String {
    match a {
        wql_ir::ArmAction::Copy => "Copy".into(),
        wql_ir::ArmAction::Skip => "Skip".into(),
        wql_ir::ArmAction::Decode { reg, encoding } => format!("Decode(R{reg},{encoding:?})"),
        wql_ir::ArmAction::Frame(idx) => format!("Frame(L{idx})"),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Argument parsing
// ═══════════════════════════════════════════════════════════════════════

struct Opts {
    query: Option<String>,
    schema_bytes: Option<Vec<u8>>,
    message: Option<String>,
    output: Option<String>,
}

fn parse_common_opts(args: &[String]) -> Result<Opts, String> {
    let mut query = None;
    let mut schema_path = None;
    let mut message = None;
    let mut output = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-q" => {
                i += 1;
                query = Some(args.get(i).ok_or("missing value for -q")?.clone());
            }
            "-s" => {
                i += 1;
                schema_path = Some(args.get(i).ok_or("missing value for -s")?.clone());
            }
            "-m" => {
                i += 1;
                message = Some(args.get(i).ok_or("missing value for -m")?.clone());
            }
            "-o" => {
                i += 1;
                output = Some(args.get(i).ok_or("missing value for -o")?.clone());
            }
            other => return Err(format!("unknown option '{other}'")),
        }
        i += 1;
    }

    let schema_bytes = schema_path
        .map(|p| std::fs::read(&p).map_err(|e| format!("read schema {p}: {e}")))
        .transpose()?;

    Ok(Opts {
        query,
        schema_bytes,
        message,
        output,
    })
}

fn build_compile_opts<'a>(
    schema_bytes: Option<&'a [u8]>,
    message: Option<&'a str>,
) -> wql_compiler::CompileOptions<'a> {
    wql_compiler::CompileOptions {
        schema: schema_bytes,
        root_message: message,
    }
}

fn read_stdin() -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    io::stdin()
        .read_to_end(&mut buf)
        .map_err(|e| format!("read stdin: {e}"))?;
    Ok(buf)
}
