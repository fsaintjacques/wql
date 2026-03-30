pub mod ast;
pub mod bind;
pub mod compile;
pub mod emit;
pub mod error;
pub mod lexer;
pub mod parser;

use ast::Query;
use error::ParseError;

pub use compile::{compile, CompileOptions};
pub use error::CompileError;

/// Parse a WQL source string into a `Query` AST.
///
/// The returned AST has no IR knowledge and no schema binding.
/// Pass it to the schema binder to resolve field names and validate
/// literal types, or call [`compile`] for the full pipeline.
///
/// # Errors
///
/// Returns `ParseError` on invalid syntax.
pub fn parse(source: &str) -> Result<Query, ParseError> {
    let mut parser = parser::Parser::new(source);
    parser.parse_query()
}
