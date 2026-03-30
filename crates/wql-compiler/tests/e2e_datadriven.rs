//! Data-driven end-to-end tests.
//!
//! Test cases are defined in `testdata/e2e.txt`. Format:
//!
//! ```text
//! # message: testdata.Person      ← sets message type for subsequent cases
//!
//! { name, age }                   ← WQL query
//! {"name": "Alice", "age": 30}    ← input record(s), one JSON per line
//! {"name": "Bob", "age": 17}
//! ----                            ← separator
//! {"name": "Alice", "age": 30}    ← expected output(s), 1:1 with inputs
//! {"name": "Bob", "age": 17}
//!                                 ← blank line ends the case
//! age > 18                        ← filter query
//! {"age": 30}
//! {"age": 17}
//! ----
//! true                            ← filter results: true/false
//! false
//!
//! WHERE age > 18 SELECT { name }  ← combined query
//! {"name": "Alice", "age": 30}
//! {"name": "Bob", "age": 17}
//! ----
//! {"name": "Alice"}               ← output JSON or <none>
//! <none>
//! ```

use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::Value;
use wql_compiler::{compile, CompileOptions};

const DESCRIPTOR_BYTES: &[u8] = include_bytes!("testdata/testdata.bin");

fn descriptor_pool() -> prost_reflect::DescriptorPool {
    prost_reflect::DescriptorPool::decode(DESCRIPTOR_BYTES).expect("failed to decode descriptor")
}

// ═══════════════════════════════════════════════════════════════════════
// Parser
// ═══════════════════════════════════════════════════════════════════════

struct TestCase {
    query: String,
    message: String,
    inputs: Vec<String>,
    expected: Vec<String>,
    line: usize,
}

fn parse_test_file(content: &str) -> Vec<TestCase> {
    let mut cases = Vec::new();
    let mut current_message = String::new();
    let mut lines = content.lines().enumerate().peekable();

    loop {
        // Skip blank lines and comments, watching for `# message:` directives
        loop {
            match lines.peek() {
                None => return cases,
                Some(&(_, line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        lines.next();
                    } else if let Some(msg) = trimmed.strip_prefix("# message:") {
                        current_message = msg.trim().to_string();
                        lines.next();
                    } else if trimmed.starts_with('#') {
                        lines.next();
                    } else {
                        break;
                    }
                }
            }
        }

        assert!(
            !current_message.is_empty(),
            "no `# message:` directive before first test case"
        );

        // First non-blank, non-comment line is the WQL query
        let (query_line, query) = lines.next().unwrap();
        let query = query.trim().to_string();

        // Collect input lines until "----"
        let mut inputs = Vec::new();
        loop {
            match lines.peek() {
                None => panic!("line {}: unexpected EOF, expected '----'", query_line + 1),
                Some(&(_, line)) => {
                    if line.trim() == "----" {
                        lines.next();
                        break;
                    }
                    inputs.push(line.trim().to_string());
                    lines.next();
                }
            }
        }

        // Collect expected output lines until blank line, comment, or EOF
        let mut expected = Vec::new();
        loop {
            match lines.peek() {
                None => break,
                Some(&(_, line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') {
                        break;
                    }
                    expected.push(trimmed.to_string());
                    lines.next();
                }
            }
        }

        assert_eq!(
            inputs.len(),
            expected.len(),
            "line {}: {} inputs but {} expected outputs for query: {query}",
            query_line + 1,
            inputs.len(),
            expected.len(),
        );

        cases.push(TestCase {
            query,
            message: current_message.clone(),
            inputs,
            expected,
            line: query_line + 1,
        });
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Proto / JSON helpers
// ═══════════════════════════════════════════════════════════════════════

fn resolve_message(pool: &prost_reflect::DescriptorPool, name: &str) -> MessageDescriptor {
    pool.get_message_by_name(name)
        .unwrap_or_else(|| panic!("message type '{name}' not found in descriptor"))
}

fn json_to_proto(desc: &MessageDescriptor, json: &str) -> Vec<u8> {
    let mut de = serde_json::Deserializer::from_str(json);
    let msg = DynamicMessage::deserialize(desc.clone(), &mut de)
        .unwrap_or_else(|e| panic!("bad JSON for {}: {e}\n  json: {json}", desc.name()));
    msg.encode_to_vec()
}

fn proto_to_json(desc: &MessageDescriptor, bytes: &[u8]) -> Value {
    let msg = DynamicMessage::decode(desc.clone(), bytes)
        .unwrap_or_else(|e| panic!("bad proto for {}: {e}", desc.name()));
    let ser = serde_json::value::Serializer;
    let opts = prost_reflect::SerializeOptions::new().skip_default_fields(true);
    msg.serialize_with_options(ser, &opts)
        .unwrap_or_else(|e| panic!("JSON serialize failed: {e}"))
}

/// Normalize for comparison: strip defaults, coerce int64 strings to numbers.
/// Note: this coerces ALL numeric strings, so a proto `string` field containing
/// `"42"` would compare equal to an `int64` field with value `42`. Acceptable
/// since the test schema doesn't mix these, but be aware when adding test cases.
fn normalize(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let m: serde_json::Map<String, Value> = map
                .iter()
                .filter(|(_, v)| !is_default(v))
                .map(|(k, v)| (k.clone(), normalize(v)))
                .collect();
            Value::Object(m)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(normalize).collect()),
        Value::String(s) => s
            .parse::<i64>()
            .map_or_else(|_| v.clone(), |n| Value::Number(n.into())),
        other => other.clone(),
    }
}

