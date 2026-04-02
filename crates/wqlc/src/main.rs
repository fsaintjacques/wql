use prost_reflect::{DynamicMessage, MessageDescriptor};
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
  --delimited      Varint length-delimited stream mode (eval only)
  --json           Output as JSON (eval only; requires -s and -m)
                   Single mode: one JSON object. Delimited: JSONL (one per line).

Single message mode (default):
  Reads one protobuf message from stdin, writes one result to stdout.
  Filter exit code: 0 = pass, 1 = filtered out.

Delimited stream mode (--delimited):
  Reads varint length-prefixed records from stdin.
  Projections: writes length-prefixed output records.
  Filters: writes length-prefixed records that pass.
  Combined: writes length-prefixed projected records that pass.

Examples:
  wqlc compile -q '{{ name, age }}' -o program.wql
  wqlc eval -q 'age > 18' < message.bin
  wqlc eval -q '{{ name }}' --delimited < stream.bin > filtered.bin
  wqlc eval -q '{{ name }}' -s schema.bin -m pkg.Person --json < msg.bin
  wqlc eval -q 'age > 18' -s schema.bin -m pkg.Person --json --delimited < stream.bin
  wqlc inspect program.wql"
    );
    ExitCode::from(2)
}

// ═══════════════════════════════════════════════════════════════════════
// compile
// ═══════════════════════════════════════════════════════════════════════

fn cmd_compile(args: &[String]) -> Result<ExitCode, String> {
    let opts = parse_common_opts(args, false)?;
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
    let opts = parse_common_opts(args, true)?;
    let query_str = opts.query.ok_or("missing -q <query>")?;

    let json_encoder = if opts.json {
        let schema_bytes = opts
            .schema_bytes
            .as_deref()
            .ok_or("--json requires -s <schema.bin>")?;
        let msg_name = opts
            .message
            .as_deref()
            .ok_or("--json requires -m <message>")?;
        Some(JsonEncoder::new(schema_bytes, msg_name)?)
    } else {
        None
    };

    let compile_opts = build_compile_opts(opts.schema_bytes.as_deref(), opts.message.as_deref());
    let bytecode = wql_compiler::compile(&query_str, &compile_opts)
        .map_err(|e| format!("compile error: {e}"))?;
    let program = wql_runtime::LoadedProgram::from_bytes(&bytecode)
        .map_err(|e| format!("load error: {e}"))?;
    let mode = classify_program(&program);

    if opts.delimited {
        eval_delimited(&program, mode, json_encoder.as_ref())
    } else {
        eval_single(&program, mode, json_encoder.as_ref())
    }
}

#[derive(Clone, Copy)]
enum QueryMode {
    Filter,
    Project,
    Combined,
}

// ═══════════════════════════════════════════════════════════════════════
// JSON output encoder
// ═══════════════════════════════════════════════════════════════════════

struct JsonEncoder {
    desc: MessageDescriptor,
}

impl JsonEncoder {
    fn new(schema_bytes: &[u8], message_name: &str) -> Result<Self, String> {
        let pool = prost_reflect::DescriptorPool::decode(schema_bytes)
            .map_err(|e| format!("decode descriptor: {e}"))?;
        let desc = pool
            .get_message_by_name(message_name)
            .ok_or_else(|| format!("message '{message_name}' not found in schema"))?;
        Ok(Self { desc })
    }

    fn proto_to_json(&self, bytes: &[u8]) -> Result<String, String> {
        let msg = DynamicMessage::decode(self.desc.clone(), bytes)
            .map_err(|e| format!("decode proto: {e}"))?;
        let opts = prost_reflect::SerializeOptions::new().skip_default_fields(true);
        let value = msg
            .serialize_with_options(serde_json::value::Serializer, &opts)
            .map_err(|e| format!("serialize JSON: {e}"))?;
        serde_json::to_string(&value).map_err(|e| format!("format JSON: {e}"))
    }
}

fn classify_program(program: &wql_runtime::LoadedProgram) -> QueryMode {
    let h = program.header();
    match (h.has_predicate(), h.has_projection()) {
        (true, true) => QueryMode::Combined,
        (true, false) => QueryMode::Filter,
        (false, true) => QueryMode::Project,
        (false, false) => QueryMode::Project,
    }
}

