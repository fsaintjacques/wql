use crate::ast::{
    CompareOp, FieldPath, FieldRef, Literal, Predicate, PredicateKind, Projection, ProjectionItem,
    ProjectionKind, Query, StringOp,
};
use crate::error::CompileError;
use crate::lexer::Span;
use wql_ir::Encoding;

// ═══════════════════════════════════════════════════════════════════════
// Bound AST types
// ═══════════════════════════════════════════════════════════════════════

/// A field reference resolved to a proto field number.
#[derive(Debug, Clone)]
pub struct BoundField {
    pub field_num: u32,
    pub span: Span,
}

/// A resolved field path with encoding information for the leaf field.
#[derive(Debug, Clone)]
pub struct BoundFieldPath {
    /// Each segment is a field number. Length 1 = top-level field.
    pub segments: Vec<u32>,
    /// Encoding of the leaf field (for DECODE). `None` if unknown.
    pub encoding: Option<Encoding>,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundQuery {
    Projection(BoundProjection),
    Predicate(BoundPredicate),
    Combined {
        predicate: BoundPredicate,
        projection: BoundProjection,
    },
}

#[derive(Debug)]
pub struct BoundProjection {
    pub kind: BoundProjectionKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundProjectionKind {
    /// Strict inclusion: only listed fields are emitted.
    Strict { items: Vec<BoundProjectionItem> },
    /// Copy mode: listed fields + all unmatched fields. Exclusions skip specific fields.
    Copy {
        items: Vec<BoundProjectionItem>,
        exclusions: Vec<u32>,
    },
}

#[derive(Debug)]
pub enum BoundProjectionItem {
    /// Flat field copy.
    Field(BoundField),
    /// Enter sub-message, apply nested projection.
    Nested {
        field: BoundField,
        projection: Box<BoundProjection>,
    },
}

#[derive(Debug)]
pub struct BoundPredicate {
    pub kind: BoundPredicateKind,
    pub span: Span,
}

#[derive(Debug)]
pub enum BoundPredicateKind {
    And(Box<BoundPredicate>, Box<BoundPredicate>),
    Or(Box<BoundPredicate>, Box<BoundPredicate>),
    Not(Box<BoundPredicate>),
    Comparison {
        field: BoundFieldPath,
        op: CompareOp,
        value: Literal,
    },
    Presence(BoundFieldPath),
    InSet {
        field: BoundFieldPath,
        values: Vec<Literal>,
    },
    StringPredicate {
        field: BoundFieldPath,
        op: StringOp,
        value: Literal,
    },
}

// ═══════════════════════════════════════════════════════════════════════
// Schema-free binding
// ═══════════════════════════════════════════════════════════════════════

/// Bind a parsed query in schema-free mode.
///
/// All field references must be `FieldRef::Number`. Named fields produce
/// `CompileError::NamedFieldWithoutSchema`.
///
/// # Errors
///
/// Returns [`CompileError::NamedFieldWithoutSchema`] if any named field is encountered.
pub fn bind_schema_free(query: &Query) -> Result<BoundQuery, CompileError> {
    match query {
        Query::Projection(proj) => Ok(BoundQuery::Projection(bind_projection_sf(proj)?)),
        Query::Predicate(pred) => Ok(BoundQuery::Predicate(bind_predicate_sf(pred)?)),
        Query::Combined {
            predicate,
            projection,
        } => Ok(BoundQuery::Combined {
            predicate: bind_predicate_sf(predicate)?,
            projection: bind_projection_sf(projection)?,
        }),
    }
}

fn require_number(field: &FieldRef) -> Result<BoundField, CompileError> {
    match field {
        FieldRef::Number(n, span) => Ok(BoundField {
            field_num: *n,
            span: *span,
        }),
        FieldRef::Name(name, span) => Err(CompileError::NamedFieldWithoutSchema {
            field: name.clone(),
            span: *span,
        }),
    }
}

fn bind_items_sf(items: &[ProjectionItem]) -> Result<Vec<BoundProjectionItem>, CompileError> {
    items
        .iter()
        .map(|item| match item {
            ProjectionItem::Field(f) => Ok(BoundProjectionItem::Field(require_number(f)?)),
            ProjectionItem::Nested { field, projection } => Ok(BoundProjectionItem::Nested {
                field: require_number(field)?,
                projection: Box::new(bind_projection_sf(projection)?),
            }),
        })
        .collect()
}

fn bind_excl_sf(exclusions: &[FieldRef]) -> Result<Vec<u32>, CompileError> {
    exclusions
        .iter()
        .map(|f| Ok(require_number(f)?.field_num))
        .collect()
}

fn bind_projection_sf(proj: &Projection) -> Result<BoundProjection, CompileError> {
    let kind = match &proj.kind {
        ProjectionKind::Strict { items } => BoundProjectionKind::Strict {
            items: bind_items_sf(items)?,
        },
        ProjectionKind::Copy {
            items,
            exclusions,
            deep_exclusions,
        } => {
            if let Some(f) = deep_exclusions.first() {
                return Err(CompileError::DeepExclusionWithoutSchema { span: f.span() });
            }
            BoundProjectionKind::Copy {
                items: bind_items_sf(items)?,
                exclusions: bind_excl_sf(exclusions)?,
            }
        }
    };
    Ok(BoundProjection {
        kind,
        span: proj.span,
    })
}

fn bind_predicate_sf(pred: &Predicate) -> Result<BoundPredicate, CompileError> {
    let kind = match &pred.kind {
        PredicateKind::And(l, r) => BoundPredicateKind::And(
            Box::new(bind_predicate_sf(l)?),
            Box::new(bind_predicate_sf(r)?),
        ),
        PredicateKind::Or(l, r) => BoundPredicateKind::Or(
            Box::new(bind_predicate_sf(l)?),
            Box::new(bind_predicate_sf(r)?),
        ),
        PredicateKind::Not(inner) => BoundPredicateKind::Not(Box::new(bind_predicate_sf(inner)?)),
        PredicateKind::Comparison { field, op, value } => BoundPredicateKind::Comparison {
            field: bind_field_path_sf(field, Some(value))?,
            op: *op,
            value: value.clone(),
        },
        PredicateKind::Presence(field) => {
            BoundPredicateKind::Presence(bind_field_path_sf(field, None)?)
        }
        PredicateKind::InSet { field, values } => BoundPredicateKind::InSet {
            field: bind_field_path_sf(field, values.first())?,
            values: values.clone(),
        },
        PredicateKind::StringPredicate { field, op, value } => {
            BoundPredicateKind::StringPredicate {
                field: bind_field_path_sf(field, Some(value))?,
                op: *op,
                value: value.clone(),
            }
        }
    };
    Ok(BoundPredicate {
        kind,
        span: pred.span,
    })
}

/// Resolve a field path in schema-free mode, inferring encoding from the
/// literal type when available.
fn bind_field_path_sf(
    path: &FieldPath,
    literal: Option<&Literal>,
) -> Result<BoundFieldPath, CompileError> {
    let segments = path
        .segments
        .iter()
        .map(|seg| Ok(require_number(seg)?.field_num))
        .collect::<Result<Vec<_>, CompileError>>()?;

    let encoding = literal.map(infer_encoding_from_literal);

    Ok(BoundFieldPath {
        segments,
        encoding,
        span: path.span,
    })
}

/// Infer encoding from a literal type (schema-free mode).
fn infer_encoding_from_literal(lit: &Literal) -> Encoding {
    match lit {
        Literal::Int(..) | Literal::Bool(..) => Encoding::Varint,
        Literal::String(..) => Encoding::Len,
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Schema-bound binding
// ═══════════════════════════════════════════════════════════════════════

use prost::Message as _;
use prost_types::{
    field_descriptor_proto::Type as ProtoType, DescriptorProto, EnumDescriptorProto,
    FieldDescriptorProto, FileDescriptorSet,
};

/// Bind a parsed query using a `FileDescriptorSet` schema.
///
/// Resolves named field references to field numbers, validates literal types.
///
/// # Errors
///
/// Returns [`CompileError`] on unresolved fields, type mismatches, or invalid schema.
///
/// # Panics
///
/// Panics if `options.schema` is `None` (caller must check).
pub fn bind_with_schema(
    query: &Query,
    options: &crate::compile::CompileOptions,
) -> Result<BoundQuery, CompileError> {
    // schema presence is guaranteed by the caller (compile.rs checks options.schema.is_some())
    let schema_bytes = options
        .schema
        .expect("schema must be Some in schema-bound mode");
    let fds = FileDescriptorSet::decode(schema_bytes)
        .map_err(|e| CompileError::InvalidSchema(e.to_string()))?;

    let root_msg_name = options
        .root_message
        .ok_or(CompileError::MissingRootMessage)?;
    let root_msg =
        find_message(&fds, root_msg_name).ok_or_else(|| CompileError::InvalidMessageType {
            type_name: root_msg_name.to_string(),
        })?;

    bind_query(query, root_msg, &fds)
}

fn find_message<'a>(fds: &'a FileDescriptorSet, full_name: &str) -> Option<&'a DescriptorProto> {
    for file in &fds.file {
        let pkg = file.package.as_deref().unwrap_or("");
        for msg in &file.message_type {
            if let Some(found) = find_message_recursive(msg, pkg, full_name) {
                return Some(found);
            }
        }
    }
    None
}

fn find_message_recursive<'a>(
    msg: &'a DescriptorProto,
    parent_prefix: &str,
    target: &str,
) -> Option<&'a DescriptorProto> {
    let name = msg.name.as_deref().unwrap_or("");
    let fqn = if parent_prefix.is_empty() {
        name.to_string()
    } else {
        format!("{parent_prefix}.{name}")
    };
    if fqn == target {
        return Some(msg);
    }
    for nested in &msg.nested_type {
        if let Some(found) = find_message_recursive(nested, &fqn, target) {
            return Some(found);
        }
    }
    None
}

