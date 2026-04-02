use crate::ast::Query;
use crate::error::CompileError;

/// Options controlling WQL compilation.
#[derive(Default)]
pub struct CompileOptions<'a> {
    /// Serialized `FileDescriptorSet` (full transitive closure).
    /// `None` = schema-free mode; field references must use `#N` syntax.
    pub schema: Option<&'a [u8]>,

    /// Fully-qualified root message type, e.g. `"acme.events.OrderPlaced"`.
    /// Required when `schema` is `Some`; ignored when `schema` is `None`.
    pub root_message: Option<&'a str>,
}

/// Compile a WQL source string into WVM bytecode.
///
/// # Errors
///
/// Returns [`CompileError`] on parse failure, binding failure, or emission failure.
pub fn compile(source: &str, options: &CompileOptions) -> Result<Vec<u8>, CompileError> {
    let query = crate::parse(source)?;

    let semantic_flags = match &query {
        Query::Projection(_) => wql_ir::FLAG_HAS_PROJECTION,
        Query::Predicate(_) => wql_ir::FLAG_HAS_PREDICATE,
        Query::Combined { .. } => wql_ir::FLAG_HAS_PROJECTION | wql_ir::FLAG_HAS_PREDICATE,
    };

    let bound = if options.schema.is_some() {
        crate::bind::bind_with_schema(&query, options)?
    } else {
        crate::bind::bind_schema_free(&query)?
    };

    let instructions = crate::emit::emit(&bound)?;
    Ok(wql_ir::encode_with_flags(&instructions, semantic_flags))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_options_default() {
        let opts = CompileOptions::default();
        assert!(opts.schema.is_none());
        assert!(opts.root_message.is_none());
    }

    #[test]
    fn compile_parse_error_propagates() {
        let result = compile("{ unclosed", &CompileOptions::default());
        assert!(matches!(result, Err(CompileError::Parse(_))));
    }

    #[test]
    fn compile_named_field_without_schema() {
        let result = compile("{ name }", &CompileOptions::default());
        assert!(matches!(
            result,
            Err(CompileError::NamedFieldWithoutSchema { .. })
        ));
    }

    fn compile_and_header(source: &str) -> wql_ir::ProgramHeader {
        let bytecode = compile(source, &CompileOptions::default()).unwrap();
        let (header, _) = wql_ir::decode(&bytecode).unwrap();
        header
    }

    #[test]
    fn flags_projection_only() {
        let h = compile_and_header("{ #1, #2 }");
        assert!(h.has_projection());
        assert!(!h.has_predicate());
    }

    #[test]
    fn flags_empty_projection() {
        // `{ }` is a skip-all projection — still a projection, not a filter.
        let h = compile_and_header("{ }");
        assert!(h.has_projection());
        assert!(!h.has_predicate());
    }

    #[test]
    fn flags_filter_only() {
        let h = compile_and_header("#1 == 42");
        assert!(!h.has_projection());
        assert!(h.has_predicate());
    }

    #[test]
    fn flags_filter_nested_predicate() {
        // Filter on a nested field uses FRAME for descent — must NOT set
        // has_projection even though FRAME is present in the instructions.
        let h = compile_and_header("#1.#2 == 42");
        assert!(!h.has_projection());
        assert!(h.has_predicate());
    }

    #[test]
    fn flags_combined() {
        let h = compile_and_header("WHERE #1 > 0 SELECT { #2 }");
        assert!(h.has_projection());
        assert!(h.has_predicate());
    }
}