fn eval_single(
    program: &wql_runtime::LoadedProgram,
    mode: QueryMode,
    json: Option<&JsonEncoder>,
) -> Result<ExitCode, String> {
    let input = read_stdin()?;
    let mut stdout = io::stdout().lock();

    match mode {
        QueryMode::Combined => {
            let mut output = vec![0u8; input.len() * 2 + 256];
            let result = wql_runtime::project_and_filter(program, &input, &mut output)
                .map_err(|e| format!("runtime error: {e}"))?;
            match result {
                Some(len) => {
                    write_output(&mut stdout, &output[..len], json)?;
                    Ok(ExitCode::SUCCESS)
                }
                None => Ok(ExitCode::FAILURE),
            }
        }
        QueryMode::Filter => {
            let result =
                wql_runtime::filter(program, &input).map_err(|e| format!("runtime error: {e}"))?;
            if result {
                write_output(&mut stdout, &input, json)?;
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        QueryMode::Project => {
            let mut output = vec![0u8; input.len() * 2 + 256];
            let len = wql_runtime::project(program, &input, &mut output)
                .map_err(|e| format!("runtime error: {e}"))?;
            write_output(&mut stdout, &output[..len], json)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Write a record in streaming mode: JSON line or delimited proto.
fn write_stream_output(
    w: &mut impl Write,
    proto_bytes: &[u8],
    json: Option<&JsonEncoder>,
) -> Result<(), String> {
    if let Some(enc) = json {
        let json_str = enc.proto_to_json(proto_bytes)?;
        writeln!(w, "{json_str}").map_err(|e| format!("write: {e}"))
    } else {
        write_delimited_record(w, proto_bytes).map_err(|e| format!("write: {e}"))
    }
}

/// Write a single output (non-streaming): JSON line or raw proto.
fn write_output(
    w: &mut impl Write,
    proto_bytes: &[u8],
    json: Option<&JsonEncoder>,
) -> Result<(), String> {
    if let Some(enc) = json {
        let json_str = enc.proto_to_json(proto_bytes)?;
        writeln!(w, "{json_str}").map_err(|e| format!("write: {e}"))
    } else {
        w.write_all(proto_bytes).map_err(|e| format!("write: {e}"))
    }
}

fn eval_delimited(
    program: &wql_runtime::LoadedProgram,
    mode: QueryMode,
    json: Option<&JsonEncoder>,
) -> Result<ExitCode, String> {
    let mut stdin = io::BufReader::new(io::stdin().lock());
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    let mut record_buf = Vec::new();
    let mut output_buf = Vec::new();
    let mut i = 0usize;

    loop {
        let rec_len = match read_varint_from(&mut stdin) {
            #[allow(clippy::cast_possible_truncation)]
            Ok(n) => n as usize,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break, // clean EOF
            Err(e) => return Err(format!("record {i}: read varint: {e}")),
        };

        // Read exactly rec_len bytes
        record_buf.resize(rec_len, 0);
        stdin
            .read_exact(&mut record_buf)
            .map_err(|e| format!("record {i}: read: {e}"))?;

        // Ensure output buffer is large enough (only needed for project/combined)
        if !matches!(mode, QueryMode::Filter) {
            let out_cap = rec_len
                .checked_mul(2)
                .and_then(|n| n.checked_add(256))
                .ok_or_else(|| format!("record {i}: too large"))?;
            if output_buf.len() < out_cap {
                output_buf.resize(out_cap, 0);
            }
        }

        match mode {
            QueryMode::Project => {
                let len = wql_runtime::project(program, &record_buf, &mut output_buf)
                    .map_err(|e| format!("record {i}: project error: {e}"))?;
                write_stream_output(&mut stdout, &output_buf[..len], json)?;
            }
            QueryMode::Filter => {
                let pass = wql_runtime::filter(program, &record_buf)
                    .map_err(|e| format!("record {i}: filter error: {e}"))?;
                if pass {
                    write_stream_output(&mut stdout, &record_buf, json)?;
                }
            }
            QueryMode::Combined => {
                let result = wql_runtime::project_and_filter(program, &record_buf, &mut output_buf)
                    .map_err(|e| format!("record {i}: runtime error: {e}"))?;
                if let Some(len) = result {
                    write_stream_output(&mut stdout, &output_buf[..len], json)?;
                }
            }
        }
        i += 1;
    }

    stdout.flush().map_err(|e| format!("flush: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

// ═══════════════════════════════════════════════════════════════════════
// Varint length-delimited I/O
// ═══════════════════════════════════════════════════════════════════════

/// Read a varint from a byte stream. Returns `UnexpectedEof` at clean EOF
/// (no bytes read), or `InvalidData` on a truncated/malformed varint.
fn read_varint_from(r: &mut impl Read) -> io::Result<u64> {
    let mut val: u64 = 0;
    let mut shift = 0u32;
    let mut byte = [0u8; 1];

    loop {
        match r.read_exact(&mut byte) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && shift == 0 => {
                // Clean EOF before any bytes read
                return Err(e);
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated varint",
                ));
            }
            Err(e) => return Err(e),
        }
        val |= u64::from(byte[0] & 0x7F) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(val);
        }
        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "varint overflow",
            ));
        }
    }
}

fn write_delimited_record(w: &mut impl Write, record: &[u8]) -> io::Result<()> {
    let mut len_buf = [0u8; 10];
    let mut v = record.len() as u64;
    let mut i = 0;
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        len_buf[i] = byte;
        i += 1;
        if v == 0 {
            break;
        }
    }
    w.write_all(&len_buf[..i])?;
    w.write_all(record)?;
    Ok(())
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
    delimited: bool,
    json: bool,
}

fn parse_common_opts(args: &[String], allow_delimited: bool) -> Result<Opts, String> {
    let mut query = None;
    let mut schema_path = None;
    let mut message = None;
    let mut output = None;
    let mut delimited = false;
    let mut json = false;

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
            "--delimited" => {
                if !allow_delimited {
                    return Err("--delimited is only supported with 'eval'".into());
                }
                delimited = true;
            }
            "--json" => {
                if !allow_delimited {
                    return Err("--json is only supported with 'eval'".into());
                }
                json = true;
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
        delimited,
        json,
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