/// Resolve a `FieldDescriptorProto`'s `type_name` to a `DescriptorProto`.
fn resolve_type_name<'a>(
    fds: &'a FileDescriptorSet,
    type_name: &str,
) -> Option<&'a DescriptorProto> {
    // type_name is usually fully-qualified with a leading dot, e.g. ".pkg.Msg"
    let stripped = type_name.strip_prefix('.').unwrap_or(type_name);
    find_message(fds, stripped)
}

/// Resolve a `FieldDescriptorProto`'s `type_name` to an `EnumDescriptorProto`.
fn resolve_enum_type_name<'a>(
    fds: &'a FileDescriptorSet,
    type_name: &str,
) -> Option<&'a EnumDescriptorProto> {
    let target = type_name.strip_prefix('.').unwrap_or(type_name);
    for file in &fds.file {
        let pkg = file.package.as_deref().unwrap_or("");
        // Top-level enums
        for e in &file.enum_type {
            let name = e.name.as_deref().unwrap_or("");
            let fqn = if pkg.is_empty() {
                name.to_string()
            } else {
                format!("{pkg}.{name}")
            };
            if fqn == target {
                return Some(e);
            }
        }
        // Nested enums inside messages
        for msg in &file.message_type {
            if let Some(found) = find_enum_recursive(msg, pkg, target) {
                return Some(found);
            }
        }
    }
    None
}

fn find_enum_recursive<'a>(
    msg: &'a DescriptorProto,
    parent_prefix: &str,
    target: &str,
) -> Option<&'a EnumDescriptorProto> {
    let msg_name = msg.name.as_deref().unwrap_or("");
    let prefix = if parent_prefix.is_empty() {
        msg_name.to_string()
    } else {
        format!("{parent_prefix}.{msg_name}")
    };
    for e in &msg.enum_type {
        let name = e.name.as_deref().unwrap_or("");
        let fqn = format!("{prefix}.{name}");
        if fqn == target {
            return Some(e);
        }
    }
    for nested in &msg.nested_type {
        if let Some(found) = find_enum_recursive(nested, &prefix, target) {
            return Some(found);
        }
    }
    None
}

/// If `literal` is a string and `field_desc` is an enum, resolve the name to its integer value.
fn resolve_enum_literal(
    field_desc: &FieldDescriptorProto,
    literal: &Literal,
    fds: &FileDescriptorSet,
) -> Result<Option<Literal>, CompileError> {
    if field_desc.r#type() != ProtoType::Enum {
        return Ok(None);
    }
    let Literal::String(name, span) = literal else {
        return Ok(None);
    };
    let type_name = field_desc.type_name.as_deref().unwrap_or("");
    let enum_desc =
        resolve_enum_type_name(fds, type_name).ok_or_else(|| CompileError::InvalidMessageType {
            type_name: type_name.to_string(),
        })?;
    for v in &enum_desc.value {
        if v.name.as_deref() == Some(name.as_str()) {
            return Ok(Some(Literal::Int(i64::from(v.number.unwrap_or(0)), *span)));
        }
    }
    Err(CompileError::UnresolvedEnumValue {
        value: name.clone(),
        enum_name: enum_desc.name.clone().unwrap_or_default(),
        span: *span,
    })
}

