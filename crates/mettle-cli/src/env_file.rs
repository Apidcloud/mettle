//! File-scoped dotenv loading for CLI executions.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;

pub fn load_environment(
    entry: &Path,
    project_root: Option<&Path>,
    profile: Option<&str>,
) -> Result<Arc<HashMap<String, String>>, String> {
    let entry_directory = if entry == Path::new("<stdin>") {
        env::current_dir().map_err(|error| format!("could not find current directory: {error}"))?
    } else {
        entry
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", entry.display()))?
            .to_path_buf()
    };
    let mut directories = Vec::new();
    if let Some(root) = project_root {
        directories.push(root.to_path_buf());
    }
    if !directories.contains(&entry_directory) {
        directories.push(entry_directory);
    }

    let mut values = HashMap::new();
    for directory in &directories {
        load_optional_file(&directory.join(".env"), &mut values)?;
    }
    if let Some(profile) = profile {
        let mut found = false;
        for directory in &directories {
            found |= load_optional_file(&directory.join(format!(".env.{profile}")), &mut values)?;
        }
        if !found {
            return Err(format!(
                "profile `{profile}` was selected, but no `.env.{profile}` exists beside the entry file or at the project root"
            ));
        }
    }

    for (name, value) in env::vars_os() {
        if let (Ok(name), Ok(value)) = (name.into_string(), value.into_string()) {
            values.insert(name, value);
        }
    }
    Ok(Arc::new(values))
}

fn load_optional_file(path: &Path, values: &mut HashMap<String, String>) -> Result<bool, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    parse_file(path, &contents, values)?;
    Ok(true)
}

fn parse_file(
    path: &Path,
    contents: &str,
    values: &mut HashMap<String, String>,
) -> Result<(), String> {
    let mut declared = HashMap::new();
    for (index, line) in contents.trim_start_matches('\u{feff}').lines().enumerate() {
        let Some((name, value)) = parse_line(line)
            .map_err(|message| format!("{}:{}: {message}", path.display(), index + 1))?
        else {
            continue;
        };
        if let Some(first_line) = declared.insert(name.clone(), index + 1) {
            return Err(format!(
                "{}:{}: `{name}` was already defined on line {first_line}",
                path.display(),
                index + 1
            ));
        }
        values.insert(name, value);
    }
    Ok(())
}

fn parse_line(line: &str) -> Result<Option<(String, String)>, &'static str> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (name, raw_value) = line.split_once('=').ok_or("expected `NAME=value`")?;
    let name = name.trim();
    if !valid_name(name) {
        return Err(
            "environment variable name must use letters, digits, or `_` and cannot start with a digit",
        );
    }
    let raw_value = raw_value.trim_start();
    let value = if let Some(rest) = raw_value.strip_prefix('"') {
        parse_quoted(rest, '"')?
    } else if let Some(rest) = raw_value.strip_prefix('\'') {
        parse_quoted(rest, '\'')?
    } else {
        let comment = raw_value.char_indices().find_map(|(index, character)| {
            (character == '#'
                && (index == 0
                    || raw_value[..index]
                        .chars()
                        .last()
                        .is_some_and(char::is_whitespace)))
            .then_some(index)
        });
        raw_value[..comment.unwrap_or(raw_value.len())]
            .trim_end()
            .to_owned()
    };
    Ok(Some((name.to_owned(), value)))
}

fn parse_quoted(value: &str, quote: char) -> Result<String, &'static str> {
    let mut output = String::new();
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == quote {
            let trailing = characters.as_str().trim_start();
            if trailing.is_empty() || trailing.starts_with('#') {
                return Ok(output);
            }
            return Err("unexpected text after quoted value");
        }
        if quote == '"' && character == '\\' {
            let next = characters
                .next()
                .ok_or("unterminated escape in quoted value")?;
            match next {
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                other => {
                    output.push('\\');
                    output.push(other);
                }
            }
        } else {
            output.push(character);
        }
    }
    Err("unterminated quoted value")
}

fn valid_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::{parse_file, parse_line};

    #[test]
    fn parses_common_dotenv_forms_without_shell_expansion() {
        let mut values = HashMap::new();
        parse_file(
            Path::new(".env"),
            "# comment\nexport URL=https://example.test/path#fragment\nTOKEN='literal ${NAME}'\nMESSAGE=\"hello\\nworld\" # note\nEMPTY=\n",
            &mut values,
        )
        .expect("dotenv file should parse");
        assert_eq!(values["URL"], "https://example.test/path#fragment");
        assert_eq!(values["TOKEN"], "literal ${NAME}");
        assert_eq!(values["MESSAGE"], "hello\nworld");
        assert_eq!(values["EMPTY"], "");
    }

    #[test]
    fn rejects_malformed_lines_without_exposing_values() {
        assert!(parse_line("1BAD=secret").is_err());
        assert!(parse_line("TOKEN=\"unterminated").is_err());
        let mut values = HashMap::new();
        let error = parse_file(
            Path::new(".env"),
            "TOKEN=first\nTOKEN=second\n",
            &mut values,
        )
        .expect_err("duplicate should fail");
        assert!(error.contains(".env:2"));
        assert!(!error.contains("first"));
        assert!(!error.contains("second"));
    }
}
