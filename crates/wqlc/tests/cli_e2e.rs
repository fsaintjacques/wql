//! Integration tests that drive `wqlc eval --delimited` using the shared
//! e2e.txt test data. Each test case is fed as a varint length-delimited
//! stream and the output is compared record-by-record.

use prost::Message;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use serde_json::Value;
use std::process::Command;

const DESCRIPTOR_BYTES: &[u8] = include_bytes!("../../wql-compiler/tests/testdata/testdata.bin");
const E2E_DATA: &str = include_str!("../../wql-compiler/tests/testdata/e2e.txt");

fn descriptor_pool() -> prost_reflect::DescriptorPool {
    prost_reflect::DescriptorPool::decode(DESCRIPTOR_BYTES).expect("failed to decode descriptor")
}

fn wqlc_bin() -> std::path::PathBuf {
    // cargo test builds the binary in the same target dir
    let mut path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    path.push("wqlc");
    assert!(path.exists(), "wqlc binary not found at {path:?}");
    path
}

// ═══════════════════════════════════════════════════════════════════════
// Test file parser (same format as e2e_datadriven.rs)
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

        let (query_line, query) = lines.next().unwrap();
        let query = query.trim().to_string();

        let mut inputs = Vec::new();
        loop {
            match lines.peek() {
                None => panic!("line {}: unexpected EOF", query_line + 1),
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
    pool.get_message_by_name(name).unwrap()
}

fn json_to_proto(desc: &MessageDescriptor, json: &str) -> Vec<u8> {
    let mut de = serde_json::Deserializer::from_str(json);
    DynamicMessage::deserialize(desc.clone(), &mut de)
        .unwrap()
        .encode_to_vec()
}

fn proto_to_json(desc: &MessageDescriptor, bytes: &[u8]) -> Value {
    let msg = DynamicMessage::decode(desc.clone(), bytes).unwrap();
    let opts = prost_reflect::SerializeOptions::new().skip_default_fields(true);
    msg.serialize_with_options(serde_json::value::Serializer, &opts)
        .unwrap()
}

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
// Varint encoding/decoding
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

fn encode_delimited_stream(records: &[Vec<u8>]) -> Vec<u8> {
    let mut buf = Vec::new();
    for record in records {
        buf.extend(encode_varint(record.len() as u64));
        buf.extend(record);
    }
    buf
}

fn decode_delimited_stream(mut buf: &[u8]) -> Vec<Vec<u8>> {
    let mut records = Vec::new();
    while !buf.is_empty() {
        let mut val: u64 = 0;
        let mut shift = 0;
        let mut consumed = 0;
        for &b in buf.iter() {
            consumed += 1;
            val |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            assert!(shift < 64, "varint overflow in delimited stream");
        }
        buf = &buf[consumed..];
        let len = val as usize;
        assert!(
            buf.len() >= len,
            "truncated record: expected {len} bytes, got {}",
            buf.len()
        );
        records.push(buf[..len].to_vec());
        buf = &buf[len..];
    }
    records
}

// ═══════════════════════════════════════════════════════════════════════
// Runner
// ═══════════════════════════════════════════════════════════════════════

fn write_schema_file() -> tempfile::TempPath {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(DESCRIPTOR_BYTES).unwrap();
    f.into_temp_path()
}

use std::io::Write;

#[test]
fn cli_eval_delimited_e2e() {
    run_e2e(&[]);
}

#[test]
#[cfg(feature = "wasm")]
fn cli_eval_delimited_e2e_wasm() {
    run_e2e(&["--wasm"]);
}

fn run_e2e(extra_args: &[&str]) {
    let cases = parse_test_file(E2E_DATA);
    let pool = descriptor_pool();
    let wqlc = wqlc_bin();
    let schema_path = write_schema_file();
    let schema_str = schema_path.to_str().unwrap();

    let mut passed = 0;
    let mut failed = Vec::new();

    for case in &cases {
        let desc = resolve_message(&pool, &case.message);
        // Infer mode from expected output format, not query string.
        let is_filter = case.expected.iter().all(|e| e == "true" || e == "false");
        let is_combined = !is_filter && case.expected.iter().any(|e| e == "<none>");

        // Encode input records as delimited stream
        let input_records: Vec<Vec<u8>> = case
            .inputs
            .iter()
            .map(|json| json_to_proto(&desc, json))
            .collect();
        let stream = encode_delimited_stream(&input_records);

        // Run wqlc eval --delimited [extra_args]
        let mut args = vec![
            "eval",
            "-q",
            &case.query,
            "-s",
            schema_str,
            "-m",
            &case.message,
            "--delimited",
        ];
        args.extend_from_slice(extra_args);

        let output = Command::new(&wqlc)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                child.stdin.take().unwrap().write_all(&stream).unwrap();
                child.wait_with_output()
            });

        let output = match output {
            Ok(o) => o,
            Err(e) => {
                failed.push(format!("  line {}: spawn error: {e}", case.line));
                continue;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            failed.push(format!(
                "  line {}: {:?} exited {}: {}",
                case.line,
                case.query,
                output.status,
                if stderr.is_empty() {
                    "(no stderr)"
                } else {
                    stderr.trim()
                }
            ));
            continue;
        }

        let output_records = decode_delimited_stream(&output.stdout);

        if is_filter {
            // In delimited filter mode, output contains only passing records.
            // We need to compare which inputs passed vs the expected true/false.
            let expected_passing: Vec<usize> = case
                .expected
                .iter()
                .enumerate()
                .filter(|(_, e)| *e == "true")
                .map(|(i, _)| i)
                .collect();

            if output_records.len() != expected_passing.len() {
                failed.push(format!(
                    "  line {}: {:?} filter: expected {} passing records, got {}",
                    case.line,
                    case.query,
                    expected_passing.len(),
                    output_records.len()
                ));
                continue;
            }

            let mut ok = true;
            for (out_idx, &inp_idx) in expected_passing.iter().enumerate() {
                if output_records[out_idx] != input_records[inp_idx] {
                    failed.push(format!(
                        "  line {}: {:?} filter record {}: output mismatch",
                        case.line, case.query, inp_idx
                    ));
                    ok = false;
                    break;
                }
            }
            if ok {
                passed += case.inputs.len();
            }
        } else if is_combined {
            // Combined: output contains only records that passed the filter.
            let expected_outputs: Vec<(usize, &str)> = case
                .expected
                .iter()
                .enumerate()
                .filter(|(_, e)| *e != "<none>")
                .map(|(i, e)| (i, e.as_str()))
                .collect();

            if output_records.len() != expected_outputs.len() {
                failed.push(format!(
                    "  line {}: {:?} combined: expected {} output records, got {}",
                    case.line,
                    case.query,
                    expected_outputs.len(),
                    output_records.len()
                ));
                continue;
            }

            let mut ok = true;
            for (out_idx, &(_, expected_json)) in expected_outputs.iter().enumerate() {
                let expected: Value = serde_json::from_str(expected_json).unwrap();
                let expected = normalize(&expected);
                let actual = normalize(&proto_to_json(&desc, &output_records[out_idx]));
                if actual != expected {
                    failed.push(format!(
                        "  line {}: {:?} combined record {out_idx}: expected {expected}, got {actual}",
                        case.line, case.query
                    ));
                    ok = false;
                    break;
                }
            }
            if ok {
                passed += case.inputs.len();
            }
        } else {
            // Projection: 1:1 mapping
            if output_records.len() != case.expected.len() {
                failed.push(format!(
                    "  line {}: {:?} project: expected {} records, got {}",
                    case.line,
                    case.query,
                    case.expected.len(),
                    output_records.len()
                ));
                continue;
            }

            let mut ok = true;
            for (i, (out_record, expected_json)) in
                output_records.iter().zip(&case.expected).enumerate()
            {
                let expected: Value = serde_json::from_str(expected_json).unwrap();
                let expected = normalize(&expected);
                let actual = if out_record.is_empty() {
                    normalize(&serde_json::json!({}))
                } else {
                    normalize(&proto_to_json(&desc, out_record))
                };
                if actual != expected {
                    failed.push(format!(
                        "  line {}: {:?} record {i}: expected {expected}, got {actual}",
                        case.line, case.query
                    ));
                    ok = false;
                    break;
                }
            }
            if ok {
                passed += case.inputs.len();
            }
        }
    }

    if !failed.is_empty() {
        panic!(
            "\n{passed} records passed, {} failed:\n{}\n",
            failed.len(),
            failed.join("\n")
        );
    }

    let label = if extra_args.is_empty() {
        "native"
    } else {
        "wasm"
    };
    eprintln!("{passed} CLI e2e records passed ({label})");
}