/// Apply a string predicate against enum value names at compile time,
/// returning the list of matching integer literals.
fn resolve_enum_string_predicate(
    field_desc: &FieldDescriptorProto,
    op: StringOp,
    pattern: &Literal,
    fds: &FileDescriptorSet,
) -> Result<Vec<Literal>, CompileError> {
    let Literal::String(pat, span) = pattern else {
        return Err(CompileError::TypeError {
            field: field_desc.name.as_deref().unwrap_or("?").to_string(),
            expected: "string",
            actual: literal_type_name(pattern),
            span: pattern.span(),
        });
    };
    let type_name = field_desc.type_name.as_deref().unwrap_or("");
    let enum_desc =
        resolve_enum_type_name(fds, type_name).ok_or_else(|| CompileError::InvalidMessageType {
            type_name: type_name.to_string(),
        })?;
    if matches!(op, StringOp::Matches) {
        return Err(CompileError::UnsupportedComparison {
            op: "matches",
            literal_type: "enum",
        });
    }
    let matches: Vec<Literal> = enum_desc
        .value
        .iter()
        .filter(|v| {
            let name = v.name.as_deref().unwrap_or("");
            match op {
                StringOp::StartsWith => name.starts_with(pat.as_str()),
                StringOp::EndsWith => name.ends_with(pat.as_str()),
                StringOp::Contains => name.contains(pat.as_str()),
                StringOp::Matches => unreachable!(),
            }
        })
        .map(|v| Literal::Int(i64::from(v.number.unwrap_or(0)), *span))
        .collect();
    Ok(matches)
}

fn find_field_by_name<'a>(
    msg: &'a DescriptorProto,
    name: &str,
) -> Option<&'a FieldDescriptorProto> {
    msg.field.iter().find(|f| f.name.as_deref() == Some(name))
}

fn find_field_by_number(msg: &DescriptorProto, number: u32) -> Option<&FieldDescriptorProto> {
    msg.field.iter().find(|f| {
        #[allow(clippy::cast_possible_wrap)]
        let num_i32 = number as i32;
        f.number == Some(num_i32)
    })
}

fn proto_type_to_encoding(ty: ProtoType) -> Encoding {
    match ty {
        ProtoType::Int32
        | ProtoType::Int64
        | ProtoType::Uint32
        | ProtoType::Uint64
        | ProtoType::Bool
        | ProtoType::Enum => Encoding::Varint,

        ProtoType::Sint32 | ProtoType::Sint64 => Encoding::Sint,

        ProtoType::Fixed32 | ProtoType::Sfixed32 | ProtoType::Float => Encoding::I32,

        ProtoType::Fixed64 | ProtoType::Sfixed64 | ProtoType::Double => Encoding::I64,

        ProtoType::String | ProtoType::Bytes | ProtoType::Message | ProtoType::Group => {
            Encoding::Len
        }
    }
}

fn validate_literal_type(
    field_desc: &FieldDescriptorProto,
    literal: &Literal,
    span: Span,
) -> Result<(), CompileError> {
    let proto_type = field_desc.r#type();
    let field_name = field_desc.name.as_deref().unwrap_or("?").to_string();

    match proto_type {
        ProtoType::Int32
        | ProtoType::Int64
        | ProtoType::Uint32
        | ProtoType::Uint64
        | ProtoType::Sint32
        | ProtoType::Sint64
        | ProtoType::Fixed32
        | ProtoType::Sfixed32
        | ProtoType::Fixed64
        | ProtoType::Sfixed64 => match literal {
            Literal::Int(..) => Ok(()),
            _ => Err(CompileError::TypeError {
                field: field_name,
                expected: "integer",
                actual: literal_type_name(literal),
                span,
            }),
        },
        ProtoType::Bool => match literal {
            Literal::Bool(..) => Ok(()),
            _ => Err(CompileError::TypeError {
                field: field_name,
                expected: "bool",
                actual: literal_type_name(literal),
                span,
            }),
        },
        ProtoType::String | ProtoType::Bytes => match literal {
            Literal::String(..) => Ok(()),
            _ => Err(CompileError::TypeError {
                field: field_name,
                expected: "string",
                actual: literal_type_name(literal),
                span,
            }),
        },
        ProtoType::Enum => match literal {
            Literal::Int(..) | Literal::String(..) => Ok(()),
            Literal::Bool(..) => Err(CompileError::TypeError {
                field: field_name,
                expected: "integer or string (enum)",
                actual: literal_type_name(literal),
                span,
            }),
        },
        ProtoType::Float | ProtoType::Double => Err(CompileError::TypeError {
            field: field_name,
            expected: "no comparison (float not supported in v1)",
            actual: literal_type_name(literal),
            span,
        }),
        ProtoType::Message | ProtoType::Group => Err(CompileError::TypeError {
            field: field_name,
            expected: "no comparison (sub-message)",
            actual: literal_type_name(literal),
            span,
        }),
    }
}

fn literal_type_name(lit: &Literal) -> &'static str {
    match lit {
        Literal::Int(..) => "integer",
        Literal::String(..) => "string",
        Literal::Bool(..) => "bool",
    }
}

// ─── Schema-bound query binding ───

fn bind_query(
    query: &Query,
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
) -> Result<BoundQuery, CompileError> {
    match query {
        Query::Projection(proj) => Ok(BoundQuery::Projection(bind_projection_schema(
            proj, msg, fds,
        )?)),
        Query::Predicate(pred) => Ok(BoundQuery::Predicate(bind_predicate_schema(
            pred, msg, fds,
        )?)),
        Query::Combined {
            predicate,
            projection,
        } => Ok(BoundQuery::Combined {
            predicate: bind_predicate_schema(predicate, msg, fds)?,
            projection: bind_projection_schema(projection, msg, fds)?,
        }),
    }
}

fn resolve_field_ref<'a>(
    field: &FieldRef,
    msg: &'a DescriptorProto,
) -> Result<(u32, &'a FieldDescriptorProto), CompileError> {
    match field {
        FieldRef::Name(name, span) => {
            let fd =
                find_field_by_name(msg, name).ok_or_else(|| CompileError::UnresolvedField {
                    field: name.clone(),
                    span: *span,
                })?;
            #[allow(clippy::cast_sign_loss)]
            let num = fd.number.unwrap_or(0) as u32;
            Ok((num, fd))
        }
        FieldRef::Number(n, _span) => {
            let fd = find_field_by_number(msg, *n);
            // In schema-bound mode, numbered fields are accepted even if not found
            // (allows mixing), but we try to look them up for encoding info.
            if let Some(fd) = fd {
                Ok((*n, fd))
            } else {
                // Create a synthetic bound field with no descriptor info.
                // We can't validate types but allow the number through.
                // Return a reference to a dummy — instead, just handle in callers.
                Err(CompileError::UnresolvedField {
                    field: format!("#{n}"),
                    span: match field {
                        FieldRef::Number(_, s) | FieldRef::Name(_, s) => *s,
                    },
                })
            }
        }
    }
}

