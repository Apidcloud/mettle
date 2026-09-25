//! Lexing, parsing, and source locations for the Mettle language.

use std::fmt;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Span {
    pub source: usize,
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self {
            source: 0,
            start,
            end,
        }
    }

    #[must_use]
    pub const fn with_source(self, source: usize) -> Self {
        Self { source, ..self }
    }

    #[must_use]
    pub const fn join(self, other: Self) -> Self {
        Self {
            source: self.source,
            start: self.start,
            end: other.end,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Program {
    pub namespace: Option<Spanned<String>>,
    pub namespace_uses: Vec<Spanned<String>>,
    pub contexts: Vec<ContextDecl>,
    pub file_contexts: Vec<FileContextUse>,
    pub flows: Vec<MettleDecl>,
}

impl Program {
    /// Assign a source-file identity to every span in this parsed program.
    pub fn set_source(&mut self, source: usize) {
        if let Some(namespace) = &mut self.namespace {
            namespace.span = namespace.span.with_source(source);
        }
        for namespace in &mut self.namespace_uses {
            namespace.span = namespace.span.with_source(source);
        }
        for context in &mut self.contexts {
            context.span = context.span.with_source(source);
            if let Some(name) = &mut context.name {
                name.span = name.span.with_source(source);
            }
            for namespace in &mut context.namespace_uses {
                namespace.span = namespace.span.with_source(source);
            }
            for member in &mut context.members {
                match member {
                    ContextMember::UseContext { name, span } => {
                        *span = span.with_source(source);
                        name.span = name.span.with_source(source);
                    }
                    ContextMember::Field(field) => set_field_source(field, source),
                    ContextMember::Defaults {
                        capability,
                        fields,
                        span,
                    } => {
                        *span = span.with_source(source);
                        capability.span = capability.span.with_source(source);
                        for field in fields {
                            set_field_source(field, source);
                        }
                    }
                }
            }
        }
        for context in &mut self.file_contexts {
            match context {
                FileContextUse::Named(name) => name.span = name.span.with_source(source),
                FileContextUse::Inline(span) => *span = span.with_source(source),
            }
        }
        for flow in &mut self.flows {
            flow.span = flow.span.with_source(source);
            if let Some(name) = &mut flow.name {
                name.span = name.span.with_source(source);
            }
            for namespace in &mut flow.namespace_uses {
                namespace.span = namespace.span.with_source(source);
            }
            for parameter in &mut flow.parameters {
                parameter.span = parameter.span.with_source(source);
            }
            for statement in &mut flow.body {
                set_statement_source(statement, source);
            }
        }
    }
}

fn set_statement_source(statement: &mut Statement, source: usize) {
    match statement {
        Statement::If {
            branches,
            else_body,
            span,
        } => {
            *span = span.with_source(source);
            for branch in branches {
                branch.span = branch.span.with_source(source);
                set_expression_source(&mut branch.condition, source);
                for statement in &mut branch.body {
                    set_statement_source(statement, source);
                }
            }
            if let Some(body) = else_body {
                for statement in body {
                    set_statement_source(statement, source);
                }
            }
        }
        Statement::UseContext { name, span } | Statement::Bind { name, span, .. } => {
            name.span = name.span.with_source(source);
            *span = span.with_source(source);
        }
        Statement::Return { expression, span } => {
            *span = span.with_source(source);
            set_expression_source(expression, source);
        }
        Statement::Assert {
            expression,
            message,
            span,
        } => {
            *span = span.with_source(source);
            set_expression_source(expression, source);
            if let Some(message) = message {
                set_expression_source(message, source);
            }
        }
        Statement::Expression(expression) => set_expression_source(expression, source),
    }
    if let Statement::Bind { expression, .. } = statement {
        set_expression_source(expression, source);
    }
}

fn set_field_source(field: &mut ObjectField, source: usize) {
    field.span = field.span.with_source(source);
    field.name.span = field.name.span.with_source(source);
    set_expression_source(&mut field.expression, source);
}

#[allow(clippy::too_many_lines)]
fn set_expression_source(expression: &mut Expression, source: usize) {
    expression.span = expression.span.with_source(source);
    match &mut expression.kind {
        ExpressionKind::Array(values) => {
            for value in values {
                set_expression_source(value, source);
            }
        }
        ExpressionKind::Object(fields) => {
            for field in fields {
                set_field_source(field, source);
            }
        }
        ExpressionKind::Block(statements) => {
            for statement in statements {
                set_statement_source(statement, source);
            }
        }
        ExpressionKind::Fail(message) => set_expression_source(message, source),
        ExpressionKind::For {
            key,
            value,
            iterable,
            body,
        } => {
            if let Some(key) = key {
                key.span = key.span.with_source(source);
            }
            value.span = value.span.with_source(source);
            set_expression_source(iterable, source);
            for statement in body {
                set_statement_source(statement, source);
            }
        }
        ExpressionKind::Call {
            callee,
            arguments,
            named_arguments,
            options,
        } => {
            callee.span = callee.span.with_source(source);
            for argument in arguments {
                set_expression_source(argument, source);
            }
            for field in named_arguments {
                set_field_source(field, source);
            }
            for field in options {
                set_field_source(field, source);
            }
        }
        ExpressionKind::Member { value, member } => {
            set_expression_source(value, source);
            member.span = member.span.with_source(source);
        }
        ExpressionKind::Index { value, index }
        | ExpressionKind::Binary {
            left: value,
            right: index,
            ..
        } => {
            set_expression_source(value, source);
            set_expression_source(index, source);
        }
        ExpressionKind::Not(value) | ExpressionKind::Negate(value) => {
            set_expression_source(value, source);
        }
        ExpressionKind::Within { timeout, body } => {
            set_expression_source(timeout, source);
            set_expression_source(body, source);
        }
        ExpressionKind::Retry {
            attempts,
            delay,
            body,
        } => {
            set_expression_source(attempts, source);
            if let Some(delay) = delay {
                set_expression_source(delay, source);
            }
            set_expression_source(body, source);
        }
        ExpressionKind::Parallel { limit, branches } => {
            if let Some(limit) = limit {
                set_expression_source(limit, source);
            }
            for branch in branches {
                if let Some(name) = &mut branch.name {
                    name.span = name.span.with_source(source);
                }
                set_expression_source(&mut branch.expression, source);
            }
        }
        ExpressionKind::Rate {
            target,
            period,
            duration,
            limit,
            body,
        } => {
            set_expression_source(target, source);
            set_expression_source(period, source);
            set_expression_source(duration, source);
            if let Some(limit) = limit {
                set_expression_source(limit, source);
            }
            set_expression_source(body, source);
        }
        ExpressionKind::Concurrency {
            limit,
            duration,
            body,
        } => {
            set_expression_source(limit, source);
            set_expression_source(duration, source);
            set_expression_source(body, source);
        }
        ExpressionKind::Null
        | ExpressionKind::Boolean(_)
        | ExpressionKind::Integer(_)
        | ExpressionKind::Float(_)
        | ExpressionKind::String(_)
        | ExpressionKind::DurationNanos(_)
        | ExpressionKind::Name(_) => {}
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ContextDecl {
    pub namespace: String,
    pub namespace_uses: Vec<Spanned<String>>,
    pub name: Option<Spanned<String>>,
    pub members: Vec<ContextMember>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FileContextUse {
    Named(Spanned<String>),
    Inline(Span),
}

impl FileContextUse {
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Named(name) => name.span,
            Self::Inline(span) => *span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContextMember {
    UseContext {
        name: Spanned<String>,
        span: Span,
    },
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
pub struct MettleDecl {
    pub kind: DeclarationKind,
    pub namespace: String,
    pub namespace_uses: Vec<Spanned<String>>,
    pub name: Option<Spanned<String>>,
    pub parameters: Vec<Spanned<String>>,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    Flow,
    Test,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    If {
        branches: Vec<IfBranch>,
        else_body: Option<Vec<Statement>>,
        span: Span,
    },
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
    Assert {
        expression: Expression,
        message: Option<Expression>,
        span: Span,
    },
    Expression(Expression),
}

impl Statement {
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::If { span, .. }
            | Self::UseContext { span, .. }
            | Self::Bind { span, .. }
            | Self::Return { span, .. }
            | Self::Assert { span, .. } => *span,
            Self::Expression(expression) => expression.span,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IfBranch {
    pub condition: Expression,
    pub body: Vec<Statement>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParallelBranch {
    pub name: Option<Spanned<String>>,
    pub expression: Expression,
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
    Block(Vec<Statement>),
    Fail(Box<Expression>),
    For {
        key: Option<Spanned<String>>,
        value: Spanned<String>,
        iterable: Box<Expression>,
        body: Vec<Statement>,
    },
    Call {
        callee: Spanned<String>,
        arguments: Vec<Expression>,
        named_arguments: Vec<ObjectField>,
        options: Vec<ObjectField>,
    },
    Member {
        value: Box<Expression>,
        member: Spanned<String>,
    },
    Index {
        value: Box<Expression>,
        index: Box<Expression>,
    },
    Not(Box<Expression>),
    Negate(Box<Expression>),
    Binary {
        left: Box<Expression>,
        operator: BinaryOperator,
        right: Box<Expression>,
    },
    Within {
        timeout: Box<Expression>,
        body: Box<Expression>,
    },
    Retry {
        attempts: Box<Expression>,
        delay: Option<Box<Expression>>,
        body: Box<Expression>,
    },
    Parallel {
        limit: Option<Box<Expression>>,
        branches: Vec<ParallelBranch>,
    },
    Rate {
        target: Box<Expression>,
        period: Box<Expression>,
        duration: Box<Expression>,
        limit: Option<Box<Expression>>,
        body: Box<Expression>,
    },
    Concurrency {
        limit: Box<Expression>,
        duration: Box<Expression>,
        body: Box<Expression>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    And,
    Or,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
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
    If,
    For,
    In,
    Else,
    And,
    Or,
    Not,
    Mettle,
    Test,
    Context,
    Namespace,
    Defaults,
    Use,
    Return,
    Assert,
    Fail,
    Within,
    Retry,
    Parallel,
    Rate,
    Concurrency,
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
    EqualEqual,
    BangEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Dot,
    Minus,
    End,
}

mod parser;
pub use parser::{parse, parse_value};

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
            b'=' if bytes.get(cursor + 1) == Some(&b'=') => {
                push_double_symbol(&mut tokens, &mut cursor, TokenKind::EqualEqual);
            }
            b'=' => push_symbol(&mut tokens, &mut cursor, TokenKind::Equal),
            b'!' if bytes.get(cursor + 1) == Some(&b'=') => {
                push_double_symbol(&mut tokens, &mut cursor, TokenKind::BangEqual);
            }
            b'<' if bytes.get(cursor + 1) == Some(&b'=') => {
                push_double_symbol(&mut tokens, &mut cursor, TokenKind::LessEqual);
            }
            b'<' => push_symbol(&mut tokens, &mut cursor, TokenKind::Less),
            b'>' if bytes.get(cursor + 1) == Some(&b'=') => {
                push_double_symbol(&mut tokens, &mut cursor, TokenKind::GreaterEqual);
            }
            b'>' => push_symbol(&mut tokens, &mut cursor, TokenKind::Greater),
            b'.' => push_symbol(&mut tokens, &mut cursor, TokenKind::Dot),
            b'-' if bytes.get(cursor + 1).is_some_and(u8::is_ascii_digit) => {
                tokens.push(lex_number(source, &mut cursor, true)?);
            }
            b'-' => push_symbol(&mut tokens, &mut cursor, TokenKind::Minus),
            b'"' => tokens.push(lex_string(source, &mut cursor)?),
            byte if byte.is_ascii_digit() => tokens.push(lex_number(source, &mut cursor, false)?),
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

fn push_double_symbol(tokens: &mut Vec<Token>, cursor: &mut usize, kind: TokenKind) {
    let start = *cursor;
    *cursor += 2;
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
        "if" => TokenKind::If,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "else" => TokenKind::Else,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "flow" => TokenKind::Mettle,
        "test" => TokenKind::Test,
        "context" => TokenKind::Context,
        "namespace" => TokenKind::Namespace,
        "defaults" => TokenKind::Defaults,
        "use" => TokenKind::Use,
        "return" => TokenKind::Return,
        "assert" => TokenKind::Assert,
        "fail" => TokenKind::Fail,
        "within" => TokenKind::Within,
        "retry" => TokenKind::Retry,
        "parallel" => TokenKind::Parallel,
        "rate" => TokenKind::Rate,
        "concurrency" => TokenKind::Concurrency,
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

#[allow(clippy::too_many_lines)]
fn lex_number(source: &str, cursor: &mut usize, negative: bool) -> Result<Token, SyntaxError> {
    let bytes = source.as_bytes();
    let start = *cursor;
    if negative {
        *cursor += 1;
    }
    let number_start = *cursor;
    if bytes[number_start] == b'0'
        && let Some(base) = bytes.get(number_start + 1).and_then(|prefix| match prefix {
            b'x' | b'X' => Some(16),
            b'b' | b'B' => Some(2),
            _ => None,
        })
    {
        *cursor += 2;
        let digits_start = *cursor;
        while *cursor < bytes.len() && is_identifier_continue(bytes[*cursor]) {
            *cursor += 1;
        }
        let span = Span::new(start, *cursor);
        let digits = &source[digits_start..*cursor];
        if digits.is_empty() || !valid_digits(digits, base) {
            return Err(SyntaxError::new(
                "invalid hexadecimal or binary literal",
                span,
            ));
        }
        let magnitude = u128::from_str_radix(&digits.replace('_', ""), base).map_err(|_| {
            SyntaxError::new(
                "integer literal is outside the supported 64-bit range",
                span,
            )
        })?;
        let value = signed_integer(magnitude, negative, span)?;
        return Ok(Token {
            kind: TokenKind::Integer(value),
            span,
        });
    }

    while *cursor < bytes.len() && (bytes[*cursor].is_ascii_digit() || bytes[*cursor] == b'_') {
        *cursor += 1;
    }
    let integer_end = *cursor;

    let mut has_fraction = false;
    if bytes.get(*cursor) == Some(&b'.') && bytes.get(*cursor + 1).is_some_and(u8::is_ascii_digit) {
        has_fraction = true;
        *cursor += 1;
        while *cursor < bytes.len() && (bytes[*cursor].is_ascii_digit() || bytes[*cursor] == b'_') {
            *cursor += 1;
        }
    }
    let fraction_end = *cursor;
    let mut has_exponent = false;
    if matches!(bytes.get(*cursor), Some(b'e' | b'E')) {
        has_exponent = true;
        *cursor += 1;
        if matches!(bytes.get(*cursor), Some(b'+' | b'-')) {
            *cursor += 1;
        }
        let exponent_start = *cursor;
        while *cursor < bytes.len() && (bytes[*cursor].is_ascii_digit() || bytes[*cursor] == b'_') {
            *cursor += 1;
        }
        if exponent_start == *cursor {
            return Err(SyntaxError::new(
                "exponent requires digits",
                Span::new(start, *cursor),
            ));
        }
    }

    let number_end = *cursor;
    while *cursor < bytes.len() && is_identifier_continue(bytes[*cursor]) {
        *cursor += 1;
    }
    let suffix = &source[number_end..*cursor];
    let cleaned = source[number_start..number_end].replace('_', "");
    let span = Span::new(start, *cursor);
    if !valid_digits(&source[number_start..integer_end], 10)
        || (has_fraction && !valid_digits(&source[integer_end + 1..fraction_end], 10))
        || (has_exponent
            && !valid_digits(
                source[fraction_end..number_end]
                    .trim_start_matches(['e', 'E'])
                    .trim_start_matches(['+', '-']),
                10,
            ))
    {
        return Err(SyntaxError::new("invalid numeric separator", span));
    }

    if !suffix.is_empty() {
        if negative {
            return Err(SyntaxError::new("durations cannot be negative", span));
        }
        if has_exponent {
            return Err(SyntaxError::new(
                "duration exponents are not supported",
                span,
            ));
        }
        let multiplier: u128 = match suffix {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60 * 1_000_000_000,
            "h" => 60 * 60 * 1_000_000_000,
            _ => return Err(SyntaxError::new("unknown numeric suffix", span)),
        };
        let (integer, fraction) = cleaned.split_once('.').unwrap_or((&cleaned, ""));
        let integer = integer.parse::<u128>().map_err(|_| {
            SyntaxError::new("duration literal is outside the supported range", span)
        })?;
        let whole = integer.checked_mul(multiplier).ok_or_else(|| {
            SyntaxError::new("duration literal is outside the supported range", span)
        })?;
        let fractional = if fraction.is_empty() {
            0
        } else {
            let scale = 10_u128
                .checked_pow(u32::try_from(fraction.len()).unwrap_or(u32::MAX))
                .ok_or_else(|| SyntaxError::new("duration literal is too precise", span))?;
            let digits = fraction
                .parse::<u128>()
                .map_err(|_| SyntaxError::new("duration literal is too precise", span))?;
            let scaled = digits
                .checked_mul(multiplier)
                .ok_or_else(|| SyntaxError::new("duration literal is too precise", span))?;
            if scaled % scale != 0 {
                return Err(SyntaxError::new(
                    "duration is finer than one nanosecond",
                    span,
                ));
            }
            scaled / scale
        };
        let nanos = whole
            .checked_add(fractional)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| {
                SyntaxError::new("duration literal is outside the supported range", span)
            })?;
        return Ok(Token {
            kind: TokenKind::DurationNanos(nanos),
            span,
        });
    }

    let kind = if has_fraction || has_exponent {
        let value = cleaned
            .parse::<f64>()
            .map_err(|_| SyntaxError::new("invalid floating-point literal", span))?;
        if !value.is_finite() {
            return Err(SyntaxError::new(
                "floating-point literal must be finite",
                span,
            ));
        }
        TokenKind::Float(if negative { -value } else { value })
    } else {
        let magnitude = cleaned.parse::<u128>().map_err(|_| {
            SyntaxError::new(
                "integer literal is outside the supported 64-bit range",
                span,
            )
        })?;
        TokenKind::Integer(signed_integer(magnitude, negative, span)?)
    };
    Ok(Token { kind, span })
}

fn signed_integer(magnitude: u128, negative: bool, span: Span) -> Result<i64, SyntaxError> {
    let signed = i128::try_from(magnitude)
        .ok()
        .and_then(|value| {
            if negative {
                value.checked_neg()
            } else {
                Some(value)
            }
        })
        .and_then(|value| i64::try_from(value).ok());
    signed.ok_or_else(|| {
        SyntaxError::new(
            "integer literal is outside the supported 64-bit range",
            span,
        )
    })
}

fn valid_digits(text: &str, base: u32) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.iter().enumerate().all(|(index, byte)| {
            if *byte == b'_' {
                index > 0
                    && index + 1 < bytes.len()
                    && (bytes[index - 1] as char).is_digit(base)
                    && (bytes[index + 1] as char).is_digit(base)
            } else {
                (*byte as char).is_digit(base)
            }
        })
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

#[cfg(test)]
mod tests {
    use super::{
        BinaryOperator, ContextMember, ExpressionKind, FileContextUse, Statement, parse,
        parse_value,
    };

    #[test]
    fn parses_conditionals_boolean_precedence_and_indexes() {
        let program = parse(
            r#"
            flow main() {
                values = [{ name: "Ada" }, { name: "Lin" }]
                if (not false and values[1]["name"] == "Lin" or false) {
                    return values[0].name
                } else if (false) {
                    return "other"
                } else {
                    return "fallback"
                }
            }
        "#,
        )
        .expect("conditional source should parse");
        let Statement::If {
            branches,
            else_body: Some(_),
            ..
        } = &program.flows[0].body[1]
        else {
            panic!("expected conditional");
        };
        assert_eq!(branches.len(), 2);
        assert!(matches!(
            branches[0].condition.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Or,
                ..
            }
        ));
    }

    #[test]
    fn requires_explicit_separators() {
        for source in [
            "flow main() { value = 1 return value }",
            "flow main() = { a: 1 b: 2 }",
            "flow main() = parallel() { first() second() }",
        ] {
            let error = parse(source).expect_err("adjacent items should fail");
            assert!(error.message.contains("separate"), "{}", error.message);
        }
        parse("flow main() = { a: 1, b: 2 }").expect("comma-separated fields should parse");
        parse("flow main() = { a: 1\n b: 2 }").expect("newline-separated fields should parse");
    }

    #[test]
    fn parses_namespaces_context_composition_and_assertions() {
        let program = parse(
            r"
            namespace users
            use namespace core
            context api {
                use context base
            }
            flow main() {
                response = lookup()
                assert(response.status == 200)
                return response
            }
            ",
        )
        .expect("project syntax should parse");

        assert_eq!(program.namespace.as_ref().unwrap().value, "users");
        assert_eq!(program.namespace_uses[0].value, "core");
        assert!(matches!(
            program.contexts[0].members[0],
            ContextMember::UseContext { .. }
        ));
        let Statement::Assert { expression, .. } = &program.flows[0].body[1] else {
            panic!("expected assertion");
        };
        assert!(matches!(
            expression.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::Equal,
                ..
            }
        ));
    }

    #[test]
    fn parses_assertion_messages_and_rejects_non_literals() {
        let program = parse("test(\"checks\") { assert(false, \"expected a response\") }")
            .expect("assertion message should parse");
        let Statement::Assert {
            message: Some(message),
            ..
        } = &program.flows[0].body[0]
        else {
            panic!("expected a message-bearing assertion");
        };
        assert!(matches!(message.kind, ExpressionKind::String(_)));

        let error = parse("test(\"checks\") { assert(false, 123) }")
            .expect_err("non-string assertion message should be rejected");
        assert_eq!(error.message, "assertion message must be a string literal");
    }

    #[test]
    fn parses_fail_as_a_terminal_expression() {
        let program = parse("flow main { if (true) { fail(\"stop\") } else { return 1 } }")
            .expect("terminal failure should parse");
        let Statement::If { branches, .. } = &program.flows[0].body[0] else {
            panic!("expected conditional");
        };
        assert!(matches!(
            &branches[0].body[0],
            Statement::Expression(expression) if matches!(expression.kind, ExpressionKind::Fail(_))
        ));
        parse("flow main = parallel { fail(\"stop\"), 1 }")
            .expect("fail should also be valid in an expression branch");
        assert!(parse("flow main { fail() }").is_err());
        assert!(parse("flow main { fail(\"a\", \"b\") }").is_err());
    }

    #[test]
    fn parses_structured_execution_policies() {
        let program = parse(
            r"
            flow main() {
                return within(timeout: 2s) {
                    retry(attempts: 3, delay: 10ms) {
                        parallel(limit: 2) { first(), second(), third() }
                    }
                }
            }
            ",
        )
        .expect("policies should parse");
        let Statement::Return { expression, .. } = &program.flows[0].body[0] else {
            panic!("expected return");
        };
        let ExpressionKind::Within { body, .. } = &expression.kind else {
            panic!("expected within");
        };
        let ExpressionKind::Block(statements) = &body.kind else {
            panic!("expected value-producing block");
        };
        let Some(Statement::Expression(body)) = statements.last() else {
            panic!("expected final block expression");
        };
        let ExpressionKind::Retry { body, delay, .. } = &body.kind else {
            panic!("expected retry");
        };
        assert!(delay.is_some());
        let ExpressionKind::Block(statements) = &body.kind else {
            panic!("expected value-producing block");
        };
        let Some(Statement::Expression(body)) = statements.last() else {
            panic!("expected final block expression");
        };
        let ExpressionKind::Parallel { limit, branches } = &body.kind else {
            panic!("expected parallel");
        };
        assert!(limit.is_some());
        assert_eq!(branches.len(), 3);
    }

    #[test]
    fn parses_load_execution_policies() {
        let program = parse(
            r"
            flow probe() = true
            flow main() {
                load = rate(target: 100, period: 1s, duration: 10s, limit: 20) {
                    probe()
                }
                return concurrency(limit: 5, duration: 2s) { probe() }
            }
            ",
        )
        .expect("load policies should parse");
        let Statement::Bind { expression, .. } = &program.flows[1].body[0] else {
            panic!("expected rate binding");
        };
        let ExpressionKind::Rate { limit, .. } = &expression.kind else {
            panic!("expected rate");
        };
        assert!(limit.is_some());
        let Statement::Return { expression, .. } = &program.flows[1].body[1] else {
            panic!("expected concurrency return");
        };
        assert!(matches!(
            expression.kind,
            ExpressionKind::Concurrency { .. }
        ));
    }

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
                    json: { name: "Mettle", roles: ["tester"] }
                }
                return response.headers["content-type"]
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
            parse("flow main() { value = 1.25\n return 200ms }").expect("source should parse");
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
    fn parses_signed_radix_exponent_and_fractional_duration_literals() {
        assert_eq!(
            parse_value("-42").unwrap().kind,
            ExpressionKind::Integer(-42)
        );
        assert_eq!(
            parse_value("-0x2A").unwrap().kind,
            ExpressionKind::Integer(-42)
        );
        assert_eq!(
            parse_value("0b1010_0011").unwrap().kind,
            ExpressionKind::Integer(163)
        );
        assert_eq!(
            parse_value("1.25e2").unwrap().kind,
            ExpressionKind::Float(125.0)
        );
        assert_eq!(
            parse_value("1.5s").unwrap().kind,
            ExpressionKind::DurationNanos(1_500_000_000)
        );
        assert_eq!(
            parse_value("0.25ms").unwrap().kind,
            ExpressionKind::DurationNanos(250_000)
        );
        assert_eq!(
            parse_value("-9223372036854775808").unwrap().kind,
            ExpressionKind::Integer(i64::MIN),
        );
        for source in [
            "1__0",
            "1_",
            "0x_FF",
            "0b102",
            "0.1ns",
            "1e999",
            "-1s",
            "0x8000000000000000",
        ] {
            assert!(parse_value(source).is_err(), "{source} should fail");
        }
    }

    #[test]
    fn parses_refined_declarations_calls_and_named_parallel_branches() {
        let program = parse(
            r#"
            flow add(first, second) = second
            flow main {
                response = parallel {
                    users: add(second: 2, first: 1)
                    posts: [1, 2, 3,]
                }
                response
            }
            test "names work without parentheses" { assert(true) }
        "#,
        )
        .expect("refined syntax should parse");
        assert_eq!(program.flows[1].parameters.len(), 0);
        assert_eq!(
            program.flows[2].name.as_ref().unwrap().value,
            "names work without parentheses"
        );
        let Statement::Bind { expression, .. } = &program.flows[1].body[0] else {
            panic!("expected binding")
        };
        let ExpressionKind::Parallel { branches, .. } = &expression.kind else {
            panic!("expected parallel")
        };
        assert_eq!(branches[0].name.as_ref().unwrap().value, "users");
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

        assert!(matches!(
            &program.file_contexts[0],
            FileContextUse::Named(name) if name.value == "api"
        ));
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
