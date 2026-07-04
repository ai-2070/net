//! Pre-flight argument validation against a tool's input schema
//! (`MCP_BRIDGE_PLAN.md` Phase 2, `serve/validation.rs`).
//!
//! The plan requires validating `arguments` against the descriptor's input
//! schema **before** routing, and returning "a crisp, field-naming validation
//! error the model can self-repair from." Validation-failure rate is a core
//! metric — a bad arg should never round-trip to the provider only to fail
//! there.
//!
//! This is a **pragmatic subset** of JSON Schema, not a full validator:
//! top-level object shape, `required` fields, and declared property `type`s
//! (including type unions and nested `object`/`array` element checks are kept
//! shallow). It is deliberately conservative in what it *rejects* — it only
//! flags violations it is certain about, so a schema feature it does not model
//! never produces a false rejection that blocks a valid call. Anything it
//! cannot check is left to the provider.

use serde_json::Value;

/// A validation failure, naming the offending field where one applies so the
/// model can correct that argument specifically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    /// The field the error is about, if it is field-specific.
    pub field: Option<String>,
    /// A human-facing message the model can act on.
    pub message: String,
}

impl ValidationError {
    fn field(name: &str, message: impl Into<String>) -> Self {
        Self {
            field: Some(name.to_string()),
            message: message.into(),
        }
    }

