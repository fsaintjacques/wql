use std::collections::HashMap;

use crate::ast::{CompareOp, Literal, StringOp};
use crate::bind::{
    BoundFieldPath, BoundPredicate, BoundPredicateKind, BoundProjection, BoundProjectionItem,
    BoundProjectionKind, BoundQuery,
};
use crate::error::CompileError;
use wql_ir::{ArmAction, ArmMatch, DefaultAction, DispatchArm, Encoding, Instruction};

// ═══════════════════════════════════════════════════════════════════════
// Emitter state
// ═══════════════════════════════════════════════════════════════════════

struct Emitter {
    instructions: Vec<Instruction>,
    next_label: u32,
    next_register: u8,
}

impl Emitter {
    fn new() -> Self {
        Self {
            instructions: Vec::new(),
            next_label: 0,
            next_register: 0,
        }
    }

    fn alloc_label(&mut self) -> u32 {
        let idx = self.next_label;
        self.next_label += 1;
        idx
    }

    fn alloc_register(&mut self) -> Result<u8, CompileError> {
        if self.next_register >= 16 {
            return Err(CompileError::TooManyRegisters);
        }
        let reg = self.next_register;
        self.next_register += 1;
        Ok(reg)
    }

    fn push(&mut self, instr: Instruction) {
        self.instructions.push(instr);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Public entry point
// ═══════════════════════════════════════════════════════════════════════

/// Emit WVM instructions from a bound query.
///
/// # Errors
///
/// Returns [`CompileError::TooManyRegisters`] if more than 16 registers are needed.
pub fn emit(bound: &BoundQuery) -> Result<Vec<Instruction>, CompileError> {
    let mut emitter = Emitter::new();

    match bound {
        BoundQuery::Projection(proj) => {
            emit_projection(&mut emitter, proj)?;
        }
        BoundQuery::Predicate(pred) => {
            emit_predicate_program(&mut emitter, pred)?;
        }
        BoundQuery::Combined {
            predicate,
            projection,
        } => {
            emit_combined(&mut emitter, predicate, projection)?;
        }
    }

    Ok(emitter.instructions)
}

// ═══════════════════════════════════════════════════════════════════════
// Projection lowering
// ═══════════════════════════════════════════════════════════════════════

/// Deferred sub-program: nested projection to emit after the parent DISPATCH.
struct DeferredNested<'a> {
    projection: &'a BoundProjection,
}

fn emit_projection(emitter: &mut Emitter, proj: &BoundProjection) -> Result<(), CompileError> {
    match &proj.kind {
        BoundProjectionKind::Strict { items } => emit_strict(emitter, items),
        BoundProjectionKind::Copy { items, exclusions } => emit_copy(emitter, items, exclusions),
    }
}

fn emit_strict(emitter: &mut Emitter, items: &[BoundProjectionItem]) -> Result<(), CompileError> {
    let mut arms = Vec::new();
    let mut deferred: Vec<DeferredNested<'_>> = Vec::new();
    let mut has_deep_search = false;

    for item in items {
        if matches!(item, BoundProjectionItem::DeepSearch(_)) {
            has_deep_search = true;
            break;
        }
    }

    let self_label = if has_deep_search {
        let label = emitter.alloc_label();
        emitter.push(Instruction::Label);
        Some(label)
    } else {
        None
    };

    for item in items {
        match item {
            BoundProjectionItem::Field(f) | BoundProjectionItem::DeepSearch(f) => {
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(f.field_num),
                    actions: vec![ArmAction::Copy],
                });
            }
            BoundProjectionItem::Nested { field, projection } => {
                let label = emitter.alloc_label();
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(field.field_num),
                    actions: vec![ArmAction::Frame(label)],
                });
                deferred.push(DeferredNested { projection });
            }
        }
    }

    let default = if has_deep_search {
        DefaultAction::Recurse(self_label.expect("self_label set when has_deep_search"))
    } else {
        DefaultAction::Skip
    };

    emitter.push(Instruction::Dispatch { default, arms });
    emitter.push(Instruction::Return);

    for nested in deferred {
        emitter.push(Instruction::Label);
        emit_projection(emitter, nested.projection)?;
    }

    Ok(())
}

