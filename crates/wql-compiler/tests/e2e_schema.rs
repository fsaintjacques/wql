//! End-to-end tests using a real protobuf schema compiled by `protoc`.
//!
//! These tests use generated Rust types (from `prost_build`) to encode
//! input messages and a real `FileDescriptorSet` for schema-bound compilation.
//! The WQL queries and assertions read like specification examples.

use prost::Message;
use wql_compiler::{compile, CompileOptions};

// ── Generated types from proto/testdata.proto ──
// Checked in at tests/testdata/testdata.{rs,bin}. Regenerate with:
//   protoc --descriptor_set_out=tests/testdata/testdata.bin --include_imports proto/testdata.proto
//   (prost-build also generates testdata.rs from the proto)
#[allow(clippy::all, clippy::pedantic)]
mod testdata {
    include!("testdata/testdata.rs");
}

/// The serialized `FileDescriptorSet` for the test schema.
const DESCRIPTOR: &[u8] = include_bytes!("testdata/testdata.bin");

// ═══════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════

fn opts_person() -> CompileOptions<'static> {
    CompileOptions {
        schema: Some(DESCRIPTOR),
        root_message: Some("testdata.Person"),
    }
}

fn opts_order() -> CompileOptions<'static> {
    CompileOptions {
        schema: Some(DESCRIPTOR),
        root_message: Some("testdata.Order"),
    }
}

fn project(wql: &str, opts: &CompileOptions, input: &impl Message) -> Vec<u8> {
    let bytecode = compile(wql, opts).expect("compile failed");
    let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).expect("load failed");
    let input_bytes = input.encode_to_vec();
    let mut output = vec![0u8; input_bytes.len() * 2 + 256];
    let result = program
        .eval(&input_bytes, &mut output)
        .unwrap_or_else(|e| panic!("eval({wql:?}) failed: {e:?}"));
    output.truncate(result.output_len);
    output
}

fn filter(wql: &str, opts: &CompileOptions, input: &impl Message) -> bool {
    let bytecode = compile(wql, opts).expect("compile failed");
    let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).expect("load failed");
    let input_bytes = input.encode_to_vec();
    program
        .eval(&input_bytes, &mut [])
        .unwrap_or_else(|e| panic!("eval({wql:?}) failed: {e:?}"))
        .matched
}

fn project_and_filter(wql: &str, opts: &CompileOptions, input: &impl Message) -> Option<Vec<u8>> {
    let bytecode = compile(wql, opts).expect("compile failed");
    let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).expect("load failed");
    let input_bytes = input.encode_to_vec();
    let mut output = vec![0u8; input_bytes.len() * 2 + 256];
    let result = program
        .eval(&input_bytes, &mut output)
        .unwrap_or_else(|e| panic!("eval({wql:?}) failed: {e:?}"));
    if result.matched {
        output.truncate(result.output_len);
        Some(output)
    } else {
        None
    }
}

/// Decode the output back to a Person.
fn decode_person(output: &[u8]) -> testdata::Person {
    testdata::Person::decode(output).expect("failed to decode Person from output")
}

/// Decode the output back to an Order.
fn decode_order(output: &[u8]) -> testdata::Order {
    testdata::Order::decode(output).expect("failed to decode Order from output")
}

fn alice() -> testdata::Person {
    testdata::Person {
        name: "Alice".into(),
        age: 30,
        address: Some(testdata::Address {
            city: "New York".into(),
            country: "US".into(),
            zip: 10001,
            location: Some(testdata::GeoPoint {
                lat: 40_712_776,
                lon: -74_005_974,
            }),
        }),
        status: testdata::Status::Active.into(),
        avatar: b"\x89PNG\r\n".to_vec(),
        tags: vec!["admin".into(), "staff".into()],
    }
}

fn bob() -> testdata::Person {
    testdata::Person {
        name: "Bob".into(),
        age: 17,
        address: Some(testdata::Address {
            city: "London".into(),
            country: "UK".into(),
            zip: 0,
            location: None,
        }),
        status: testdata::Status::Inactive.into(),
        avatar: vec![],
        tags: vec![],
    }
}

