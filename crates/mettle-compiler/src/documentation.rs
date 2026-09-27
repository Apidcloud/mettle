//! Core helper documentation beside compiler-owned intrinsic resolution.

use mettle_syntax::language::Item;

macro_rules! intrinsics {
    ($($kind:ident => ($name:literal, $form:literal, $description:literal, $example:literal, $parameters:expr)),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum Intrinsic { $($kind),* }
        pub const BUILTINS: &[Item] = &[$(Item { name: $name, form: $form, description: $description, example: $example, parameters: $parameters }),*];
        #[must_use]
        pub fn intrinsic(name: &str) -> Option<Intrinsic> {
            match name { $($name => Some(Intrinsic::$kind),)* _ => None }
        }
    };
}

intrinsics! {
    Environment => ("env", "env(name)", "Read a string from the execution environment, including the selected .env/profile files. The name must be a string literal; an absent variable fails. This does not mark the value secret: use senv for credentials. Named arguments and option blocks are not accepted.", "flow main = env(\"DEMO_VALUE\")", &[("name", "String literal naming the environment variable.")]),
    SecretEnvironment => ("senv", "senv(name)", "Read an environment variable and mark its value sensitive, combining env and secret. Derived values retain sensitivity and reports redact them. The name must be a string literal; an absent variable fails. This is redaction metadata, not encryption.", "flow main = senv(\"DEMO_TOKEN\")", &[("name", "String literal naming the secret environment variable.")]),
    Secret => ("secret", "secret(value)", "Mark a value sensitive without changing its underlying kind. Sensitivity follows supported transformations and redacts report/diagnostic output. This is not encryption or a separate string type. The helper accepts one positional argument and no option block.", "flow main = secret(\"demo-token\")", &[("value", "Value to mark sensitive; its underlying kind is preserved.")]),
    Echo => ("echo", "echo(value)", "Record a flow-scoped message, retaining sensitive-value redaction and logical parallel/retry labels. Use it as a standalone statement, not a result expression. Normal human reports display messages; quiet/raw modes suppress their presentation, and machine-readable reports retain captured events. Load capture is bounded and may report omissions.", "flow main { echo(\"Starting\")\n 42 }", &[("value", "Value to record in the execution report, with secrets redacted.")]),
}

#[must_use]
pub fn item(name: &str) -> Option<&'static Item> {
    BUILTINS.iter().find(|item| item.name == name)
}

/// Reference names may use the language namespace to avoid capability collisions.
#[must_use]
pub fn reference(name: &str) -> Option<String> {
    let name = name.strip_prefix("language.").unwrap_or(name);
    if name == "language" {
        let mut output = mettle_syntax::language::reference(name)?;
        output.push_str("\n## Core helpers\n");
        for item in BUILTINS {
            use std::fmt::Write as _;
            let _ = writeln!(output, "\n- `{}` — `{}`", item.name, item.form);
        }
        return Some(output);
    }
    item(name)
        .map(|item| item.reference())
        .or_else(|| mettle_syntax::language::reference(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_helpers_have_documented_compilable_examples() {
        for item in BUILTINS {
            assert!(intrinsic(item.name).is_some());
            let program = mettle_syntax::parse(item.example).unwrap();
            crate::compile(&program).unwrap();
            assert!(!item.parameters.is_empty());
        }
    }
}