fn emit_copy(
    emitter: &mut Emitter,
    items: &[BoundProjectionItem],
    exclusions: &[u32],
) -> Result<(), CompileError> {
    let mut arms = Vec::new();
    let mut deferred: Vec<DeferredNested<'_>> = Vec::new();

    // Copy mode always uses DefaultAction::Copy (shallow copy) to preserve
    // all unmatched fields verbatim. Exclusions apply at the current level only.

    // Only Nested items need explicit Frame arms. Field/DeepSearch items
    // are handled by the DefaultAction::Copy — no explicit arm needed.
    for item in items {
        if let BoundProjectionItem::Nested { field, projection } = item {
            let label = emitter.alloc_label();
            arms.push(DispatchArm {
                match_: ArmMatch::Field(field.field_num),
                actions: vec![ArmAction::Frame(label)],
            });
            deferred.push(DeferredNested { projection });
        }
    }

    // Exclusions get Skip arms
    for &field_num in exclusions {
        arms.push(DispatchArm {
            match_: ArmMatch::Field(field_num),
            actions: vec![ArmAction::Skip],
        });
    }

    emitter.push(Instruction::Dispatch {
        default: DefaultAction::Copy,
        arms,
    });
    emitter.push(Instruction::Return);

    for nested in deferred {
        emitter.push(Instruction::Label);
        emit_projection(emitter, nested.projection)?;
    }

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// Predicate lowering
// ═══════════════════════════════════════════════════════════════════════

/// Info about a field that needs to be decoded into a register.
struct FieldDecodeInfo {
    path: Vec<u32>,
    reg: u8,
    encoding: Encoding,
}

/// Collect all unique `(field_path, encoding)` pairs from a predicate and
/// allocate a register for each.
fn collect_predicate_fields(
    pred: &BoundPredicate,
    emitter: &mut Emitter,
    map: &mut HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    match &pred.kind {
        BoundPredicateKind::And(l, r) | BoundPredicateKind::Or(l, r) => {
            collect_predicate_fields(l, emitter, map)?;
            collect_predicate_fields(r, emitter, map)?;
        }
        BoundPredicateKind::Not(inner) => {
            collect_predicate_fields(inner, emitter, map)?;
        }
        BoundPredicateKind::Comparison { field, .. }
        | BoundPredicateKind::StringPredicate { field, .. }
        | BoundPredicateKind::Presence(field)
        | BoundPredicateKind::InSet { field, .. } => {
            insert_field_decode(field, emitter, map)?;
        }
    }
    Ok(())
}

fn insert_field_decode(
    field: &BoundFieldPath,
    emitter: &mut Emitter,
    map: &mut HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    let encoding = field.encoding.unwrap_or(Encoding::Varint);
    if let Some(existing) = map.get(&field.segments) {
        if existing.encoding != encoding {
            let path_str = field
                .segments
                .iter()
                .map(|s| format!("#{s}"))
                .collect::<Vec<_>>()
                .join(".");
            return Err(CompileError::ConflictingEncoding { field: path_str });
        }
    } else {
        let reg = emitter.alloc_register()?;
        map.insert(
            field.segments.clone(),
            FieldDecodeInfo {
                path: field.segments.clone(),
                reg,
                encoding,
            },
        );
    }
    Ok(())
}

/// Build DISPATCH arms for predicate-only mode (decode fields into registers).
fn build_predicate_dispatch(
    fields: &HashMap<Vec<u32>, FieldDecodeInfo>,
    emitter: &mut Emitter,
) -> (Vec<DispatchArm>, Vec<DeferredPredicateNested>) {
    let mut top_arms: Vec<DispatchArm> = Vec::new();
    let mut deferred: Vec<DeferredPredicateNested> = Vec::new();

    // Group fields by their first segment
    let mut by_first_seg: HashMap<u32, Vec<&FieldDecodeInfo>> = HashMap::new();
    for info in fields.values() {
        by_first_seg.entry(info.path[0]).or_default().push(info);
    }

    for (&first_seg, infos) in &by_first_seg {
        // Check if all fields in this group are top-level (single segment)
        let all_top_level = infos.iter().all(|info| info.path.len() == 1);

        if all_top_level {
            // Single-segment path: direct Decode arm
            assert_eq!(infos.len(), 1);
            let info = infos[0];
            top_arms.push(DispatchArm {
                match_: ArmMatch::Field(first_seg),
                actions: vec![ArmAction::Decode {
                    reg: info.reg,
                    encoding: info.encoding,
                }],
            });
        } else {
            // Multi-segment path: need a Frame for nested dispatch
            let label = emitter.alloc_label();
            top_arms.push(DispatchArm {
                match_: ArmMatch::Field(first_seg),
                actions: vec![ArmAction::Frame(label)],
            });
            // Collect nested field infos (strip first segment)
            let nested_infos: Vec<_> = infos
                .iter()
                .map(|info| NestedFieldInfo {
                    remaining_path: info.path[1..].to_vec(),
                    reg: info.reg,
                    encoding: info.encoding,
                })
                .collect();
            deferred.push(DeferredPredicateNested {
                fields: nested_infos,
            });
        }
    }

    // Sort arms by field number for deterministic output
    top_arms.sort_by_key(|arm| match arm.match_ {
        ArmMatch::Field(n) | ArmMatch::FieldAndWireType(n, _) => n,
    });

    (top_arms, deferred)
}

struct NestedFieldInfo {
    remaining_path: Vec<u32>,
    reg: u8,
    encoding: Encoding,
}

struct DeferredPredicateNested {
    fields: Vec<NestedFieldInfo>,
}

fn emit_nested_predicate_dispatches(
    emitter: &mut Emitter,
    deferred: Vec<DeferredPredicateNested>,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
    // Explicit group key from the parent, or None to auto-detect from field_map.
    parent_group: Option<u32>,
) -> Result<(), CompileError> {
    for nested in deferred {
        emitter.push(Instruction::Label);
        let mut arms = Vec::new();
        let mut sub_deferred: Vec<DeferredPredicateNested> = Vec::new();

        // Group by first segment of remaining path
        let mut by_first: HashMap<u32, Vec<&NestedFieldInfo>> = HashMap::new();
        for info in &nested.fields {
            by_first
                .entry(info.remaining_path[0])
                .or_default()
                .push(info);
        }

        for (&seg, infos) in &by_first {
            let all_leaf = infos.iter().all(|info| info.remaining_path.len() == 1);
            if all_leaf {
                let actions: Vec<ArmAction> = infos
                    .iter()
                    .map(|info| ArmAction::Decode {
                        reg: info.reg,
                        encoding: info.encoding,
                    })
                    .collect();
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(seg),
                    actions,
                });
            } else {
                let label = emitter.alloc_label();
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(seg),
                    actions: vec![ArmAction::Frame(label)],
                });
                let sub_infos: Vec<_> = infos
                    .iter()
                    .map(|info| NestedFieldInfo {
                        remaining_path: info.remaining_path[1..].to_vec(),
                        reg: info.reg,
                        encoding: info.encoding,
                    })
                    .collect();
                sub_deferred.push(DeferredPredicateNested { fields: sub_infos });
            }
        }

        arms.sort_by_key(|arm| match arm.match_ {
            ArmMatch::Field(n) | ArmMatch::FieldAndWireType(n, _) => n,
        });

        emitter.push(Instruction::Dispatch {
            default: DefaultAction::Skip,
            arms,
        });

        // Emit per-element predicate for this Frame group + Or for ANY accumulation.
        let group = parent_group.unwrap_or_else(|| {
            field_map
                .values()
                .find(|info| info.path.len() > 1 && nested.fields.iter().any(|f| f.reg == info.reg))
                .map(|info| info.path[0])
                .expect("Frame must correspond to a field_map entry")
        });
        let did_emit = emit_frame_predicate(emitter, pred, field_map, group)?;
        if did_emit {
            emitter.push(Instruction::Or);
        }

        emitter.push(Instruction::Return);

        if !sub_deferred.is_empty() {
            emit_nested_predicate_dispatches(emitter, sub_deferred, pred, field_map, Some(group))?;
        }
    }
    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// Predicate tree classification and split emission
// ═══════════════════════════════════════════════════════════════════════
//
// Predicate trees may reference fields at different nesting depths:
// - Local leaves: single-segment paths (e.g., `shipped == true`)
// - Nested leaves: multi-segment paths (e.g., `items.price > 3000`)
//
// Nested leaves are decoded inside Frame sub-programs. For repeated fields,
// each Frame invocation evaluates the per-element predicate and Or-accumulates
// the result (ANY semantics).
//
// Classification:
// - `Local`: all descendants are local leaves
// - `Nested(group)`: all descendants share the same first path segment
// - `Mixed`: descendants span local + nested, or different nested groups
//
// A Nested sub-tree is emitted inside its Frame. The outer tree sees it as a
// single "already on stack" value. Not(Nested) is Mixed — the Not is outer,
// ensuring `!items.price > 3000` means "NOT ANY" not "ANY NOT".

/// The first path segment of a nested field (the Frame group key).
fn nested_group(pred: &BoundPredicate) -> Option<u32> {
    let segments = match &pred.kind {
        BoundPredicateKind::Comparison { field, .. }
        | BoundPredicateKind::Presence(field)
        | BoundPredicateKind::InSet { field, .. }
        | BoundPredicateKind::StringPredicate { field, .. } => &field.segments,
        _ => return None,
    };
    if segments.len() > 1 {
        Some(segments[0])
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq)]
enum PredClass {
    Local,
    Nested(u32),
    Mixed,
}

