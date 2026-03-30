use crate::ast::{
    CompareOp, FieldPath, FieldRef, Literal, Predicate, PredicateKind, Projection, ProjectionItem,
    ProjectionKind, Query, StringOp,
};
use crate::error::{ParseError, ParseErrorKind};
use crate::lexer::{Lexer, Token, TokenKind};

pub struct Parser<'a> {
    lexer: Lexer<'a>,
}

impl<'a> Parser<'a> {
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            lexer: Lexer::new(source),
        }
    }

    /// Parse a complete WQL query.
    ///
    /// # Errors
    ///
    /// Returns `ParseError` on invalid syntax.
    pub fn parse_query(&mut self) -> Result<Query, ParseError> {
        let query = match self.peek_kind()? {
            TokenKind::LBrace => {
                let proj = self.parse_projection()?;
                Query::Projection(proj)
            }
            TokenKind::Where => {
                self.lexer.next_token()?; // consume WHERE
                let predicate = self.parse_predicate()?;
                self.expect_token(&TokenKind::Select, "'SELECT'")?;
                let projection = self.parse_projection()?;
                Query::Combined {
                    predicate,
                    projection,
                }
            }
            _ => {
                let predicate = self.parse_predicate()?;
                Query::Predicate(predicate)
            }
        };
        self.expect_eof()?;
        Ok(query)
    }

    // ── Projection parsing ──

    pub(crate) fn parse_projection(&mut self) -> Result<Projection, ParseError> {
        let open = self.expect_token(&TokenKind::LBrace, "'{'")?;
        let kind = self.parse_projection_body()?;
        let close = self.expect_token(&TokenKind::RBrace, "'}'")?;
        Ok(Projection {
            kind,
            span: open.span.merge(close.span),
        })
    }

    fn parse_projection_body(&mut self) -> Result<ProjectionKind, ParseError> {
        match self.peek_kind()? {
            // Empty projection: `{ }`
            TokenKind::RBrace => {
                return Ok(ProjectionKind::Inclusion {
                    items: vec![],
                    preserve_unknowns: false,
                });
            }

            // Deep copy: `{ .. }` or `{ .. -field ... }`
            TokenKind::DotDot => {
                self.lexer.next_token()?; // consume ..

                // Check if followed by a field ref (would be deep search, not deep copy).
                // Deep copy body only has exclusions (-field), not bare field refs.
                match self.peek_kind()? {
                    TokenKind::Minus => {
                        let exclusions = self.parse_exclusions()?;
                        return Ok(ProjectionKind::DeepCopy { exclusions });
                    }
                    TokenKind::RBrace => {
                        return Ok(ProjectionKind::DeepCopy { exclusions: vec![] });
                    }
                    _ => {
                        // `..name` — this is actually a deep search item in inclusion mode.
                        // Put back by parsing as inclusion starting with a deep search.
                        let field = self.parse_field_ref()?;
                        let first_item = ProjectionItem::DeepSearch(field);
                        return self.parse_inclusion_rest(vec![first_item]);
                    }
                }
            }

            // Preserve all: `{ ... }`
            TokenKind::Ellipsis => {
                self.lexer.next_token()?; // consume ...
                return Ok(ProjectionKind::Inclusion {
                    items: vec![],
                    preserve_unknowns: true,
                });
            }

            _ => {}
        }

        // Inclusion mode: parse items
        let first = self.parse_inclusion_item()?;
        self.parse_inclusion_rest(vec![first])
    }

    /// Parse the rest of an inclusion body after the first item is already collected.
    fn parse_inclusion_rest(
        &mut self,
        mut items: Vec<ProjectionItem>,
    ) -> Result<ProjectionKind, ParseError> {
        let mut preserve_unknowns = false;

        loop {
            match self.peek_kind()? {
                TokenKind::Comma => {
                    self.lexer.next_token()?; // consume comma
                    match self.peek_kind()? {
                        TokenKind::Ellipsis => {
                            self.lexer.next_token()?;
                            preserve_unknowns = true;
                            break;
                        }
                        TokenKind::RBrace => break, // trailing comma
                        _ => items.push(self.parse_inclusion_item()?),
                    }
                }
                TokenKind::Ellipsis => {
                    self.lexer.next_token()?;
                    preserve_unknowns = true;
                    break;
                }
                TokenKind::RBrace => break,
                _ => {
                    let tok = self.lexer.peek()?;
                    return Err(ParseError {
                        kind: ParseErrorKind::Expected {
                            expected: "',' or '}'",
                            found: tok.kind.describe(),
                        },
                        span: tok.span,
                    });
                }
            }
        }

        Ok(ProjectionKind::Inclusion {
            items,
            preserve_unknowns,
        })
    }

    fn parse_inclusion_item(&mut self) -> Result<ProjectionItem, ParseError> {
        // Deep search: `..field`
        if matches!(self.peek_kind()?, TokenKind::DotDot) {
            self.lexer.next_token()?;
            let field = self.parse_field_ref()?;
            return Ok(ProjectionItem::DeepSearch(field));
        }

        let field = self.parse_field_ref()?;

        // Nested: `field { ... }`
        if matches!(self.peek_kind()?, TokenKind::LBrace) {
            let projection = self.parse_projection()?;
            return Ok(ProjectionItem::Nested {
                field,
                projection: Box::new(projection),
            });
        }

        // Flat include
        Ok(ProjectionItem::Field(field))
    }

    pub(crate) fn parse_field_ref(&mut self) -> Result<FieldRef, ParseError> {
        let tok = self.lexer.peek()?;
        match &tok.kind {
            TokenKind::Hash => {
                let hash_tok = self.lexer.next_token()?;
                let num_tok = self.lexer.next_token()?;
                match num_tok.kind {
                    TokenKind::IntLit(n) => {
                        let n = u32::try_from(n).map_err(|_| ParseError {
                            kind: ParseErrorKind::Expected {
                                expected: "field number (0..2^32)",
                                found: format!("integer {n}"),
                            },
                            span: num_tok.span,
                        })?;
                        Ok(FieldRef::Number(n, hash_tok.span.merge(num_tok.span)))
                    }
                    _ => Err(ParseError {
                        kind: ParseErrorKind::Expected {
                            expected: "field number after '#'",
                            found: num_tok.kind.describe(),
                        },
                        span: num_tok.span,
                    }),
                }
            }
            TokenKind::Ident(_) => {
                let tok = self.lexer.next_token()?;
                let TokenKind::Ident(name) = tok.kind else {
                    unreachable!()
                };
                Ok(FieldRef::Name(name, tok.span))
            }
            // Contextual keywords as field names
            _ if is_contextual_keyword(&tok.kind) => {
                let tok = self.lexer.next_token()?;
                let name = keyword_to_str(&tok.kind).to_string();
                Ok(FieldRef::Name(name, tok.span))
            }
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected {
                    expected: "field name or '#number'",
                    found: tok.kind.describe(),
                },
                span: tok.span,
            }),
        }
    }

    fn parse_exclusions(&mut self) -> Result<Vec<FieldRef>, ParseError> {
        let mut exclusions = Vec::new();
        while matches!(self.peek_kind()?, TokenKind::Minus) {
            self.lexer.next_token()?; // consume -
            exclusions.push(self.parse_field_ref()?);
        }
        Ok(exclusions)
    }

    // ── Predicate parsing ──

    pub(crate) fn parse_predicate(&mut self) -> Result<Predicate, ParseError> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Predicate, ParseError> {
        let mut left = self.parse_and_expr()?;
        while matches!(self.peek_kind()?, TokenKind::PipePipe | TokenKind::Or) {
            self.lexer.next_token()?;
            let right = self.parse_and_expr()?;
            let span = left.span.merge(right.span);
            left = Predicate {
                kind: PredicateKind::Or(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> Result<Predicate, ParseError> {
        let mut left = self.parse_unary_expr()?;
        while matches!(self.peek_kind()?, TokenKind::AmpAmp | TokenKind::And) {
            self.lexer.next_token()?;
            let right = self.parse_unary_expr()?;
            let span = left.span.merge(right.span);
            left = Predicate {
                kind: PredicateKind::And(Box::new(left), Box::new(right)),
                span,
            };
        }
        Ok(left)
    }

    fn parse_unary_expr(&mut self) -> Result<Predicate, ParseError> {
        match self.peek_kind()? {
            TokenKind::Bang | TokenKind::Not => {
                let op_tok = self.lexer.next_token()?;
                let inner = self.parse_unary_expr()?;
                let span = op_tok.span.merge(inner.span);
                Ok(Predicate {
                    kind: PredicateKind::Not(Box::new(inner)),
                    span,
                })
            }
            _ => self.parse_primary_expr(),
        }
    }

    fn parse_primary_expr(&mut self) -> Result<Predicate, ParseError> {
        // Parenthesized expression
        if matches!(self.peek_kind()?, TokenKind::LParen) {
            let open = self.lexer.next_token()?;
            let inner = self.parse_predicate()?;
            let close = self.expect_token(&TokenKind::RParen, "')'")?;
            return Ok(Predicate {
                kind: inner.kind,
                span: open.span.merge(close.span),
            });
        }

        // exists(field) / has(field) — only if followed by '('
        if matches!(self.peek_kind()?, TokenKind::Exists | TokenKind::Has) {
            // Lookahead: check if next-next token is '(' to disambiguate from
            // a field named `exists` used in a comparison.
            let kw_tok = self.lexer.next_token()?;
            if matches!(self.peek_kind()?, TokenKind::LParen) {
                self.lexer.next_token()?; // consume (
                let path = self.parse_field_path()?;
                let close = self.expect_token(&TokenKind::RParen, "')'")?;
                return Ok(Predicate {
                    kind: PredicateKind::Presence(path),
                    span: kw_tok.span.merge(close.span),
                });
            }
            // Not followed by '(' — treat as field name, continue to field path parsing.
            let field_ref = FieldRef::Name(keyword_to_str(&kw_tok.kind).to_string(), kw_tok.span);
            let path = self.parse_field_path_rest(field_ref)?;
            return self.parse_predicate_after_path(path);
        }

        // Field path followed by operator
        let path = self.parse_field_path()?;
        self.parse_predicate_after_path(path)
    }

    /// After parsing a field path, determine the predicate form.
    fn parse_predicate_after_path(&mut self, path: FieldPath) -> Result<Predicate, ParseError> {
        let start = path.span;

        match self.peek_kind()? {
            // Comparison operators
            TokenKind::EqEq
            | TokenKind::BangEq
            | TokenKind::Lt
            | TokenKind::Lte
            | TokenKind::Gt
            | TokenKind::Gte => {
                let op = self.parse_compare_op()?;
                let value = self.parse_literal()?;
                let span = start.merge(value.span());
                Ok(Predicate {
                    kind: PredicateKind::Comparison {
                        field: path,
                        op,
                        value,
                    },
                    span,
                })
            }

            // Set membership: `field in [...]`
            TokenKind::In => {
                self.lexer.next_token()?;
                self.expect_token(&TokenKind::LBracket, "'['")?;
                let values = self.parse_literal_list()?;
                let close = self.expect_token(&TokenKind::RBracket, "']'")?;
                let span = start.merge(close.span);
                Ok(Predicate {
                    kind: PredicateKind::InSet {
                        field: path,
                        values,
                    },
                    span,
                })
            }

            // String predicates
            TokenKind::StartsWith => {
                self.lexer.next_token()?;
                let value = self.parse_literal()?;
                let span = start.merge(value.span());
                Ok(Predicate {
                    kind: PredicateKind::StringPredicate {
                        field: path,
                        op: StringOp::StartsWith,
                        value,
                    },
                    span,
                })
            }
            TokenKind::EndsWith => {
                self.lexer.next_token()?;
                let value = self.parse_literal()?;
                let span = start.merge(value.span());
                Ok(Predicate {
                    kind: PredicateKind::StringPredicate {
                        field: path,
                        op: StringOp::EndsWith,
                        value,
                    },
                    span,
                })
            }
            TokenKind::Contains => {
                self.lexer.next_token()?;
                let value = self.parse_literal()?;
                let span = start.merge(value.span());
                Ok(Predicate {
                    kind: PredicateKind::StringPredicate {
                        field: path,
                        op: StringOp::Contains,
                        value,
                    },
                    span,
                })
            }
            TokenKind::Matches => {
                self.lexer.next_token()?;
                let value = self.parse_literal()?;
                let span = start.merge(value.span());
                Ok(Predicate {
                    kind: PredicateKind::StringPredicate {
                        field: path,
                        op: StringOp::Matches,
                        value,
                    },
                    span,
                })
            }

            _ => {
                let tok = self.lexer.peek()?;
                Err(ParseError {
                    kind: ParseErrorKind::Expected {
                        expected: "comparison operator, 'in', or string predicate",
                        found: tok.kind.describe(),
                    },
                    span: tok.span,
                })
            }
        }
    }

    fn parse_compare_op(&mut self) -> Result<CompareOp, ParseError> {
        let tok = self.lexer.next_token()?;
        match tok.kind {
            TokenKind::EqEq => Ok(CompareOp::Eq),
            TokenKind::BangEq => Ok(CompareOp::Neq),
            TokenKind::Lt => Ok(CompareOp::Lt),
            TokenKind::Lte => Ok(CompareOp::Lte),
            TokenKind::Gt => Ok(CompareOp::Gt),
            TokenKind::Gte => Ok(CompareOp::Gte),
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected {
                    expected: "comparison operator",
                    found: tok.kind.describe(),
                },
                span: tok.span,
            }),
        }
    }

    fn parse_field_path(&mut self) -> Result<FieldPath, ParseError> {
        let first = self.parse_field_ref()?;
        self.parse_field_path_rest(first)
    }

    /// Build a field path given an already-parsed first segment.
    fn parse_field_path_rest(&mut self, first: FieldRef) -> Result<FieldPath, ParseError> {
        let start = first.span();
        let mut segments = vec![first];
        while matches!(self.peek_kind()?, TokenKind::Dot) {
            self.lexer.next_token()?; // consume .
            segments.push(self.parse_field_ref()?);
        }
        let end = segments.last().expect("at least one segment").span();
        Ok(FieldPath {
            segments,
            span: start.merge(end),
        })
    }

    fn parse_literal(&mut self) -> Result<Literal, ParseError> {
        let tok = self.lexer.peek()?;
        match &tok.kind {
            TokenKind::IntLit(_) => {
                let tok = self.lexer.next_token()?;
                let TokenKind::IntLit(n) = tok.kind else {
                    unreachable!()
                };
                Ok(Literal::Int(n, tok.span))
            }
            TokenKind::Minus => {
                let minus_tok = self.lexer.next_token()?;
                let num_tok = self.lexer.next_token()?;
                match num_tok.kind {
                    TokenKind::IntLit(n) => {
                        let span = minus_tok.span.merge(num_tok.span);
                        Ok(Literal::Int(-n, span))
                    }
                    _ => Err(ParseError {
                        kind: ParseErrorKind::Expected {
                            expected: "integer after '-'",
                            found: num_tok.kind.describe(),
                        },
                        span: num_tok.span,
                    }),
                }
            }
            TokenKind::StringLit(_) => {
                let tok = self.lexer.next_token()?;
                let TokenKind::StringLit(s) = tok.kind else {
                    unreachable!()
                };
                Ok(Literal::String(s, tok.span))
            }
            TokenKind::True => {
                let tok = self.lexer.next_token()?;
                Ok(Literal::Bool(true, tok.span))
            }
            TokenKind::False => {
                let tok = self.lexer.next_token()?;
                Ok(Literal::Bool(false, tok.span))
            }
            _ => Err(ParseError {
                kind: ParseErrorKind::Expected {
                    expected: "literal (integer, string, or boolean)",
                    found: tok.kind.describe(),
                },
                span: tok.span,
            }),
        }
    }

    fn parse_literal_list(&mut self) -> Result<Vec<Literal>, ParseError> {
        let mut values = Vec::new();
        // Empty list: `[]`
        if matches!(self.peek_kind()?, TokenKind::RBracket) {
            return Ok(values);
        }
        values.push(self.parse_literal()?);
        loop {
            match self.peek_kind()? {
                TokenKind::Comma => {
                    self.lexer.next_token()?;
                    // Trailing comma before ]
                    if matches!(self.peek_kind()?, TokenKind::RBracket) {
                        break;
                    }
                    values.push(self.parse_literal()?);
                }
                TokenKind::RBracket => break,
                _ => {
                    let tok = self.lexer.peek()?;
                    return Err(ParseError {
                        kind: ParseErrorKind::Expected {
                            expected: "',' or ']'",
                            found: tok.kind.describe(),
                        },
                        span: tok.span,
                    });
                }
            }
        }
        Ok(values)
    }

    // ── Helpers ──

    fn expect_token(
        &mut self,
        expected_kind: &TokenKind,
        expected_desc: &'static str,
    ) -> Result<Token, ParseError> {
        let tok = self.lexer.next_token()?;
        if std::mem::discriminant(&tok.kind) == std::mem::discriminant(expected_kind) {
            Ok(tok)
        } else {
            Err(ParseError {
                kind: ParseErrorKind::Expected {
                    expected: expected_desc,
                    found: tok.kind.describe(),
                },
                span: tok.span,
            })
        }
    }

    fn expect_eof(&mut self) -> Result<(), ParseError> {
        let tok = self.lexer.peek()?;
        if tok.kind == TokenKind::Eof {
            Ok(())
        } else {
            Err(ParseError {
                kind: ParseErrorKind::Expected {
                    expected: "end of input",
                    found: tok.kind.describe(),
                },
                span: tok.span,
            })
        }
    }

    fn peek_kind(&mut self) -> Result<&TokenKind, ParseError> {
        Ok(&self.lexer.peek()?.kind)
    }
}

/// Returns true if the token kind is a contextual keyword that can also serve
/// as a field name.
fn is_contextual_keyword(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::And
            | TokenKind::Or
            | TokenKind::Not
            | TokenKind::In
            | TokenKind::Exists
            | TokenKind::Has
            | TokenKind::StartsWith
            | TokenKind::EndsWith
            | TokenKind::Contains
            | TokenKind::Matches
            | TokenKind::True
            | TokenKind::False
    )
}

