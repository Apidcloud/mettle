//! Source parsing and grammar rules.

use super::{
    BinaryOperator, ContextDecl, ContextMember, DeclarationKind, Expression, ExpressionKind,
    FileContextUse, MettleDecl, ObjectField, Program, Span, Spanned, Statement, SyntaxError, Token,
    TokenKind, lex,
};

/// Parse a complete Mettle source file.
///
/// # Errors
///
/// Returns the first lexical or grammatical error with a byte span into `source`.
pub fn parse(source: &str) -> Result<Program, SyntaxError> {
    Parser::new(lex(source)?).parse_program()
}

/// Parse one standalone value expression, primarily for CLI flow arguments.
///
/// # Errors
///
/// Returns a lexical or grammatical error when the complete input is not one expression.
pub fn parse_value(source: &str) -> Result<Expression, SyntaxError> {
    let mut parser = Parser::new(lex(source)?);
    let expression = parser.parse_expression()?;
    if !parser.at(&TokenKind::End) {
        return Err(parser.expected("the end of the value"));
    }
    Ok(expression)
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, cursor: 0 }
    }

    fn parse_program(mut self) -> Result<Program, SyntaxError> {
        let mut namespace = None;
        let mut namespace_uses = Vec::new();
        let mut contexts = Vec::new();
        let mut file_contexts = Vec::new();
        let mut flows = Vec::new();
        while !self.at(&TokenKind::End) {
            if self.at(&TokenKind::Namespace) {
                let start = self.advance().span;
                let name = self.take_identifier("a namespace name")?;
                if namespace.replace(name.clone()).is_some() {
                    return Err(SyntaxError::new(
                        "a file may declare only one namespace",
                        start.join(name.span),
                    ));
                }
            } else if self.at(&TokenKind::Use) {
                let start = self.advance().span;
                if self.take_if(&TokenKind::Namespace) {
                    namespace_uses.push(self.take_identifier("a namespace name")?);
                } else {
                    self.take(&TokenKind::Context)?;
                    if self.at(&TokenKind::LeftBrace) {
                        let mut context = self.parse_context_body(start, None)?;
                        file_contexts.push(FileContextUse::Inline(context.span));
                        context.namespace = namespace
                            .as_ref()
                            .map_or_else(String::new, |name| name.value.clone());
                        context.namespace_uses.clone_from(&namespace_uses);
                        contexts.push(context);
                    } else {
                        let name = self.take_identifier("a context name")?;
                        if self.at(&TokenKind::LeftBrace) {
                            let mut context = self.parse_context_body(start, Some(name.clone()))?;
                            file_contexts.push(FileContextUse::Named(name));
                            context.namespace = namespace
                                .as_ref()
                                .map_or_else(String::new, |name| name.value.clone());
                            context.namespace_uses.clone_from(&namespace_uses);
                            contexts.push(context);
                        } else {
                            file_contexts.push(FileContextUse::Named(name));
                        }
                    }
                }
            } else if self.at(&TokenKind::Context) {
                let mut context = self.parse_context()?;
                context.namespace = namespace
                    .as_ref()
                    .map_or_else(String::new, |name| name.value.clone());
                context.namespace_uses.clone_from(&namespace_uses);
                contexts.push(context);
            } else if self.at(&TokenKind::Mettle) {
                let mut flow = self.parse_flow()?;
                flow.namespace = namespace
                    .as_ref()
                    .map_or_else(String::new, |name| name.value.clone());
                flow.namespace_uses.clone_from(&namespace_uses);
                flows.push(flow);
            } else if self.at(&TokenKind::Test) {
                let mut test = self.parse_test()?;
                test.namespace = namespace
                    .as_ref()
                    .map_or_else(String::new, |name| name.value.clone());
                test.namespace_uses.clone_from(&namespace_uses);
                flows.push(test);
            } else {
                let mut flow = self.parse_anonymous_expression_flow()?;
                flow.namespace = namespace
                    .as_ref()
                    .map_or_else(String::new, |name| name.value.clone());
                flow.namespace_uses.clone_from(&namespace_uses);
                flows.push(flow);
            }
        }
        Ok(Program {
            namespace,
            namespace_uses,
            contexts,
            file_contexts,
            flows,
        })
    }

    fn parse_context(&mut self) -> Result<ContextDecl, SyntaxError> {
        let start = self.take(&TokenKind::Context)?.span;
        let name = self.take_identifier("a context name")?;
        self.parse_context_body(start, Some(name))
    }

    fn parse_context_body(
        &mut self,
        start: Span,
        name: Option<Spanned<String>>,
    ) -> Result<ContextDecl, SyntaxError> {
        self.take(&TokenKind::LeftBrace)?;
        let mut members = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::End) {
                return Err(self.expected("a context member or `}`"));
            }
            if self.at(&TokenKind::Use) {
                let start = self.advance().span;
                self.take(&TokenKind::Context)?;
                let name = self.take_identifier("a context name")?;
                members.push(ContextMember::UseContext {
                    span: start.join(name.span),
                    name,
                });
            } else if self.at(&TokenKind::Defaults) {
                let defaults = self.advance().span;
                let capability = self.take_identifier("a capability name")?;
                let (fields, block_span) = self.parse_object_fields()?;
                members.push(ContextMember::Defaults {
                    capability,
                    fields,
                    span: defaults.join(block_span),
                });
            } else {
                members.push(ContextMember::Field(self.parse_object_field()?));
            }
        }
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(ContextDecl {
            namespace: String::new(),
            namespace_uses: Vec::new(),
            name,
            members,
            span: start.join(end),
        })
    }

    fn parse_flow(&mut self) -> Result<MettleDecl, SyntaxError> {
        let start = self.take(&TokenKind::Mettle)?.span;
        if self.at(&TokenKind::LeftBrace) {
            let (body, end) = self.parse_statement_block()?;
            return Ok(MettleDecl {
                kind: DeclarationKind::Flow,
                namespace: String::new(),
                namespace_uses: Vec::new(),
                name: None,
                parameters: Vec::new(),
                body,
                span: start.join(end),
            });
        }

        let name = self.take_identifier("a flow name or `{`")?;
        self.take(&TokenKind::LeftParen)?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                parameters.push(self.take_identifier("a parameter name")?);
                if !self.take_if(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.take(&TokenKind::RightParen)?;
        if self.take_if(&TokenKind::Equal) {
            let expression = self.parse_expression()?;
            let span = start.join(expression.span);
            return Ok(MettleDecl {
                kind: DeclarationKind::Flow,
                namespace: String::new(),
                namespace_uses: Vec::new(),
                name: Some(name),
                parameters,
                body: vec![Statement::Return { expression, span }],
                span,
            });
        }

        let (body, end) = self.parse_statement_block()?;
        Ok(MettleDecl {
            kind: DeclarationKind::Flow,
            namespace: String::new(),
            namespace_uses: Vec::new(),
            name: Some(name),
            parameters,
            body,
            span: start.join(end),
        })
    }

    fn parse_test(&mut self) -> Result<MettleDecl, SyntaxError> {
        let start = self.take(&TokenKind::Test)?.span;
        self.take(&TokenKind::LeftParen)?;
        let token = self.advance();
        let TokenKind::String(value) = token.kind else {
            return Err(SyntaxError::new("expected a test name string", token.span));
        };
        if value.trim().is_empty() {
            return Err(SyntaxError::new("test name cannot be empty", token.span));
        }
        self.take(&TokenKind::RightParen)?;
        let (body, end) = self.parse_statement_block()?;
        Ok(MettleDecl {
            kind: DeclarationKind::Test,
            namespace: String::new(),
            namespace_uses: Vec::new(),
            name: Some(Spanned {
                value,
                span: token.span,
            }),
            parameters: Vec::new(),
            body,
            span: start.join(end),
        })
    }

    fn parse_statement_block(&mut self) -> Result<(Vec<Statement>, Span), SyntaxError> {
        self.take(&TokenKind::LeftBrace)?;
        let mut body = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::End) {
                return Err(self.expected("a statement or `}`"));
            }
            body.push(self.parse_statement()?);
        }
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok((body, end))
    }

    fn parse_anonymous_expression_flow(&mut self) -> Result<MettleDecl, SyntaxError> {
        let expression = self.parse_expression()?;
        if !matches!(expression.kind, ExpressionKind::Call { .. }) {
            return Err(SyntaxError::new(
                "a top-level anonymous flow must be a call expression",
                expression.span,
            ));
        }
        let span = expression.span;
        Ok(MettleDecl {
            kind: DeclarationKind::Flow,
            namespace: String::new(),
            namespace_uses: Vec::new(),
            name: None,
            parameters: Vec::new(),
            body: vec![Statement::Return { expression, span }],
            span,
        })
    }

    fn parse_statement(&mut self) -> Result<Statement, SyntaxError> {
        if self.at(&TokenKind::Use) {
            let start = self.advance().span;
            self.take(&TokenKind::Context)?;
            let name = self.take_identifier("a context name")?;
            return Ok(Statement::UseContext {
                span: start.join(name.span),
                name,
            });
        }

        if self.at(&TokenKind::Return) {
            let start = self.advance().span;
            let expression = self.parse_expression()?;
            return Ok(Statement::Return {
                span: start.join(expression.span),
                expression,
            });
        }

        if self.at(&TokenKind::Assert) {
            let start = self.advance().span;
            self.take(&TokenKind::LeftParen)?;
            let expression = self.parse_expression()?;
            let message = if self.take_if(&TokenKind::Comma) {
                let message = self.parse_expression()?;
                if !matches!(message.kind, ExpressionKind::String(_)) {
                    return Err(SyntaxError::new(
                        "assertion message must be a string literal",
                        message.span,
                    ));
                }
                Some(message)
            } else {
                None
            };
            let end = self.take(&TokenKind::RightParen)?.span;
            return Ok(Statement::Assert {
                expression,
                message,
                span: start.join(end),
            });
        }

        if matches!(self.current().kind, TokenKind::Identifier(_))
            && self.peek_at(1, &TokenKind::Equal)
        {
            let name = self.take_identifier("a binding name")?;
            self.take(&TokenKind::Equal)?;
            let expression = self.parse_expression()?;
            return Ok(Statement::Bind {
                span: name.span.join(expression.span),
                name,
                expression,
            });
        }

        Ok(Statement::Expression(self.parse_expression()?))
    }

    fn parse_expression(&mut self) -> Result<Expression, SyntaxError> {
        let left = self.parse_primary_expression()?;
        let operator = match self.current().kind {
            TokenKind::EqualEqual => BinaryOperator::Equal,
            TokenKind::BangEqual => BinaryOperator::NotEqual,
            TokenKind::Less => BinaryOperator::Less,
            TokenKind::LessEqual => BinaryOperator::LessEqual,
            TokenKind::Greater => BinaryOperator::Greater,
            TokenKind::GreaterEqual => BinaryOperator::GreaterEqual,
            _ => return Ok(left),
        };
        self.advance();
        let right = self.parse_primary_expression()?;
        let span = left.span.join(right.span);
        Ok(Expression {
            kind: ExpressionKind::Binary {
                left: Box::new(left),
                operator,
                right: Box::new(right),
            },
            span,
        })
    }

    fn parse_primary_expression(&mut self) -> Result<Expression, SyntaxError> {
        let token = self.advance();
        let mut expression = match token.kind {
            TokenKind::Within => return self.parse_within(token.span),
            TokenKind::Retry => return self.parse_retry(token.span),
            TokenKind::Parallel => return self.parse_parallel(token.span),
            TokenKind::Rate => return self.parse_rate(token.span),
            TokenKind::Concurrency => return self.parse_concurrency(token.span),
            TokenKind::Null => literal(ExpressionKind::Null, token.span),
            TokenKind::True => literal(ExpressionKind::Boolean(true), token.span),
            TokenKind::False => literal(ExpressionKind::Boolean(false), token.span),
            TokenKind::Integer(value) => literal(ExpressionKind::Integer(value), token.span),
            TokenKind::Float(value) => literal(ExpressionKind::Float(value), token.span),
            TokenKind::DurationNanos(value) => {
                literal(ExpressionKind::DurationNanos(value), token.span)
            }
            TokenKind::String(value) => literal(ExpressionKind::String(value), token.span),
            TokenKind::Identifier(value) => {
                let mut name = value;
                let mut span = token.span;
                while self.at(&TokenKind::Dot)
                    && matches!(
                        self.tokens.get(self.cursor + 1).map(|token| &token.kind),
                        Some(TokenKind::Identifier(_))
                    )
                {
                    self.advance();
                    let member = self.take_identifier("a name after `.`")?;
                    name.push('.');
                    name.push_str(&member.value);
                    span = span.join(member.span);
                }
                literal(ExpressionKind::Name(name), span)
            }
            TokenKind::LeftBracket => self.parse_array(token.span)?,
            TokenKind::LeftBrace => {
                let (fields, end) = self.parse_object_fields_after_open()?;
                literal(ExpressionKind::Object(fields), token.span.join(end))
            }
            _ => return Err(SyntaxError::new("expected an expression", token.span)),
        };

        if self.at(&TokenKind::LeftParen) {
            let ExpressionKind::Name(name) = expression.kind else {
                return Err(SyntaxError::new(
                    "only a named flow or capability operation can be called",
                    expression.span,
                ));
            };
            let callee = Spanned {
                value: name,
                span: expression.span,
            };
            self.advance();
            let mut arguments = Vec::new();
            if !self.at(&TokenKind::RightParen) {
                loop {
                    arguments.push(self.parse_expression()?);
                    if !self.take_if(&TokenKind::Comma) {
                        break;
                    }
                }
            }
            let close = self.take(&TokenKind::RightParen)?.span;
            let (options, end) = if self.at(&TokenKind::LeftBrace) {
                self.parse_object_fields()?
            } else {
                (Vec::new(), close)
            };
            expression = Expression {
                span: callee.span.join(end),
                kind: ExpressionKind::Call {
                    callee,
                    arguments,
                    options,
                },
            };
        }

        self.parse_member_access(expression)
    }

    fn parse_member_access(
        &mut self,
        mut expression: Expression,
    ) -> Result<Expression, SyntaxError> {
        loop {
            let member = if self.take_if(&TokenKind::Dot) {
                self.take_identifier("a member name after `.`")?
            } else if self.take_if(&TokenKind::LeftBracket) {
                let token = self.advance();
                let TokenKind::String(value) = token.kind else {
                    return Err(SyntaxError::new(
                        "object key access requires a string literal",
                        token.span,
                    ));
                };
                let close = self.take(&TokenKind::RightBracket)?.span;
                Spanned {
                    value,
                    span: token.span.join(close),
                }
            } else {
                break;
            };
            let span = expression.span.join(member.span);
            expression = Expression {
                kind: ExpressionKind::Member {
                    value: Box::new(expression),
                    member,
                },
                span,
            };
        }

        Ok(expression)
    }

    fn parse_within(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        self.take(&TokenKind::LeftParen)?;
        self.take_named_option("timeout")?;
        let timeout = self.parse_expression()?;
        self.take(&TokenKind::RightParen)?;
        self.take(&TokenKind::LeftBrace)?;
        let body = self.parse_expression()?;
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(Expression {
            kind: ExpressionKind::Within {
                timeout: Box::new(timeout),
                body: Box::new(body),
            },
            span: start.join(end),
        })
    }

    fn parse_retry(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        self.take(&TokenKind::LeftParen)?;
        self.take_named_option("attempts")?;
        let attempts = self.parse_expression()?;
        let delay = if self.take_if(&TokenKind::Comma) {
            self.take_named_option("delay")?;
            Some(Box::new(self.parse_expression()?))
        } else {
            None
        };
        self.take(&TokenKind::RightParen)?;
        self.take(&TokenKind::LeftBrace)?;
        let body = self.parse_expression()?;
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(Expression {
            kind: ExpressionKind::Retry {
                attempts: Box::new(attempts),
                delay,
                body: Box::new(body),
            },
            span: start.join(end),
        })
    }

    fn parse_parallel(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        self.take(&TokenKind::LeftParen)?;
        let limit = if self.at(&TokenKind::RightParen) {
            None
        } else {
            self.take_named_option("limit")?;
            Some(Box::new(self.parse_expression()?))
        };
        self.take(&TokenKind::RightParen)?;
        self.take(&TokenKind::LeftBrace)?;
        let mut branches = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::End) {
                return Err(self.expected("a parallel branch or `}`"));
            }
            branches.push(self.parse_expression()?);
        }
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(Expression {
            kind: ExpressionKind::Parallel { limit, branches },
            span: start.join(end),
        })
    }

    fn parse_rate(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        self.take(&TokenKind::LeftParen)?;
        self.take_named_option("target")?;
        let target = self.parse_expression()?;
        self.take(&TokenKind::Comma)?;
        self.take_named_option("period")?;
        let period = self.parse_expression()?;
        self.take(&TokenKind::Comma)?;
        self.take_named_option("duration")?;
        let duration = self.parse_expression()?;
        let limit = if self.take_if(&TokenKind::Comma) {
            self.take_named_option("limit")?;
            Some(Box::new(self.parse_expression()?))
        } else {
            None
        };
        self.take(&TokenKind::RightParen)?;
        self.take(&TokenKind::LeftBrace)?;
        let body = self.parse_expression()?;
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(Expression {
            kind: ExpressionKind::Rate {
                target: Box::new(target),
                period: Box::new(period),
                duration: Box::new(duration),
                limit,
                body: Box::new(body),
            },
            span: start.join(end),
        })
    }

    fn parse_concurrency(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        self.take(&TokenKind::LeftParen)?;
        self.take_named_option("limit")?;
        let limit = self.parse_expression()?;
        self.take(&TokenKind::Comma)?;
        self.take_named_option("duration")?;
        let duration = self.parse_expression()?;
        self.take(&TokenKind::RightParen)?;
        self.take(&TokenKind::LeftBrace)?;
        let body = self.parse_expression()?;
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok(Expression {
            kind: ExpressionKind::Concurrency {
                limit: Box::new(limit),
                duration: Box::new(duration),
                body: Box::new(body),
            },
            span: start.join(end),
        })
    }

    fn take_named_option(&mut self, expected: &'static str) -> Result<(), SyntaxError> {
        let name = self.take_identifier("a policy option name")?;
        if name.value != expected {
            return Err(SyntaxError::new(
                format!("expected `{expected}` policy option"),
                name.span,
            ));
        }
        self.take(&TokenKind::Colon)?;
        Ok(())
    }

    fn parse_array(&mut self, start: Span) -> Result<Expression, SyntaxError> {
        let mut values = Vec::new();
        if !self.at(&TokenKind::RightBracket) {
            loop {
                values.push(self.parse_expression()?);
                if !self.take_if(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let end = self.take(&TokenKind::RightBracket)?.span;
        Ok(Expression {
            kind: ExpressionKind::Array(values),
            span: start.join(end),
        })
    }

    fn parse_object_fields(&mut self) -> Result<(Vec<ObjectField>, Span), SyntaxError> {
        self.take(&TokenKind::LeftBrace)?;
        self.parse_object_fields_after_open()
    }

    fn parse_object_fields_after_open(&mut self) -> Result<(Vec<ObjectField>, Span), SyntaxError> {
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::End) {
                return Err(self.expected("an object field or `}`"));
            }
            fields.push(self.parse_object_field()?);
            self.take_if(&TokenKind::Comma);
        }
        let end = self.take(&TokenKind::RightBrace)?.span;
        Ok((fields, end))
    }

    fn parse_object_field(&mut self) -> Result<ObjectField, SyntaxError> {
        let token = self.advance();
        let name = match token.kind {
            TokenKind::Identifier(value) | TokenKind::String(value) => Spanned {
                value,
                span: token.span,
            },
            _ => {
                return Err(SyntaxError::new(
                    "expected an object field name",
                    token.span,
                ));
            }
        };
        self.take(&TokenKind::Colon)?;
        let expression = self.parse_expression()?;
        Ok(ObjectField {
            span: name.span.join(expression.span),
            name,
            expression,
        })
    }

    fn take_identifier(&mut self, expected: &'static str) -> Result<Spanned<String>, SyntaxError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(value) => Ok(Spanned {
                value,
                span: token.span,
            }),
            _ => Err(SyntaxError::new(format!("expected {expected}"), token.span)),
        }
    }

    fn take(&mut self, expected: &TokenKind) -> Result<Token, SyntaxError> {
        if self.at(expected) {
            Ok(self.advance())
        } else {
            Err(self.expected(token_description(expected)))
        }
    }

    fn take_if(&mut self, expected: &TokenKind) -> bool {
        if self.at(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, expected: &TokenKind) -> bool {
        same_variant(&self.current().kind, expected)
    }

    fn peek_at(&self, offset: usize, expected: &TokenKind) -> bool {
        self.tokens
            .get(self.cursor + offset)
            .is_some_and(|token| same_variant(&token.kind, expected))
    }

    fn current(&self) -> &Token {
        &self.tokens[self.cursor]
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.cursor].clone();
        if !matches!(token.kind, TokenKind::End) {
            self.cursor += 1;
        }
        token
    }

    fn expected(&self, expected: &'static str) -> SyntaxError {
        SyntaxError::new(format!("expected {expected}"), self.current().span)
    }
}