/// Classify a predicate node bottom-up.
fn classify_predicate(pred: &BoundPredicate) -> PredClass {
    match &pred.kind {
        BoundPredicateKind::And(l, r) | BoundPredicateKind::Or(l, r) => {
            let lc = classify_predicate(l);
            let rc = classify_predicate(r);
            match (&lc, &rc) {
                (PredClass::Nested(g1), PredClass::Nested(g2)) if g1 == g2 => {
                    PredClass::Nested(*g1)
                }
                (PredClass::Local, PredClass::Local) => PredClass::Local,
                _ => PredClass::Mixed,
            }
        }
        // Not is always outer — ensures NOT(ANY(...)) semantics, not ANY(NOT(...))
        BoundPredicateKind::Not(inner) => {
            let ic = classify_predicate(inner);
            match ic {
                PredClass::Local => PredClass::Local,
                _ => PredClass::Mixed,
            }
        }
        _ => {
            if let Some(g) = nested_group(pred) {
                PredClass::Nested(g)
            } else {
                PredClass::Local
            }
        }
    }
}

/// Emit the "outer" predicate tree, replacing Nested sub-trees with no-ops
/// (their values are already on the bool stack from Frame evaluation).
fn emit_outer_predicate(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    let class = classify_predicate(pred);
    match class {
        PredClass::Nested(_) => {
            // Entire sub-tree was evaluated inside a Frame — value on stack.
        }
        PredClass::Local => {
            // Pure local sub-tree — emit all nodes.
            emit_predicate_full(emitter, pred, field_map)?;
        }
        PredClass::Mixed => match &pred.kind {
            BoundPredicateKind::And(l, r) => {
                emit_outer_predicate(emitter, l, field_map)?;
                emit_outer_predicate(emitter, r, field_map)?;
                emitter.push(Instruction::And);
            }
            BoundPredicateKind::Or(l, r) => {
                emit_outer_predicate(emitter, l, field_map)?;
                emit_outer_predicate(emitter, r, field_map)?;
                emitter.push(Instruction::Or);
            }
            BoundPredicateKind::Not(inner) => {
                emit_outer_predicate(emitter, inner, field_map)?;
                emitter.push(Instruction::Not);
            }
            _ => {
                // Mixed leaf — must be local (nested leaves are PredClass::Nested)
                emit_predicate_leaf(emitter, pred, field_map)?;
            }
        },
    }
    Ok(())
}

/// Emit the per-element predicate for a Frame group. Only emits nodes
/// classified as Nested(group). Returns true if anything was emitted.
fn emit_frame_predicate(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
    group: u32,
) -> Result<bool, CompileError> {
    let class = classify_predicate(pred);
    match class {
        PredClass::Nested(g) if g == group => {
            // This entire sub-tree belongs to our group — emit it fully.
            emit_predicate_full(emitter, pred, field_map)?;
            Ok(true)
        }
        PredClass::Nested(_) | PredClass::Local => Ok(false),
        PredClass::Mixed => match &pred.kind {
            BoundPredicateKind::And(l, r) => {
                let left = emit_frame_predicate(emitter, l, field_map, group)?;
                let right = emit_frame_predicate(emitter, r, field_map, group)?;
                if left && right {
                    emitter.push(Instruction::And);
                }
                Ok(left || right)
            }
            BoundPredicateKind::Or(l, r) => {
                let left = emit_frame_predicate(emitter, l, field_map, group)?;
                let right = emit_frame_predicate(emitter, r, field_map, group)?;
                if left && right {
                    emitter.push(Instruction::Or);
                }
                Ok(left || right)
            }
            BoundPredicateKind::Not(inner) => {
                // Not is outer — descend into child to find nested leaves.
                // The child should be Nested (Not promotes Nested to Mixed),
                // never itself Mixed (that would require Not(And(Nested, Local))
                // which the grammar can produce but would need And emission here).
                debug_assert!(
                    classify_predicate(inner) != PredClass::Mixed,
                    "Not(Mixed) in Frame context is not supported"
                );
                emit_frame_predicate(emitter, inner, field_map, group)
            }
            _ => Ok(false),
        },
    }
}

/// Emit a predicate sub-tree unconditionally (all nodes, no skipping).
fn emit_predicate_full(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    match &pred.kind {
        BoundPredicateKind::And(l, r) => {
            emit_predicate_full(emitter, l, field_map)?;
            emit_predicate_full(emitter, r, field_map)?;
            emitter.push(Instruction::And);
        }
        BoundPredicateKind::Or(l, r) => {
            emit_predicate_full(emitter, l, field_map)?;
            emit_predicate_full(emitter, r, field_map)?;
            emitter.push(Instruction::Or);
        }
        BoundPredicateKind::Not(inner) => {
            emit_predicate_full(emitter, inner, field_map)?;
            emitter.push(Instruction::Not);
        }
        _ => emit_predicate_leaf(emitter, pred, field_map)?,
    }
    Ok(())
}

/// Emit a single predicate leaf (comparison, presence, in-set, string op).
fn emit_predicate_leaf(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    match &pred.kind {
        BoundPredicateKind::Comparison { field, op, value } => {
            let reg = field_map[&field.segments].reg;
            emit_comparison(emitter, reg, *op, value)?;
        }
        BoundPredicateKind::Presence(field) => {
            let reg = field_map[&field.segments].reg;
            emitter.push(Instruction::IsSet { reg });
        }
        BoundPredicateKind::InSet { field, values } => {
            let reg = field_map[&field.segments].reg;
            let int_values: Vec<i64> = values
                .iter()
                .map(|v| match v {
                    Literal::Int(n, _) => *n,
                    Literal::Bool(b, _) => i64::from(*b),
                    Literal::String(..) => 0, // should not happen after type check
                })
                .collect();
            emitter.push(Instruction::InSet {
                reg,
                values: int_values,
            });
        }
        BoundPredicateKind::StringPredicate { field, op, value } => {
            let reg = field_map[&field.segments].reg;
            let bytes = match value {
                Literal::String(s, _) => s.as_bytes().to_vec(),
                _ => Vec::new(), // should not happen after type check
            };
            match op {
                StringOp::StartsWith => emitter.push(Instruction::BytesStarts { reg, bytes }),
                StringOp::EndsWith => emitter.push(Instruction::BytesEnds { reg, bytes }),
                StringOp::Contains => emitter.push(Instruction::BytesContains { reg, bytes }),
                StringOp::Matches => {
                    #[cfg(feature = "regex")]
                    emitter.push(Instruction::BytesMatches {
                        reg,
                        pattern: bytes,
                    });
                    #[cfg(not(feature = "regex"))]
                    return Err(CompileError::RegexNotEnabled);
                }
            }
        }
        _ => {} // And/Or/Not handled by callers
    }
    Ok(())
}

