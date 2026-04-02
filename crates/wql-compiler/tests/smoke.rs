use wql_compiler::ast::*;
use wql_compiler::parse;

#[test]
fn flat_projection() {
    let q = parse("{ name, age }").unwrap();
    let Query::Projection(p) = q else {
        panic!("expected Projection");
    };
    let ProjectionKind::Strict { items } = &p.kind else {
        panic!("expected Strict");
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn nested_projection_with_copy() {
    let q = parse("{ name, address { city, .. }, .. }").unwrap();
    let Query::Projection(p) = q else {
        panic!("expected Projection");
    };
    let ProjectionKind::Copy { items, .. } = &p.kind else {
        panic!("expected Copy");
    };
    assert!(matches!(&items[1], ProjectionItem::Nested { .. }));
}

#[test]
fn copy_with_exclusion() {
    let q = parse("{ -payload, -thumbnail, .. }").unwrap();
    let Query::Projection(p) = q else {
        panic!("expected Projection");
    };
    let ProjectionKind::Copy {
        items, exclusions, ..
    } = &p.kind
    else {
        panic!("expected Copy");
    };
    assert!(items.is_empty());
    assert_eq!(exclusions.len(), 2);
}

#[test]
fn schema_free_projection() {
    let q = parse("{ #1, #3 { #1, .. } }").unwrap();
    let Query::Projection(p) = q else {
        panic!("expected Projection");
    };
    let ProjectionKind::Strict { items } = &p.kind else {
        panic!("expected Strict");
    };
    assert!(matches!(
        &items[0],
        ProjectionItem::Field(FieldRef::Number(1, _))
    ));
}

#[test]
fn predicate_with_nested_field() {
    let q = parse(r#"age > 18 && address.city == "NYC""#).unwrap();
    let Query::Predicate(p) = q else {
        panic!("expected Predicate");
    };
    assert!(matches!(&p.kind, PredicateKind::And(_, _)));
}

#[test]
fn predicate_presence_and_set() {
    let q = parse("exists(vip) OR status in [1, 2, 3]").unwrap();
    let Query::Predicate(p) = q else {
        panic!("expected Predicate");
    };
    assert!(matches!(&p.kind, PredicateKind::Or(_, _)));
}

#[test]
fn predicate_string_ops() {
    let q = parse(r#"name starts_with "A" AND tag contains "urgent""#).unwrap();
    let Query::Predicate(p) = q else {
        panic!("expected Predicate");
    };
    assert!(matches!(&p.kind, PredicateKind::And(_, _)));
}

#[test]
fn combined_filter_and_project() {
    let q =
        parse(r#"WHERE age > 18 AND address.city == "NYC" SELECT { name, address { city }, .. }"#)
            .unwrap();
    let Query::Combined {
        predicate,
        projection,
    } = q
    else {
        panic!("expected Combined");
    };
    assert!(matches!(&predicate.kind, PredicateKind::And(_, _)));
    let ProjectionKind::Copy { items, .. } = &projection.kind else {
        panic!("expected Copy");
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn predicate_parenthesized_precedence() {
    // Without parens: a > 1 || b > 2 && c > 3 -> Or(a, And(b, c))
    let q1 = parse("a > 1 || b > 2 && c > 3").unwrap();
    let Query::Predicate(p1) = q1 else { panic!() };
    assert!(
        matches!(&p1.kind, PredicateKind::Or(_, rhs) if matches!(&rhs.kind, PredicateKind::And(_, _)))
    );

    // With parens: (a > 1 || b > 2) && c > 3 -> And(Or(a, b), c)
    let q2 = parse("(a > 1 || b > 2) && c > 3").unwrap();
    let Query::Predicate(p2) = q2 else { panic!() };
    assert!(
        matches!(&p2.kind, PredicateKind::And(lhs, _) if matches!(&lhs.kind, PredicateKind::Or(_, _)))
    );
}

#[test]
fn where_without_select() {
    let q = parse("WHERE age > 18").unwrap();
    let Query::Predicate(p) = q else {
        panic!("expected Predicate");
    };
    assert!(matches!(&p.kind, PredicateKind::Comparison { .. }));
}

#[test]
fn select_without_where() {
    let q = parse("SELECT { name, age }").unwrap();
    let Query::Projection(p) = q else {
        panic!("expected Projection");
    };
    let ProjectionKind::Strict { items } = &p.kind else {
        panic!("expected Strict");
    };
    assert_eq!(items.len(), 2);
}

#[test]
fn error_has_span() {
    let err = parse("age >").unwrap_err();
    assert!(err.span.start <= err.span.end);
    // Verify Display impl works
    let msg = err.to_string();
    assert!(msg.contains("error at byte"));
}