fn bind_projection_schema(
    proj: &Projection,
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
) -> Result<BoundProjection, CompileError> {
    let kind = match &proj.kind {
        ProjectionKind::Strict { items } => {
            let bound_items = items
                .iter()
                .map(|item| bind_projection_item_schema(item, msg, fds))
                .collect::<Result<Vec<_>, CompileError>>()?;
            BoundProjectionKind::Strict { items: bound_items }
        }
        ProjectionKind::Copy {
            items,
            exclusions,
            deep_exclusions,
        } => {
            let bound_items = items
                .iter()
                .map(|item| bind_projection_item_schema(item, msg, fds))
                .collect::<Result<Vec<_>, CompileError>>()?;
            let bound_excl = exclusions
                .iter()
                .map(|f| {
                    let (num, _) = resolve_field_ref(f, msg)?;
                    Ok(num)
                })
                .collect::<Result<Vec<_>, CompileError>>()?;

            if deep_exclusions.is_empty() {
                BoundProjectionKind::Copy {
                    items: bound_items,
                    exclusions: bound_excl,
                }
            } else {
                expand_deep_exclusions(
                    bound_items, bound_excl, deep_exclusions, msg, fds,
                )?
            }
        }
    };
    Ok(BoundProjection {
        kind,
        span: proj.span,
    })
}

/// Expand `..-field` deep exclusions into a regular Copy projection tree.
///
/// Walks the message schema tree and, for every sub-message field that
/// transitively contains a deep-excluded field name, generates a Nested
/// item with a Copy sub-projection carrying the exclusion. Fields that
/// already have explicit Nested items in `items` get the exclusion merged
/// into their sub-projection.
fn expand_deep_exclusions(
    mut items: Vec<BoundProjectionItem>,
    mut exclusions: Vec<u32>,
    deep_exclusions: &[FieldRef],
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
) -> Result<BoundProjectionKind, CompileError> {
    // Resolve deep exclusion field names to their target name strings.
    // We search by name (not number) so the same name matches at any level.
    let target_names: Vec<String> = deep_exclusions
        .iter()
        .map(|f| match f {
            FieldRef::Name(name, _) => Ok(name.clone()),
            FieldRef::Number(n, span) => {
                // For numbered fields, look up the name in the current message
                let fd = find_field_by_number(msg, *n).ok_or_else(|| {
                    CompileError::UnresolvedField {
                        field: format!("#{n}"),
                        span: *span,
                    }
                })?;
                Ok(fd.name.clone().unwrap_or_else(|| format!("#{n}")))
            }
        })
        .collect::<Result<Vec<_>, CompileError>>()?;

    // Validate that each target name exists somewhere in the schema tree.
    let msg_type_name = msg.name.as_deref().unwrap_or("");
    for (name, field_ref) in target_names.iter().zip(deep_exclusions.iter()) {
        let mut visited = std::collections::HashSet::new();
        if !msg_contains_field_deep(msg, &[name.clone()], fds, &mut visited) {
            return Err(CompileError::UnresolvedField {
                field: name.clone(),
                span: field_ref.span(),
            });
        }
    }

    // Add top-level exclusions for fields that exist at the current level.
    for name in &target_names {
        if let Some(fd) = find_field_by_name(msg, name) {
            #[allow(clippy::cast_sign_loss)]
            let num = fd.number.unwrap_or(0) as u32;
            if !exclusions.contains(&num) {
                exclusions.push(num);
            }
        }
    }

    // Collect field numbers that already have Nested items.
    let existing_nested: std::collections::HashSet<u32> = items
        .iter()
        .filter_map(|item| match item {
            BoundProjectionItem::Nested { field, .. } => Some(field.field_num),
            _ => None,
        })
        .collect();

    // For every sub-message field, check if deep exclusions need to be
    // threaded into it.
    for fd in &msg.field {
        if fd.r#type() != ProtoType::Message {
            continue;
        }
        #[allow(clippy::cast_sign_loss)]
        let field_num = fd.number.unwrap_or(0) as u32;

        let type_name = fd.type_name.as_deref().unwrap_or("");
        let nested_msg = match resolve_type_name(fds, type_name) {
            Some(m) => m,
            None => continue,
        };

        // Check if any deep exclusion target exists transitively in this sub-message.
        let mut visited = std::collections::HashSet::new();
        visited.insert(msg_type_name.to_string());
        if !msg_contains_field_deep(nested_msg, &target_names, fds, &mut visited) {
            continue;
        }

        if existing_nested.contains(&field_num) {
            // Merge deep exclusions into the existing Nested item's sub-projection.
            let mut visited = std::collections::HashSet::new();
            visited.insert(msg_type_name.to_string());
            for item in &mut items {
                if let BoundProjectionItem::Nested { field, projection } = item {
                    if field.field_num == field_num {
                        merge_deep_exclusions_into(
                            projection,
                            &target_names,
                            nested_msg,
                            fds,
                            &mut visited,
                        );
                        break;
                    }
                }
            }
        } else {
            // Create a new Nested item with a Copy sub-projection.
            let mut visited = std::collections::HashSet::new();
            visited.insert(msg_type_name.to_string());
            let sub_proj =
                build_deep_exclusion_projection(nested_msg, &target_names, fds, &mut visited);
            items.push(BoundProjectionItem::Nested {
                field: BoundField {
                    field_num,
                    span: Span { start: 0, end: 0 },
                },
                projection: Box::new(sub_proj),
            });
        }
    }

    Ok(BoundProjectionKind::Copy { items, exclusions })
}

/// Check if a message (or any of its sub-messages) contains a field with
/// one of the given names. Uses `visited` to break cycles on
/// self-referential or mutually-recursive message types.
fn msg_contains_field_deep(
    msg: &DescriptorProto,
    names: &[String],
    fds: &FileDescriptorSet,
    visited: &mut std::collections::HashSet<String>,
) -> bool {
    // Check direct fields of this message.
    for fd in &msg.field {
        if let Some(ref name) = fd.name {
            if names.iter().any(|n| n == name) {
                return true;
            }
        }
    }
    // Recurse into sub-message fields.
    for fd in &msg.field {
        if fd.r#type() != ProtoType::Message {
            continue;
        }
        let type_name = fd.type_name.as_deref().unwrap_or("");
        let short_name = type_name.strip_prefix('.').unwrap_or(type_name);
        if !visited.insert(short_name.to_string()) {
            continue; // already visited — break cycle
        }
        if let Some(nested_msg) = resolve_type_name(fds, type_name) {
            if msg_contains_field_deep(nested_msg, names, fds, visited) {
                return true;
            }
        }
    }
    false
}

