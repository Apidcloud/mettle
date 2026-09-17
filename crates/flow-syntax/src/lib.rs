//! Lexing, parsing, and source locations for the Flow language.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        Self::new(self.start, other.end)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub contexts: Vec<ContextDecl>,
    pub file_contexts: Vec<Spanned<String>>,
    pub flows: Vec<FlowDecl>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextDecl {
    pub name: Spanned<String>,
    pub members: Vec<ContextMember>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContextMember {
    Field(ObjectField),
    Defaults {
        capability: Spanned<String>,
        fields: Vec<ObjectField>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectField {
    pub name: Spanned<String>,
    pub expression: Expression,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowDecl {
    pub name: Option<Spanned<String>>,
    pub parameters: Vec<Spanned<String>>,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    UseContext {
        name: Spanned<String>,
        span: Span,
    },
    Bind {
        name: Spanned<String>,
        expression: Expression,
        span: Span,
    },
    Return {
        expression: Expression,
        span: Span,
    },
    Expression(Expression),
}

impl Statement {
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::UseContext { span, .. } | Self::Bind { span, .. } | Self::Return { span, .. } => {
                *span
            }
            Self::Expression(expression) => expression.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExpressionKind {
    Null,
    Boolean(bool),
    Integer(i64),
    Float(f64),
    String(String),
    DurationNanos(u64),
    Name(String),
    Array(Vec<Expression>),
    Object(Vec<ObjectField>),
    Call {
        callee: Spanned<String>,
        arguments: Vec<Expression>,
        options: Vec<ObjectField>,
    },
    Member {
        value: Box<Expression>,
        member: Spanned<String>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    pub message: String,
    pub span: Span,
}

impl SyntaxError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SyntaxError {}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
    span: Span,
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Flow,
    Context,
    Defaults,
    Use,
    Return,
    True,
    False,
    Null,
    Identifier(String),
    Integer(i64),
    Float(f64),
    DurationNanos(u64),
    String(String),
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    LeftParen,
    RightParen,
    Comma,
    Colon,
    Equal,
    Dot,
    End,
}

/// Parse a complete Flow source file.
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

fn lex(source: &str) -> Result<Vec<Token>, SyntaxError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b' ' | b'\t' | b'\r' | b'\n' => cursor += 1,
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => {
                cursor += 2;
                while cursor < bytes.len() && bytes[cursor] != b'\n' {
                    cursor += 1;
                }
            }
            b'{' => push_symbol(&mut tokens, &mut cursor, TokenKind::LeftBrace),
            b'}' => push_symbol(&mut tokens, &mut cursor, TokenKind::RightBrace),
            b'[' => push_symbol(&mut tokens, &mut cursor, TokenKind::LeftBracket),
            b']' => push_symbol(&mut tokens, &mut cursor, TokenKind::RightBracket),
            b'(' => push_symbol(&mut tokens, &mut cursor, TokenKind::LeftParen),
            b')' => push_symbol(&mut tokens, &mut cursor, TokenKind::RightParen),
            b',' => push_symbol(&mut tokens, &mut cursor, TokenKind::Comma),
            b':' => push_symbol(&mut tokens, &mut cursor, TokenKind::Colon),
            b'=' => push_symbol(&mut tokens, &mut cursor, TokenKind::Equal),
            b'.' => push_symbol(&mut tokens, &mut cursor, TokenKind::Dot),
            b'"' => tokens.push(lex_string(source, &mut cursor)?),
            byte if byte.is_ascii_digit() => tokens.push(lex_number(source, &mut cursor)?),
            byte if is_identifier_start(byte) => {
                tokens.push(lex_identifier(source, &mut cursor));
            }
            _ => {
                let end = next_char_boundary(source, cursor);
                return Err(SyntaxError::new(
                    format!("unexpected character `{}`", &source[cursor..end]),
                    Span::new(cursor, end),
                ));
            }
        }
    }

    tokens.push(Token {
        kind: TokenKind::End,
        span: Span::new(source.len(), source.len()),
    });
    Ok(tokens)
}

fn push_symbol(tokens: &mut Vec<Token>, cursor: &mut usize, kind: TokenKind) {
    let start = *cursor;
    *cursor += 1;
    tokens.push(Token {
        kind,
        span: Span::new(start, *cursor),
    });
}

fn lex_identifier(source: &str, cursor: &mut usize) -> Token {
    let bytes = source.as_bytes();
    let start = *cursor;
    *cursor += 1;
    while *cursor < bytes.len() && is_identifier_continue(bytes[*cursor]) {
        *cursor += 1;
    }
    let text = &source[start..*cursor];
    let kind = match text {
        "flow" => TokenKind::Flow,
        "context" => TokenKind::Context,
        "defaults" => TokenKind::Defaults,
        "use" => TokenKind::Use,
        "return" => TokenKind::Return,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        "null" => TokenKind::Null,
        _ => TokenKind::Identifier(text.to_owned()),
    };
    Token {
        kind,
        span: Span::new(start, *cursor),
    }
}

fn lex_number(source: &str, cursor: &mut usize) -> Result<Token, SyntaxError> {
    let bytes = source.as_bytes();
    let start = *cursor;
    while *cursor < bytes.len() && (bytes[*cursor].is_ascii_digit() || bytes[*cursor] == b'_') {
        *cursor += 1;
    }

    let mut is_float = false;
    if bytes.get(*cursor) == Some(&b'.') && bytes.get(*cursor + 1).is_some_and(u8::is_ascii_digit) {
        is_float = true;
        *cursor += 1;
        while *cursor < bytes.len() && (bytes[*cursor].is_ascii_digit() || bytes[*cursor] == b'_') {
            *cursor += 1;
        }
    }

    let number_end = *cursor;
    while *cursor < bytes.len() && is_identifier_continue(bytes[*cursor]) {
        *cursor += 1;
    }
    let suffix = &source[number_end..*cursor];
    let cleaned = source[start..number_end].replace('_', "");
    let span = Span::new(start, *cursor);

    if !suffix.is_empty() {
        if is_float {
            return Err(SyntaxError::new(
                "fractional duration literals are not available yet",
                span,
            ));
        }
        let amount = cleaned.parse::<u64>().map_err(|_| {
            SyntaxError::new("duration literal is outside the supported range", span)
        })?;
        let multiplier = match suffix {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60 * 1_000_000_000,
            "h" => 60 * 60 * 1_000_000_000,
            _ => return Err(SyntaxError::new("unknown numeric suffix", span)),
        };
        let nanos = amount.checked_mul(multiplier).ok_or_else(|| {
            SyntaxError::new("duration literal is outside the supported range", span)
        })?;
        return Ok(Token {
            kind: TokenKind::DurationNanos(nanos),
            span,
        });
    }

    let kind = if is_float {
        TokenKind::Float(
            cleaned
                .parse::<f64>()
                .map_err(|_| SyntaxError::new("invalid floating-point literal", span))?,
        )
    } else {
        TokenKind::Integer(cleaned.parse::<i64>().map_err(|_| {
            SyntaxError::new(
                "integer literal is outside the supported 64-bit range",
                span,
            )
        })?)
    };
    Ok(Token { kind, span })
}

fn lex_string(source: &str, cursor: &mut usize) -> Result<Token, SyntaxError> {
    let bytes = source.as_bytes();
    let start = *cursor;
    *cursor += 1;
    let mut value = String::new();

    while *cursor < bytes.len() {
        match bytes[*cursor] {
            b'"' => {
                *cursor += 1;
                return Ok(Token {
                    kind: TokenKind::String(value),
                    span: Span::new(start, *cursor),
                });
            }
            b'\\' => {
                *cursor += 1;
                let Some(escaped) = bytes.get(*cursor).copied() else {
                    break;
                };
                let character = match escaped {
                    b'"' => '"',
                    b'\\' => '\\',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    _ => {
                        return Err(SyntaxError::new(
                            "unsupported string escape",
                            Span::new(*cursor - 1, *cursor + 1),
                        ));
                    }
                };
                value.push(character);
                *cursor += 1;
            }
            b'\n' | b'\r' => {
                return Err(SyntaxError::new(
                    "strings cannot contain an unescaped line break",
                    Span::new(start, *cursor),
                ));
            }
            _ => {
                let end = next_char_boundary(source, *cursor);
                value.push_str(&source[*cursor..end]);
                *cursor = end;
            }
        }
    }

    Err(SyntaxError::new(
        "unterminated string",
        Span::new(start, source.len()),
    ))
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit()
}

fn next_char_boundary(source: &str, start: usize) -> usize {
    source[start..]
        .char_indices()
        .nth(1)
        .map_or(source.len(), |(offset, _)| start + offset)
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
        let mut contexts = Vec::new();
        let mut file_contexts = Vec::new();
        let mut flows = Vec::new();
        while !self.at(&TokenKind::End) {
            if self.at(&TokenKind::Context) {
                contexts.push(self.parse_context()?);
            } else if self.at(&TokenKind::Use) {
                self.advance();
                self.take(&TokenKind::Context)?;
                file_contexts.push(self.take_identifier("a context name")?);
            } else if self.at(&TokenKind::Flow) {
                flows.push(self.parse_flow()?);
            } else {
                flows.push(self.parse_anonymous_expression_flow()?);
            }
        }
        Ok(Program {
            contexts,
            file_contexts,
            flows,
        })
    }

    fn parse_context(&mut self) -> Result<ContextDecl, SyntaxError> {
        let start = self.take(&TokenKind::Context)?.span;
        let name = self.take_identifier("a context name")?;
        self.take(&TokenKind::LeftBrace)?;
        let mut members = Vec::new();
        while !self.at(&TokenKind::RightBrace) {
            if self.at(&TokenKind::End) {
                return Err(self.expected("a context member or `}`"));
            }
            if self.at(&TokenKind::Defaults) {
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
            name,
            members,
            span: start.join(end),
        })
    }

    fn parse_flow(&mut self) -> Result<FlowDecl, SyntaxError> {
        let start = self.take(&TokenKind::Flow)?.span;
        if self.at(&TokenKind::LeftBrace) {
            let (body, end) = self.parse_statement_block()?;
            return Ok(FlowDecl {
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
            return Ok(FlowDecl {
                name: Some(name),
                parameters,
                body: vec![Statement::Return { expression, span }],
                span,
            });
        }

        let (body, end) = self.parse_statement_block()?;
        Ok(FlowDecl {
            name: Some(name),
            parameters,
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

    fn parse_anonymous_expression_flow(&mut self) -> Result<FlowDecl, SyntaxError> {
        let expression = self.parse_expression()?;
        if !matches!(expression.kind, ExpressionKind::Call { .. }) {
            return Err(SyntaxError::new(
                "a top-level anonymous flow must be a call expression",
                expression.span,
            ));
        }
        let span = expression.span;
        Ok(FlowDecl {
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
        let token = self.advance();
        let mut expression = match token.kind {
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

        while self.at(&TokenKind::Dot) {
            self.advance();
            let member = self.take_identifier("a member name after `.`")?;
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
        TokenKind::Flow => "`flow`",
        TokenKind::Context => "`context`",
        TokenKind::Defaults => "`defaults`",
        TokenKind::Use => "`use`",
        TokenKind::Return => "`return`",
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
        TokenKind::Dot => "`.`",
        TokenKind::End => "the end of the file",
    }
}

#[cfg(test)]
mod tests {
    use super::{ContextMember, ExpressionKind, Statement, parse, parse_value};

    #[test]
    fn parses_contexts_http_calls_and_structured_values() {
        let program = parse(
            r#"
            context api {
                apiUrl: env("API_URL")
                defaults http {
                    baseUrl: apiUrl
                    timeout: 5s
                    headers: { "Accept": "application/json" }
                }
            }

            flow main() {
                use context api
                response = http.post("/users") {
                    json: { name: "Flow", roles: ["tester"] }
                }
                return response.json.id
            }
            "#,
        )
        .expect("source should parse");

        assert_eq!(program.contexts.len(), 1);
        assert!(matches!(
            program.contexts[0].members[1],
            ContextMember::Defaults { .. }
        ));
        assert_eq!(program.flows.len(), 1);
        assert!(matches!(
            program.flows[0].body[0],
            Statement::UseContext { .. }
        ));
        let Statement::Bind { expression, .. } = &program.flows[0].body[1] else {
            panic!("expected a binding");
        };
        assert!(matches!(expression.kind, ExpressionKind::Call { .. }));
    }

    #[test]
    fn parses_floats_and_duration_units() {
        let program =
            parse("flow main() { value = 1.25 return 200ms }").expect("source should parse");
        let Statement::Bind { expression, .. } = &program.flows[0].body[0] else {
            panic!("expected binding");
        };
        assert_eq!(expression.kind, ExpressionKind::Float(1.25));
        let Statement::Return { expression, .. } = &program.flows[0].body[1] else {
            panic!("expected return");
        };
        assert_eq!(expression.kind, ExpressionKind::DurationNanos(200_000_000));
    }

    #[test]
    fn reports_unterminated_strings_at_the_source() {
        let error = parse("flow main() { return \"missing }").expect_err("source should fail");
        assert_eq!(error.message, "unterminated string");
        assert_eq!(error.span.start, 21);
    }

    #[test]
    fn rejects_unknown_duration_units() {
        let error = parse("flow main() { return 5days }").expect_err("source should fail");
        assert_eq!(error.message, "unknown numeric suffix");
    }

    #[test]
    fn parses_named_expression_and_anonymous_flows() {
        let program = parse(
            r#"
            context api { defaults http { baseUrl: "http://localhost" } }
            use context api
            http.get("/health")
            flow health() = http.get("/health")
            flow { return http.get("/ready") }
            "#,
        )
        .expect("source should parse");

        assert_eq!(program.file_contexts[0].value, "api");
        assert_eq!(program.flows.len(), 3);
        assert!(program.flows[0].name.is_none());
        assert_eq!(
            program.flows[1].name.as_ref().expect("named flow").value,
            "health"
        );
        assert!(program.flows[2].name.is_none());
        assert!(matches!(program.flows[0].body[0], Statement::Return { .. }));
    }

    #[test]
    fn parses_standalone_cli_values() {
        assert_eq!(
            parse_value("[true, 2s]").expect("value should parse").kind,
            ExpressionKind::Array(vec![
                super::Expression {
                    kind: ExpressionKind::Boolean(true),
                    span: super::Span::new(1, 5),
                },
                super::Expression {
                    kind: ExpressionKind::DurationNanos(2_000_000_000),
                    span: super::Span::new(7, 9),
                },
            ])
        );
    }
}
