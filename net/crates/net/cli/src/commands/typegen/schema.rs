//! Shared JSON Schema (draft 2020-12) → intermediate representation used
//! by every language renderer. Parsing happens once here; `ts.rs` /
//! `python.rs` walk the [`Schema`] tree rather than re-parsing the raw
//! `serde_json::Value`, so the supported-construct surface is defined in a
//! single place.
//!
//! Scope is the common subset tool schemas actually use (objects with
//! primitive/array/enum/union properties, local `$ref`s). Constructs
//! outside that subset surface as a [`SchemaError`] naming the construct,
//! rather than silently producing wrong output.

use serde_json::Value;
use std::collections::BTreeSet;

/// Parsed JSON Schema node — the language-neutral shape both renderers
/// consume.
#[derive(Debug, Clone, PartialEq)]
pub enum Schema {
    /// A scalar JSON type.
    Primitive(Primitive),
    /// `type: array` with homogeneous `items`.
    Array(Box<Schema>),
    /// `type: array` with positional `prefixItems` (a tuple).
    Tuple(Vec<Schema>),
    /// `type: object` with `properties` / `required` / `additionalProperties`.
    Object(ObjectSchema),
    /// `enum: [...]` — a closed set of literal values.
    Enum(Vec<Value>),
    /// `const: x` — a single literal value.
    Const(Value),
    /// `oneOf` / `anyOf` — a union of alternatives.
    Union(Vec<Schema>),
    /// `allOf` — an intersection of components.
    Intersection(Vec<Schema>),
    /// Local `$ref` to `#/$defs/<name>` (or `#/definitions/<name>`),
    /// carried as the bare definition name.
    Ref(String),
    /// No type information (`{}` / true schema) — "anything".
    Unknown,
}

/// Scalar JSON types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Primitive {
    String,
    Integer,
    Number,
    Boolean,
    Null,
}

/// An object schema's shape. Property order is preserved from the source
/// schema so generated fields are stable.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectSchema {
    /// `(name, schema)` in source order.
    pub properties: Vec<(String, Schema)>,
    /// Names listed in the schema's `required` array.
    pub required: BTreeSet<String>,
    /// How `additionalProperties` was specified.
    pub additional: Additional,
}

/// `additionalProperties` handling.
#[derive(Debug, Clone, PartialEq)]
pub enum Additional {
    /// `additionalProperties: false` — closed object.
    Denied,
    /// `additionalProperties: true` or absent — open object, untyped extras.
    Allowed,
    /// `additionalProperties: <schema>` — open object, typed extras.
    Typed(Box<Schema>),
}

/// A parse failure naming the unsupported / malformed construct. Carries
/// no tool id — the caller wraps it with the tool context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// The schema string did not parse as JSON.
    #[error("schema is not valid JSON: {0}")]
    NotJson(String),
    /// A construct outside the supported subset (named).
    #[error("unsupported JSON Schema construct: {0}")]
    Unsupported(String),
    /// An external `$ref` (anything not `#/$defs/...` or `#/definitions/...`).
    #[error("external $ref not supported (needs network fetch): {0}")]
    ExternalRef(String),
}

/// A fully-parsed tool schema: the root plus any `$defs` it references,
/// each parsed into a named [`Schema`]. `$defs` preserve source order.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedSchema {
    /// The root schema.
    pub root: Schema,
    /// `(name, schema)` for every `$defs` / `definitions` entry.
    pub defs: Vec<(String, Schema)>,
    /// `description` or `title` on the root, for a doc comment.
    pub doc: Option<String>,
}

/// Parse a JSON Schema string into a [`ParsedSchema`].
pub fn parse(schema_json: &str) -> Result<ParsedSchema, SchemaError> {
    let value: Value =
        serde_json::from_str(schema_json).map_err(|e| SchemaError::NotJson(e.to_string()))?;
    let root = parse_node(&value)?;
    let doc = string_field(&value, "description").or_else(|| string_field(&value, "title"));
    let mut defs = Vec::new();
    for key in ["$defs", "definitions"] {
        if let Some(Value::Object(map)) = value.get(key) {
            for (name, def) in map {
                defs.push((name.clone(), parse_node(def)?));
            }
        }
    }
    Ok(ParsedSchema { root, defs, doc })
}

