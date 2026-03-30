pub mod ast;
pub mod error;
pub mod lexer;
pub mod parser;

use ast::Query;
use error::ParseError;

/// Parse a WQL source string into a `Query` AST.
///
/// The returned AST has no IR knowledge and no schema binding.
/// Pass it to the schema binder (Block 5) to resolve field names
/// and validate literal types.
///
/// # Errors
///
/// Returns `ParseError` on invalid syntax.
pub fn parse(source: &str) -> Result<Query, ParseError> {
    let mut parser = parser::Parser::new(source);
    parser.parse_query()
}