fn sample_order() -> testdata::Order {
    testdata::Order {
        id: 42,
        customer: "Alice".into(),
        items: vec![
            testdata::LineItem {
                sku: "WIDGET-1".into(),
                quantity: 2,
                price: 1999,
                discounts: vec![],
            },
            testdata::LineItem {
                sku: "GADGET-2".into(),
                quantity: 1,
                price: 4999,
                discounts: vec![],
            },
        ],
        total_cents: 8997,
        shipped: true,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Projection tests — Person
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn select_name_and_age() {
    let out = project("{ name, age }", &opts_person(), &alice());
    let p = decode_person(&out);

    assert_eq!(p.name, "Alice");
    assert_eq!(p.age, 30);
    assert!(p.address.is_none());
    assert_eq!(p.status, 0); // default (not included)
    assert!(p.avatar.is_empty());
    assert!(p.tags.is_empty());
}

#[test]
fn select_nested_address_city() {
    let out = project("{ name, address { city } }", &opts_person(), &alice());
    let p = decode_person(&out);

    assert_eq!(p.name, "Alice");
    let addr = p.address.unwrap();
    assert_eq!(addr.city, "New York");
    assert!(addr.country.is_empty()); // stripped
    assert_eq!(addr.zip, 0); // stripped
    assert!(addr.location.is_none()); // stripped
}

#[test]
fn select_nested_deep_address() {
    let out = project(
        "{ name, address { city, location { lat } } }",
        &opts_person(),
        &alice(),
    );
    let p = decode_person(&out);

    assert_eq!(p.name, "Alice");
    let addr = p.address.unwrap();
    assert_eq!(addr.city, "New York");
    let loc = addr.location.unwrap();
    assert_eq!(loc.lat, 40_712_776);
    assert_eq!(loc.lon, 0); // stripped
}

#[test]
fn select_with_preserve_unknowns() {
    let out = project("{ name, .. }", &opts_person(), &alice());
    let p = decode_person(&out);

    // All fields preserved, name explicitly included
    assert_eq!(p.name, "Alice");
    assert_eq!(p.age, 30);
    assert!(p.address.is_some());
    assert_eq!(p.tags.len(), 2);
}

#[test]
fn select_empty_strips_everything() {
    let out = project("{ }", &opts_person(), &alice());
    assert!(out.is_empty());
}

#[test]
fn identity_projection() {
    let input = alice();
    let input_bytes = input.encode_to_vec();
    let out = project("{ .. }", &opts_person(), &input);
    assert_eq!(out, input_bytes);
}

#[test]
fn select_by_field_number() {
    // Mix named and numbered references
    let out = project("{ name, #2 }", &opts_person(), &alice());
    let p = decode_person(&out);

    assert_eq!(p.name, "Alice");
    assert_eq!(p.age, 30);
    assert!(p.address.is_none());
}

// ═══════════════════════════════════════════════════════════════════════
// Projection tests — Order (repeated fields)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn select_order_fields() {
    let out = project("{ id, customer }", &opts_order(), &sample_order());
    let o = decode_order(&out);

    assert_eq!(o.id, 42);
    assert_eq!(o.customer, "Alice");
    assert!(o.items.is_empty());
    assert_eq!(o.total_cents, 0);
    assert!(!o.shipped);
}

#[test]
fn select_order_with_items() {
    let out = project(
        "{ customer, items { sku } }",
        &opts_order(),
        &sample_order(),
    );
    let o = decode_order(&out);

    assert_eq!(o.customer, "Alice");
    assert_eq!(o.items.len(), 2);
    assert_eq!(o.items[0].sku, "WIDGET-1");
    assert_eq!(o.items[1].sku, "GADGET-2");
    // quantity and price stripped from each item
    assert_eq!(o.items[0].quantity, 0);
    assert_eq!(o.items[0].price, 0);
}

// ═══════════════════════════════════════════════════════════════════════
// Filter tests — Person
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn filter_age_over_18() {
    assert!(filter("age > 18", &opts_person(), &alice()));
    assert!(!filter("age > 18", &opts_person(), &bob()));
}

#[test]
fn filter_name_equals() {
    assert!(filter(r#"name == "Alice""#, &opts_person(), &alice()));
    assert!(!filter(r#"name == "Alice""#, &opts_person(), &bob()));
}

#[test]
fn filter_status_enum() {
    // Status::Active = 1, Status::Inactive = 2
    assert!(filter("status == 1", &opts_person(), &alice()));
    assert!(!filter("status == 1", &opts_person(), &bob()));
}

#[test]
fn filter_status_in_set() {
    assert!(filter("status in [1, 2]", &opts_person(), &alice()));
    assert!(!filter("status in [0]", &opts_person(), &alice()));
}

#[test]
fn filter_status_enum_by_name() {
    // String enum names are resolved to their integer values
    assert!(filter(r#"status == "ACTIVE""#, &opts_person(), &alice()));
    assert!(!filter(r#"status == "ACTIVE""#, &opts_person(), &bob()));
    assert!(filter(r#"status == "INACTIVE""#, &opts_person(), &bob()));
}

#[test]
fn filter_status_in_set_by_name() {
    assert!(filter(
        r#"status in ["ACTIVE", "INACTIVE"]"#,
        &opts_person(),
        &alice()
    ));
    assert!(!filter(
        r#"status in ["UNKNOWN"]"#,
        &opts_person(),
        &alice()
    ));
}

#[test]
fn filter_status_enum_neq_by_name() {
    assert!(!filter(r#"status != "ACTIVE""#, &opts_person(), &alice()));
    assert!(filter(r#"status != "ACTIVE""#, &opts_person(), &bob()));
}

#[test]
fn filter_status_enum_name_invalid() {
    let opts = opts_person();
    let result = compile(r#"status == "BOGUS""#, &opts);
    assert!(result.is_err());
}

#[test]
fn filter_status_enum_matches_rejected() {
    let opts = opts_person();
    let result = compile(r#"status matches "ACT.*""#, &opts);
    assert!(result.is_err());
}

#[test]
fn filter_status_enum_starts_with() {
    assert!(filter(
        r#"status starts_with "ACT""#,
        &opts_person(),
        &alice()
    ));
    assert!(!filter(
        r#"status starts_with "ACT""#,
        &opts_person(),
        &bob()
    ));
}

#[test]
fn filter_status_enum_ends_with() {
    // Both ACTIVE and INACTIVE end with "IVE"
    assert!(filter(
        r#"status ends_with "IVE""#,
        &opts_person(),
        &alice()
    ));
    assert!(filter(r#"status ends_with "IVE""#, &opts_person(), &bob()));
}

#[test]
fn filter_status_enum_contains() {
    assert!(filter(r#"status contains "ACT""#, &opts_person(), &alice()));
    // INACTIVE also contains "ACT"
    assert!(filter(r#"status contains "ACT""#, &opts_person(), &bob()));
}

#[test]
fn filter_status_enum_no_match() {
    // No enum value starts with "ZZZ" — expands to empty IN set, always false
    assert!(!filter(
        r#"status starts_with "ZZZ""#,
        &opts_person(),
        &alice()
    ));
}

#[test]
fn filter_nested_city() {
    assert!(filter(
        r#"address.city == "New York""#,
        &opts_person(),
        &alice()
    ));
    assert!(!filter(
        r#"address.city == "New York""#,
        &opts_person(),
        &bob()
    ));
}

#[test]
fn filter_name_starts_with() {
    assert!(filter(
        r#"name starts_with "Ali""#,
        &opts_person(),
        &alice()
    ));
    assert!(!filter(r#"name starts_with "Ali""#, &opts_person(), &bob()));
}

#[test]
fn filter_name_contains() {
    assert!(filter(r#"name contains "lic""#, &opts_person(), &alice()));
    assert!(!filter(r#"name contains "lic""#, &opts_person(), &bob()));
}

#[test]
fn filter_and_logic() {
    // Alice: age=30, status=ACTIVE(1)
    assert!(filter("age > 18 && status == 1", &opts_person(), &alice()));
    // Bob: age=17, status=INACTIVE(2)
    assert!(!filter("age > 18 && status == 1", &opts_person(), &bob()));
}

#[test]
fn filter_or_logic() {
    assert!(filter("age > 18 || status == 2", &opts_person(), &bob()));
    assert!(!filter("age > 18 && status == 1", &opts_person(), &bob()));
}

#[test]
fn filter_not() {
    assert!(filter("!age > 50", &opts_person(), &alice()));
    assert!(!filter("!age > 18", &opts_person(), &alice()));
}

#[test]
fn filter_exists() {
    assert!(filter("exists(address)", &opts_person(), &alice()));
}

// ═══════════════════════════════════════════════════════════════════════
// Filter tests — Order
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn filter_order_shipped() {
    assert!(filter("shipped == true", &opts_order(), &sample_order()));
}

#[test]
fn filter_order_total() {
    assert!(filter("total_cents > 5000", &opts_order(), &sample_order()));
    assert!(!filter(
        "total_cents > 10000",
        &opts_order(),
        &sample_order()
    ));
}

// ═══════════════════════════════════════════════════════════════════════
// Combined (WHERE ... SELECT) tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn combined_adults_name_only() {
    let result = project_and_filter("WHERE age > 18 SELECT { name }", &opts_person(), &alice());
    assert!(result.is_some());
    let p = decode_person(&result.unwrap());
    assert_eq!(p.name, "Alice");
    assert_eq!(p.age, 0); // not projected
    assert!(p.address.is_none());

    let result = project_and_filter("WHERE age > 18 SELECT { name }", &opts_person(), &bob());
    assert!(result.is_none()); // Bob is 17
}

#[test]
fn combined_active_with_address() {
    let result = project_and_filter(
        "WHERE status == 1 SELECT { name, address { city, country } }",
        &opts_person(),
        &alice(),
    );
    assert!(result.is_some());
    let p = decode_person(&result.unwrap());
    assert_eq!(p.name, "Alice");
    let addr = p.address.unwrap();
    assert_eq!(addr.city, "New York");
    assert_eq!(addr.country, "US");
    assert_eq!(addr.zip, 0); // stripped
}

#[test]
fn combined_expensive_orders() {
    let result = project_and_filter(
        "WHERE total_cents > 5000 SELECT { id, customer }",
        &opts_order(),
        &sample_order(),
    );
    assert!(result.is_some());
    let o = decode_order(&result.unwrap());
    assert_eq!(o.id, 42);
    assert_eq!(o.customer, "Alice");
    assert!(o.items.is_empty());
}

#[test]
fn combined_filter_preserves_unknowns() {
    let result = project_and_filter(
        "WHERE age > 18 SELECT { name, .. }",
        &opts_person(),
        &alice(),
    );
    assert!(result.is_some());
    let p = decode_person(&result.unwrap());
    assert_eq!(p.name, "Alice");
    assert_eq!(p.age, 30); // preserved
    assert!(p.address.is_some()); // preserved
    assert_eq!(p.tags.len(), 2); // preserved
}

// ═══════════════════════════════════════════════════════════════════════
// Compile error tests
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn error_unresolved_field() {
    let result = compile("{ nonexistent }", &opts_person());
    assert!(result.is_err());
}

#[test]
fn error_type_mismatch() {
    // age is int64, string literal is wrong
    let result = compile(r#"age == "old""#, &opts_person());
    assert!(result.is_err());
}

#[test]
fn error_wrong_root_message() {
    let opts = CompileOptions {
        schema: Some(DESCRIPTOR),
        root_message: Some("testdata.DoesNotExist"),
    };
    let result = compile("{ name }", &opts);
    assert!(result.is_err());
}

#[test]
fn error_missing_root_message() {
    let opts = CompileOptions {
        schema: Some(DESCRIPTOR),
        root_message: None,
    };
    let result = compile("{ name }", &opts);
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
// Deep exclusion (..-field) tests
// ═══════════════════════════════════════════════════════════════════════

fn opts_team() -> CompileOptions<'static> {
    CompileOptions {
        schema: Some(DESCRIPTOR),
        root_message: Some("testdata.Team"),
    }
}

fn team() -> testdata::Team {
    testdata::Team {
        name: "Engineering".into(),
        id: 42,
        members: vec![
            testdata::Member {
                name: "Alice".into(),
                role: "Lead".into(),
                secret: "alice-token".into(),
            },
            testdata::Member {
                name: "Bob".into(),
                role: "Dev".into(),
                secret: "bob-token".into(),
            },
        ],
        secret: "team-api-key".into(),
    }
}

fn decode_team(output: &[u8]) -> testdata::Team {
    testdata::Team::decode(output).expect("failed to decode Team from output")
}

#[test]
fn deep_exclusion_removes_secret_at_all_levels() {
    let t = team();
    let output = project("{ ..-secret, .. }", &opts_team(), &t);
    let result = decode_team(&output);
    // Top-level secret should be stripped.
    assert_eq!(result.secret, "");
    // Nested member secrets should be stripped.
    assert_eq!(result.members.len(), 2);
    assert_eq!(result.members[0].secret, "");
    assert_eq!(result.members[1].secret, "");
    // Other fields preserved.
    assert_eq!(result.name, "Engineering");
    assert_eq!(result.id, 42);
    assert_eq!(result.members[0].name, "Alice");
    assert_eq!(result.members[0].role, "Lead");
    assert_eq!(result.members[1].name, "Bob");
}

#[test]
fn deep_exclusion_field_only_at_top_level() {
    // "id" only exists in Team, not in Member — no Nested Frame generated.
    let t = team();
    let output = project("{ ..-id, .. }", &opts_team(), &t);
    let result = decode_team(&output);
    assert_eq!(result.id, 0); // stripped
    assert_eq!(result.name, "Engineering"); // preserved
    assert_eq!(result.secret, "team-api-key"); // preserved
    assert_eq!(result.members.len(), 2); // preserved
}

#[test]
fn deep_exclusion_combined_with_predicate() {
    let t = team();
    let result = project_and_filter("WHERE id > 0 SELECT { ..-secret, .. }", &opts_team(), &t);
    let output = result.expect("predicate should match");
    let decoded = decode_team(&output);
    assert_eq!(decoded.secret, "");
    assert_eq!(decoded.members[0].secret, "");
    assert_eq!(decoded.name, "Engineering");
}

#[test]
fn deep_exclusion_combined_with_shallow_exclusion() {
    let t = team();
    // Remove "secret" at all levels AND "name" at top level only.
    let output = project("{ -name, ..-secret, .. }", &opts_team(), &t);
    let result = decode_team(&output);
    assert_eq!(result.name, ""); // shallow exclusion
    assert_eq!(result.secret, ""); // deep exclusion
    assert_eq!(result.members[0].name, "Alice"); // name NOT excluded in members
    assert_eq!(result.members[0].secret, ""); // deep exclusion
}

#[test]
fn deep_exclusion_with_explicit_nested_projection() {
    let t = team();
    // User writes an explicit Nested for members; deep exclusion should merge.
    let output = project("{ members { name, .. }, ..-secret, .. }", &opts_team(), &t);
    let result = decode_team(&output);
    assert_eq!(result.secret, ""); // top-level secret stripped
    assert_eq!(result.members[0].name, "Alice"); // explicitly included
    assert_eq!(result.members[0].role, "Lead"); // copy mode preserves
    assert_eq!(result.members[0].secret, ""); // deep exclusion merged in
}