    fn top(message: impl Into<String>) -> Self {
        Self {
            field: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.field {
            Some(field) => write!(f, "invalid argument `{field}`: {}", self.message),
            None => write!(f, "invalid arguments: {}", self.message),
        }
    }
}

/// Validate `args` against `schema`. `Ok(())` means "nothing we can check
/// failed" — not "fully conforms". Only object schemas are enforced (MCP tool
/// inputs are objects); a schema with no object shape is accepted as-is.
pub fn validate_args(args: &Value, schema: &Value) -> Result<(), ValidationError> {
    let Some(schema_obj) = schema.as_object() else {
        // A non-object schema (or `true`) constrains nothing we model.
        return Ok(());
    };

    let declared_type = schema_obj.get("type").and_then(|t| t.as_str());
    let has_object_shape = declared_type == Some("object")
        || schema_obj.contains_key("properties")
        || schema_obj.contains_key("required");
    if !has_object_shape {
        // No object contract to enforce (e.g. an empty `{}` schema, which MCP
        // uses for a no-argument tool).
        return Ok(());
    }

    let args_obj = args
        .as_object()
        .ok_or_else(|| ValidationError::top("arguments must be a JSON object"))?;

    // Required fields must be present.
    if let Some(required) = schema_obj.get("required").and_then(|r| r.as_array()) {
        for r in required {
            if let Some(name) = r.as_str() {
                if !args_obj.contains_key(name) {
                    return Err(ValidationError::field(
                        name,
                        format!("missing required field `{name}`"),
                    ));
                }
            }
        }
    }

    // Declared property types must match for the fields that are present.
    if let Some(props) = schema_obj.get("properties").and_then(|p| p.as_object()) {
        for (name, pschema) in props {
            let Some(value) = args_obj.get(name) else {
                continue; // absent optional field — `required` handled above
            };
            let Some(type_decl) = pschema.get("type") else {
                continue; // untyped property — nothing to check
            };
            if !type_matches(type_decl, value) {
                let want = describe_type_decl(type_decl);
                return Err(ValidationError::field(
                    name,
                    format!(
                        "field `{name}` must be of type {want}, got {}",
                        json_type(value)
                    ),
                ));
            }
        }
    }

    Ok(())
}

/// Does `value` satisfy a JSON Schema `type` declaration? The declaration is
/// either a string (`"string"`) or an array of alternatives
/// (`["string","null"]`); an array passes if any alternative matches. An
/// unrecognised type string is treated as satisfiable (not modelled → not
/// rejected).
fn type_matches(type_decl: &Value, value: &Value) -> bool {
    match type_decl {
        Value::String(t) => json_type_is(t, value),
        Value::Array(alts) => alts.iter().any(|alt| match alt.as_str() {
            Some(t) => json_type_is(t, value),
            None => true,
        }),
        // A malformed `type` (object/number/…) is not something we model.
        _ => true,
    }
}

/// Does `value` match a single JSON Schema primitive type name?
fn json_type_is(type_name: &str, value: &Value) -> bool {
    match type_name {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        "null" => value.is_null(),
        // JSON Schema distinguishes `integer` from `number`: an integer must
        // have no fractional part. `number` accepts any JSON number.
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        // Unrecognised type keyword — do not reject on something we don't model.
        _ => true,
    }
}

/// The JSON type name of a value, for error messages.
fn json_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Render a `type` declaration for an error message.
fn describe_type_decl(type_decl: &Value) -> String {
    match type_decl {
        Value::String(t) => format!("`{t}`"),
        Value::Array(alts) => {
            let names: Vec<String> = alts
                .iter()
                .filter_map(|a| a.as_str())
                .map(|s| format!("`{s}`"))
                .collect();
            names.join(" or ")
        }
        _ => "the declared type".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string" },
                "count": { "type": "integer" },
                "flag": { "type": "boolean" },
                "maybe": { "type": ["string", "null"] }
            },
            "required": ["message"]
        })
    }

    #[test]
    fn accepts_a_conforming_object() {
        let args = json!({ "message": "hi", "count": 3, "flag": true, "maybe": null });
        assert!(validate_args(&args, &schema()).is_ok());
    }

    #[test]
    fn rejects_missing_required_field_naming_it() {
        let err = validate_args(&json!({ "count": 1 }), &schema()).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("message"));
        assert!(err.message.contains("message"));
    }

    #[test]
    fn rejects_wrong_type_naming_the_field() {
        let err = validate_args(&json!({ "message": 5 }), &schema()).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("message"));
        assert!(err.to_string().contains("must be of type `string`"));
    }

    #[test]
    fn integer_rejects_fractional_but_accepts_whole() {
        assert!(validate_args(&json!({ "message": "x", "count": 2 }), &schema()).is_ok());
        let err = validate_args(&json!({ "message": "x", "count": 2.5 }), &schema()).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("count"));
    }

    #[test]
    fn type_union_accepts_any_alternative() {
        assert!(validate_args(&json!({ "message": "x", "maybe": "s" }), &schema()).is_ok());
        assert!(validate_args(&json!({ "message": "x", "maybe": null }), &schema()).is_ok());
        let err = validate_args(&json!({ "message": "x", "maybe": 7 }), &schema()).unwrap_err();
        assert_eq!(err.field.as_deref(), Some("maybe"));
    }

    #[test]
    fn non_object_args_against_object_schema_is_rejected() {
        let err = validate_args(&json!([1, 2, 3]), &schema()).unwrap_err();
        assert_eq!(err.field, None);
        assert!(err.message.contains("must be a JSON object"));
    }

    #[test]
    fn empty_schema_accepts_anything() {
        // MCP uses `{}` for a no-argument tool — must not reject any args.
        assert!(validate_args(&json!({}), &json!({})).is_ok());
        assert!(validate_args(&json!({ "extra": 1 }), &json!({})).is_ok());
        assert!(validate_args(&json!("whatever"), &json!({})).is_ok());
    }

    #[test]
    fn unmodelled_schema_features_do_not_false_reject() {
        // `additionalProperties: false`, `minLength`, nested `$ref` etc. are
        // not modelled — a value that would violate them still passes here
        // (left to the provider), so we never block a valid call over a
        // feature we don't understand.
        let s = json!({
            "type": "object",
            "properties": { "a": { "type": "string", "minLength": 5 } },
            "additionalProperties": false
        });
        assert!(validate_args(&json!({ "a": "x", "b": "extra" }), &s).is_ok());
    }
}
