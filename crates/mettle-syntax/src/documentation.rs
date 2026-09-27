//! Non-executing documentation comments and tolerant editor call-site discovery.

use crate::{Program, Span, Token, TokenKind, lex, parse};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Documentation {
    pub description: String,
    pub parameters: Vec<(String, String)>,
    pub returns: String,
    pub warnings: Vec<(Span, String)>,
}

/// Read contiguous `///` lines immediately before a declaration. Blank physical
/// lines or ordinary comments detach the block. Doc tags do not change execution.
#[must_use]
pub fn declaration(source: &str, start: usize, parameters: &[&str]) -> Documentation {
    let Some(prefix) = source.get(..start) else {
        return Documentation::default();
    };
    let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
    if !prefix[line_start..].trim().is_empty() {
        return Documentation::default();
    }
    let mut lines = Vec::new();
    let mut end = line_start;
    for line in source[..line_start].split_inclusive('\n').rev() {
        end -= line.len();
        let trimmed = line.trim_start_matches([' ', '\t']);
        let Some(text) = trimmed.strip_prefix("///") else {
            break;
        };
        let text = text
            .trim_end_matches(['\r', '\n'])
            .strip_prefix(' ')
            .unwrap_or(text.trim_end_matches(['\r', '\n']));
        lines.push((
            Span::new(end, end + line.trim_end_matches(['\r', '\n']).len()),
            text,
        ));
    }
    lines.reverse();
    let mut doc = Documentation::default();
    let mut section: Option<usize> = None;
    let mut returns = false;
    for (span, text) in lines {
        if let Some(rest) = text
            .strip_prefix("@param")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            returns = false;
            let rest = rest.trim_start();
            let (name, description) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if name.is_empty() || !parameters.contains(&name) {
                doc.warnings.push((
                    span,
                    format!("documentation @param `{name}` does not name a declared parameter"),
                ));
                section = None;
            } else if doc.parameters.iter().any(|(existing, _)| existing == name) {
                doc.warnings.push((
                    span,
                    format!("duplicate documentation for parameter `{name}`"),
                ));
                section = None;
            } else {
                doc.parameters
                    .push((name.to_owned(), description.trim_start().to_owned()));
                section = Some(doc.parameters.len() - 1);
            }
        } else if let Some(rest) = text
            .strip_prefix("@returns")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        {
            if returns || !doc.returns.is_empty() {
                doc.warnings
                    .push((span, "duplicate @returns documentation".into()));
            }
            rest.trim_start().clone_into(&mut doc.returns);
            section = None;
            returns = true;
        } else {
            let target = if returns {
                &mut doc.returns
            } else if let Some(index) = section {
                &mut doc.parameters[index].1
            } else {
                &mut doc.description
            };
            if !target.is_empty() {
                target.push('\n');
            }
            target.push_str(text);
        }
    }
    doc.description = doc.description.trim().to_owned();
    doc
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallSite {
    pub name: String,
    pub span: Span,
    pub argument: usize,
    pub named: Option<String>,
    pub path: Vec<String>,
}

struct Frame {
    opener: TokenKind,
    call: Option<CallSite>,
    label: Option<String>,
    pending: Option<String>,
}

fn tokens(source: &str) -> Vec<Token> {
    lex(source).unwrap_or_else(|error| lex(&source[..error.span.start]).unwrap_or_default())
}

fn qualified(tokens: &[Token], end: usize) -> Option<(String, Span)> {
    let TokenKind::Identifier(last) = &tokens.get(end)?.kind else {
        return None;
    };
    let mut name = last.clone();
    let mut span = tokens[end].span;
    let mut index = end;
    while index >= 2 && tokens[index - 1].kind == TokenKind::Dot {
        let TokenKind::Identifier(part) = &tokens[index - 2].kind else {
            break;
        };
        name = format!("{part}.{name}");
        span = tokens[index - 2].span.join(span);
        index -= 2;
    }
    Some((name, span))
}

fn call_target(tokens: &[Token], end: usize) -> Option<(String, Span)> {
    // Match the callable-name parentheses already supported by the parser.
    let mut left = end;
    let mut right = end;
    while tokens.get(right)?.kind == TokenKind::RightParen {
        let mut depth = 1usize;
        let mut opener = right;
        while depth > 0 {
            opener = opener.checked_sub(1)?;
            match tokens[opener].kind {
                TokenKind::RightParen => depth += 1,
                TokenKind::LeftParen => depth -= 1,
                _ => {}
            }
        }
        if left != end && opener != left {
            return None;
        }
        left = opener + 1;
        right = right.checked_sub(1)?;
    }
    let (name, span) = qualified(tokens, right)?;
    if left != end && span.start != tokens[left].span.start {
        return None;
    }
    let start = tokens[..=right]
        .iter()
        .rposition(|token| token.span.start == span.start)?;
    // Declaration parameter lists are not calls.
    if start > 0 && tokens[start - 1].kind == TokenKind::Mettle {
        return None;
    }
    Some((name, span))
}

