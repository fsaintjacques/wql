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
    Inclusion {
        items: Vec<BoundProjectionItem>,
        preserve_unknowns: bool,
    },
    DeepCopy {
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
    /// Deep field search at any nesting depth.
    DeepSearch(BoundField),
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

fn bind_projection_sf(proj: &Projection) -> Result<BoundProjection, CompileError> {
    let kind = match &proj.kind {
        ProjectionKind::Inclusion {
            items,
            preserve_unknowns,
        } => {
            let bound_items = items
                .iter()
                .map(|item| match item {
                    ProjectionItem::Field(f) => Ok(BoundProjectionItem::Field(require_number(f)?)),
                    ProjectionItem::Nested { field, projection } => {
                        Ok(BoundProjectionItem::Nested {
                            field: require_number(field)?,
                            projection: Box::new(bind_projection_sf(projection)?),
                        })
                    }
                    ProjectionItem::DeepSearch(f) => {
                        Ok(BoundProjectionItem::DeepSearch(require_number(f)?))
                    }
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            BoundProjectionKind::Inclusion {
                items: bound_items,
                preserve_unknowns: *preserve_unknowns,
            }
        }
        ProjectionKind::DeepCopy { exclusions } => {
            let bound_excl = exclusions
                .iter()
                .map(|f| Ok(require_number(f)?.field_num))
                .collect::<Result<Vec<_>, CompileError>>()?;
            BoundProjectionKind::DeepCopy {
                exclusions: bound_excl,
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
    field_descriptor_proto::Type as ProtoType, DescriptorProto, FieldDescriptorProto,
    FileDescriptorSet,
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
            Literal::Int(..) => Ok(()),
            _ => Err(CompileError::TypeError {
                field: field_name,
                expected: "integer (enum ordinal)",
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
        ProjectionKind::Inclusion {
            items,
            preserve_unknowns,
        } => {
            let bound_items = items
                .iter()
                .map(|item| bind_projection_item_schema(item, msg, fds))
                .collect::<Result<Vec<_>, CompileError>>()?;
            BoundProjectionKind::Inclusion {
                items: bound_items,
                preserve_unknowns: *preserve_unknowns,
            }
        }
        ProjectionKind::DeepCopy { exclusions } => {
            let bound_excl = exclusions
                .iter()
                .map(|f| {
                    let (num, _) = resolve_field_ref(f, msg)?;
                    Ok(num)
                })
                .collect::<Result<Vec<_>, CompileError>>()?;
            BoundProjectionKind::DeepCopy {
                exclusions: bound_excl,
            }
        }
    };
    Ok(BoundProjection {
        kind,
        span: proj.span,
    })
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
        ProjectionItem::DeepSearch(f) => {
            let (num, _) = resolve_field_ref(f, msg)?;
            Ok(BoundProjectionItem::DeepSearch(BoundField {
                field_num: num,
                span: f.span(),
            }))
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
            BoundPredicateKind::Comparison {
                field: bound_path,
                op: *op,
                value: value.clone(),
            }
        }
        PredicateKind::Presence(field) => {
            let bound_path = bind_field_path_schema(field, msg, fds, None)?;
            BoundPredicateKind::Presence(bound_path)
        }
        PredicateKind::InSet { field, values } => {
            let bound_path = bind_field_path_schema(field, msg, fds, values.first())?;
            // Validate all values in the set
            if let Some(fd) = get_leaf_field_descriptor(field, msg, fds)? {
                for v in values {
                    validate_literal_type(fd, v, v.span())?;
                }
            }
            BoundPredicateKind::InSet {
                field: bound_path,
                values: values.clone(),
            }
        }
        PredicateKind::StringPredicate { field, op, value } => {
            let bound_path = bind_field_path_schema(field, msg, fds, Some(value))?;
            BoundPredicateKind::StringPredicate {
                field: bound_path,
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
                BoundProjectionKind::Inclusion {
                    items,
                    preserve_unknowns,
                } => {
                    assert!(!preserve_unknowns);
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
                other => panic!("expected Inclusion, got {other:?}"),
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
    fn bind_sf_preserve_unknowns() {
        let q = parse("{ #1, ... }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::Inclusion {
                    preserve_unknowns, ..
                } => {
                    assert!(preserve_unknowns);
                }
                other => panic!("expected Inclusion, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_deep_copy() {
        let q = parse("{ .. }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::DeepCopy { exclusions } => {
                    assert!(exclusions.is_empty());
                }
                other => panic!("expected DeepCopy, got {other:?}"),
            },
            other => panic!("expected Projection, got {other:?}"),
        }
    }

    #[test]
    fn bind_sf_deep_copy_exclusion() {
        let q = parse("{ .. -#7 }").unwrap();
        let bound = bind_schema_free(&q).unwrap();
        match &bound {
            BoundQuery::Projection(proj) => match &proj.kind {
                BoundProjectionKind::DeepCopy { exclusions } => {
                    assert_eq!(exclusions, &[7]);
                }
                other => panic!("expected DeepCopy, got {other:?}"),
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
                BoundProjectionKind::Inclusion { items, .. } => {
                    assert_eq!(items.len(), 2);
                    match &items[1] {
                        BoundProjectionItem::Nested { field, projection } => {
                            assert_eq!(field.field_num, 3);
                            match &projection.kind {
                                BoundProjectionKind::Inclusion { items: inner, .. } => {
                                    assert_eq!(inner.len(), 1);
                                }
                                other => panic!("expected inner Inclusion, got {other:?}"),
                            }
                        }
                        other => panic!("expected Nested, got {other:?}"),
                    }
                }
                other => panic!("expected Inclusion, got {other:?}"),
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
                BoundProjectionKind::Inclusion { items, .. } => {
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
                other => panic!("expected Inclusion, got {other:?}"),
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
                BoundProjectionKind::Inclusion { items, .. } => {
                    assert_eq!(items.len(), 1);
                    match &items[0] {
                        BoundProjectionItem::Nested { field, projection } => {
                            assert_eq!(field.field_num, 3);
                            match &projection.kind {
                                BoundProjectionKind::Inclusion { items: inner, .. } => {
                                    assert_eq!(inner.len(), 1);
                                    match &inner[0] {
                                        BoundProjectionItem::Field(f) => assert_eq!(f.field_num, 1),
                                        other => panic!("expected Field, got {other:?}"),
                                    }
                                }
                                other => panic!("expected Inclusion, got {other:?}"),
                            }
                        }
                        other => panic!("expected Nested, got {other:?}"),
                    }
                }
                other => panic!("expected Inclusion, got {other:?}"),
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
                BoundProjectionKind::Inclusion { items, .. } => {
                    assert_eq!(items.len(), 2);
                }
                other => panic!("expected Inclusion, got {other:?}"),
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
                BoundProjectionKind::Inclusion { items, .. } => {
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
                other => panic!("expected Inclusion, got {other:?}"),
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
}