/// Convert a contextual keyword token to its source string form.
fn keyword_to_str(kind: &TokenKind) -> &'static str {
    match kind {
        TokenKind::And => "AND",
        TokenKind::Or => "OR",
        TokenKind::Not => "NOT",
        TokenKind::In => "in",
        TokenKind::Exists => "exists",
        TokenKind::Has => "has",
        TokenKind::StartsWith => "starts_with",
        TokenKind::EndsWith => "ends_with",
        TokenKind::Contains => "contains",
        TokenKind::Matches => "matches",
        TokenKind::True => "true",
        TokenKind::False => "false",
        _ => unreachable!("not a contextual keyword"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::PredicateKind;
    use crate::lexer::Span;

    /// Helper: parse a projection from source.
    fn parse_proj(source: &str) -> Result<Projection, ParseError> {
        let mut parser = Parser::new(source);
        let proj = parser.parse_projection()?;
        parser.expect_eof()?;
        Ok(proj)
    }

    /// Helper: parse a full query from source.
    fn parse_query(source: &str) -> Result<Query, ParseError> {
        crate::parse(source)
    }

    /// Compare projection kind ignoring spans.
    fn assert_inclusion(proj: &Projection, expected_items: &[&str], preserve: bool) {
        match &proj.kind {
            ProjectionKind::Inclusion {
                items,
                preserve_unknowns,
            } => {
                assert_eq!(*preserve_unknowns, preserve, "preserve_unknowns mismatch");
                let names: Vec<String> = items.iter().map(item_debug).collect();
                let expected: Vec<String> =
                    expected_items.iter().map(|s| (*s).to_string()).collect();
                assert_eq!(names, expected);
            }
            ProjectionKind::DeepCopy { .. } => panic!("expected Inclusion, got DeepCopy"),
        }
    }

    fn item_debug(item: &ProjectionItem) -> String {
        match item {
            ProjectionItem::Field(FieldRef::Name(n, _)) => n.clone(),
            ProjectionItem::Field(FieldRef::Number(n, _)) => format!("#{n}"),
            ProjectionItem::Nested {
                field: FieldRef::Name(n, _),
                ..
            } => format!("{n}{{...}}"),
            ProjectionItem::Nested {
                field: FieldRef::Number(n, _),
                ..
            } => format!("#{n}{{...}}"),
            ProjectionItem::DeepSearch(FieldRef::Name(n, _)) => format!("..{n}"),
            ProjectionItem::DeepSearch(FieldRef::Number(n, _)) => format!("..#{n}"),
        }
    }

    #[test]
    fn proj_flat_strict() {
        let proj = parse_proj("{ name, age }").unwrap();
        assert_inclusion(&proj, &["name", "age"], false);
    }

    #[test]
    fn proj_flat_preserve() {
        let proj = parse_proj("{ name, age, ... }").unwrap();
        assert_inclusion(&proj, &["name", "age"], true);
    }

    #[test]
    fn proj_trailing_comma() {
        let proj = parse_proj("{ name, age, }").unwrap();
        assert_inclusion(&proj, &["name", "age"], false);
    }

    #[test]
    fn proj_nested() {
        let proj = parse_proj("{ address { city } }").unwrap();
        assert_inclusion(&proj, &["address{...}"], false);

        // Verify the nested projection
        if let ProjectionKind::Inclusion { items, .. } = &proj.kind {
            if let ProjectionItem::Nested { projection, .. } = &items[0] {
                assert_inclusion(projection, &["city"], false);
            } else {
                panic!("expected Nested");
            }
        }
    }

    #[test]
    fn proj_nested_preserve() {
        let proj = parse_proj("{ address { city, ... }, ... }").unwrap();
        assert_inclusion(&proj, &["address{...}"], true);

        if let ProjectionKind::Inclusion { items, .. } = &proj.kind {
            if let ProjectionItem::Nested { projection, .. } = &items[0] {
                assert_inclusion(projection, &["city"], true);
            }
        }
    }

    #[test]
    fn proj_deep_copy() {
        let proj = parse_proj("{ .. }").unwrap();
        match &proj.kind {
            ProjectionKind::DeepCopy { exclusions } => {
                assert!(exclusions.is_empty());
            }
            _ => panic!("expected DeepCopy"),
        }
    }

    #[test]
    fn proj_deep_exclusion() {
        let proj = parse_proj("{ .. -payload -thumbnail }").unwrap();
        match &proj.kind {
            ProjectionKind::DeepCopy { exclusions } => {
                assert_eq!(exclusions.len(), 2);
                assert!(matches!(&exclusions[0], FieldRef::Name(n, _) if n == "payload"));
                assert!(matches!(&exclusions[1], FieldRef::Name(n, _) if n == "thumbnail"));
            }
            _ => panic!("expected DeepCopy"),
        }
    }

    #[test]
    fn proj_deep_search() {
        let proj = parse_proj("{ ..name }").unwrap();
        assert_inclusion(&proj, &["..name"], false);
    }

    #[test]
    fn proj_scoped_deep() {
        let proj = parse_proj("{ departments { ..name } }").unwrap();
        assert_inclusion(&proj, &["departments{...}"], false);

        if let ProjectionKind::Inclusion { items, .. } = &proj.kind {
            if let ProjectionItem::Nested { projection, .. } = &items[0] {
                assert_inclusion(projection, &["..name"], false);
            }
        }
    }

    #[test]
    fn proj_field_number() {
        let proj = parse_proj("{ #1, #3 { #1 } }").unwrap();
        assert_inclusion(&proj, &["#1", "#3{...}"], false);

        if let ProjectionKind::Inclusion { items, .. } = &proj.kind {
            assert!(matches!(
                &items[0],
                ProjectionItem::Field(FieldRef::Number(1, _))
            ));
            if let ProjectionItem::Nested {
                field: FieldRef::Number(3, _),
                projection,
            } = &items[1]
            {
                assert_inclusion(projection, &["#1"], false);
            } else {
                panic!("expected Nested with #3");
            }
        }
    }

    #[test]
    fn proj_empty() {
        let proj = parse_proj("{ }").unwrap();
        assert_inclusion(&proj, &[], false);
    }

    #[test]
    fn proj_preserve_all() {
        let proj = parse_proj("{ ... }").unwrap();
        assert_inclusion(&proj, &[], true);
    }

    #[test]
    fn proj_mixed_items() {
        let proj = parse_proj("{ name, ..tags, address { city }, ... }").unwrap();
        assert_inclusion(&proj, &["name", "..tags", "address{...}"], true);
    }

    #[test]
    fn proj_deep_exclusion_by_number() {
        let proj = parse_proj("{ .. -#7 }").unwrap();
        match &proj.kind {
            ProjectionKind::DeepCopy { exclusions } => {
                assert_eq!(exclusions.len(), 1);
                assert!(matches!(&exclusions[0], FieldRef::Number(7, _)));
            }
            _ => panic!("expected DeepCopy"),
        }
    }

    #[test]
    fn proj_err_unclosed() {
        let err = parse_proj("{ name").unwrap_err();
        // After parsing `name`, the parser expects `,` or `}` but finds EOF.
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "',' or '}'",
                ..
            }
        ));
    }

    #[test]
    fn proj_err_ellipsis_not_last() {
        let err = parse_proj("{ ..., name }").unwrap_err();
        // After `...`, we expect `}` but find `,`
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "'}'",
                ..
            }
        ));
    }

    #[test]
    fn proj_contextual_keyword_as_field() {
        let proj = parse_proj("{ exists, has, in }").unwrap();
        assert_inclusion(&proj, &["exists", "has", "in"], false);
    }

    #[test]
    fn proj_spans() {
        let proj = parse_proj("{ name }").unwrap();
        assert_eq!(proj.span, Span::new(0, 8));
    }

    // ── Predicate tests ──

    /// Extract the PredicateKind from a parsed predicate query.
    fn parse_pred(source: &str) -> Predicate {
        match parse_query(source).unwrap() {
            Query::Predicate(p) => p,
            other => panic!("expected Predicate, got {other:?}"),
        }
    }

    #[test]
    fn pred_cmp_eq() {
        let p = parse_pred("age == 42");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Eq,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_neq() {
        let p = parse_pred("status != 0");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Neq,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_lt() {
        let p = parse_pred("age < 18");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Lt,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_lte() {
        let p = parse_pred("age <= 18");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Lte,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_gt() {
        let p = parse_pred("age > 18");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Gt,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_gte() {
        let p = parse_pred("age >= 18");
        assert!(matches!(
            &p.kind,
            PredicateKind::Comparison {
                op: CompareOp::Gte,
                ..
            }
        ));
    }

    #[test]
    fn pred_cmp_negative() {
        let p = parse_pred("temp > -10");
        if let PredicateKind::Comparison { value, .. } = &p.kind {
            assert!(matches!(value, Literal::Int(-10, _)));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn pred_cmp_string() {
        let p = parse_pred(r#"name == "Alice""#);
        if let PredicateKind::Comparison { value, .. } = &p.kind {
            assert!(matches!(value, Literal::String(s, _) if s == "Alice"));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn pred_cmp_bool() {
        let p = parse_pred("active == true");
        if let PredicateKind::Comparison { value, .. } = &p.kind {
            assert!(matches!(value, Literal::Bool(true, _)));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn pred_nested_field() {
        let p = parse_pred(r#"address.city == "NYC""#);
        if let PredicateKind::Comparison { field, .. } = &p.kind {
            assert_eq!(field.segments.len(), 2);
            assert!(matches!(&field.segments[0], FieldRef::Name(n, _) if n == "address"));
            assert!(matches!(&field.segments[1], FieldRef::Name(n, _) if n == "city"));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn pred_field_number_path() {
        let p = parse_pred(r#"#3.#1 == "NYC""#);
        if let PredicateKind::Comparison { field, .. } = &p.kind {
            assert_eq!(field.segments.len(), 2);
            assert!(matches!(&field.segments[0], FieldRef::Number(3, _)));
            assert!(matches!(&field.segments[1], FieldRef::Number(1, _)));
        } else {
            panic!("expected Comparison");
        }
    }

    #[test]
    fn pred_and_symbolic() {
        let p = parse_pred("a > 1 && b > 2");
        assert!(matches!(&p.kind, PredicateKind::And(_, _)));
    }

    #[test]
    fn pred_and_keyword() {
        let p = parse_pred("a > 1 AND b > 2");
        assert!(matches!(&p.kind, PredicateKind::And(_, _)));
    }

    #[test]
    fn pred_or() {
        let p = parse_pred("a > 1 || b > 2");
        assert!(matches!(&p.kind, PredicateKind::Or(_, _)));
    }

    #[test]
    fn pred_or_keyword() {
        let p = parse_pred("a > 1 OR b > 2");
        assert!(matches!(&p.kind, PredicateKind::Or(_, _)));
    }

    #[test]
    fn pred_not_bang() {
        let p = parse_pred("!active == true");
        assert!(matches!(&p.kind, PredicateKind::Not(_)));
    }

    #[test]
    fn pred_not_keyword() {
        let p = parse_pred("NOT active == true");
        assert!(matches!(&p.kind, PredicateKind::Not(_)));
    }

    #[test]
    fn pred_precedence() {
        // && binds tighter than ||
        let p = parse_pred("a > 1 || b > 2 && c > 3");
        // Should parse as: Or(Cmp(a), And(Cmp(b), Cmp(c)))
        if let PredicateKind::Or(left, right) = &p.kind {
            assert!(matches!(&left.kind, PredicateKind::Comparison { .. }));
            assert!(matches!(&right.kind, PredicateKind::And(_, _)));
        } else {
            panic!("expected Or at top level, got {:?}", p.kind);
        }
    }

    #[test]
    fn pred_parens() {
        let p = parse_pred("(a > 1 || b > 2) && c > 3");
        // Should parse as: And(Or(Cmp(a), Cmp(b)), Cmp(c))
        if let PredicateKind::And(left, right) = &p.kind {
            assert!(matches!(&left.kind, PredicateKind::Or(_, _)));
            assert!(matches!(&right.kind, PredicateKind::Comparison { .. }));
        } else {
            panic!("expected And at top level");
        }
    }

    #[test]
    fn pred_double_not() {
        let p = parse_pred("!!x == 1");
        if let PredicateKind::Not(inner) = &p.kind {
            assert!(matches!(&inner.kind, PredicateKind::Not(_)));
        } else {
            panic!("expected Not");
        }
    }

    #[test]
    fn pred_exists() {
        let p = parse_pred("exists(name)");
        if let PredicateKind::Presence(path) = &p.kind {
            assert_eq!(path.segments.len(), 1);
            assert!(matches!(&path.segments[0], FieldRef::Name(n, _) if n == "name"));
        } else {
            panic!("expected Presence");
        }
    }

    #[test]
    fn pred_has() {
        let p = parse_pred("has(address.city)");
        if let PredicateKind::Presence(path) = &p.kind {
            assert_eq!(path.segments.len(), 2);
        } else {
            panic!("expected Presence");
        }
    }

    #[test]
    fn pred_in_set() {
        let p = parse_pred("status in [1, 2, 3]");
        if let PredicateKind::InSet { values, .. } = &p.kind {
            assert_eq!(values.len(), 3);
        } else {
            panic!("expected InSet");
        }
    }

    #[test]
    fn pred_in_set_empty() {
        let p = parse_pred("status in []");
        if let PredicateKind::InSet { values, .. } = &p.kind {
            assert!(values.is_empty());
        } else {
            panic!("expected InSet");
        }
    }

    #[test]
    fn pred_in_set_trailing_comma() {
        let p = parse_pred("status in [1, 2,]");
        if let PredicateKind::InSet { values, .. } = &p.kind {
            assert_eq!(values.len(), 2);
        } else {
            panic!("expected InSet");
        }
    }

    #[test]
    fn pred_starts_with() {
        let p = parse_pred(r#"name starts_with "Al""#);
        assert!(matches!(
            &p.kind,
            PredicateKind::StringPredicate {
                op: StringOp::StartsWith,
                ..
            }
        ));
    }

    #[test]
    fn pred_ends_with() {
        let p = parse_pred(r#"name ends_with "ce""#);
        assert!(matches!(
            &p.kind,
            PredicateKind::StringPredicate {
                op: StringOp::EndsWith,
                ..
            }
        ));
    }

    #[test]
    fn pred_contains() {
        let p = parse_pred(r#"name contains "lic""#);
        assert!(matches!(
            &p.kind,
            PredicateKind::StringPredicate {
                op: StringOp::Contains,
                ..
            }
        ));
    }

    #[test]
    fn pred_matches() {
        let p = parse_pred(r#"name matches "^A.*""#);
        assert!(matches!(
            &p.kind,
            PredicateKind::StringPredicate {
                op: StringOp::Matches,
                ..
            }
        ));
    }

    #[test]
    fn pred_complex() {
        let p = parse_pred(
            r#"age > 18 AND (name starts_with "A" OR exists(vip)) AND status in [1, 2]"#,
        );
        // Top level: And(And(Cmp, Or(...)), InSet)
        assert!(matches!(&p.kind, PredicateKind::And(_, _)));
    }

    // ── Combined form tests ──

    #[test]
    fn combined_simple() {
        let q = parse_query(r#"WHERE age > 18 SELECT { name }"#).unwrap();
        match q {
            Query::Combined {
                predicate,
                projection,
            } => {
                assert!(matches!(
                    &predicate.kind,
                    PredicateKind::Comparison {
                        op: CompareOp::Gt,
                        ..
                    }
                ));
                assert_inclusion(&projection, &["name"], false);
            }
            _ => panic!("expected Combined"),
        }
    }

    #[test]
    fn combined_complex() {
        let q = parse_query(
            r#"WHERE age > 18 AND address.city == "NYC" SELECT { name, address { city }, ... }"#,
        )
        .unwrap();
        match q {
            Query::Combined {
                predicate,
                projection,
            } => {
                assert!(matches!(&predicate.kind, PredicateKind::And(_, _)));
                assert_inclusion(&projection, &["name", "address{...}"], true);
            }
            _ => panic!("expected Combined"),
        }
    }

    // ── Contextual keyword as field name in predicates ──

    #[test]
    fn field_named_exists() {
        // `exists == 1` should be a comparison on a field called `exists`,
        // not a presence check (which requires `exists(field)`).
        let p = parse_pred("exists == 1");
        if let PredicateKind::Comparison { field, .. } = &p.kind {
            assert!(matches!(&field.segments[0], FieldRef::Name(n, _) if n == "exists"));
        } else {
            panic!("expected Comparison, got {:?}", p.kind);
        }
    }

    #[test]
    fn field_named_has() {
        let p = parse_pred("has == 1");
        if let PredicateKind::Comparison { field, .. } = &p.kind {
            assert!(matches!(&field.segments[0], FieldRef::Name(n, _) if n == "has"));
        } else {
            panic!("expected Comparison");
        }
    }

    // ── Query form detection ──

    #[test]
    fn query_projection() {
        let q = parse_query("{ name }").unwrap();
        assert!(matches!(q, Query::Projection(_)));
    }

    #[test]
    fn query_predicate() {
        let q = parse_query("age > 18").unwrap();
        assert!(matches!(q, Query::Predicate(_)));
    }

    #[test]
    fn query_combined() {
        let q = parse_query("WHERE age > 18 SELECT { name }").unwrap();
        assert!(matches!(q, Query::Combined { .. }));
    }

    // ── Error cases ──

    #[test]
    fn err_empty_input() {
        let err = parse_query("").unwrap_err();
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected { .. } | ParseErrorKind::UnexpectedEof
        ));
    }

    #[test]
    fn err_bare_operator() {
        let err = parse_query("==").unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::Expected { .. }));
    }

    #[test]
    fn err_missing_rhs() {
        let err = parse_query("age >").unwrap_err();
        assert!(matches!(err.kind, ParseErrorKind::Expected { .. }));
    }

    #[test]
    fn err_missing_select() {
        let err = parse_query("WHERE age > 18").unwrap_err();
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "'SELECT'",
                ..
            }
        ));
    }

    #[test]
    fn err_missing_projection() {
        let err = parse_query("WHERE age > 18 SELECT").unwrap_err();
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "'{'",
                ..
            }
        ));
    }

    #[test]
    fn err_unclosed_paren() {
        let err = parse_query("(age > 18").unwrap_err();
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "')'",
                ..
            }
        ));
    }

    #[test]
    fn err_in_set_unclosed() {
        let err = parse_query("age in [1, 2").unwrap_err();
        // After parsing `2`, expects `,` or `]` but finds EOF.
        assert!(matches!(
            err.kind,
            ParseErrorKind::Expected {
                expected: "',' or ']'",
                ..
            }
        ));
    }

    // ── Public API ──

    #[test]
    fn public_parse_fn() {
        let q = crate::parse(r#"WHERE age > 18 SELECT { name, ... }"#).unwrap();
        assert!(matches!(q, Query::Combined { .. }));
    }
}