/// Build a Copy projection that excludes the target fields and recurses
/// into sub-messages that transitively contain them.
fn build_deep_exclusion_projection(
    msg: &DescriptorProto,
    target_names: &[String],
    fds: &FileDescriptorSet,
    visited: &mut std::collections::HashSet<String>,
) -> BoundProjection {
    let mut exclusions = Vec::new();
    let mut items = Vec::new();

    // Exclude target fields at this level.
    for name in target_names {
        if let Some(fd) = find_field_by_name(msg, name) {
            #[allow(clippy::cast_sign_loss)]
            let num = fd.number.unwrap_or(0) as u32;
            if !exclusions.contains(&num) {
                exclusions.push(num);
            }
        }
    }

    // Recurse into sub-message fields that transitively contain targets.
    for fd in &msg.field {
        if fd.r#type() != ProtoType::Message {
            continue;
        }
        let type_name = fd.type_name.as_deref().unwrap_or("");
        let short_name = type_name.strip_prefix('.').unwrap_or(type_name);
        if visited.contains(short_name) {
            continue; // already visited — break cycle
        }
        let nested_msg = match resolve_type_name(fds, type_name) {
            Some(m) => m,
            None => continue,
        };
        let mut child_visited = visited.clone();
        child_visited.insert(short_name.to_string());
        if !msg_contains_field_deep(nested_msg, target_names, fds, &mut child_visited.clone()) {
            continue;
        }
        #[allow(clippy::cast_sign_loss)]
        let field_num = fd.number.unwrap_or(0) as u32;
        visited.insert(short_name.to_string());
        let sub_proj =
            build_deep_exclusion_projection(nested_msg, target_names, fds, visited);
        items.push(BoundProjectionItem::Nested {
            field: BoundField {
                field_num,
                span: Span { start: 0, end: 0 },
            },
            projection: Box::new(sub_proj),
        });
    }

    BoundProjection {
        kind: BoundProjectionKind::Copy { items, exclusions },
        span: Span { start: 0, end: 0 },
    }
}