/// Discover the innermost call without parsing an unfinished body. Strings,
/// comments and nested collections use the language lexer, not editor regexes.
#[must_use]
pub fn call_site(source: &str, byte: usize) -> Option<CallSite> {
    let prefix = source.get(..byte)?;
    let tokens = tokens(prefix);
    let mut frames: Vec<Frame> = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        match token.kind {
            TokenKind::LeftParen | TokenKind::LeftBrace | TokenKind::LeftBracket => {
                let call = if token.kind == TokenKind::LeftParen {
                    index
                        .checked_sub(1)
                        .and_then(|index| call_target(&tokens, index))
                        .map(|(name, span)| CallSite {
                            name,
                            span,
                            argument: 0,
                            named: None,
                            path: Vec::new(),
                        })
                } else {
                    None
                };
                let label = frames.last().and_then(|frame| frame.pending.clone());
                frames.push(Frame {
                    opener: token.kind.clone(),
                    call,
                    label,
                    pending: None,
                });
            }
            TokenKind::RightParen | TokenKind::RightBrace | TokenKind::RightBracket => {
                let opener = match token.kind {
                    TokenKind::RightParen => TokenKind::LeftParen,
                    TokenKind::RightBrace => TokenKind::LeftBrace,
                    _ => TokenKind::LeftBracket,
                };
                if frames.last().is_some_and(|frame| frame.opener == opener) {
                    frames.pop();
                }
            }
            TokenKind::Comma => {
                if let Some(frame) = frames.last_mut() {
                    frame.pending = None;
                    if let Some(call) = &mut frame.call {
                        call.argument += 1;
                        call.named = None;
                    }
                }
            }
            TokenKind::Colon => {
                if let Some(frame) = frames.last_mut() {
                    frame.pending =
                        index
                            .checked_sub(1)
                            .and_then(|index| match &tokens[index].kind {
                                TokenKind::Identifier(name) | TokenKind::String(name) => {
                                    Some(name.clone())
                                }
                                _ => None,
                            });
                    if let Some(call) = &mut frame.call {
                        call.named.clone_from(&frame.pending);
                    }
                }
            }
            _ => {}
        }
    }
    let index = frames.iter().rposition(|frame| frame.call.is_some())?;
    let mut call = frames[index].call.clone()?;
    call.path = frames[index + 1..]
        .iter()
        .filter_map(|frame| frame.label.clone())
        .collect();
    Some(call)
}

/// Qualified identifier under the cursor, excluding strings and comments.
#[must_use]
pub fn symbol(source: &str, byte: usize) -> Option<(String, Span)> {
    let tokens = tokens(source);
    let index = tokens.iter().position(|token| {
        token.span.start <= byte
            && byte < token.span.end
            && matches!(token.kind, TokenKind::Identifier(_))
    })?;
    let mut end = index;
    while tokens
        .get(end + 1)
        .is_some_and(|token| token.kind == TokenKind::Dot)
        && tokens
            .get(end + 2)
            .is_some_and(|token| matches!(token.kind, TokenKind::Identifier(_)))
    {
        end += 2;
    }
    qualified(&tokens, end)
}

/// Best-effort completed declarations for editor documentation only. Execution
/// and diagnostics always parse the full original source and never use recovery.
#[must_use]
pub fn recover(source: &str) -> Program {
    if let Ok(program) = parse(source) {
        return program;
    }
    let mut depth = 0usize;
    let mut starts = Vec::new();
    for token in tokens(source) {
        match token.kind {
            TokenKind::Mettle | TokenKind::Test | TokenKind::Context if depth == 0 => {
                starts.push(token.span.start);
            }
            TokenKind::LeftBrace | TokenKind::LeftParen | TokenKind::LeftBracket => depth += 1,
            TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    starts
        .into_iter()
        .rev()
        .take(32)
        .find_map(|start| parse(&source[..start]).ok())
        .unwrap_or_else(|| Program {
            namespace: None,
            namespace_uses: Vec::new(),
            contexts: Vec::new(),
            file_contexts: Vec::new(),
            flows: Vec::new(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_attach_and_validate_without_changing_execution() {
        let source = "/// Fetch.\r\n/// @param id User identifier.\r\n///   More detail.\r\n/// @param typo Incorrect.\r\n/// @param id Duplicate.\r\n/// @returns A response.\r\nflow get(id) = id";
        let program = parse(source).unwrap();
        let doc = declaration(source, program.flows[0].span.start, &["id"]);
        assert_eq!(doc.description, "Fetch.");
        assert!(doc.parameters[0].1.contains("More detail"));
        assert_eq!(doc.warnings.len(), 2);
        assert_eq!(doc.returns, "A response.");
        assert!(
            declaration("/// Detached.\n\nflow x = 1", 15, &[])
                .description
                .is_empty()
        );
    }
    #[test]
    fn unfinished_calls_ignore_comments_strings_and_nested_commas() {
        let text = "flow helper(id) = id\nflow main { http.post(\"fake(,\", body: { x: [1, 2] }, mediaType: ";
        let call = call_site(text, text.len()).unwrap();
        assert_eq!(call.name, "http.post");
        assert_eq!(call.named.as_deref(), Some("mediaType"));
        assert_eq!(call.argument, 2);
        assert_eq!(recover(text).flows.len(), 1);
        let text = "http.post(\"/\", tls: { verifyCertificates: ";
        assert_eq!(call_site(text, text.len()).unwrap().path, ["tls"]);
        assert!(symbol("\"http.post\"", 4).is_none());
        assert!(symbol("// http.post", 5).is_none());
        for text in ["(http.post)(\"/\", body: ", "((http.post))(\"/\", body: "] {
            let call = call_site(text, text.len()).unwrap();
            assert_eq!(call.name, "http.post");
            assert_eq!(call.named.as_deref(), Some("body"));
        }
        let text = "flow helper(id";
        assert!(call_site(text, text.len()).is_none());
        let text = "(other + http.post)(\"/\", body: ";
        assert!(call_site(text, text.len()).is_none());
    }
}
