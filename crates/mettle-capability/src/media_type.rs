//! Concrete representation media types, independent of HTTP header storage.

use std::collections::BTreeMap;

use crate::content::BuiltinCodec;
use crate::{CapabilityError, Span};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaType {
    pub essence: String,
    pub parameters: BTreeMap<String, String>,
}

impl MediaType {
    /// # Errors
    /// Rejects malformed/wildcard media types, duplicate parameters, and excessive metadata.
    pub fn parse(input: &str, span: Span) -> Result<Self, CapabilityError> {
        let invalid = || {
            CapabilityError::new(
                "invalid media type; use a concrete type/subtype and unique parameters",
                span,
            )
        };
        if input.len() > 8192 || !input.is_ascii() {
            return Err(invalid());
        }
        let (essence, rest) = input.split_once(';').unwrap_or((input, ""));
        let essence = essence.trim_matches([' ', '\t']);
        let (top, sub) = essence.split_once('/').ok_or_else(invalid)?;
        if !valid_token(top) || !valid_token(sub) || top.contains('*') || sub.contains('*') {
            return Err(invalid());
        }
        let mut parameters = BTreeMap::new();
        let mut rest = rest.trim_start_matches([' ', '\t']);
        while !rest.is_empty() {
            if parameters.len() >= 32 {
                return Err(invalid());
            }
            let (name, value_start) = rest.split_once('=').ok_or_else(invalid)?;
            // Whitespace around '=' is not part of a media-type parameter.
            if !valid_token(name) {
                return Err(invalid());
            }
            let name = name.to_ascii_lowercase();
            let (mut value, remaining) = if let Some(quoted) = value_start.strip_prefix('"') {
                let mut value = String::new();
                let mut chars = quoted.char_indices();
                let end = loop {
                    let (index, character) = chars.next().ok_or_else(invalid)?;
                    match character {
                        '"' => break index + 1,
                        '\\' => {
                            let (_, escaped) = chars.next().ok_or_else(invalid)?;
                            if escaped.is_ascii_control() && escaped != '\t' {
                                return Err(invalid());
                            }
                            value.push(escaped);
                        }
                        character if character.is_ascii_control() && character != '\t' => {
                            return Err(invalid());
                        }
                        character => value.push(character),
                    }
                };
                (value, &quoted[end..])
            } else {
                let end = value_start
                    .find([';', ' ', '\t'])
                    .unwrap_or(value_start.len());
                let value = &value_start[..end];
                if !valid_token(value) {
                    return Err(invalid());
                }
                (value.to_owned(), &value_start[end..])
            };
            if name == "charset" {
                value.make_ascii_lowercase();
            }
            if parameters.insert(name, value).is_some() {
                return Err(invalid());
            }
            let remaining = remaining.trim_start_matches([' ', '\t']);
            if remaining.is_empty() {
                break;
            }
            rest = remaining
                .strip_prefix(';')
                .ok_or_else(invalid)?
                .trim_start_matches([' ', '\t']);
            if rest.is_empty() {
                return Err(invalid());
            }
        }
        if input.contains(';')
            && input
                .split_once(';')
                .is_some_and(|(_, rest)| rest.trim().is_empty())
        {
            return Err(invalid());
        }
        Ok(Self {
            essence: essence.to_ascii_lowercase(),
            parameters,
        })
    }

    /// Exact built-ins first, then a structured JSON suffix or text representation;
    /// unknown representations remain raw bytes. No dynamic code is loaded.
    #[must_use]
    pub fn codec(&self) -> BuiltinCodec {
        if self.essence == "application/json" || self.essence.ends_with("+json") {
            BuiltinCodec::Json
        } else if self.essence.starts_with("text/") {
            BuiltinCodec::Text
        } else {
            BuiltinCodec::Bytes
        }
    }

    /// A deterministic spelling preserving explicit parameters, without guessing defaults.
    #[must_use]
    pub fn normalized(&self) -> String {
        use std::fmt::Write as _;
        let mut output = self.essence.clone();
        for (name, value) in &self.parameters {
            let _ = write!(output, "; {name}=");
            if valid_token(value) {
                output.push_str(value);
            } else {
                output.push('"');
                for character in value.chars() {
                    if matches!(character, '"' | '\\') {
                        output.push('\\');
                    }
                    output.push(character);
                }
                output.push('"');
            }
        }
        output
    }

    #[must_use]
    pub fn equivalent(&self, other: &Self) -> bool {
        if self.essence != other.essence {
            return false;
        }
        let normalized = |media: &Self| {
            let mut parameters = media.parameters.clone();
            if media.codec() != BuiltinCodec::Bytes {
                parameters
                    .entry("charset".into())
                    .or_insert_with(|| "utf-8".into());
            }
            parameters
        };
        normalized(self) == normalized(other)
    }

    /// # Errors
    /// The initial text/JSON codecs only support UTF-8, not arbitrary charset conversion.
    pub fn validate_encoding(&self, span: Span) -> Result<(), CapabilityError> {
        if self.codec() != BuiltinCodec::Bytes
            && self
                .parameters
                .get("charset")
                .is_some_and(|charset| charset != "utf-8")
        {
            return Err(CapabilityError::new(
                "built-in JSON/text codecs only support charset=utf-8; encode other charsets explicitly as bytes",
                span,
            ));
        }
        Ok(())
    }
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_normalizes_names_quotes_order_and_charset_but_not_arbitrary_values() {
        let span = Span::default();
        let a = MediaType::parse(
            "Application/Vnd.Example+JSON; Charset=\"UTF-8\"; version=One",
            span,
        )
        .unwrap();
        let b = MediaType::parse(
            "application/vnd.example+json;version=One;charset=utf-8",
            span,
        )
        .unwrap();
        assert_eq!(a, b);
        assert_eq!(
            a.normalized(),
            "application/vnd.example+json; charset=utf-8; version=One"
        );
        let quoted = MediaType::parse("text/plain; note=\"one;two\\\"three\"", span).unwrap();
        assert_eq!(
            MediaType::parse(&quoted.normalized(), span).unwrap(),
            quoted
        );
        assert_eq!(a.codec(), BuiltinCodec::Json);
        assert_ne!(
            a,
            MediaType::parse(
                "application/vnd.example+json;version=one;charset=utf-8",
                span
            )
            .unwrap()
        );
        assert_eq!(
            MediaType::parse("text/plain; note=\"one;two\"", span)
                .unwrap()
                .parameters["note"],
            "one;two"
        );
        for bad in [
            "text",
            "text/*",
            "text/plain;",
            "text/plain;x=1;x=2",
            "text/plain; x =1",
            "text/plain; x=\"unfinished",
            "text/plain\r\nInjected: yes",
        ] {
            assert!(MediaType::parse(bad, span).is_err(), "{bad}");
        }
        assert!(
            MediaType::parse("text/plain;charset=iso-8859-1", span)
                .unwrap()
                .validate_encoding(span)
                .is_err()
        );
    }
}