fn literal(kind: ExpressionKind, span: Span) -> Expression {
    Expression { kind, span }
}

fn same_variant(left: &TokenKind, right: &TokenKind) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

const fn token_description(token: &TokenKind) -> &'static str {
    match token {
        TokenKind::Mettle => "`flow`",
        TokenKind::Test => "`test`",
        TokenKind::Context => "`context`",
        TokenKind::Namespace => "`namespace`",
        TokenKind::Defaults => "`defaults`",
        TokenKind::Use => "`use`",
        TokenKind::Return => "`return`",
        TokenKind::Assert => "`assert`",
        TokenKind::Within => "`within`",
        TokenKind::Retry => "`retry`",
        TokenKind::Parallel => "`parallel`",
        TokenKind::Rate => "`rate`",
        TokenKind::Concurrency => "`concurrency`",
        TokenKind::True => "`true`",
        TokenKind::False => "`false`",
        TokenKind::Null => "`null`",
        TokenKind::Identifier(_) => "an identifier",
        TokenKind::Integer(_) => "an integer",
        TokenKind::Float(_) => "a number",
        TokenKind::DurationNanos(_) => "a duration",
        TokenKind::String(_) => "a string",
        TokenKind::LeftBrace => "`{`",
        TokenKind::RightBrace => "`}`",
        TokenKind::LeftBracket => "`[`",
        TokenKind::RightBracket => "`]`",
        TokenKind::LeftParen => "`(`",
        TokenKind::RightParen => "`)`",
        TokenKind::Comma => "`,`",
        TokenKind::Colon => "`:`",
        TokenKind::Equal => "`=`",
        TokenKind::EqualEqual => "`==`",
        TokenKind::BangEqual => "`!=`",
        TokenKind::Less => "`<`",
        TokenKind::LessEqual => "`<=`",
        TokenKind::Greater => "`>`",
        TokenKind::GreaterEqual => "`>=`",
        TokenKind::Dot => "`.`",
        TokenKind::End => "the end of the file",
    }
}