/// Merge deep exclusions into an existing Nested projection's sub-tree.
fn merge_deep_exclusions_into(
    proj: &mut BoundProjection,
    target_names: &[String],
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
    visited: &mut std::collections::HashSet<String>,
) {
    match &mut proj.kind {
        BoundProjectionKind::Copy {
            items, exclusions, ..
        } => {
            // Add exclusions at this level.
            for name in target_names {
                if let Some(fd) = find_field_by_name(msg, name) {
                    #[allow(clippy::cast_sign_loss)]
                    let num = fd.number.unwrap_or(0) as u32;
                    if !exclusions.contains(&num) {
                        exclusions.push(num);
                    }
                }
            }

            // Collect existing nested field numbers.
            let existing_nested: std::collections::HashSet<u32> = items
                .iter()
                .filter_map(|item| match item {
                    BoundProjectionItem::Nested { field, .. } => Some(field.field_num),
                    _ => None,
                })
                .collect();

            // Recurse into sub-message fields.
            let mut new_nested = Vec::new();
            for fd in &msg.field {
                if fd.r#type() != ProtoType::Message {
                    continue;
                }
                #[allow(clippy::cast_sign_loss)]
                let field_num = fd.number.unwrap_or(0) as u32;
                let type_name = fd.type_name.as_deref().unwrap_or("");
                let short_name = type_name.strip_prefix('.').unwrap_or(type_name);
                if visited.contains(short_name) {
                    continue; // break cycle
                }
                let nested_msg = match resolve_type_name(fds, type_name) {
                    Some(m) => m,
                    None => continue,
                };
                if !msg_contains_field_deep(
                    nested_msg,
                    target_names,
                    fds,
                    &mut visited.clone(),
                ) {
                    continue;
                }

                visited.insert(short_name.to_string());
                if existing_nested.contains(&field_num) {
                    // Merge into existing Nested item.
                    for item in items.iter_mut() {
                        if let BoundProjectionItem::Nested { field, projection } = item {
                            if field.field_num == field_num {
                                merge_deep_exclusions_into(
                                    projection,
                                    target_names,
                                    nested_msg,
                                    fds,
                                    visited,
                                );
                                break;
                            }
                        }
                    }
                } else {
                    let sub_proj =
                        build_deep_exclusion_projection(nested_msg, target_names, fds, visited);
                    new_nested.push(BoundProjectionItem::Nested {
                        field: BoundField {
                            field_num,
                            span: Span { start: 0, end: 0 },
                        },
                        projection: Box::new(sub_proj),
                    });
                }
            }
            items.extend(new_nested);
        }
        BoundProjectionKind::Strict { items } => {
            // For Strict projections, only merge into existing Nested items.
            for item in items.iter_mut() {
                if let BoundProjectionItem::Nested { field, projection } = item {
                    let fd = find_field_by_number(msg, field.field_num);
                    if let Some(fd) = fd {
                        if fd.r#type() == ProtoType::Message {
                            let type_name = fd.type_name.as_deref().unwrap_or("");
                            let short_name =
                                type_name.strip_prefix('.').unwrap_or(type_name);
                            if visited.contains(short_name) {
                                continue;
                            }
                            visited.insert(short_name.to_string());
                            if let Some(nested_msg) = resolve_type_name(fds, type_name) {
                                merge_deep_exclusions_into(
                                    projection,
                                    target_names,
                                    nested_msg,
                                    fds,
                                    visited,
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

fn bind_projection_item_schema(
    item: &ProjectionItem,
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
) -> Result<BoundProjectionItem, CompileError> {
    match item {
        ProjectionItem::Field(f) => {
            let (num, _) = resolve_field_ref(f, msg)?;
            Ok(BoundProjectionItem::Field(BoundField {
                field_num: num,
                span: f.span(),
            }))
        }
        ProjectionItem::Nested { field, projection } => {
            let (num, fd) = resolve_field_ref(field, msg)?;
            let nested_msg = if fd.r#type() == ProtoType::Message {
                let type_name = fd.type_name.as_deref().unwrap_or("");
                resolve_type_name(fds, type_name).ok_or_else(|| {
                    CompileError::InvalidMessageType {
                        type_name: type_name.to_string(),
                    }
                })?
            } else {
                return Err(CompileError::TypeError {
                    field: fd.name.as_deref().unwrap_or("?").to_string(),
                    expected: "message (for nested projection)",
                    actual: "non-message",
                    span: field.span(),
                });
            };
            Ok(BoundProjectionItem::Nested {
                field: BoundField {
                    field_num: num,
                    span: field.span(),
                },
                projection: Box::new(bind_projection_schema(projection, nested_msg, fds)?),
            })
        }
    }
}

fn bind_predicate_schema(
    pred: &Predicate,
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
) -> Result<BoundPredicate, CompileError> {
    let kind = match &pred.kind {
        PredicateKind::And(l, r) => BoundPredicateKind::And(
            Box::new(bind_predicate_schema(l, msg, fds)?),
            Box::new(bind_predicate_schema(r, msg, fds)?),
        ),
        PredicateKind::Or(l, r) => BoundPredicateKind::Or(
            Box::new(bind_predicate_schema(l, msg, fds)?),
            Box::new(bind_predicate_schema(r, msg, fds)?),
        ),
        PredicateKind::Not(inner) => {
            BoundPredicateKind::Not(Box::new(bind_predicate_schema(inner, msg, fds)?))
        }
        PredicateKind::Comparison { field, op, value } => {
            let bound_path = bind_field_path_schema(field, msg, fds, Some(value))?;
            let resolved = if let Some(fd) = get_leaf_field_descriptor(field, msg, fds)? {
                resolve_enum_literal(fd, value, fds)?.unwrap_or_else(|| value.clone())
            } else {
                value.clone()
            };
            BoundPredicateKind::Comparison {
                field: bound_path,
                op: *op,
                value: resolved,
            }
        }
        PredicateKind::Presence(field) => {
            let bound_path = bind_field_path_schema(field, msg, fds, None)?;
            BoundPredicateKind::Presence(bound_path)
        }
        PredicateKind::InSet { field, values } => {
            let bound_path = bind_field_path_schema(field, msg, fds, values.first())?;
            let resolved = if let Some(fd) = get_leaf_field_descriptor(field, msg, fds)? {
                for v in values {
                    validate_literal_type(fd, v, v.span())?;
                }
                values
                    .iter()
                    .map(|v| {
                        resolve_enum_literal(fd, v, fds).map(|opt| opt.unwrap_or_else(|| v.clone()))
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                values.clone()
            };
            BoundPredicateKind::InSet {
                field: bound_path,
                values: resolved,
            }
        }
        PredicateKind::StringPredicate { field, op, value } => {
            if let Some(fd) = get_leaf_field_descriptor(field, msg, fds)? {
                if fd.r#type() == ProtoType::Enum {
                    // Expand string predicate on enum to InSet at compile time
                    let matching = resolve_enum_string_predicate(fd, *op, value, fds)?;
                    let bound_path = bind_field_path_schema(field, msg, fds, matching.first())?;
                    BoundPredicateKind::InSet {
                        field: bound_path,
                        values: matching,
                    }
                } else {
                    let bound_path = bind_field_path_schema(field, msg, fds, Some(value))?;
                    BoundPredicateKind::StringPredicate {
                        field: bound_path,
                        op: *op,
                        value: value.clone(),
                    }
                }
            } else {
                let bound_path = bind_field_path_schema(field, msg, fds, Some(value))?;
                BoundPredicateKind::StringPredicate {
                    field: bound_path,
                    op: *op,
                    value: value.clone(),
                }
            }
        }
    };
    Ok(BoundPredicate {
        kind,
        span: pred.span,
    })
}

/// Walk a field path through the schema, resolving each segment.
fn bind_field_path_schema(
    path: &FieldPath,
    msg: &DescriptorProto,
    fds: &FileDescriptorSet,
    literal: Option<&Literal>,
) -> Result<BoundFieldPath, CompileError> {
    let mut segments = Vec::with_capacity(path.segments.len());
    let mut current_msg = msg;
    let mut last_fd: Option<&FieldDescriptorProto> = None;

    for (i, seg) in path.segments.iter().enumerate() {
        let (num, fd) = resolve_field_ref(seg, current_msg)?;
        segments.push(num);

        if i + 1 == path.segments.len() {
            last_fd = Some(fd);
        } else {
            // Intermediate segments must be message types
            if fd.r#type() == ProtoType::Message {
                let type_name = fd.type_name.as_deref().unwrap_or("");
                current_msg = resolve_type_name(fds, type_name).ok_or_else(|| {
                    CompileError::InvalidMessageType {
                        type_name: type_name.to_string(),
                    }
                })?;
            } else {
                return Err(CompileError::TypeError {
                    field: fd.name.as_deref().unwrap_or("?").to_string(),
                    expected: "message (for path traversal)",
                    actual: "non-message",
                    span: seg.span(),
                });
            }
        }
    }

    // Determine encoding from the leaf field's proto type
    let encoding = last_fd.map(|fd| proto_type_to_encoding(fd.r#type()));

    // Validate literal type if present
    if let (Some(fd), Some(lit)) = (last_fd, literal) {
        validate_literal_type(fd, lit, lit.span())?;
    }

    Ok(BoundFieldPath {
        segments,
        encoding,
        span: path.span,
    })
}

/// Get the leaf field descriptor for a field path (for validation purposes).
fn get_leaf_field_descriptor<'a>(
    path: &FieldPath,
    msg: &'a DescriptorProto,
    fds: &'a FileDescriptorSet,
) -> Result<Option<&'a FieldDescriptorProto>, CompileError> {
    let mut current_msg = msg;
    let mut last_fd: Option<&FieldDescriptorProto> = None;

    for (i, seg) in path.segments.iter().enumerate() {
        let (_, fd) = resolve_field_ref(seg, current_msg)?;
        if i + 1 == path.segments.len() {
            last_fd = Some(fd);
        } else if fd.r#type() == ProtoType::Message {
            let type_name = fd.type_name.as_deref().unwrap_or("");
            current_msg = resolve_type_name(fds, type_name).ok_or_else(|| {
                CompileError::InvalidMessageType {
                    type_name: type_name.to_string(),
                }
            })?;
        }
    }
    Ok(last_fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::CompileOptions;
    use crate::parse;
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

    /// Build a test schema:
    /// Person { name: string=1, age: int64=2, address: Address=3, status: Status=4 }
    /// Address { city: string=1, country: string=2 }
    /// enum Status { ACTIVE=0, INACTIVE=1 }
    fn test_schema() -> Vec<u8> {
        let address_msg = DescriptorProto {
            name: Some("Address".to_string()),
            field: vec![
                make_field("city", 1, ProtoType::String, None),
                make_field("country", 2, ProtoType::String, None),
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
        let person_msg = DescriptorProto {
            name: Some("Person".to_string()),
            field: vec![
                make_field("name", 1, ProtoType::String, None),
                make_field("age", 2, ProtoType::Int64, None),
                make_field("address", 3, ProtoType::Message, Some(".test.Address")),
                make_field("status", 4, ProtoType::Enum, Some(".test.Status")),
            ],
            nested_type: vec![],
            enum_type: vec![],
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
    fn bind_sf_flat_projection() {
        let q = parse("{ #1, #2 }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 2);
                    match &items[0] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 1),
                        other => panic!("expected Field, got {other:?}"),
                    }
                    match &items[1] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 2),
                        other => panic!("expected Field, got {other:?}"),
                    }
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_named_field_rejected() {
        let q = parse("{ name }").unwrap();
        let err = bind_schema_free(&q).unwrap_err();
        match err {
            CompileError::NamedFieldWithoutSchema { field, .. } => {
                assert_eq!(field, "name");
            }
            other => panic!("expected NamedFieldWithoutSchema, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_copy_mode() {
        let q = parse("{ #1, .. }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Copy { items, exclusions } => {
                    assert_eq!(items.len(), 1);
                    assert!(exclusions.is_empty());
                }
                other => panic!("expected Copy, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_copy_no_items() {
        let q = parse("{ .. }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Copy { items, exclusions } => {
                    assert!(items.is_empty());
                    assert!(exclusions.is_empty());
                }
                other => panic!("expected Copy, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_copy_exclusion() {
        let q = parse("{ -#7, .. }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Copy { items, exclusions } => {
                    assert!(items.is_empty());
                    assert_eq!(exclusions, &[7]);
                }
                other => panic!("expected Copy, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_predicate_encoding_inference() {
        let q = parse("#1 > 42").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Comparison { field, .. } => {
                    assert_eq!(field.segments, vec![1]);
                    assert_eq!(field.encoding, Some(Encoding::Varint));
                }
                other => panic!("expected Comparison, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_string_predicate_encoding() {
        let q = parse(r#"#1 == "hello""#).unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Comparison { field, .. } => {
                    assert_eq!(field.encoding, Some(Encoding::Len));
                }
                other => panic!("expected Comparison, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_presence_no_encoding() {
        let q = parse("exists(#1)").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Presence(field) => {
                    assert_eq!(field.segments, vec![1]);
                    assert_eq!(field.encoding, None);
                }
                other => panic!("expected Presence, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_nested_projection() {
        let q = parse("{ #1, #3 { #1 } }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 2);
                    match &items[1] {
                        BoundProjectionItem::Nested { field, projection } => {
                            assert_eq!(field.field_num, 3);
                            match &projection.kind {
                                BoundProjectionKind::Strict { items: inner } => {
                                    assert_eq!(inner.len(), 1);
                                }
                                other => panic!("expected inner Strict, got {other:?}"),
                            }
                        }
                        other => panic!("expected Nested, got {other:?}"),
                    }
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_combined() {
        let q = parse("WHERE #2 > 18 SELECT { #1 }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        assert!(matches!(bound, BoundQuery::Combined { .. }));
    }

    // ─── Schema-bound tests ───

    #[test]
    fn bind_resolve_name() {
        let schema = test_schema();
        let q = parse("{ name, age }").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 2);
                    match &items[0] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 1),
                        other => panic!("expected Field, got {other:?}"),
                    }
                    match &items[1] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 2),
                        other => panic!("expected Field, got {other:?}"),
                    }
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_resolve_nested() {
        let schema = test_schema();
        let q = parse("{ address { city } }").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 1);
                    match &items[0] {
                        BoundProjectionItem::Nested { field, projection } => {
                            assert_eq!(field.field_num, 3);
                            match &projection.kind {
                                BoundProjectionKind::Strict { items: inner } => {
                                    assert_eq!(inner.len(), 1);
                                    match &inner[0] {
                                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 1),
                                        other => panic!("expected Field, got {other:?}"),
                                    }
                                }
                                other => panic!("expected Strict, got {other:?}"),
                            }
                        }
                        other => panic!("expected Nested, got {other:?}"),
                    }
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_field_number_in_schema() {
        let schema = test_schema();
        let q = parse("{ #1, #2 }").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 2);
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_mixed_name_and_number() {
        let schema = test_schema();
        let q = parse("{ name, #2 }").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Strict { items } => {
                    assert_eq!(items.len(), 2);
                    match &items[0] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 1),
                        other => panic!("expected Field, got {other:?}"),
                    }
                    match &items[1] {
                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 2),
                        other => panic!("expected Field, got {other:?}"),
                    }
                }
                other => panic!("expected Strict, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_unresolved_field() {
        let schema = test_schema();
        let q = parse("{ unknown }").unwrap();
        let err = bind_with_schema(&q, &schema_opts(&schema)).unwrap_err();
        assert!(matches!(err, CompileError::UnresolvedField { .. }));
    }

    #[test]
    fn bind_invalid_message_type() {
        let schema = test_schema();
        let opts = CompileOptions {
            schema: Some(&schema),
            root_message: Some("test.NonExistent"),
        };
        let q = parse("{ name }").unwrap();
        let err = bind_with_schema(&q, &opts).unwrap_err();
        assert!(matches!(err, CompileError::InvalidMessageType { .. }));
    }

    #[test]
    fn bind_missing_root_message() {
        let schema = test_schema();
        let opts = CompileOptions {
            schema: Some(&schema),
            root_message: None,
        };
        let q = parse("{ name }").unwrap();
        let err = bind_with_schema(&q, &opts).unwrap_err();
        assert!(matches!(err, CompileError::MissingRootMessage));
    }

    #[test]
    fn bind_type_check_int_ok() {
        let schema = test_schema();
        let q = parse("age > 18").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Comparison { field, .. } => {
                    assert_eq!(field.segments, vec![2]);
                    assert_eq!(field.encoding, Some(Encoding::Varint));
                }
                other => panic!("expected Comparison, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_type_check_string_ok() {
        let schema = test_schema();
        let q = parse(r#"name == "Alice""#).unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Comparison { field, .. } => {
                    assert_eq!(field.segments, vec![1]);
                    assert_eq!(field.encoding, Some(Encoding::Len));
                }
                other => panic!("expected Comparison, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_type_check_mismatch() {
        let schema = test_schema();
        let q = parse(r#"age == "old""#).unwrap();
        let err = bind_with_schema(&q, &schema_opts(&schema)).unwrap_err();
        assert!(matches!(err, CompileError::TypeError { .. }));
    }

    #[test]
    fn bind_predicate_nested() {
        let schema = test_schema();
        let q = parse(r#"address.city == "NYC""#).unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::Comparison { field, .. } => {
                    assert_eq!(field.segments, vec![3, 1]);
                    assert_eq!(field.encoding, Some(Encoding::Len));
                }
                other => panic!("expected Comparison, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn bind_in_set_type_check() {
        let schema = test_schema();
        let q = parse("status in [0, 1]").unwrap();
        let bound = bind_with_schema(&q, &schema_opts(&schema)).unwrap();
        match &bound {
            BoundQuery::Predicate(pred) => match &pred.kind {
                BoundPredicateKind::InSet { field, values } => {
                    assert_eq!(field.segments, vec![4]);
                    assert_eq!(values.len(), 2);
                }
                other => panic!("expected InSet, got {other:?}"),
            },
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    // ─── Deep exclusion tests ───

    /// Build a schema where "secret" appears at multiple levels:
    /// Outer { id: int64=1, secret: string=2, inner: Inner=3, label: string=4 }
    /// Inner { value: string=1, secret: string=2 }
    fn deep_excl_schema() -> Vec<u8> {
        let inner_msg = DescriptorProto {
            name: Some("Inner".to_string()),
            field: vec![
                make_field("value", 1, ProtoType::String, None),
                make_field("secret", 2, ProtoType::String, None),
            ],
            ..Default::default()
        };
        let outer_msg = DescriptorProto {
            name: Some("Outer".to_string()),
            field: vec![
                make_field("id", 1, ProtoType::Int64, None),
                make_field("secret", 2, ProtoType::String, None),
                make_field("inner", 3, ProtoType::Message, Some(".test.Inner")),
                make_field("label", 4, ProtoType::String, None),
            ],
            ..Default::default()
        };
        let fds = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("test.proto".to_string()),
                package: Some("test".to_string()),
                message_type: vec![outer_msg, inner_msg],
                ..Default::default()
            }],
        };
        prost::Message::encode_to_vec(&fds)
    }

    fn deep_excl_opts(schema: &[u8]) -> CompileOptions<'_> {
        CompileOptions {
            schema: Some(schema),
            root_message: Some("test.Outer"),
        }
    }

    #[test]
    fn bind_deep_exclusion_expands_at_both_levels() {
        let schema = deep_excl_schema();
        let q = parse("{ ..-secret, .. }").unwrap();
        let bound = bind_with_schema(&q, &deep_excl_opts(&schema)).unwrap();
        let BoundQuery::Projection(proj) = &bound else {
            panic!("expected Projection");
        };
        let BoundProjectionKind::Copy { exclusions, items } = &proj.kind else {
            panic!("expected Copy");
        };
        // Top-level: secret (field 2) is excluded.
        assert!(exclusions.contains(&2), "top-level secret should be excluded");
        // A Nested item for inner (field 3) should be generated.
        let nested = items
            .iter()
            .find_map(|item| match item {
                BoundProjectionItem::Nested { field, projection } if field.field_num == 3 => {
                    Some(projection)
                }
                _ => None,
            })
            .expect("should have Nested item for field 3 (inner)");
        // The nested projection should exclude secret (field 2) inside Inner.
        let BoundProjectionKind::Copy {
            exclusions: inner_excl,
            ..
        } = &nested.kind
        else {
            panic!("expected Copy for nested projection");
        };
        assert!(
            inner_excl.contains(&2),
            "inner secret should be excluded"
        );
    }

    #[test]
    fn bind_deep_exclusion_no_match_at_nested_level() {
        // "id" only exists at top level, not in Inner.
        let schema = deep_excl_schema();
        let q = parse("{ ..-id, .. }").unwrap();
        let bound = bind_with_schema(&q, &deep_excl_opts(&schema)).unwrap();
        let BoundQuery::Projection(proj) = &bound else {
            panic!("expected Projection");
        };
        let BoundProjectionKind::Copy { exclusions, items } = &proj.kind else {
            panic!("expected Copy");
        };
        // Top-level: id (field 1) is excluded.
        assert!(exclusions.contains(&1));
        // No Nested item should be generated since Inner doesn't have "id".
        assert!(
            !items.iter().any(|item| matches!(item, BoundProjectionItem::Nested { .. })),
            "no Nested item needed when field only exists at top level"
        );
    }

    #[test]
    fn bind_deep_exclusion_schema_free_rejected() {
        let q = parse("{ ..-#2, .. }").unwrap();
        let err = bind_schema_free(&q).unwrap_err();
        assert!(matches!(err, CompileError::DeepExclusionWithoutSchema { .. }));
    }

    #[test]
    fn bind_deep_exclusion_merges_with_explicit_nested() {
        // User writes `{ inner { value }, ..-secret, .. }` — the explicit
        // Nested for inner should get secret excluded too.
        let schema = deep_excl_schema();
        let q = parse("{ inner { value, .. }, ..-secret, .. }").unwrap();
        let bound = bind_with_schema(&q, &deep_excl_opts(&schema)).unwrap();
        let BoundQuery::Projection(proj) = &bound else {
            panic!("expected Projection");
        };
        let BoundProjectionKind::Copy { exclusions, items } = &proj.kind else {
            panic!("expected Copy");
        };
        // Top-level secret excluded.
        assert!(exclusions.contains(&2));
        // The user's explicit Nested for inner should now also have secret excluded.
        let nested = items
            .iter()
            .find_map(|item| match item {
                BoundProjectionItem::Nested { field, projection } if field.field_num == 3 => {
                    Some(projection)
                }
                _ => None,
            })
            .expect("should have Nested item for field 3 (inner)");
        let BoundProjectionKind::Copy {
            exclusions: inner_excl,
            items: inner_items,
        } = &nested.kind
        else {
            panic!("expected Copy for nested projection");
        };
        // Inner.secret (field 2) should be excluded.
        assert!(inner_excl.contains(&2));
        // Inner.value (field 1) should still be present as an explicit item.
        assert!(inner_items
            .iter()
            .any(|item| matches!(item, BoundProjectionItem::Field(f) if f.field_num == 1)));
    }

    /// Build a self-referential schema:
    /// Tree { name: string=1, child: Tree=2 }
    fn recursive_schema() -> Vec<u8> {
        let tree_msg = DescriptorProto {
            name: Some("Tree".to_string()),
            field: vec![
                make_field("name", 1, ProtoType::String, None),
                make_field("child", 2, ProtoType::Message, Some(".test.Tree")),
            ],
            ..Default::default()
        };
        let fds = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("test.proto".to_string()),
                package: Some("test".to_string()),
                message_type: vec![tree_msg],
                ..Default::default()
            }],
        };
        prost::Message::encode_to_vec(&fds)
    }

    #[test]
    fn bind_deep_exclusion_self_referential_no_infinite_recursion() {
        let schema = recursive_schema();
        let opts = CompileOptions {
            schema: Some(&schema),
            root_message: Some("test.Tree"),
        };
        // This would stack-overflow without cycle detection.
        let q = parse("{ ..-name, .. }").unwrap();
        let bound = bind_with_schema(&q, &opts).unwrap();
        let BoundQuery::Projection(proj) = &bound else {
            panic!("expected Projection");
        };
        let BoundProjectionKind::Copy { exclusions, items } = &proj.kind else {
            panic!("expected Copy");
        };
        // name (field 1) excluded at top level.
        assert!(exclusions.contains(&1));
        // child (field 2) should have a Nested Frame that also excludes name.
        let nested = items
            .iter()
            .find_map(|item| match item {
                BoundProjectionItem::Nested { field, projection } if field.field_num == 2 => {
                    Some(projection)
                }
                _ => None,
            })
            .expect("should have Nested item for child");
        let BoundProjectionKind::Copy {
            exclusions: child_excl,
            ..
        } = &nested.kind
        else {
            panic!("expected Copy for child projection");
        };
        assert!(child_excl.contains(&1), "child.name should be excluded");
    }

    #[test]
    fn bind_deep_exclusion_unresolved_field_error() {
        // "..-typo" should error when the field doesn't exist anywhere.
        let schema = deep_excl_schema();
        let q = parse("{ ..-typo, .. }").unwrap();
        let err = bind_with_schema(&q, &deep_excl_opts(&schema)).unwrap_err();
        assert!(
            matches!(err, CompileError::UnresolvedField { .. }),
            "expected UnresolvedField, got {err:?}"
        );
    }
}