fn emit_comparison(
    emitter: &mut Emitter,
    reg: u8,
    op: CompareOp,
    value: &Literal,
) -> Result<(), CompileError> {
    match value {
        Literal::Int(n, _) => {
            let imm = *n;
            match op {
                CompareOp::Eq => emitter.push(Instruction::CmpEq { reg, imm }),
                CompareOp::Neq => emitter.push(Instruction::CmpNeq { reg, imm }),
                CompareOp::Lt => emitter.push(Instruction::CmpLt { reg, imm }),
                CompareOp::Lte => emitter.push(Instruction::CmpLte { reg, imm }),
                CompareOp::Gt => emitter.push(Instruction::CmpGt { reg, imm }),
                CompareOp::Gte => emitter.push(Instruction::CmpGte { reg, imm }),
            }
        }
        Literal::Bool(b, _) => {
            let imm = i64::from(*b);
            match op {
                CompareOp::Eq => emitter.push(Instruction::CmpEq { reg, imm }),
                CompareOp::Neq => emitter.push(Instruction::CmpNeq { reg, imm }),
                _ => {
                    return Err(CompileError::UnsupportedComparison {
                        op: compare_op_name(op),
                        literal_type: "bool",
                    })
                }
            }
        }
        Literal::String(s, _) => {
            let bytes = s.as_bytes().to_vec();
            match op {
                CompareOp::Eq => emitter.push(Instruction::CmpLenEq { reg, bytes }),
                CompareOp::Neq => {
                    emitter.push(Instruction::CmpLenEq { reg, bytes });
                    emitter.push(Instruction::Not);
                }
                _ => {
                    return Err(CompileError::UnsupportedComparison {
                        op: compare_op_name(op),
                        literal_type: "string",
                    })
                }
            }
        }
    }
    Ok(())
}

fn compare_op_name(op: CompareOp) -> &'static str {
    match op {
        CompareOp::Eq => "==",
        CompareOp::Neq => "!=",
        CompareOp::Lt => "<",
        CompareOp::Lte => "<=",
        CompareOp::Gt => ">",
        CompareOp::Gte => ">=",
    }
}

/// Emit a predicate-only program: DISPATCH(decode fields), predicate logic, RETURN.
fn emit_predicate_program(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
) -> Result<(), CompileError> {
    let mut field_map = HashMap::new();
    collect_predicate_fields(pred, emitter, &mut field_map)?;

    let has_nested = field_map.values().any(|info| info.path.len() > 1);

    // Seed false on the bool stack for each nested Frame group.
    if has_nested {
        let seed_reg = emitter.alloc_register()?;
        emitter.push(Instruction::IsSet { reg: seed_reg });
    }

    let (arms, deferred) = build_predicate_dispatch(&field_map, emitter);

    emitter.push(Instruction::Dispatch {
        default: DefaultAction::Skip,
        arms,
    });

    // Emit the outer predicate tree (Nested sub-trees are no-ops — on stack from Frames).
    emit_outer_predicate(emitter, pred, &field_map)?;
    emitter.push(Instruction::Return);

    emit_nested_predicate_dispatches(emitter, deferred, pred, &field_map, None)?;

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════
// Combined form (predicate + projection)
// ═══════════════════════════════════════════════════════════════════════

fn emit_combined(
    emitter: &mut Emitter,
    pred: &BoundPredicate,
    proj: &BoundProjection,
) -> Result<(), CompileError> {
    // Collect predicate fields
    let mut field_map = HashMap::new();
    collect_predicate_fields(pred, emitter, &mut field_map)?;

    let is_copy = matches!(&proj.kind, BoundProjectionKind::Copy { .. });

    // Build the merged DISPATCH: combine projection arms with predicate decode arms
    let (merged_arms, proj_deferred, pred_deferred, merged_pred_fields) =
        build_combined_dispatch(proj, &field_map, emitter, is_copy);

    let has_nested = field_map.values().any(|info| info.path.len() > 1);

    let default = match &proj.kind {
        BoundProjectionKind::Strict { .. } => DefaultAction::Skip,
        BoundProjectionKind::Copy { .. } => DefaultAction::Copy,
    };

    // Seed false before DISPATCH for repeated field ANY accumulation.
    if has_nested {
        let seed_reg = emitter.alloc_register()?;
        emitter.push(Instruction::IsSet { reg: seed_reg });
    }

    emitter.push(Instruction::Dispatch {
        default,
        arms: merged_arms,
    });

    emit_outer_predicate(emitter, pred, &field_map)?;
    emitter.push(Instruction::Return);

    // Emit deferred nested sub-programs for projection, injecting any
    // predicate decode arms that share the same parent field.
    for nested in proj_deferred {
        emitter.push(Instruction::Label);
        let pred_fields = merged_pred_fields.get(&nested.label);
        emit_combined_nested_projection(emitter, nested.projection, pred_fields, pred, &field_map)?;
    }

    // Emit deferred nested dispatches for predicate
    emit_nested_predicate_dispatches(emitter, pred_deferred, pred, &field_map, None)?;

    Ok(())
}

struct DeferredCombinedNested<'a> {
    label: u32,
    projection: &'a BoundProjection,
}

// Fourth element: predicate fields to inject into projection Frames (keyed by label).
type CombinedDispatchResult<'a> = (
    Vec<DispatchArm>,
    Vec<DeferredCombinedNested<'a>>,
    Vec<DeferredPredicateNested>,
    HashMap<u32, Vec<NestedFieldInfo>>,
);