/// Parse a single schema node (recursively).
pub fn parse_node(value: &Value) -> Result<Schema, SchemaError> {
    // Booleans are valid schemas in 2020-12: `true` = anything, `false` =
    // nothing. Treat both as "unknown" (no useful type to emit).
    let obj = match value {
        Value::Bool(_) => return Ok(Schema::Unknown),
        Value::Object(map) => map,
        other => {
            return Err(SchemaError::Unsupported(format!(
                "schema node must be an object or boolean, got {other}"
            )))
        }
    };

    // Reject constructs we don't translate, with a clear name.
    for unsupported in [
        "not",
        "if",
        "then",
        "else",
        "dependentSchemas",
        "dependentRequired",
        "unevaluatedProperties",
    ] {
        if obj.contains_key(unsupported) {
            return Err(SchemaError::Unsupported(unsupported.to_string()));
        }
    }

    // `$ref` short-circuits everything else.
    if let Some(Value::String(r)) = obj.get("$ref") {
        return parse_ref(r);
    }

    // `const` and `enum` are value-set schemas.
    if let Some(c) = obj.get("const") {
        return Ok(Schema::Const(c.clone()));
    }
    if let Some(Value::Array(values)) = obj.get("enum") {
        return Ok(Schema::Enum(values.clone()));
    }

    // Composition.
    if let Some(Value::Array(branches)) = obj.get("oneOf").or_else(|| obj.get("anyOf")) {
        let parsed = branches.iter().map(parse_node).collect::<Result<_, _>>()?;
        return Ok(Schema::Union(parsed));
    }
    if let Some(Value::Array(parts)) = obj.get("allOf") {
        let parsed = parts.iter().map(parse_node).collect::<Result<_, _>>()?;
        return Ok(Schema::Intersection(parsed));
    }

    // `nullable: true` (OpenAPI dialect) wraps the type in a null union.
    let nullable = matches!(obj.get("nullable"), Some(Value::Bool(true)));
    let base = parse_typed(obj)?;
    if nullable {
        return Ok(union_with_null(base));
    }
    Ok(base)
}

/// Parse a node that carries an explicit (or array-of) `type`.
fn parse_typed(obj: &serde_json::Map<String, Value>) -> Result<Schema, SchemaError> {
    match obj.get("type") {
        // `type: ["string", "null"]` → union of the listed primitives.
        Some(Value::Array(types)) => {
            let mut branches = Vec::with_capacity(types.len());
            for t in types {
                let name = t.as_str().ok_or_else(|| {
                    SchemaError::Unsupported("non-string entry in `type` array".into())
                })?;
                branches.push(Schema::Primitive(primitive(name)?));
            }
            Ok(Schema::Union(branches))
        }
        Some(Value::String(t)) => match t.as_str() {
            "object" => Ok(Schema::Object(parse_object(obj)?)),
            "array" => parse_array(obj),
            scalar => Ok(Schema::Primitive(primitive(scalar)?)),
        },
        // No `type`: infer object when `properties` present, else unknown.
        None if obj.contains_key("properties") => Ok(Schema::Object(parse_object(obj)?)),
        None => Ok(Schema::Unknown),
        Some(other) => Err(SchemaError::Unsupported(format!(
            "`type` must be a string or array of strings, got {other}"
        ))),
    }
}

fn parse_object(obj: &serde_json::Map<String, Value>) -> Result<ObjectSchema, SchemaError> {
    let mut properties = Vec::new();
    if let Some(Value::Object(props)) = obj.get("properties") {
        for (name, prop) in props {
            properties.push((name.clone(), parse_node(prop)?));
        }
    }
    let required = match obj.get("required") {
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => BTreeSet::new(),
    };
    let additional = match obj.get("additionalProperties") {
        None | Some(Value::Bool(true)) => Additional::Allowed,
        Some(Value::Bool(false)) => Additional::Denied,
        Some(schema) => Additional::Typed(Box::new(parse_node(schema)?)),
    };
    Ok(ObjectSchema {
        properties,
        required,
        additional,
    })
}

fn parse_array(obj: &serde_json::Map<String, Value>) -> Result<Schema, SchemaError> {
    // `prefixItems` (2020-12) → tuple.
    if let Some(Value::Array(prefix)) = obj.get("prefixItems") {
        let items = prefix.iter().map(parse_node).collect::<Result<_, _>>()?;
        return Ok(Schema::Tuple(items));
    }
    match obj.get("items") {
        Some(items) => Ok(Schema::Array(Box::new(parse_node(items)?))),
        // Itemless array → array of unknown.
        None => Ok(Schema::Array(Box::new(Schema::Unknown))),
    }
}