fn is_default(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        Value::Bool(b) => !b,
        Value::Array(a) => a.is_empty(),
        Value::Object(m) => m.is_empty(),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Runner
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn run_e2e_datadriven() {
    let content = include_str!("testdata/e2e.txt");
    let cases = parse_test_file(content);
    let pool = descriptor_pool();

    let mut passed = 0;
    let mut failed = Vec::new();

    for case in &cases {
        match run_case(&pool, case) {
            Ok(n) => passed += n,
            Err(errors) => failed.extend(errors),
        }
    }

    if !failed.is_empty() {
        panic!(
            "\n{passed} passed, {} failed:\n{}\n",
            failed.len(),
            failed.join("\n")
        );
    }

    eprintln!("{passed} data-driven test records passed");
}

/// Run all input records for a case. Returns Ok(count) or Err(messages).
fn run_case(pool: &prost_reflect::DescriptorPool, case: &TestCase) -> Result<usize, Vec<String>> {
    let desc = resolve_message(pool, &case.message);

    let opts = CompileOptions {
        schema: Some(DESCRIPTOR_BYTES),
        root_message: Some(&case.message),
    };

    let bytecode = match compile(&case.query, &opts) {
        Ok(b) => b,
        Err(e) => {
            return Err(vec![format!(
                "  line {}: compile({:?}) failed: {e}",
                case.line, case.query
            )]);
        }
    };
    let program = match wql_runtime::LoadedProgram::from_bytes(&bytecode) {
        Ok(p) => p,
        Err(e) => {
            return Err(vec![format!(
                "  line {}: load({:?}) failed: {e}",
                case.line, case.query
            )]);
        }
    };

    let is_filter = case.expected.iter().all(|e| e == "true" || e == "false");
    let is_combined = case.query.contains("WHERE") && case.query.contains("SELECT");

    let mut errors = Vec::new();
    let mut ok_count = 0;

    for (i, (input_json, expected)) in case.inputs.iter().zip(&case.expected).enumerate() {
        let record_line = case.line + 1 + i;
        let input_bytes = json_to_proto(&desc, input_json);

        let result = if is_filter {
            run_filter(&program, &input_bytes, expected, record_line)
        } else if is_combined {
            run_combined(&program, &desc, &input_bytes, expected, record_line)
        } else {
            run_project(&program, &desc, &input_bytes, expected, record_line)
        };

        match result {
            Ok(()) => ok_count += 1,
            Err(msg) => errors.push(format!(
                "  line {record_line}: {:?} | {input_json}\n    {msg}",
                case.query
            )),
        }
    }

    if errors.is_empty() {
        Ok(ok_count)
    } else {
        Err(errors)
    }
}

fn run_filter(
    program: &wql_runtime::LoadedProgram,
    input: &[u8],
    expected: &str,
    _line: usize,
) -> Result<(), String> {
    let result = wql_runtime::filter(program, input).map_err(|e| format!("filter error: {e}"))?;
    let expected_bool: bool = expected
        .parse()
        .map_err(|_| format!("bad expected: {expected}"))?;
    if result != expected_bool {
        return Err(format!("got {result}, expected {expected_bool}"));
    }
    Ok(())
}

fn run_project(
    program: &wql_runtime::LoadedProgram,
    desc: &MessageDescriptor,
    input: &[u8],
    expected_json: &str,
    _line: usize,
) -> Result<(), String> {
    let mut output = vec![0u8; input.len() * 2 + 256];
    let len = wql_runtime::project(program, input, &mut output)
        .map_err(|e| format!("project error: {e}"))?;

    let expected: Value =
        serde_json::from_str(expected_json).map_err(|e| format!("bad expected JSON: {e}"))?;
    let expected = normalize(&expected);

    let actual = if len == 0 {
        normalize(&serde_json::json!({}))
    } else {
        normalize(&proto_to_json(desc, &output[..len]))
    };

    if actual != expected {
        Err(format!("expected {expected}, got {actual}"))
    } else {
        Ok(())
    }
}

fn run_combined(
    program: &wql_runtime::LoadedProgram,
    desc: &MessageDescriptor,
    input: &[u8],
    expected: &str,
    _line: usize,
) -> Result<(), String> {
    let mut output = vec![0u8; input.len() * 2 + 256];
    let result = wql_runtime::project_and_filter(program, input, &mut output)
        .map_err(|e| format!("project_and_filter error: {e}"))?;

    if expected == "<none>" {
        if result.is_some() {
            return Err("expected <none>, got output".into());
        }
    } else {
        let len = result.ok_or("expected output, got <none>")?;
        let expected_val: Value =
            serde_json::from_str(expected).map_err(|e| format!("bad expected JSON: {e}"))?;
        let expected_val = normalize(&expected_val);
        let actual = normalize(&proto_to_json(desc, &output[..len]));
        if actual != expected_val {
            return Err(format!("expected {expected_val}, got {actual}"));
        }
    }
    Ok(())
}
