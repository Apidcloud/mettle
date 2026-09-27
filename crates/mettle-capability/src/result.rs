//! A single declaration owns result-object construction and field metadata.

/// Declare a capability result record and its documented schema together.
///
/// Every field must be supplied when constructing the record. `into_value`
/// generates the language-facing keys from this same declaration, preserving
/// sensitive values without conversion or inspection.
#[macro_export]
macro_rules! result_object {
    ($vis:vis struct $name:ident {
        $( $field:ident => ($key:literal, $kind:expr, $description:literal) ),* $(,)?
    }) => {
        $vis struct $name {
            $( pub $field: $crate::Value, )*
        }
        impl $name {
            pub const FIELDS: &'static [$crate::FieldSchema] = &[
                $( $crate::FieldSchema::new($key, $kind).documented($description), )*
            ];
            pub fn into_value(self) -> $crate::Value {
                $crate::Value::Object(std::collections::BTreeMap::from([
                    $( ($key.to_owned(), self.$field), )*
                ]))
            }
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::{SchemaType, Value};

    crate::result_object! {
        struct Example {
            count => ("count", SchemaType::Integer, "Number of items."),
            token => ("token", SchemaType::String, "Sensitive credential."),
        }
    }

    #[test]
    fn result_keys_metadata_and_sensitive_values_share_one_record() {
        let result = Example {
            count: Value::Integer(3),
            token: Value::String("dummy-token".into()).sensitive(),
        }
        .into_value();
        let fields = result.as_object().unwrap();
        assert_eq!(fields.len(), Example::FIELDS.len());
        for field in Example::FIELDS {
            assert!(fields.contains_key(field.name));
        }
        assert_eq!(Example::FIELDS[0].value_type, SchemaType::Integer);
        assert_eq!(Example::FIELDS[0].description, "Number of items.");
        assert!(fields["token"].is_sensitive());
        assert!(!result.to_string().contains("dummy-token"));
    }
}
