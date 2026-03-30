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

    let bound = if options.schema.is_some() {
        crate::bind::bind_with_schema(&query, options)?
    } else {
        crate::bind::bind_schema_free(&query)?
    };

    let instructions = crate::emit::emit(&bound)?;
    Ok(wql_ir::encode(&instructions))
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
}