/// Resolve a `$ref` to a local definition name, or error for external refs.
fn parse_ref(r: &str) -> Result<Schema, SchemaError> {
    for prefix in ["#/$defs/", "#/definitions/"] {
        if let Some(name) = r.strip_prefix(prefix) {
            return Ok(Schema::Ref(name.to_string()));
        }
    }
    Err(SchemaError::ExternalRef(r.to_string()))
}

fn primitive(name: &str) -> Result<Primitive, SchemaError> {
    Ok(match name {
        "string" => Primitive::String,
        "integer" => Primitive::Integer,
        "number" => Primitive::Number,
        "boolean" => Primitive::Boolean,
        "null" => Primitive::Null,
        other => return Err(SchemaError::Unsupported(format!("type `{other}`"))),
    })
}

/// `T` → `T | null`, flattening if `T` is already a union.
fn union_with_null(schema: Schema) -> Schema {
    match schema {
        Schema::Union(mut branches) => {
            if !branches.contains(&Schema::Primitive(Primitive::Null)) {
                branches.push(Schema::Primitive(Primitive::Null));
            }
            Schema::Union(branches)
        }
        other => Schema::Union(vec![other, Schema::Primitive(Primitive::Null)]),
    }
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(json: &str) -> Schema {
        parse(json).expect("parse").root
    }

    #[test]
    fn primitives_and_arrays() {
        assert_eq!(
            p(r#"{"type":"string"}"#),
            Schema::Primitive(Primitive::String)
        );
        assert_eq!(
            p(r#"{"type":"array","items":{"type":"integer"}}"#),
            Schema::Array(Box::new(Schema::Primitive(Primitive::Integer)))
        );
    }

    #[test]
    fn object_required_and_additional() {
        let s = p(
            r#"{"type":"object","properties":{"a":{"type":"string"},"b":{"type":"integer"}},"required":["a"],"additionalProperties":false}"#,
        );
        let Schema::Object(o) = s else {
            panic!("expected object")
        };
        assert_eq!(o.properties.len(), 2);
        assert_eq!(o.properties[0].0, "a"); // order preserved
        assert!(o.required.contains("a") && !o.required.contains("b"));
        assert_eq!(o.additional, Additional::Denied);
    }

    #[test]
    fn enum_const_union_intersection() {
        assert!(matches!(p(r#"{"enum":["a","b"]}"#), Schema::Enum(v) if v.len() == 2));
        assert!(matches!(p(r#"{"const":"x"}"#), Schema::Const(_)));
        assert!(matches!(
            p(r#"{"oneOf":[{"type":"string"},{"type":"integer"}]}"#),
            Schema::Union(v) if v.len() == 2
        ));
        assert!(matches!(
            p(r#"{"allOf":[{"type":"object"},{"type":"object"}]}"#),
            Schema::Intersection(v) if v.len() == 2
        ));
    }

    #[test]
    fn nullable_and_type_array() {
        // OpenAPI nullable.
        assert_eq!(
            p(r#"{"type":"string","nullable":true}"#),
            Schema::Union(vec![
                Schema::Primitive(Primitive::String),
                Schema::Primitive(Primitive::Null)
            ])
        );
        // type array form.
        assert_eq!(
            p(r#"{"type":["string","null"]}"#),
            Schema::Union(vec![
                Schema::Primitive(Primitive::String),
                Schema::Primitive(Primitive::Null)
            ])
        );
    }

    #[test]
    fn local_ref_and_defs() {
        let parsed = parse(
            r##"{"type":"object","properties":{"child":{"$ref":"#/$defs/Child"}},"$defs":{"Child":{"type":"object","properties":{"x":{"type":"number"}}}}}"##,
        )
        .expect("parse");
        let Schema::Object(o) = &parsed.root else {
            panic!()
        };
        assert_eq!(o.properties[0].1, Schema::Ref("Child".into()));
        assert_eq!(parsed.defs.len(), 1);
        assert_eq!(parsed.defs[0].0, "Child");
    }

    #[test]
    fn unsupported_constructs_error() {
        assert!(matches!(
            parse(r#"{"not":{"type":"string"}}"#),
            Err(SchemaError::Unsupported(_))
        ));
        assert!(matches!(
            parse(r#"{"$ref":"https://example.com/schema.json"}"#),
            Err(SchemaError::ExternalRef(_))
        ));
        assert!(matches!(parse("not json"), Err(SchemaError::NotJson(_))));
    }

    #[test]
    fn empty_schema_is_unknown() {
        assert_eq!(p("{}"), Schema::Unknown);
        assert_eq!(p("true"), Schema::Unknown);
    }
}