fn build_combined_dispatch<'a>(
    proj: &'a BoundProjection,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
    emitter: &mut Emitter,
    is_copy: bool,
) -> CombinedDispatchResult<'a> {
    let mut arms_map: HashMap<u32, Vec<ArmAction>> = HashMap::new();
    let mut proj_deferred: Vec<DeferredCombinedNested<'a>> = Vec::new();
    let mut pred_deferred: Vec<DeferredPredicateNested> = Vec::new();
    // Predicate fields to inject into existing projection Frames (keyed by label).
    let mut merged_pred_fields: HashMap<u32, Vec<NestedFieldInfo>> = HashMap::new();

    // Add projection arms from items
    let items = match &proj.kind {
        BoundProjectionKind::Strict { items } | BoundProjectionKind::Copy { items, .. } => items,
    };
    for item in items {
        match item {
            BoundProjectionItem::Field(f) | BoundProjectionItem::DeepSearch(f) => {
                arms_map
                    .entry(f.field_num)
                    .or_default()
                    .push(ArmAction::Copy);
            }
            BoundProjectionItem::Nested { field, projection } => {
                let label = emitter.alloc_label();
                arms_map
                    .entry(field.field_num)
                    .or_default()
                    .push(ArmAction::Frame(label));
                proj_deferred.push(DeferredCombinedNested { label, projection });
            }
        }
    }

    // Add exclusion arms (Copy mode only)
    if let BoundProjectionKind::Copy { exclusions, .. } = &proj.kind {
        for &field_num in exclusions {
            arms_map.entry(field_num).or_default().push(ArmAction::Skip);
        }
    }

    // Add predicate decode arms
    for info in field_map.values() {
        if info.path.len() == 1 {
            let field_num = info.path[0];
            let is_new = !arms_map.contains_key(&field_num);
            let actions = arms_map.entry(field_num).or_default();
            // Insert Decode before any Copy (Decode first, then Copy)
            actions.insert(
                0,
                ArmAction::Decode {
                    reg: info.reg,
                    encoding: info.encoding,
                },
            );
            // If this field is predicate-only and in copy mode,
            // the default Copy action no longer applies (explicit arm overrides),
            // so we must add Copy to preserve the field in the output.
            if is_new && is_copy {
                actions.push(ArmAction::Copy);
            }
        } else {
            // Multi-segment: need Frame
            let field_num = info.path[0];
            let is_new = !arms_map.contains_key(&field_num);
            let actions = arms_map.entry(field_num).or_default();
            let nested_field = NestedFieldInfo {
                remaining_path: info.path[1..].to_vec(),
                reg: info.reg,
                encoding: info.encoding,
            };

            // Check if a Frame already exists (from projection)
            let existing_frame_label = actions.iter().find_map(|a| match a {
                ArmAction::Frame(label) => Some(*label),
                _ => None,
            });

            if let Some(label) = existing_frame_label {
                // Merge into the projection Frame's sub-program — these will be
                // injected as Decode arms when emitting the nested projection.
                merged_pred_fields
                    .entry(label)
                    .or_default()
                    .push(nested_field);
            } else {
                let label = emitter.alloc_label();
                // If predicate-only and in copy mode, add Copy before Frame
                // so the nested message is preserved in the output.
                if is_new && is_copy {
                    actions.push(ArmAction::Copy);
                }
                actions.push(ArmAction::Frame(label));
                pred_deferred.push(DeferredPredicateNested {
                    fields: vec![nested_field],
                });
            }
        }
    }

    // Build sorted arm list
    let mut arms: Vec<DispatchArm> = arms_map
        .into_iter()
        .map(|(field_num, actions)| DispatchArm {
            match_: ArmMatch::Field(field_num),
            actions,
        })
        .collect();
    arms.sort_by_key(|arm| match arm.match_ {
        ArmMatch::Field(n) | ArmMatch::FieldAndWireType(n, _) => n,
    });

    (arms, proj_deferred, pred_deferred, merged_pred_fields)
}

/// Emit a nested projection for combined mode, injecting predicate decode
/// arms into the DISPATCH for any predicate fields that share this parent.
fn emit_combined_nested_projection(
    emitter: &mut Emitter,
    proj: &BoundProjection,
    pred_fields: Option<&Vec<NestedFieldInfo>>,
    pred: &BoundPredicate,
    field_map: &HashMap<Vec<u32>, FieldDecodeInfo>,
) -> Result<(), CompileError> {
    if pred_fields.is_none() || pred_fields.is_some_and(Vec::is_empty) {
        // No predicate fields to merge — use standard projection emission.
        return emit_projection(emitter, proj);
    }
    let pred_fields = pred_fields.unwrap();

    // Build the projection's DISPATCH arms, then inject Decode arms for
    // predicate fields before emitting.
    let (items, is_copy) = match &proj.kind {
        BoundProjectionKind::Strict { items } => (items.as_slice(), false),
        BoundProjectionKind::Copy { items, .. } => (items.as_slice(), true),
    };

    let mut arms = Vec::new();
    let mut deferred: Vec<DeferredNested<'_>> = Vec::new();

    for item in items {
        match item {
            BoundProjectionItem::Field(f) | BoundProjectionItem::DeepSearch(f) => {
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(f.field_num),
                    actions: vec![ArmAction::Copy],
                });
            }
            BoundProjectionItem::Nested { field, projection } => {
                let label = emitter.alloc_label();
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(field.field_num),
                    actions: vec![ArmAction::Frame(label)],
                });
                deferred.push(DeferredNested { projection });
            }
        }
    }

    // Add exclusion arms (Copy mode only)
    if let BoundProjectionKind::Copy { exclusions, .. } = &proj.kind {
        for &field_num in exclusions {
            arms.push(DispatchArm {
                match_: ArmMatch::Field(field_num),
                actions: vec![ArmAction::Skip],
            });
        }
    }

    // Inject predicate decode arms (single-segment remaining paths only for now)
    for pf in pred_fields {
        if pf.remaining_path.len() == 1 {
            let field_num = pf.remaining_path[0];
            // Check if this field already has an arm
            if let Some(arm) = arms
                .iter_mut()
                .find(|a| a.match_ == ArmMatch::Field(field_num))
            {
                // Insert Decode before existing actions
                arm.actions.insert(
                    0,
                    ArmAction::Decode {
                        reg: pf.reg,
                        encoding: pf.encoding,
                    },
                );
            } else {
                arms.push(DispatchArm {
                    match_: ArmMatch::Field(field_num),
                    actions: vec![ArmAction::Decode {
                        reg: pf.reg,
                        encoding: pf.encoding,
                    }],
                });
            }
        }
        // Multi-segment remaining paths within nested projections are
        // not supported in v1 combined mode.
    }

    let default = if is_copy {
        DefaultAction::Copy
    } else {
        DefaultAction::Skip
    };

    arms.sort_by_key(|arm| match arm.match_ {
        ArmMatch::Field(n) | ArmMatch::FieldAndWireType(n, _) => n,
    });

    emitter.push(Instruction::Dispatch { default, arms });

    // Emit per-element predicate for this Frame group + Or for ANY accumulation.
    let group = pred_fields[0].remaining_path[0];
    // The group key in field_map is the original first segment, not remaining_path.
    // Find it from the pred_fields' register → field_map lookup.
    let frame_group = field_map
        .values()
        .find(|info| info.path.len() > 1 && pred_fields.iter().any(|pf| pf.reg == info.reg))
        .map_or(group, |info| info.path[0]);
    let did_emit = emit_frame_predicate(emitter, pred, field_map, frame_group)?;
    if did_emit {
        emitter.push(Instruction::Or);
    }

    emitter.push(Instruction::Return);

    for nested in deferred {
        emitter.push(Instruction::Label);
        emit_projection(emitter, nested.projection)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::compile::{compile, CompileOptions};
    use wql_ir::{ArmAction, ArmMatch, DefaultAction, Encoding, Instruction};

    fn compile_and_decode(source: &str) -> Vec<Instruction> {
        let bytecode = compile(source, &CompileOptions::default()).unwrap();
        let (_header, instructions) = wql_ir::decode(&bytecode).unwrap();
        instructions
    }

    // ─── Projection tests ───

    #[test]
    fn emit_flat_strict() {
        let instrs = compile_and_decode("{ #1, #2 }");
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                assert_eq!(arms.len(), 2);
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
                assert_eq!(arms[0].actions, vec![ArmAction::Copy]);
                assert_eq!(arms[1].match_, ArmMatch::Field(2));
                assert_eq!(arms[1].actions, vec![ArmAction::Copy]);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
    }

    #[test]
    fn emit_flat_copy() {
        let instrs = compile_and_decode("{ #1, .. }");
        // Copy mode: Field items are redundant (default is Copy), no explicit arms
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Copy);
                assert!(arms.is_empty());
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
    }

    #[test]
    fn emit_empty() {
        let instrs = compile_and_decode("{ }");
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                assert!(arms.is_empty());
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
    }

    #[test]
    fn emit_identity() {
        let instrs = compile_and_decode("{ .. }");
        // Copy mode (no items): Dispatch(Copy), Return
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Copy);
                assert!(arms.is_empty());
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
    }

    #[test]
    fn emit_nested() {
        let instrs = compile_and_decode("{ #1, #3 { #1 } }");
        assert_eq!(instrs.len(), 5);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                assert_eq!(arms.len(), 2);
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
                assert_eq!(arms[0].actions, vec![ArmAction::Copy]);
                assert_eq!(arms[1].match_, ArmMatch::Field(3));
                assert!(matches!(arms[1].actions[0], ArmAction::Frame(_)));
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
        assert_eq!(instrs[2], Instruction::Label);
        match &instrs[3] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
            }
            other => panic!("expected nested Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[4], Instruction::Return);
    }

    #[test]
    fn emit_nested_copy() {
        let instrs = compile_and_decode("{ #1, #3 { #1, .. }, .. }");
        // Shallow copy mode (items present): Dispatch(Copy)
        match &instrs[0] {
            Instruction::Dispatch { default, .. } => {
                assert_eq!(*default, DefaultAction::Copy);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        // Find the nested dispatch (after the outer Return)
        let nested_dispatches: Vec<_> = instrs
            .iter()
            .skip(2)
            .filter(|i| matches!(i, Instruction::Dispatch { .. }))
            .collect();
        assert!(!nested_dispatches.is_empty(), "expected nested Dispatch");
        match nested_dispatches[0] {
            Instruction::Dispatch { default, .. } => {
                // Inner projection also has items, so shallow Copy
                assert_eq!(*default, DefaultAction::Copy);
            }
            other => panic!("expected nested Dispatch with Copy default, got {other:?}"),
        }
    }

    #[test]
    fn emit_deep_copy() {
        let instrs = compile_and_decode("{ .. }");
        // Copy mode: Dispatch(Copy), Return
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Copy);
                assert!(arms.is_empty());
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::Return);
    }

    #[test]
    fn emit_deep_exclusion() {
        let instrs = compile_and_decode("{ -#7, .. }");
        // Copy mode with exclusion: Dispatch(Copy), Return
        assert_eq!(instrs.len(), 2);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Copy);
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].match_, ArmMatch::Field(7));
                assert_eq!(arms[0].actions, vec![ArmAction::Skip]);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn emit_deep_search() {
        let instrs = compile_and_decode("{ ..#1 }");
        assert_eq!(instrs.len(), 3);
        assert_eq!(instrs[0], Instruction::Label);
        match &instrs[1] {
            Instruction::Dispatch { default, arms } => {
                assert!(matches!(default, DefaultAction::Recurse(_)));
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
                assert_eq!(arms[0].actions, vec![ArmAction::Copy]);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn emit_nested_two_levels() {
        let instrs = compile_and_decode("{ #1 { #2 { #3 } } }");
        assert_eq!(instrs.len(), 8);
        assert!(matches!(&instrs[0], Instruction::Dispatch { .. }));
        assert_eq!(instrs[1], Instruction::Return);
        assert_eq!(instrs[2], Instruction::Label);
        assert!(matches!(&instrs[3], Instruction::Dispatch { .. }));
        assert_eq!(instrs[4], Instruction::Return);
        assert_eq!(instrs[5], Instruction::Label);
        match &instrs[6] {
            Instruction::Dispatch { arms, .. } => {
                assert_eq!(arms[0].match_, ArmMatch::Field(3));
                assert_eq!(arms[0].actions, vec![ArmAction::Copy]);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[7], Instruction::Return);
    }

    // ─── Predicate tests ───

    #[test]
    fn emit_pred_cmp_eq() {
        let instrs = compile_and_decode("#1 == 42");
        // Dispatch(Skip, [1→Decode(R0,Varint)]), CmpEq(R0,42), Return
        assert!(instrs.len() >= 3);
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
                assert_eq!(
                    arms[0].actions,
                    vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint
                    }]
                );
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(instrs[1], Instruction::CmpEq { reg: 0, imm: 42 });
        assert_eq!(instrs[2], Instruction::Return);
    }

    #[test]
    fn emit_pred_cmp_gt() {
        let instrs = compile_and_decode("#1 > 18");
        assert_eq!(instrs[1], Instruction::CmpGt { reg: 0, imm: 18 });
    }

    #[test]
    fn emit_pred_cmp_neq() {
        let instrs = compile_and_decode("#1 != 0");
        assert_eq!(instrs[1], Instruction::CmpNeq { reg: 0, imm: 0 });
    }

    #[test]
    fn emit_pred_string_eq() {
        let instrs = compile_and_decode(r#"#1 == "hello""#);
        match &instrs[0] {
            Instruction::Dispatch { arms, .. } => {
                assert_eq!(
                    arms[0].actions,
                    vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Len
                    }]
                );
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert_eq!(
            instrs[1],
            Instruction::CmpLenEq {
                reg: 0,
                bytes: b"hello".to_vec()
            }
        );
    }

    #[test]
    fn emit_pred_bool() {
        let instrs = compile_and_decode("#1 == true");
        assert_eq!(instrs[1], Instruction::CmpEq { reg: 0, imm: 1 });
    }

    #[test]
    fn emit_pred_and() {
        let instrs = compile_and_decode("#1 > 0 && #2 > 0");
        // Dispatch(Skip, [1→Decode(R0), 2→Decode(R1)]), CmpGt(R0,0), CmpGt(R1,0), And, Return
        let dispatch = &instrs[0];
        match dispatch {
            Instruction::Dispatch { arms, .. } => {
                assert_eq!(arms.len(), 2);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        // Find CmpGt instructions
        let cmp_gts: Vec<_> = instrs
            .iter()
            .filter(|i| matches!(i, Instruction::CmpGt { .. }))
            .collect();
        assert_eq!(cmp_gts.len(), 2);
        assert!(instrs.contains(&Instruction::And));
    }

    #[test]
    fn emit_pred_or() {
        let instrs = compile_and_decode("#1 > 0 || #2 > 0");
        assert!(instrs.contains(&Instruction::Or));
    }

    #[test]
    fn emit_pred_not() {
        let instrs = compile_and_decode("!#1 == 0");
        assert!(instrs.contains(&Instruction::Not));
        assert!(instrs.contains(&Instruction::CmpEq { reg: 0, imm: 0 }));
    }

    #[test]
    fn emit_pred_nested() {
        let instrs = compile_and_decode("#3.#1 > 0");
        // First instruction is IsSet seed for repeated field ANY semantics
        assert!(matches!(instrs[0], Instruction::IsSet { .. }));
        // Then DISPATCH with Frame for field 3
        match &instrs[1] {
            Instruction::Dispatch { arms, .. } => {
                assert_eq!(arms.len(), 1);
                assert_eq!(arms[0].match_, ArmMatch::Field(3));
                assert!(matches!(arms[0].actions[0], ArmAction::Frame(_)));
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        // Find nested dispatch
        let nested = instrs
            .iter()
            .skip(2)
            .find(|i| matches!(i, Instruction::Dispatch { .. }));
        match nested {
            Some(Instruction::Dispatch { arms, .. }) => {
                assert_eq!(arms[0].match_, ArmMatch::Field(1));
                assert!(matches!(
                    arms[0].actions[0],
                    ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint
                    }
                ));
            }
            other => panic!("expected nested Dispatch, got {other:?}"),
        }
        // CmpGt is now inside the Frame sub-program (not after outer DISPATCH)
        assert!(instrs.contains(&Instruction::CmpGt { reg: 0, imm: 0 }));
        // Or accumulates across repeated elements
        assert!(instrs.contains(&Instruction::Or));
    }

    #[test]
    fn emit_pred_exists() {
        let instrs = compile_and_decode("exists(#1)");
        // Should have Decode(R0, Varint) and IsSet(R0)
        match &instrs[0] {
            Instruction::Dispatch { arms, .. } => {
                assert_eq!(
                    arms[0].actions,
                    vec![ArmAction::Decode {
                        reg: 0,
                        encoding: Encoding::Varint
                    }]
                );
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert!(instrs.contains(&Instruction::IsSet { reg: 0 }));
    }

    #[test]
    fn emit_pred_in_set() {
        let instrs = compile_and_decode("#1 in [1, 2, 3]");
        assert!(instrs.contains(&Instruction::InSet {
            reg: 0,
            values: vec![1, 2, 3]
        }));
    }

    #[test]
    fn emit_pred_starts_with() {
        let instrs = compile_and_decode(r#"#1 starts_with "pre""#);
        assert!(instrs.contains(&Instruction::BytesStarts {
            reg: 0,
            bytes: b"pre".to_vec()
        }));
    }

    // ─── Combined form tests ───

    #[test]
    fn emit_combined_simple() {
        let instrs = compile_and_decode("WHERE #2 > 18 SELECT { #1 }");
        // Dispatch(Skip, [1→Copy, 2→Decode(R0)]), CmpGt(R0,18), Return
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Skip);
                // Arms should include field 1 (Copy) and field 2 (Decode)
                let arm1 = arms.iter().find(|a| a.match_ == ArmMatch::Field(1));
                let arm2 = arms.iter().find(|a| a.match_ == ArmMatch::Field(2));
                assert!(arm1.is_some(), "missing arm for field 1");
                assert!(arm2.is_some(), "missing arm for field 2");
                assert!(arm1.unwrap().actions.contains(&ArmAction::Copy));
                assert!(arm2.unwrap().actions.iter().any(|a| matches!(
                    a,
                    ArmAction::Decode {
                        encoding: Encoding::Varint,
                        ..
                    }
                )));
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        assert!(instrs.contains(&Instruction::CmpGt { reg: 0, imm: 18 }));
    }

    #[test]
    fn emit_combined_shared_field() {
        let instrs = compile_and_decode("WHERE #1 > 0 SELECT { #1 }");
        // Field 1 should have both Decode and Copy in its arm
        match &instrs[0] {
            Instruction::Dispatch { arms, .. } => {
                let arm = arms.iter().find(|a| a.match_ == ArmMatch::Field(1));
                assert!(arm.is_some());
                let actions = &arm.unwrap().actions;
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, ArmAction::Decode { .. })),
                    "missing Decode"
                );
                assert!(actions.contains(&ArmAction::Copy), "missing Copy");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    #[test]
    fn emit_combined_copy() {
        let instrs = compile_and_decode("WHERE #1 > 0 SELECT { #1, .. }");
        // Combined + shallow copy mode (items present): Dispatch(Copy)
        match &instrs[0] {
            Instruction::Dispatch { default, .. } => {
                assert_eq!(*default, DefaultAction::Copy);
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }

    // ─── End-to-end tests (compile → execute) ───

    /// Build a simple protobuf message by hand.
    /// Format: (field_number, wire_type, value) tuples encoded as protobuf.
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

    fn encode_tag(field: u32, wire_type: u8) -> Vec<u8> {
        encode_varint(u64::from(field) << 3 | u64::from(wire_type))
    }

    fn build_proto_varint(field: u32, val: u64) -> Vec<u8> {
        let mut buf = encode_tag(field, 0); // wire_type 0 = varint
        buf.extend(encode_varint(val));
        buf
    }

    fn build_proto_len(field: u32, val: &[u8]) -> Vec<u8> {
        let mut buf = encode_tag(field, 2); // wire_type 2 = LEN
        buf.extend(encode_varint(val.len() as u64));
        buf.extend(val);
        buf
    }

    #[test]
    fn e2e_project_flat() {
        let bytecode = compile("{ #1, #2 }", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        // Input: {1: varint(42), 2: LEN("hi"), 3: varint(99)}
        let mut input = Vec::new();
        input.extend(build_proto_varint(1, 42));
        input.extend(build_proto_len(2, b"hi"));
        input.extend(build_proto_varint(3, 99));

        let mut output = vec![0u8; 256];
        let len = wql_runtime::project(&program, &input, &mut output).unwrap();
        let output = &output[..len];

        // Output should have fields 1 and 2, but not 3
        assert!(output.len() < input.len());
        // Verify field 3 is not in output by checking no tag for field 3
        // Field 3 varint tag = (3 << 3) | 0 = 24
        assert!(!output.contains(&24));
    }

    #[test]
    fn e2e_project_nested() {
        let bytecode = compile("{ #1, #3 { #1 } }", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        // Build nested message: Address { city: "NYC"=1, country: "US"=2 }
        let mut inner = Vec::new();
        inner.extend(build_proto_len(1, b"NYC"));
        inner.extend(build_proto_len(2, b"US"));

        // Input: {1: LEN("Alice"), 3: LEN(inner)}
        let mut input = Vec::new();
        input.extend(build_proto_len(1, b"Alice"));
        input.extend(build_proto_len(3, &inner));

        let mut output = vec![0u8; 256];
        let len = wql_runtime::project(&program, &input, &mut output).unwrap();
        let output = &output[..len];

        // Output should contain field 1 (Alice) and field 3 with only city (NYC)
        assert!(!output.is_empty());
    }

    #[test]
    fn e2e_filter_true() {
        let bytecode = compile("#2 > 18", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let mut input = Vec::new();
        input.extend(build_proto_varint(2, 25));

        let result = wql_runtime::filter(&program, &input).unwrap();
        assert!(result);
    }

    #[test]
    fn e2e_filter_false() {
        let bytecode = compile("#2 > 18", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let mut input = Vec::new();
        input.extend(build_proto_varint(2, 10));

        let result = wql_runtime::filter(&program, &input).unwrap();
        assert!(!result);
    }

    #[test]
    fn e2e_combined() {
        let bytecode = compile("WHERE #2 > 18 SELECT { #1 }", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let mut input = Vec::new();
        input.extend(build_proto_len(1, b"Alice"));
        input.extend(build_proto_varint(2, 25));

        let mut output = vec![0u8; 256];
        let result = wql_runtime::project_and_filter(&program, &input, &mut output).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn e2e_deep_copy_exclusion() {
        let bytecode = compile("{ -#3, .. }", &CompileOptions::default()).unwrap();
        let program = wql_runtime::LoadedProgram::from_bytes(&bytecode).unwrap();

        let mut input = Vec::new();
        input.extend(build_proto_varint(1, 42));
        input.extend(build_proto_varint(2, 99));
        input.extend(build_proto_varint(3, 77));

        let mut output = vec![0u8; 256];
        let len = wql_runtime::project(&program, &input, &mut output).unwrap();
        let output = &output[..len];

        // Field 3 tag = (3 << 3) | 0 = 24, should not be in output
        assert!(!output.contains(&24));
    }

    // ─── Error path tests ───

    #[test]
    fn emit_error_bool_ordering() {
        let result = compile("#1 > true", &CompileOptions::default());
        assert!(matches!(
            result,
            Err(crate::error::CompileError::UnsupportedComparison {
                literal_type: "bool",
                ..
            })
        ));
    }

    #[test]
    fn emit_error_string_ordering() {
        let result = compile(r#"#1 < "abc""#, &CompileOptions::default());
        assert!(matches!(
            result,
            Err(crate::error::CompileError::UnsupportedComparison {
                literal_type: "string",
                ..
            })
        ));
    }

    #[test]
    fn emit_error_bool_eq_ok() {
        // Bool equality should work fine
        let instrs = compile_and_decode("#1 == true");
        assert!(instrs.contains(&Instruction::CmpEq { reg: 0, imm: 1 }));
    }

    #[test]
    fn emit_error_string_eq_ok() {
        // String equality should work fine
        let instrs = compile_and_decode(r#"#1 == "abc""#);
        assert!(instrs.contains(&Instruction::CmpLenEq {
            reg: 0,
            bytes: b"abc".to_vec()
        }));
    }

    #[test]
    fn emit_error_string_neq_ok() {
        // String != should emit CmpLenEq + Not
        let instrs = compile_and_decode(r#"#1 != "abc""#);
        assert!(instrs.contains(&Instruction::CmpLenEq {
            reg: 0,
            bytes: b"abc".to_vec()
        }));
        assert!(instrs.contains(&Instruction::Not));
    }

    #[test]
    fn emit_combined_shared_nested() {
        // Predicate and projection share a nested parent field
        let instrs = compile_and_decode("WHERE #3.#1 > 0 SELECT { #3 { #1 } }");
        // First: IsSet seed for repeated field accumulation
        assert!(matches!(instrs[0], Instruction::IsSet { .. }));
        // Then DISPATCH with Frame for field 3
        match &instrs[1] {
            Instruction::Dispatch { arms, .. } => {
                let arm3 = arms.iter().find(|a| a.match_ == ArmMatch::Field(3));
                assert!(arm3.is_some(), "missing arm for field 3");
                let frame_count = arm3
                    .unwrap()
                    .actions
                    .iter()
                    .filter(|a| matches!(a, ArmAction::Frame(_)))
                    .count();
                assert_eq!(frame_count, 1, "should have exactly one Frame for field 3");
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
        // Nested dispatch has Decode+Copy for field 1
        let nested = instrs
            .iter()
            .skip(2)
            .find(|i| matches!(i, Instruction::Dispatch { .. }));
        match nested {
            Some(Instruction::Dispatch { arms, .. }) => {
                let arm1 = arms.iter().find(|a| a.match_ == ArmMatch::Field(1));
                assert!(arm1.is_some(), "nested dispatch missing arm for field 1");
                let actions = &arm1.unwrap().actions;
                assert!(
                    actions
                        .iter()
                        .any(|a| matches!(a, ArmAction::Decode { .. })),
                    "nested arm for field 1 missing Decode"
                );
                assert!(
                    actions.contains(&ArmAction::Copy),
                    "nested arm for field 1 missing Copy"
                );
            }
            other => panic!("expected nested Dispatch, got {other:?}"),
        }
        // CmpGt + Or are inside the Frame sub-program
        assert!(instrs.contains(&Instruction::CmpGt { reg: 0, imm: 0 }));
        assert!(instrs.contains(&Instruction::Or));
    }

    #[test]
    fn emit_copy_with_predicate_and_exclusion() {
        // Copy + predicate + exclusion
        let instrs = compile_and_decode("WHERE #2 > 0 SELECT { -#3, .. }");
        // Structure: Dispatch(Copy, [2->Decode+Copy, 3->Skip]), CmpGt, Return
        match &instrs[0] {
            Instruction::Dispatch { default, arms } => {
                assert_eq!(*default, DefaultAction::Copy);
                // Should have arms for field 2 (Decode+Copy) and field 3 (Skip)
                assert!(arms.iter().any(|a| a.match_ == ArmMatch::Field(2)));
                assert!(arms.iter().any(|a| a.match_ == ArmMatch::Field(3)));
            }
            other => panic!("expected Dispatch, got {other:?}"),
        }
    }
}
