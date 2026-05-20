//! Phase 4D: Node APIs & Schemas (API-SCHEMA)
//!
//! This module provides runtime-discoverable API definitions for nodes:
//! - Structured API endpoint definitions
//! - JSON Schema-based type validation
//! - API versioning and compatibility checking
//! - API registry with discovery and matching

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use super::metadata::NodeId;

/// HTTP-like method types for API endpoints
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ApiMethod {
    /// Query/read operation
    Get,
    /// Create operation
    Post,
    /// Full update operation
    Put,
    /// Partial update operation
    Patch,
    /// Delete operation
    Delete,
    /// Streaming request
    Stream,
    /// Bidirectional streaming
    BiStream,
    /// Subscribe to events
    Subscribe,
    /// One-way notification
    Notify,
}

impl ApiMethod {
    /// Whether this method is idempotent
    pub fn is_idempotent(&self) -> bool {
        matches!(self, ApiMethod::Get | ApiMethod::Put | ApiMethod::Delete)
    }

    /// Whether this method involves streaming
    pub fn is_streaming(&self) -> bool {
        matches!(
            self,
            ApiMethod::Stream | ApiMethod::BiStream | ApiMethod::Subscribe
        )
    }

    /// Whether this method is safe (no side effects)
    pub fn is_safe(&self) -> bool {
        matches!(self, ApiMethod::Get | ApiMethod::Subscribe)
    }
}

/// Maximum recursion depth permitted by [`SchemaType::validate`].
///
/// `SchemaType` is `#[derive(Deserialize)]` and contains
/// recursive variants (`Array { items: Box<SchemaType> }`,
/// `Object { properties: HashMap<_, SchemaType> }`,
/// `AnyOf { schemas: Vec<SchemaType> }`). An attacker who can
/// ship a schema (announcements broadcast over the mesh, or any
/// caller that parses untrusted JSON into `SchemaType`) could
/// otherwise submit a deeply-nested schema and crash the
/// validator (and the whole process) via stack overflow on an
/// unbounded recursive `validate`. 128 is generous for realistic
/// schemas (typical JSON Schemas rarely exceed depth 10) and well
/// clear of the typical default 8 MB Linux stack.
pub const MAX_SCHEMA_DEPTH: usize = 128;

/// Scan the byte stream of a JSON document and reject if
/// nesting depth (the deepest stack of `{` and `[` after
/// balancing) exceeds `max_depth`.
///
/// This is the deserialize-side defence for [`SchemaType`]: an
/// adversarial schema with thousands of nested `{"type":"array",
/// "items":...}` levels would otherwise either trip `serde_json`'s
/// internal limit (currently 128 by default but tied to a
/// transitive dependency) or stack-overflow the typed
/// deserialize. Pre-scanning the bytes has a single linear-time
/// cost regardless of which deserialize path follows.
///
/// String literals are handled correctly: bracket characters
/// inside a `"..."` string don't change depth, and escapes
/// (`\"`, `\\`) are skipped so a `}` inside a string can't fool
/// the counter.
///
/// Returns `Err(serde_json::Error)` with a `Custom` kind so
/// callers can match on `*::is_data` / `*::is_eof` etc. uniformly
/// with the standard `serde_json::from_slice` error surface.
fn check_json_nesting_depth(data: &[u8], max_depth: usize) -> Result<(), serde_json::Error> {
    use serde::de::Error;
    let mut depth: usize = 0;
    let mut max_seen: usize = 0;
    let mut i = 0;
    let n = data.len();
    while i < n {
        let b = data[i];
        match b {
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > max_seen {
                    max_seen = depth;
                }
                if depth > max_depth {
                    return Err(serde_json::Error::custom(format!(
                        "max nesting depth exceeded ({} > {})",
                        depth, max_depth
                    )));
                }
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'"' => {
                // Skip the rest of the string. Honor `\"` (don't
                // exit) and `\\` (don't treat the following char
                // as an escape). Anything else inside the string
                // is opaque to the depth counter.
                i += 1;
                while i < n {
                    match data[i] {
                        b'\\' if i + 1 < n => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// JSON Schema type definitions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum SchemaType {
    /// Null type
    Null,
    /// Boolean type
    Boolean,
    /// Integer type
    Integer {
        /// Inclusive minimum value
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum: Option<i64>,
        /// Inclusive maximum value
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum: Option<i64>,
        /// Value must be a multiple of this
        #[serde(skip_serializing_if = "Option::is_none")]
        multiple_of: Option<i64>,
    },
    /// Number (float) type
    Number {
        /// Inclusive minimum value
        #[serde(skip_serializing_if = "Option::is_none")]
        minimum: Option<f64>,
        /// Inclusive maximum value
        #[serde(skip_serializing_if = "Option::is_none")]
        maximum: Option<f64>,
    },
    /// String type
    String {
        /// Minimum string length in characters
        #[serde(skip_serializing_if = "Option::is_none")]
        min_length: Option<usize>,
        /// Maximum string length in characters
        #[serde(skip_serializing_if = "Option::is_none")]
        max_length: Option<usize>,
        /// Regular expression pattern the string must match
        #[serde(skip_serializing_if = "Option::is_none")]
        pattern: Option<String>,
        /// Semantic format of the string value
        #[serde(skip_serializing_if = "Option::is_none")]
        format: Option<StringFormat>,
    },
    /// Array type
    Array {
        /// Schema for each array element
        items: Box<SchemaType>,
        /// Minimum number of items
        #[serde(skip_serializing_if = "Option::is_none")]
        min_items: Option<usize>,
        /// Maximum number of items
        #[serde(skip_serializing_if = "Option::is_none")]
        max_items: Option<usize>,
        /// Whether all items must be unique
        #[serde(default)]
        unique_items: bool,
    },
    /// Object type
    Object {
        /// Named property schemas
        properties: HashMap<String, SchemaType>,
        /// Property names that must be present
        #[serde(default)]
        required: Vec<String>,
        /// Whether properties not listed in `properties` are allowed
        #[serde(default)]
        additional_properties: bool,
    },
    /// Enum type (one of specific values)
    Enum {
        /// Allowed JSON values for this enum
        values: Vec<serde_json::Value>,
    },
    /// Union type (anyOf)
    AnyOf {
        /// Candidate schemas, at least one of which must validate
        schemas: Vec<SchemaType>,
    },
    /// Reference to another schema
    Ref {
        /// Name or path of the referenced schema
        schema_ref: String,
    },
    /// Any type (no validation)
    Any,
}

/// String format specifiers
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StringFormat {
    /// Date-time format (ISO 8601)
    DateTime,
    /// Date format
    Date,
    /// Time format
    Time,
    /// Duration format
    Duration,
    /// Email address
    Email,
    /// URI format
    Uri,
    /// UUID format
    Uuid,
    /// IPv4 address
    Ipv4,
    /// IPv6 address
    Ipv6,
    /// Base64 encoded binary
    Base64,
    /// Hexadecimal string
    Hex,
    /// JSON string
    Json,
    /// Markdown text
    Markdown,
}

impl SchemaType {
    /// Deserialize a `SchemaType` from JSON bytes with an explicit
    /// nesting-depth cap.
    ///
    /// Callers that deserialize peer-supplied / untrusted JSON
    /// into `SchemaType` MUST use this entry point. The
    /// derive-`Deserialize` path inherits `serde_json`'s built-in
    /// 128-frame recursion limit, but that's tied to a transitive
    /// dependency and may shift across versions; we pin a local
    /// cap matching [`MAX_SCHEMA_DEPTH`] by **pre-scanning** the
    /// input bytes for max nesting depth (cheap O(n) walk over
    /// `{`/`[`/`}`/`]` outside of strings) and rejecting before
    /// any deserialize work runs. This also guards against
    /// `serde_json::from_slice::<SchemaType>(...)` callsites that
    /// might bypass [`Self::validate`]'s post-parse cap entirely.
    ///
    /// Returns:
    /// - `Err(serde_json::Error)` with kind `Custom("max nesting
    ///   depth exceeded")` if depth > [`MAX_SCHEMA_DEPTH`].
    /// - The standard `serde_json::Error` variants from the
    ///   downstream `from_slice` call otherwise.
    pub fn try_from_slice(data: &[u8]) -> Result<Self, serde_json::Error> {
        check_json_nesting_depth(data, MAX_SCHEMA_DEPTH)?;
        serde_json::from_slice(data)
    }

    /// Deserialize a `SchemaType` from a JSON string with an
    /// explicit nesting-depth cap. See [`Self::try_from_slice`].
    pub fn try_from_str(s: &str) -> Result<Self, serde_json::Error> {
        Self::try_from_slice(s.as_bytes())
    }

    /// Create a string schema
    pub fn string() -> Self {
        SchemaType::String {
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
        }
    }

    /// Create an integer schema
    pub fn integer() -> Self {
        SchemaType::Integer {
            minimum: None,
            maximum: None,
            multiple_of: None,
        }
    }

    /// Create a number schema
    pub fn number() -> Self {
        SchemaType::Number {
            minimum: None,
            maximum: None,
        }
    }

    /// Create a boolean schema
    pub fn boolean() -> Self {
        SchemaType::Boolean
    }

    /// Create an array schema
    pub fn array(items: SchemaType) -> Self {
        SchemaType::Array {
            items: Box::new(items),
            min_items: None,
            max_items: None,
            unique_items: false,
        }
    }

    /// Create an object schema
    pub fn object() -> Self {
        SchemaType::Object {
            properties: HashMap::new(),
            required: Vec::new(),
            additional_properties: true,
        }
    }

    /// Add a property to an object schema
    pub fn with_property(mut self, name: impl Into<String>, schema: SchemaType) -> Self {
        if let SchemaType::Object {
            ref mut properties, ..
        } = self
        {
            properties.insert(name.into(), schema);
        }
        self
    }

    /// Mark a property as required
    pub fn with_required(mut self, name: impl Into<String>) -> Self {
        if let SchemaType::Object {
            ref mut required, ..
        } = self
        {
            required.push(name.into());
        }
        self
    }

    /// Set minimum for integer
    pub fn with_minimum(mut self, min: i64) -> Self {
        if let SchemaType::Integer {
            ref mut minimum, ..
        } = self
        {
            *minimum = Some(min);
        }
        self
    }

    /// Set maximum for integer
    pub fn with_maximum(mut self, max: i64) -> Self {
        if let SchemaType::Integer {
            ref mut maximum, ..
        } = self
        {
            *maximum = Some(max);
        }
        self
    }

    /// Set max length for string
    pub fn with_max_length(mut self, len: usize) -> Self {
        if let SchemaType::String {
            ref mut max_length, ..
        } = self
        {
            *max_length = Some(len);
        }
        self
    }

    /// Set format for string
    pub fn with_format(mut self, fmt: StringFormat) -> Self {
        if let SchemaType::String { ref mut format, .. } = self {
            *format = Some(fmt);
        }
        self
    }

    /// Validate a JSON value against this schema.
    ///
    /// The recursion is bounded by [`MAX_SCHEMA_DEPTH`]; exceeding
    /// it returns [`ValidationError::RecursionLimitExceeded`]
    /// instead of blowing the stack. Recursive variants (`Array`,
    /// `Object`, `AnyOf`) call `validate` recursively, so an
    /// attacker who could ship a `SchemaType` (announcements
    /// broadcast over the mesh, or any caller that parses
    /// untrusted JSON into `SchemaType`) could otherwise submit a
    /// deeply-nested schema and crash the validator — and the
    /// whole process — via stack overflow when a request got
    /// validated against it.
    pub fn validate(&self, value: &serde_json::Value) -> Result<(), ValidationError> {
        self.validate_with_depth(value, 0)
    }

    /// Internal depth-bounded validate — see [`validate`].
    fn validate_with_depth(
        &self,
        value: &serde_json::Value,
        depth: usize,
    ) -> Result<(), ValidationError> {
        if depth >= MAX_SCHEMA_DEPTH {
            return Err(ValidationError::RecursionLimitExceeded {
                limit: MAX_SCHEMA_DEPTH,
            });
        }
        match (self, value) {
            (SchemaType::Null, serde_json::Value::Null) => Ok(()),
            (SchemaType::Null, _) => Err(ValidationError::TypeMismatch {
                expected: "null".into(),
                got: value_type_name(value),
            }),

            (SchemaType::Boolean, serde_json::Value::Bool(_)) => Ok(()),
            (SchemaType::Boolean, _) => Err(ValidationError::TypeMismatch {
                expected: "boolean".into(),
                got: value_type_name(value),
            }),

            (
                SchemaType::Integer {
                    minimum,
                    maximum,
                    multiple_of,
                },
                serde_json::Value::Number(n),
            ) => {
                let i = n.as_i64().ok_or_else(|| ValidationError::TypeMismatch {
                    expected: "integer".into(),
                    got: "float".into(),
                })?;

                if let Some(min) = minimum {
                    if i < *min {
                        return Err(ValidationError::RangeError {
                            value: i as f64,
                            min: Some(*min as f64),
                            max: None,
                        });
                    }
                }
                if let Some(max) = maximum {
                    if i > *max {
                        return Err(ValidationError::RangeError {
                            value: i as f64,
                            min: None,
                            max: Some(*max as f64),
                        });
                    }
                }
                if let Some(mult) = multiple_of {
                    if i % mult != 0 {
                        return Err(ValidationError::MultipleOfError {
                            value: i,
                            multiple_of: *mult,
                        });
                    }
                }
                Ok(())
            }
            (SchemaType::Integer { .. }, _) => Err(ValidationError::TypeMismatch {
                expected: "integer".into(),
                got: value_type_name(value),
            }),

            (SchemaType::Number { minimum, maximum }, serde_json::Value::Number(n)) => {
                let f = n.as_f64().unwrap_or(0.0);

                if let Some(min) = minimum {
                    if f < *min {
                        return Err(ValidationError::RangeError {
                            value: f,
                            min: Some(*min),
                            max: None,
                        });
                    }
                }
                if let Some(max) = maximum {
                    if f > *max {
                        return Err(ValidationError::RangeError {
                            value: f,
                            min: None,
                            max: Some(*max),
                        });
                    }
                }
                Ok(())
            }
            (SchemaType::Number { .. }, _) => Err(ValidationError::TypeMismatch {
                expected: "number".into(),
                got: value_type_name(value),
            }),

            (
                SchemaType::String {
                    min_length,
                    max_length,
                    pattern,
                    format: _,
                },
                serde_json::Value::String(s),
            ) => {
                if let Some(min) = min_length {
                    if s.len() < *min {
                        return Err(ValidationError::LengthError {
                            length: s.len(),
                            min: Some(*min),
                            max: None,
                        });
                    }
                }
                if let Some(max) = max_length {
                    if s.len() > *max {
                        return Err(ValidationError::LengthError {
                            length: s.len(),
                            min: None,
                            max: Some(*max),
                        });
                    }
                }
                if let Some(pat) = pattern {
                    // Simple pattern check - in production would use regex
                    if !s.contains(pat.as_str()) {
                        return Err(ValidationError::PatternMismatch {
                            value: s.clone(),
                            pattern: pat.clone(),
                        });
                    }
                }
                // Format validation would go here
                Ok(())
            }
            (SchemaType::String { .. }, _) => Err(ValidationError::TypeMismatch {
                expected: "string".into(),
                got: value_type_name(value),
            }),

            (
                SchemaType::Array {
                    items,
                    min_items,
                    max_items,
                    unique_items,
                },
                serde_json::Value::Array(arr),
            ) => {
                if let Some(min) = min_items {
                    if arr.len() < *min {
                        return Err(ValidationError::LengthError {
                            length: arr.len(),
                            min: Some(*min),
                            max: None,
                        });
                    }
                }
                if let Some(max) = max_items {
                    if arr.len() > *max {
                        return Err(ValidationError::LengthError {
                            length: arr.len(),
                            min: None,
                            max: Some(*max),
                        });
                    }
                }
                if *unique_items {
                    let mut seen = HashSet::new();
                    for v in arr {
                        let s = serde_json::to_string(v).unwrap_or_default();
                        if !seen.insert(s) {
                            return Err(ValidationError::DuplicateItems);
                        }
                    }
                }
                for (i, v) in arr.iter().enumerate() {
                    if let Err(e) = items.validate_with_depth(v, depth + 1) {
                        // Surface the recursion-limit signal
                        // unwrapped — wrapping it in
                        // `ArrayItemError` would obscure the
                        // anti-DoS check from callers walking
                        // the error chain.
                        if matches!(e, ValidationError::RecursionLimitExceeded { .. }) {
                            return Err(e);
                        }
                        return Err(ValidationError::ArrayItemError {
                            index: i,
                            error: Box::new(e),
                        });
                    }
                }
                Ok(())
            }
            (SchemaType::Array { .. }, _) => Err(ValidationError::TypeMismatch {
                expected: "array".into(),
                got: value_type_name(value),
            }),

            (
                SchemaType::Object {
                    properties,
                    required,
                    additional_properties,
                },
                serde_json::Value::Object(obj),
            ) => {
                // Check required fields
                for req in required {
                    if !obj.contains_key(req) {
                        return Err(ValidationError::MissingRequired { field: req.clone() });
                    }
                }

                // Validate properties
                for (key, val) in obj {
                    if let Some(schema) = properties.get(key) {
                        if let Err(e) = schema.validate_with_depth(val, depth + 1) {
                            // Same as Array — surface the
                            // recursion-limit signal unwrapped.
                            if matches!(e, ValidationError::RecursionLimitExceeded { .. }) {
                                return Err(e);
                            }
                            return Err(ValidationError::PropertyError {
                                property: key.clone(),
                                error: Box::new(e),
                            });
                        }
                    } else if !additional_properties {
                        return Err(ValidationError::UnknownProperty {
                            property: key.clone(),
                        });
                    }
                }
                Ok(())
            }
            (SchemaType::Object { .. }, _) => Err(ValidationError::TypeMismatch {
                expected: "object".into(),
                got: value_type_name(value),
            }),

            (SchemaType::Enum { values }, v) => {
                if values.contains(v) {
                    Ok(())
                } else {
                    Err(ValidationError::EnumMismatch {
                        value: v.clone(),
                        allowed: values.clone(),
                    })
                }
            }

            (SchemaType::AnyOf { schemas }, v) => {
                for schema in schemas {
                    match schema.validate_with_depth(v, depth + 1) {
                        Ok(()) => return Ok(()),
                        Err(ValidationError::RecursionLimitExceeded { limit }) => {
                            // Don't swallow the recursion-limit
                            // signal — surface it instead of
                            // converting to AnyOfFailed.
                            return Err(ValidationError::RecursionLimitExceeded { limit });
                        }
                        Err(_) => {}
                    }
                }
                Err(ValidationError::AnyOfFailed {
                    schema_count: schemas.len(),
                })
            }

            (SchemaType::Ref { .. }, _) => {
                // Reference resolution would happen at registry level
                Ok(())
            }

            (SchemaType::Any, _) => Ok(()),
        }
    }
}

fn value_type_name(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".into(),
        serde_json::Value::Bool(_) => "boolean".into(),
        serde_json::Value::Number(_) => "number".into(),
        serde_json::Value::String(_) => "string".into(),
        serde_json::Value::Array(_) => "array".into(),
        serde_json::Value::Object(_) => "object".into(),
    }
}

/// Validation errors
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// Type mismatch
    TypeMismatch {
        /// Expected type name
        expected: String,
        /// Actual type name received
        got: String,
    },
    /// Value out of range
    RangeError {
        /// The value that failed the range check
        value: f64,
        /// Inclusive minimum bound, if any
        min: Option<f64>,
        /// Inclusive maximum bound, if any
        max: Option<f64>,
    },
    /// Multiple-of constraint failed
    MultipleOfError {
        /// The value that failed the constraint
        value: i64,
        /// The required divisor
        multiple_of: i64,
    },
    /// Length constraint failed
    LengthError {
        /// Actual length of the string or array
        length: usize,
        /// Minimum allowed length, if any
        min: Option<usize>,
        /// Maximum allowed length, if any
        max: Option<usize>,
    },
    /// Pattern mismatch
    PatternMismatch {
        /// The string value that did not match
        value: String,
        /// The regex pattern that was required
        pattern: String,
    },
    /// Duplicate items in array
    DuplicateItems,
    /// Array item validation failed
    ArrayItemError {
        /// Zero-based index of the failing item
        index: usize,
        /// Nested validation error for the item
        error: Box<ValidationError>,
    },
    /// Missing required field
    MissingRequired {
        /// Name of the required field that was absent
        field: String,
    },
    /// Unknown property
    UnknownProperty {
        /// Name of the disallowed additional property
        property: String,
    },
    /// Property validation failed
    PropertyError {
        /// Name of the property that failed validation
        property: String,
        /// Nested validation error for the property value
        error: Box<ValidationError>,
    },
    /// Enum value not in allowed list
    EnumMismatch {
        /// The value that was not in the allowed set
        value: serde_json::Value,
        /// The set of allowed values
        allowed: Vec<serde_json::Value>,
    },
    /// AnyOf validation failed
    AnyOfFailed {
        /// Number of candidate schemas that were all tried and failed
        schema_count: usize,
    },
    /// Schema recursion depth exceeded (anti-DoS guard).
    ///
    /// Returned by [`SchemaType::validate`] when the recursive
    /// walk through nested `Array`/`Object`/`AnyOf` variants
    /// exceeds [`MAX_SCHEMA_DEPTH`]. Without this cap, an
    /// attacker who could ship a `SchemaType` (announcements
    /// broadcast over the mesh, or any caller parsing untrusted
    /// JSON) could submit a deeply nested schema and crash the
    /// validator (and the process) via stack overflow.
    RecursionLimitExceeded {
        /// The depth limit that was exceeded.
        limit: usize,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::TypeMismatch { expected, got } => {
                write!(f, "expected {}, got {}", expected, got)
            }
            ValidationError::RangeError { value, min, max } => {
                write!(f, "value {} out of range [{:?}, {:?}]", value, min, max)
            }
            ValidationError::MultipleOfError { value, multiple_of } => {
                write!(f, "{} is not a multiple of {}", value, multiple_of)
            }
            ValidationError::LengthError { length, min, max } => {
                write!(f, "length {} out of range [{:?}, {:?}]", length, min, max)
            }
            ValidationError::PatternMismatch { value, pattern } => {
                write!(f, "'{}' does not match pattern '{}'", value, pattern)
            }
            ValidationError::DuplicateItems => write!(f, "duplicate items in array"),
            ValidationError::ArrayItemError { index, error } => {
                write!(f, "item [{}]: {}", index, error)
            }
            ValidationError::MissingRequired { field } => {
                write!(f, "missing required field: {}", field)
            }
            ValidationError::UnknownProperty { property } => {
                write!(f, "unknown property: {}", property)
            }
            ValidationError::PropertyError { property, error } => {
                write!(f, "property '{}': {}", property, error)
            }
            ValidationError::EnumMismatch { value, .. } => {
                write!(f, "{:?} is not a valid enum value", value)
            }
            ValidationError::AnyOfFailed { schema_count } => {
                write!(f, "value did not match any of {} schemas", schema_count)
            }
            ValidationError::RecursionLimitExceeded { limit } => {
                write!(f, "schema recursion depth exceeded {}", limit)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// API parameter definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiParameter {
    /// Parameter name
    pub name: String,
    /// Parameter description
    pub description: Option<String>,
    /// Whether parameter is required
    pub required: bool,
    /// Parameter schema
    pub schema: SchemaType,
    /// Default value (if not required)
    pub default: Option<serde_json::Value>,
    /// Example value
    pub example: Option<serde_json::Value>,
}

impl ApiParameter {
    /// Create a new required parameter
    pub fn required(name: impl Into<String>, schema: SchemaType) -> Self {
        Self {
            name: name.into(),
            description: None,
            required: true,
            schema,
            default: None,
            example: None,
        }
    }

    /// Create a new optional parameter
    pub fn optional(name: impl Into<String>, schema: SchemaType) -> Self {
        Self {
            name: name.into(),
            description: None,
            required: false,
            schema,
            default: None,
            example: None,
        }
    }

    /// Set description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set default value
    pub fn with_default(mut self, default: serde_json::Value) -> Self {
        self.default = Some(default);
        self
    }

    /// Set example
    pub fn with_example(mut self, example: serde_json::Value) -> Self {
        self.example = Some(example);
        self
    }
}

/// API endpoint definition
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiEndpoint {
    /// Endpoint path (e.g., "/models/{model_id}/infer")
    pub path: String,
    /// HTTP-like method
    pub method: ApiMethod,
    /// Endpoint description
    pub description: Option<String>,
    /// Path parameters
    pub path_params: Vec<ApiParameter>,
    /// Query parameters
    pub query_params: Vec<ApiParameter>,
    /// Request body schema
    pub request_body: Option<SchemaType>,
    /// Response schema
    pub response: Option<SchemaType>,
    /// Error response schema
    pub error_response: Option<SchemaType>,
    /// Required capabilities to call this endpoint
    pub required_capabilities: Vec<String>,
    /// Tags for categorization
    pub tags: Vec<String>,
    /// Whether endpoint is deprecated
    pub deprecated: bool,
    /// Rate limit (requests per minute)
    pub rate_limit: Option<u32>,
    /// Timeout in milliseconds
    pub timeout_ms: Option<u64>,
    /// Whether authentication is required
    pub auth_required: bool,
}

impl ApiEndpoint {
    /// Create a new endpoint
    pub fn new(path: impl Into<String>, method: ApiMethod) -> Self {
        Self {
            path: path.into(),
            method,
            description: None,
            path_params: Vec::new(),
            query_params: Vec::new(),
            request_body: None,
            response: None,
            error_response: None,
            required_capabilities: Vec::new(),
            tags: Vec::new(),
            deprecated: false,
            rate_limit: None,
            timeout_ms: None,
            auth_required: true,
        }
    }

    /// Set description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Add path parameter
    pub fn with_path_param(mut self, param: ApiParameter) -> Self {
        self.path_params.push(param);
        self
    }

    /// Add query parameter
    pub fn with_query_param(mut self, param: ApiParameter) -> Self {
        self.query_params.push(param);
        self
    }

    /// Set request body schema
    pub fn with_request_body(mut self, schema: SchemaType) -> Self {
        self.request_body = Some(schema);
        self
    }

    /// Set response schema
    pub fn with_response(mut self, schema: SchemaType) -> Self {
        self.response = Some(schema);
        self
    }

    /// Add required capability
    pub fn require_capability(mut self, cap: impl Into<String>) -> Self {
        self.required_capabilities.push(cap.into());
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Set rate limit
    pub fn with_rate_limit(mut self, requests_per_min: u32) -> Self {
        self.rate_limit = Some(requests_per_min);
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = Some(timeout_ms);
        self
    }

    /// Mark as not requiring auth
    pub fn no_auth(mut self) -> Self {
        self.auth_required = false;
        self
    }

    /// Mark as deprecated
    pub fn deprecated(mut self) -> Self {
        self.deprecated = true;
        self
    }

    /// Validate request parameters
    pub fn validate_request(
        &self,
        path_params: &HashMap<String, serde_json::Value>,
        query_params: &HashMap<String, serde_json::Value>,
        body: Option<&serde_json::Value>,
    ) -> Result<(), ApiValidationError> {
        // Validate path params
        for param in &self.path_params {
            if let Some(value) = path_params.get(&param.name) {
                param
                    .schema
                    .validate(value)
                    .map_err(|e| ApiValidationError::PathParameter {
                        name: param.name.clone(),
                        error: e,
                    })?;
            } else if param.required {
                return Err(ApiValidationError::MissingPathParameter {
                    name: param.name.clone(),
                });
            }
        }

        // Validate query params
        for param in &self.query_params {
            if let Some(value) = query_params.get(&param.name) {
                param
                    .schema
                    .validate(value)
                    .map_err(|e| ApiValidationError::QueryParameter {
                        name: param.name.clone(),
                        error: e,
                    })?;
            } else if param.required {
                return Err(ApiValidationError::MissingQueryParameter {
                    name: param.name.clone(),
                });
            }
        }

        // Validate body
        if let Some(body_schema) = &self.request_body {
            match body {
                Some(b) => {
                    body_schema
                        .validate(b)
                        .map_err(|e| ApiValidationError::RequestBody { error: e })?;
                }
                None => {
                    return Err(ApiValidationError::MissingRequestBody);
                }
            }
        }

        Ok(())
    }

    /// Check if endpoint matches a path
    pub fn matches_path(&self, path: &str) -> Option<HashMap<String, String>> {
        let self_parts: Vec<&str> = self.path.split('/').collect();
        let path_parts: Vec<&str> = path.split('/').collect();

        if self_parts.len() != path_parts.len() {
            return None;
        }

        let mut params = HashMap::new();

        for (self_part, path_part) in self_parts.iter().zip(path_parts.iter()) {
            if self_part.starts_with('{') && self_part.ends_with('}') {
                // Extract parameter name
                let param_name = &self_part[1..self_part.len() - 1];
                params.insert(param_name.to_string(), path_part.to_string());
            } else if self_part != path_part {
                return None;
            }
        }

        Some(params)
    }
}

/// API validation errors
#[derive(Debug, Clone, PartialEq)]
pub enum ApiValidationError {
    /// Missing path parameter
    MissingPathParameter {
        /// Name of the missing path parameter
        name: String,
    },
    /// Path parameter validation failed
    PathParameter {
        /// Name of the path parameter that failed
        name: String,
        /// Underlying schema validation error
        error: ValidationError,
    },
    /// Missing query parameter
    MissingQueryParameter {
        /// Name of the missing query parameter
        name: String,
    },
    /// Query parameter validation failed
    QueryParameter {
        /// Name of the query parameter that failed
        name: String,
        /// Underlying schema validation error
        error: ValidationError,
    },
    /// Missing request body
    MissingRequestBody,
    /// Request body validation failed
    RequestBody {
        /// Underlying schema validation error for the request body
        error: ValidationError,
    },
}

impl std::fmt::Display for ApiValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiValidationError::MissingPathParameter { name } => {
                write!(f, "missing path parameter: {}", name)
            }
            ApiValidationError::PathParameter { name, error } => {
                write!(f, "path parameter '{}': {}", name, error)
            }
            ApiValidationError::MissingQueryParameter { name } => {
                write!(f, "missing query parameter: {}", name)
            }
            ApiValidationError::QueryParameter { name, error } => {
                write!(f, "query parameter '{}': {}", name, error)
            }
            ApiValidationError::MissingRequestBody => write!(f, "missing request body"),
            ApiValidationError::RequestBody { error } => {
                write!(f, "request body: {}", error)
            }
        }
    }
}

impl std::error::Error for ApiValidationError {}

/// Semantic versioning for APIs
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApiVersion {
    /// Major version (breaking changes)
    pub major: u32,
    /// Minor version (new features, backwards compatible)
    pub minor: u32,
    /// Patch version (bug fixes)
    pub patch: u32,
}

impl ApiVersion {
    /// Create a new version
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Check if this version is compatible with a requirement
    pub fn is_compatible_with(&self, required: &ApiVersion) -> bool {
        // Major versions must match
        if self.major != required.major {
            return false;
        }
        // Our minor version must be >= required
        if self.minor < required.minor {
            return false;
        }
        // If minor versions match, patch must be >= required
        if self.minor == required.minor && self.patch < required.patch {
            return false;
        }
        true
    }

    /// Parse from string "major.minor.patch"
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Self {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for ApiVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ApiVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match self.major.cmp(&other.major) {
            std::cmp::Ordering::Equal => match self.minor.cmp(&other.minor) {
                std::cmp::Ordering::Equal => self.patch.cmp(&other.patch),
                ord => ord,
            },
            ord => ord,
        }
    }
}

/// Complete API schema for a node
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiSchema {
    /// Schema name
    pub name: String,
    /// Schema description
    pub description: Option<String>,
    /// API version
    pub version: ApiVersion,
    /// Base path prefix
    pub base_path: String,
    /// Available endpoints
    pub endpoints: Vec<ApiEndpoint>,
    /// Shared schema definitions (for $ref)
    pub definitions: HashMap<String, SchemaType>,
    /// Global tags
    pub tags: Vec<String>,
    /// Contact information
    pub contact: Option<String>,
    /// License
    pub license: Option<String>,
}

impl ApiSchema {
    /// Create a new API schema
    pub fn new(name: impl Into<String>, version: ApiVersion) -> Self {
        Self {
            name: name.into(),
            description: None,
            version,
            base_path: "/".into(),
            endpoints: Vec::new(),
            definitions: HashMap::new(),
            tags: Vec::new(),
            contact: None,
            license: None,
        }
    }

    /// Set description
    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    /// Set base path
    pub fn with_base_path(mut self, path: impl Into<String>) -> Self {
        self.base_path = path.into();
        self
    }

    /// Add endpoint
    pub fn add_endpoint(mut self, endpoint: ApiEndpoint) -> Self {
        self.endpoints.push(endpoint);
        self
    }

    /// Add schema definition
    pub fn add_definition(mut self, name: impl Into<String>, schema: SchemaType) -> Self {
        self.definitions.insert(name.into(), schema);
        self
    }

    /// Add tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Find endpoint by path and method
    pub fn find_endpoint(&self, path: &str, method: ApiMethod) -> Option<&ApiEndpoint> {
        let full_path = if path.starts_with(&self.base_path) {
            path.to_string()
        } else {
            format!("{}{}", self.base_path.trim_end_matches('/'), path)
        };

        self.endpoints
            .iter()
            .find(|e| e.method == method && e.matches_path(&full_path).is_some())
    }

    /// Get all endpoints with a specific tag
    pub fn endpoints_by_tag(&self, tag: &str) -> Vec<&ApiEndpoint> {
        self.endpoints
            .iter()
            .filter(|e| e.tags.contains(&tag.to_string()))
            .collect()
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// Node API announcement
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiAnnouncement {
    /// Node ID
    pub node_id: NodeId,
    /// API schemas provided by this node
    pub schemas: Vec<ApiSchema>,
    /// Announcement version (monotonic)
    pub version: u64,
    /// Timestamp (Unix millis)
    pub timestamp: u64,
    /// TTL in seconds
    pub ttl_secs: u32,
}

impl ApiAnnouncement {
    /// Create a new announcement
    pub fn new(node_id: NodeId, schemas: Vec<ApiSchema>) -> Self {
        Self {
            node_id,
            schemas,
            version: 1,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            ttl_secs: 300,
        }
    }

    /// Set version
    pub fn with_version(mut self, version: u64) -> Self {
        self.version = version;
        self
    }

    /// Set TTL
    pub fn with_ttl(mut self, ttl_secs: u32) -> Self {
        self.ttl_secs = ttl_secs;
        self
    }

    /// Check if expired
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let expiry = self.timestamp + (self.ttl_secs as u64 * 1000);
        now > expiry
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
}

/// API query for finding nodes with specific APIs
#[derive(Debug, Clone, Default)]
pub struct ApiQuery {
    /// Required API name
    pub api_name: Option<String>,
    /// Minimum version required
    pub min_version: Option<ApiVersion>,
    /// Required endpoint path pattern
    pub endpoint_path: Option<String>,
    /// Required endpoint method
    pub endpoint_method: Option<ApiMethod>,
    /// Required tag
    pub tag: Option<String>,
    /// Must have specific capability
    pub capability: Option<String>,
}

impl ApiQuery {
    /// Create a new query
    pub fn new() -> Self {
        Self::default()
    }

    /// Filter by API name
    pub fn with_api(mut self, name: impl Into<String>) -> Self {
        self.api_name = Some(name.into());
        self
    }

    /// Filter by minimum version
    pub fn with_min_version(mut self, version: ApiVersion) -> Self {
        self.min_version = Some(version);
        self
    }

    /// Filter by endpoint path
    pub fn with_endpoint(mut self, path: impl Into<String>) -> Self {
        self.endpoint_path = Some(path.into());
        self
    }

    /// Filter by method
    pub fn with_method(mut self, method: ApiMethod) -> Self {
        self.endpoint_method = Some(method);
        self
    }

    /// Filter by tag
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// Filter by capability
    pub fn with_capability(mut self, cap: impl Into<String>) -> Self {
        self.capability = Some(cap.into());
        self
    }

    /// Check if a schema matches this query
    pub fn matches_schema(&self, schema: &ApiSchema) -> bool {
        // Check API name
        if let Some(ref name) = self.api_name {
            if &schema.name != name {
                return false;
            }
        }

        // Check version
        if let Some(ref min_ver) = self.min_version {
            if !schema.version.is_compatible_with(min_ver) {
                return false;
            }
        }

        // Check endpoint
        if let Some(ref path) = self.endpoint_path {
            let method = self.endpoint_method;
            let found = schema.endpoints.iter().any(|e| {
                let path_matches = e.matches_path(path).is_some() || e.path.contains(path);
                let method_matches = method.is_none_or(|m| e.method == m);
                path_matches && method_matches
            });
            if !found {
                return false;
            }
        }

        // Check tag
        if let Some(ref tag) = self.tag {
            if !schema.tags.contains(tag) {
                return false;
            }
        }

        // Check capability
        if let Some(ref cap) = self.capability {
            let found = schema
                .endpoints
                .iter()
                .any(|e| e.required_capabilities.contains(cap));
            if !found {
                return false;
            }
        }

        true
    }
}

/// Registry errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Node not found
    NodeNotFound(NodeId),
    /// API not found
    ApiNotFound(String),
    /// Version conflict
    VersionConflict {
        /// Version that was required for the operation
        expected: u64,
        /// Version that was found in the registry
        actual: u64,
    },
    /// Capacity exceeded
    CapacityExceeded,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::NodeNotFound(_) => write!(f, "Node not found"),
            RegistryError::ApiNotFound(name) => write!(f, "API not found: {}", name),
            RegistryError::VersionConflict { expected, actual } => {
                write!(f, "Version conflict: expected {}, got {}", expected, actual)
            }
            RegistryError::CapacityExceeded => write!(f, "Registry capacity exceeded"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Indexed node API information
#[derive(Debug, Clone)]
pub struct IndexedApiNode {
    /// Node ID
    pub node_id: NodeId,
    /// API announcement
    pub announcement: Arc<ApiAnnouncement>,
}

/// Registry statistics
#[derive(Debug, Clone, Default)]
pub struct ApiRegistryStats {
    /// Total nodes registered
    pub total_nodes: usize,
    /// Total API schemas
    pub total_schemas: usize,
    /// Total endpoints
    pub total_endpoints: usize,
    /// APIs by name
    pub apis_by_name: HashMap<String, usize>,
    /// Query count
    pub queries: u64,
    /// Update count
    pub updates: u64,
}

/// High-performance API registry with indexes
pub struct ApiRegistry {
    /// Primary storage: node_id -> announcement
    nodes: DashMap<NodeId, Arc<ApiAnnouncement>>,
    /// Index by API name
    by_api_name: DashMap<String, HashSet<NodeId>>,
    /// Index by tag
    by_tag: DashMap<String, HashSet<NodeId>>,
    /// Index by endpoint path pattern
    by_endpoint: DashMap<String, HashSet<NodeId>>,
    /// Query counter
    query_count: AtomicU64,
    /// Update counter
    update_count: AtomicU64,
    /// Maximum capacity
    max_capacity: Option<usize>,
}

/// Extract the leading path prefix used as the `by_endpoint` index
/// key. Slices up to (but not including) the second `/` so the
/// computation matches between `add_to_indexes` and
/// `remove_from_indexes` without allocating an intermediate
/// `Vec<&str>` for the split + join. Equivalent to the previous
/// `path.split('/').take(2).collect::<Vec<_>>().join("/")`.
fn endpoint_prefix(path: &str) -> String {
    match path.match_indices('/').nth(1) {
        Some((idx, _)) => path[..idx].to_string(),
        None => path.to_string(),
    }
}

impl ApiRegistry {
    /// Create a new registry
    pub fn new() -> Self {
        Self {
            nodes: DashMap::new(),
            by_api_name: DashMap::new(),
            by_tag: DashMap::new(),
            by_endpoint: DashMap::new(),
            query_count: AtomicU64::new(0),
            update_count: AtomicU64::new(0),
            max_capacity: None,
        }
    }

    /// Create with capacity limit
    pub fn with_capacity(max: usize) -> Self {
        let mut reg = Self::new();
        reg.max_capacity = Some(max);
        reg
    }

    /// Register or update a node's APIs
    pub fn register(&self, announcement: ApiAnnouncement) -> Result<(), RegistryError> {
        let node_id = announcement.node_id;

        // Check capacity
        if let Some(max) = self.max_capacity {
            if !self.nodes.contains_key(&node_id) && self.nodes.len() >= max {
                return Err(RegistryError::CapacityExceeded);
            }
        }

        // Remove old indexes if updating
        if let Some(old) = self.nodes.get(&node_id) {
            self.remove_from_indexes(&old);
        }

        let ann = Arc::new(announcement);

        // Add to indexes
        self.add_to_indexes(&ann);

        // Store
        self.nodes.insert(node_id, ann);
        self.update_count.fetch_add(1, Ordering::Relaxed);

        Ok(())
    }

    /// Unregister a node
    pub fn unregister(&self, node_id: &NodeId) -> Option<Arc<ApiAnnouncement>> {
        if let Some((_, ann)) = self.nodes.remove(node_id) {
            self.remove_from_indexes(&ann);
            Some(ann)
        } else {
            None
        }
    }

    /// Get a node's API announcement
    pub fn get(&self, node_id: &NodeId) -> Option<Arc<ApiAnnouncement>> {
        self.nodes.get(node_id).map(|r| Arc::clone(&r))
    }

    /// Query for nodes matching criteria
    pub fn query(&self, query: &ApiQuery) -> Vec<IndexedApiNode> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        // Use indexes for initial filtering
        let candidates: Vec<NodeId> = if let Some(ref api_name) = query.api_name {
            self.by_api_name
                .get(api_name)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default()
        } else if let Some(ref tag) = query.tag {
            self.by_tag
                .get(tag)
                .map(|s| s.iter().copied().collect())
                .unwrap_or_default()
        } else {
            // Full scan
            self.nodes.iter().map(|r| *r.key()).collect()
        };

        // Filter and collect
        candidates
            .into_iter()
            .filter_map(|id| {
                let ann = self.nodes.get(&id)?;
                // Check if any schema matches
                let matches = ann.schemas.iter().any(|s| query.matches_schema(s));
                if matches && !ann.is_expired() {
                    Some(IndexedApiNode {
                        node_id: id,
                        announcement: Arc::clone(&ann),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find nodes that provide a specific API endpoint
    pub fn find_by_endpoint(&self, path: &str, method: ApiMethod) -> Vec<IndexedApiNode> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        self.nodes
            .iter()
            .filter_map(|entry| {
                let ann = entry.value();
                if ann.is_expired() {
                    return None;
                }

                // Check if any schema has this endpoint
                let has_endpoint = ann.schemas.iter().any(|schema| {
                    schema
                        .endpoints
                        .iter()
                        .any(|e| e.method == method && e.matches_path(path).is_some())
                });

                if has_endpoint {
                    Some(IndexedApiNode {
                        node_id: *entry.key(),
                        announcement: Arc::clone(ann),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find nodes with compatible API version
    pub fn find_compatible(&self, api_name: &str, min_version: &ApiVersion) -> Vec<IndexedApiNode> {
        self.query_count.fetch_add(1, Ordering::Relaxed);

        let candidates = self
            .by_api_name
            .get(api_name)
            .map(|s| s.iter().copied().collect::<Vec<_>>())
            .unwrap_or_default();

        candidates
            .into_iter()
            .filter_map(|id| {
                let ann = self.nodes.get(&id)?;
                if ann.is_expired() {
                    return None;
                }

                let compatible = ann.schemas.iter().any(|schema| {
                    schema.name == api_name && schema.version.is_compatible_with(min_version)
                });

                if compatible {
                    Some(IndexedApiNode {
                        node_id: id,
                        announcement: Arc::clone(&ann),
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get statistics
    pub fn stats(&self) -> ApiRegistryStats {
        let mut apis_by_name: HashMap<String, usize> = HashMap::new();
        let mut total_endpoints = 0;

        for entry in self.nodes.iter() {
            for schema in &entry.value().schemas {
                *apis_by_name.entry(schema.name.clone()).or_default() += 1;
                total_endpoints += schema.endpoints.len();
            }
        }

        ApiRegistryStats {
            total_nodes: self.nodes.len(),
            total_schemas: apis_by_name.values().sum(),
            total_endpoints,
            apis_by_name,
            queries: self.query_count.load(Ordering::Relaxed),
            updates: self.update_count.load(Ordering::Relaxed),
        }
    }

    /// Number of registered nodes
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Clear all registrations
    pub fn clear(&self) {
        self.nodes.clear();
        self.by_api_name.clear();
        self.by_tag.clear();
        self.by_endpoint.clear();
    }

    /// Remove expired entries
    pub fn cleanup_expired(&self) -> usize {
        let expired: Vec<NodeId> = self
            .nodes
            .iter()
            .filter(|e| e.value().is_expired())
            .map(|e| *e.key())
            .collect();

        let count = expired.len();
        for id in expired {
            self.unregister(&id);
        }
        count
    }

    // Private helper to add indexes
    fn add_to_indexes(&self, ann: &ApiAnnouncement) {
        let node_id = ann.node_id;

        for schema in &ann.schemas {
            // API name index
            self.by_api_name
                .entry(schema.name.clone())
                .or_default()
                .insert(node_id);

            // Tag index
            for tag in &schema.tags {
                self.by_tag.entry(tag.clone()).or_default().insert(node_id);
            }

            // Endpoint index (simplified - just uses path prefix).
            for endpoint in &schema.endpoints {
                let prefix = endpoint_prefix(&endpoint.path);
                self.by_endpoint.entry(prefix).or_default().insert(node_id);
            }
        }
    }

    // Private helper to remove indexes
    fn remove_from_indexes(&self, ann: &ApiAnnouncement) {
        let node_id = ann.node_id;

        for schema in &ann.schemas {
            if let Some(mut set) = self.by_api_name.get_mut(&schema.name) {
                set.remove(&node_id);
            }

            for tag in &schema.tags {
                if let Some(mut set) = self.by_tag.get_mut(tag) {
                    set.remove(&node_id);
                }
            }

            for endpoint in &schema.endpoints {
                let prefix = endpoint_prefix(&endpoint.path);
                if let Some(mut set) = self.by_endpoint.get_mut(&prefix) {
                    set.remove(&node_id);
                }
            }
        }
    }
}

impl Default for ApiRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }

    #[test]
    fn test_schema_type_validation() {
        // String validation
        let schema = SchemaType::string().with_max_length(10);
        assert!(schema.validate(&serde_json::json!("hello")).is_ok());
        assert!(schema.validate(&serde_json::json!("hello world!")).is_err());

        // Integer validation
        let schema = SchemaType::integer().with_minimum(0).with_maximum(100);
        assert!(schema.validate(&serde_json::json!(50)).is_ok());
        assert!(schema.validate(&serde_json::json!(-1)).is_err());
        assert!(schema.validate(&serde_json::json!(101)).is_err());

        // Object validation
        let schema = SchemaType::object()
            .with_property("name", SchemaType::string())
            .with_property("age", SchemaType::integer())
            .with_required("name");

        assert!(schema
            .validate(&serde_json::json!({"name": "Alice", "age": 30}))
            .is_ok());
        assert!(schema.validate(&serde_json::json!({"age": 30})).is_err()); // missing required

        // Array validation
        let schema = SchemaType::array(SchemaType::integer());
        assert!(schema.validate(&serde_json::json!([1, 2, 3])).is_ok());
        assert!(schema.validate(&serde_json::json!([1, "two", 3])).is_err());
    }

    /// Regression for BUG_AUDIT_2026_04_30_CORE.md #109: pre-fix
    /// `SchemaType::validate` recursed without bound through
    /// `Array { items }` / `Object { properties }` / `AnyOf { schemas }`.
    /// An attacker who could ship a `SchemaType` (announcements
    /// broadcast over the mesh, or any caller parsing untrusted
    /// JSON) could submit a deeply-nested schema and crash the
    /// process via stack overflow when a request was validated.
    /// Post-fix: depth is bounded by `MAX_SCHEMA_DEPTH`;
    /// exceeding it returns `RecursionLimitExceeded` instead.
    ///
    /// We pin the bound by constructing a schema deeper than the
    /// limit (chained Array variants — each `Array { items: ... }`
    /// adds one level of recursion). With a payload that walks
    /// through every level, validate must surface the
    /// recursion-limit error rather than blowing the stack.
    #[test]
    fn validate_returns_recursion_limit_error_on_deeply_nested_schema() {
        // Build an Array<Array<Array<...<Integer>...>>> with
        // depth = MAX_SCHEMA_DEPTH + 5 (well past the cap).
        let mut schema = SchemaType::integer();
        for _ in 0..MAX_SCHEMA_DEPTH + 5 {
            schema = SchemaType::array(schema);
        }

        // Build a matching nested-array payload so each level
        // descends into the next.
        let mut value = serde_json::json!(1);
        for _ in 0..MAX_SCHEMA_DEPTH + 5 {
            value = serde_json::json!([value]);
        }

        // Pre-fix: stack overflow. Post-fix: bounded error.
        let result = schema.validate(&value);
        match result {
            Err(ValidationError::RecursionLimitExceeded { limit }) => {
                assert_eq!(limit, MAX_SCHEMA_DEPTH);
            }
            other => panic!("expected RecursionLimitExceeded, got {:?}", other),
        }
    }

    /// Sanity: a schema at exactly `MAX_SCHEMA_DEPTH` levels of
    /// nesting must still validate successfully (no false-positive
    /// recursion error).
    #[test]
    fn validate_accepts_schema_at_recursion_limit() {
        let mut schema = SchemaType::integer();
        // depth = MAX_SCHEMA_DEPTH - 1 means the validator visits
        // depth values 0..MAX_SCHEMA_DEPTH, all under the cap.
        for _ in 0..(MAX_SCHEMA_DEPTH - 1) {
            schema = SchemaType::array(schema);
        }
        let mut value = serde_json::json!(1);
        for _ in 0..(MAX_SCHEMA_DEPTH - 1) {
            value = serde_json::json!([value]);
        }
        assert!(
            schema.validate(&value).is_ok(),
            "schema right at the depth limit must still validate"
        );
    }

    /// CR-9: deeply-nested input must be rejected at the deserialize
    /// boundary, BEFORE `validate` is called. Pre-fix the cap was
    /// only enforced post-parse — an adversarial schema could
    /// trigger the recursive `Deserialize` to allocate a deep
    /// `SchemaType` tree (or stack-overflow on a very deep input)
    /// before any validation ran.
    #[test]
    fn try_from_slice_rejects_input_over_max_schema_depth() {
        // Build a JSON string with MAX_SCHEMA_DEPTH + 50 nested
        // arrays. Even though valid JSON, it must trip the
        // depth-scan guard before serde_json runs.
        let depth = MAX_SCHEMA_DEPTH + 50;
        let mut s = String::new();
        for _ in 0..depth {
            s.push('[');
        }
        s.push_str("null");
        for _ in 0..depth {
            s.push(']');
        }
        let err = SchemaType::try_from_str(&s)
            .expect_err("deeply-nested JSON must be rejected by the depth pre-scan");
        let msg = format!("{}", err);
        assert!(
            msg.contains("max nesting depth exceeded"),
            "error message must name the depth cap; got: {}",
            msg
        );
    }

    /// CR-9: the depth pre-scan must not be fooled by JSON strings
    /// containing brackets. A long string of `}`s inside `"..."`
    /// must NOT be counted as depth-out (which would let an
    /// attacker mask real depth).
    #[test]
    fn try_from_slice_handles_brackets_inside_strings_correctly() {
        // A schema with a `pattern` field containing brackets in
        // a string. The string brackets must be ignored by the
        // depth counter.
        let json = r#"{"type":"string","pattern":"[}{]\""}"#;
        let r = SchemaType::try_from_str(json);
        assert!(
            r.is_ok(),
            "valid schema with bracket-bearing string must parse: {:?}",
            r.err()
        );
    }

    /// CR-9: a moderately-deep schema (well under both the depth
    /// pre-scan cap AND serde_json's internal recursion limit)
    /// must parse cleanly. The internal serde_json limit (128) and
    /// our `MAX_SCHEMA_DEPTH` (128) are intentionally aligned, but
    /// each `Box<SchemaType>` adds a serde call frame on top of
    /// the byte-counter depth, so the effective serde-side ceiling
    /// is a bit below `MAX_SCHEMA_DEPTH`. We pin a depth of 32
    /// here — comfortably representative of any real-world nested
    /// schema and well within both caps.
    #[test]
    fn try_from_slice_accepts_normal_depth_schema() {
        let depth = 32usize;
        let mut s = String::new();
        for _ in 0..depth {
            s.push_str(r#"{"type":"array","items":"#);
        }
        s.push_str(r#"{"type":"null"}"#);
        for _ in 0..depth {
            s.push('}');
        }
        let r = SchemaType::try_from_str(&s);
        assert!(
            r.is_ok(),
            "moderately-nested schema (depth {}) must parse; got: {:?}",
            depth,
            r.err()
        );
    }

    /// CR-9: direct unit test on the depth scanner — confirms
    /// it counts both `{`/`}` and `[`/`]` correctly and respects
    /// string-literal boundaries.
    #[test]
    fn check_json_nesting_depth_unit() {
        assert!(check_json_nesting_depth(b"{}", 1).is_ok());
        assert!(check_json_nesting_depth(b"{}", 0).is_err()); // depth 1 > 0
        assert!(check_json_nesting_depth(b"[[[[]]]]", 4).is_ok());
        assert!(check_json_nesting_depth(b"[[[[]]]]", 3).is_err());
        // Brackets inside a string are NOT counted.
        assert!(check_json_nesting_depth(b"\"[[[[\"", 0).is_ok());
        // Escaped quote keeps us inside the string.
        assert!(check_json_nesting_depth(b"\"[\\\"[[\"", 0).is_ok());
        // Mixed nesting.
        assert!(check_json_nesting_depth(b"{\"a\":[1,2]}", 2).is_ok());
        assert!(check_json_nesting_depth(b"{\"a\":[1,2]}", 1).is_err());
    }

    #[test]
    fn test_api_endpoint_path_matching() {
        let endpoint = ApiEndpoint::new("/models/{model_id}/infer", ApiMethod::Post)
            .with_path_param(ApiParameter::required("model_id", SchemaType::string()));

        // Should match
        let params = endpoint.matches_path("/models/llama-7b/infer");
        assert!(params.is_some());
        let params = params.unwrap();
        assert_eq!(params.get("model_id"), Some(&"llama-7b".to_string()));

        // Should not match (wrong path)
        assert!(endpoint.matches_path("/models/llama-7b/train").is_none());
        assert!(endpoint.matches_path("/models/infer").is_none());
    }

    #[test]
    fn test_api_version_compatibility() {
        let v1_0_0 = ApiVersion::new(1, 0, 0);
        let v1_1_0 = ApiVersion::new(1, 1, 0);
        let v1_1_1 = ApiVersion::new(1, 1, 1);
        let v2_0_0 = ApiVersion::new(2, 0, 0);

        // Same version is compatible
        assert!(v1_0_0.is_compatible_with(&v1_0_0));

        // Higher minor version is compatible
        assert!(v1_1_0.is_compatible_with(&v1_0_0));

        // Higher patch version is compatible
        assert!(v1_1_1.is_compatible_with(&v1_1_0));

        // Lower minor version is not compatible
        assert!(!v1_0_0.is_compatible_with(&v1_1_0));

        // Different major version is not compatible
        assert!(!v2_0_0.is_compatible_with(&v1_0_0));
        assert!(!v1_0_0.is_compatible_with(&v2_0_0));
    }

    #[test]
    fn test_api_schema() {
        let schema = ApiSchema::new("inference", ApiVersion::new(1, 0, 0))
            .with_description("Model inference API")
            .with_base_path("/api/v1")
            .with_tag("ai")
            .add_endpoint(
                ApiEndpoint::new("/models/{model_id}/infer", ApiMethod::Post)
                    .with_description("Run inference on a model")
                    .with_tag("inference"),
            )
            .add_endpoint(
                ApiEndpoint::new("/models", ApiMethod::Get)
                    .with_description("List available models")
                    .with_tag("models"),
            );

        assert_eq!(schema.endpoints.len(), 2);
        assert!(schema.tags.contains(&"ai".to_string()));

        // Find by tag
        let inference_endpoints = schema.endpoints_by_tag("inference");
        assert_eq!(inference_endpoints.len(), 1);
    }

    #[test]
    fn test_api_registry_basic() {
        let registry = ApiRegistry::new();

        let schema = ApiSchema::new("test-api", ApiVersion::new(1, 0, 0))
            .with_tag("test")
            .add_endpoint(ApiEndpoint::new("/test", ApiMethod::Get));

        let ann = ApiAnnouncement::new(make_node_id(1), vec![schema]);
        registry.register(ann).unwrap();

        assert_eq!(registry.len(), 1);

        let result = registry.get(&make_node_id(1));
        assert!(result.is_some());

        registry.unregister(&make_node_id(1));
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_api_registry_query() {
        let registry = ApiRegistry::new();

        // Add multiple nodes with different APIs
        for i in 0..10 {
            let api_name = if i < 5 { "inference" } else { "training" };
            let tag = if i % 2 == 0 { "gpu" } else { "cpu" };

            let schema = ApiSchema::new(api_name, ApiVersion::new(1, i as u32, 0))
                .with_tag(tag)
                .add_endpoint(ApiEndpoint::new("/run", ApiMethod::Post));

            let ann = ApiAnnouncement::new(make_node_id(i), vec![schema]);
            registry.register(ann).unwrap();
        }

        // Query by API name
        let results = registry.query(&ApiQuery::new().with_api("inference"));
        assert_eq!(results.len(), 5);

        // Query by tag
        let results = registry.query(&ApiQuery::new().with_tag("gpu"));
        assert_eq!(results.len(), 5);

        // Query by both
        let results = registry.query(&ApiQuery::new().with_api("inference").with_tag("gpu"));
        // inference (0-4), gpu (0,2,4,6,8) -> intersection is 0,2,4
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_api_registry_version_compatibility() {
        let registry = ApiRegistry::new();

        // Add nodes with different versions
        for i in 0..5 {
            let schema = ApiSchema::new("my-api", ApiVersion::new(1, i as u32, 0));
            let ann = ApiAnnouncement::new(make_node_id(i), vec![schema]);
            registry.register(ann).unwrap();
        }

        // Find nodes compatible with v1.2.0
        let results = registry.find_compatible("my-api", &ApiVersion::new(1, 2, 0));
        // v1.2.0, v1.3.0, v1.4.0 are compatible
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_request_validation() {
        let endpoint = ApiEndpoint::new("/users/{user_id}", ApiMethod::Get)
            .with_path_param(ApiParameter::required("user_id", SchemaType::string()))
            .with_query_param(ApiParameter::optional("limit", SchemaType::integer()));

        // Valid request
        let mut path_params = HashMap::new();
        path_params.insert("user_id".to_string(), serde_json::json!("123"));

        let query_params = HashMap::new();

        let result = endpoint.validate_request(&path_params, &query_params, None);
        assert!(result.is_ok());

        // Missing required path param
        let empty_path = HashMap::new();
        let result = endpoint.validate_request(&empty_path, &query_params, None);
        assert!(matches!(
            result,
            Err(ApiValidationError::MissingPathParameter { .. })
        ));
    }

    #[test]
    fn test_api_method_properties() {
        assert!(ApiMethod::Get.is_idempotent());
        assert!(ApiMethod::Put.is_idempotent());
        assert!(!ApiMethod::Post.is_idempotent());

        assert!(ApiMethod::Stream.is_streaming());
        assert!(ApiMethod::BiStream.is_streaming());
        assert!(!ApiMethod::Get.is_streaming());

        assert!(ApiMethod::Get.is_safe());
        assert!(!ApiMethod::Post.is_safe());
    }

    #[test]
    fn test_stats() {
        let registry = ApiRegistry::new();

        for i in 0..5 {
            let schema = ApiSchema::new("api", ApiVersion::new(1, 0, 0))
                .add_endpoint(ApiEndpoint::new("/a", ApiMethod::Get))
                .add_endpoint(ApiEndpoint::new("/b", ApiMethod::Post));

            let ann = ApiAnnouncement::new(make_node_id(i), vec![schema]);
            registry.register(ann).unwrap();
        }

        // Run some queries
        registry.query(&ApiQuery::new());
        registry.query(&ApiQuery::new());

        let stats = registry.stats();
        assert_eq!(stats.total_nodes, 5);
        assert_eq!(stats.total_schemas, 5);
        assert_eq!(stats.total_endpoints, 10);
        assert_eq!(stats.queries, 2);
        assert_eq!(stats.updates, 5);
    }

    /// `endpoint_prefix` replaces the previous
    /// `path.split('/').take(2).collect::<Vec<_>>().join("/")` with a
    /// `match_indices('/').nth(1)`-based slice. The replacement
    /// must be byte-identical for every shape we feed it — a single
    /// drift here would put `add_to_indexes` and
    /// `remove_from_indexes` out of sync and silently leak entries
    /// in `by_endpoint`. Each case below names the previous
    /// behavior explicitly so a future reviewer can see the
    /// equivalence at a glance.
    #[test]
    fn endpoint_prefix_matches_previous_split_join_behavior() {
        // Helper that runs the OLD logic for ground truth.
        fn old(path: &str) -> String {
            path.split('/').take(2).collect::<Vec<_>>().join("/")
        }

        let cases: &[&str] = &[
            "",               // empty
            "/",              // a lone separator
            "//",             // two separators, nothing between
            "//a",            // empty leading segment, then content
            "/a",             // single leading-slash segment
            "/a/",            // trailing slash
            "a",              // no slashes at all
            "a/",             // single segment + trailing slash
            "/api",           // typical absolute root
            "/api/users",     // two-segment absolute
            "/api/users/123", // deep absolute
            "api/users/123",  // deep relative
            "/api/users/v2/list",
            "////",
        ];

        for path in cases {
            assert_eq!(
                endpoint_prefix(path),
                old(path),
                "endpoint_prefix divergence for {path:?}",
            );
        }
    }

    // ---------- Validation error-branch coverage ----------
    //
    // The existing happy-path tests cover the success arms of
    // `SchemaType::validate`. These exercise the negative branches
    // that codecov flagged as uncovered: `Number` min/max (distinct
    // from `Integer`), every type-mismatch arm, string length /
    // pattern errors, array length / uniqueness errors, object
    // property errors, and the `Enum` / `AnyOf` / `Ref` arms.

    #[test]
    fn number_variant_range_and_type_errors() {
        let schema = SchemaType::Number {
            minimum: Some(0.0),
            maximum: Some(1.0),
        };
        assert!(schema.validate(&serde_json::json!(0.5)).is_ok());
        assert!(matches!(
            schema.validate(&serde_json::json!(-0.1)),
            Err(ValidationError::RangeError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!(1.5)),
            Err(ValidationError::RangeError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!("nope")),
            Err(ValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn string_length_pattern_and_type_errors() {
        let schema = SchemaType::String {
            min_length: Some(2),
            max_length: Some(5),
            pattern: Some("ab".into()),
            format: None,
        };
        assert!(schema.validate(&serde_json::json!("xab")).is_ok());
        assert!(matches!(
            schema.validate(&serde_json::json!("a")),
            Err(ValidationError::LengthError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!("abcdef")),
            Err(ValidationError::LengthError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!("xyz")),
            Err(ValidationError::PatternMismatch { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!(42)),
            Err(ValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn array_length_uniqueness_and_type_errors() {
        let schema = SchemaType::Array {
            items: Box::new(SchemaType::integer()),
            min_items: Some(2),
            max_items: Some(3),
            unique_items: true,
        };
        assert!(schema.validate(&serde_json::json!([1, 2])).is_ok());
        assert!(matches!(
            schema.validate(&serde_json::json!([1])),
            Err(ValidationError::LengthError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!([1, 2, 3, 4])),
            Err(ValidationError::LengthError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!([1, 1, 2])),
            Err(ValidationError::DuplicateItems)
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!([1, "two", 3])),
            Err(ValidationError::ArrayItemError { .. })
        ));
        assert!(matches!(
            schema.validate(&serde_json::json!("not-an-array")),
            Err(ValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn object_property_unknown_and_type_errors() {
        let schema = SchemaType::object()
            .with_property("name", SchemaType::string())
            .with_property("age", SchemaType::integer())
            .with_required("name");

        // PropertyError: known property fails its own schema.
        let err = schema
            .validate(&serde_json::json!({"name": "Alice", "age": "old"}))
            .unwrap_err();
        assert!(matches!(err, ValidationError::PropertyError { .. }));

        // UnknownProperty: requires additional_properties=false, which
        // the `SchemaType::object()` builder doesn't expose — construct
        // directly to flip it.
        let strict = SchemaType::Object {
            properties: {
                let mut m = HashMap::new();
                m.insert("name".into(), SchemaType::string());
                m
            },
            required: vec!["name".into()],
            additional_properties: false,
        };
        let err = strict
            .validate(&serde_json::json!({"name": "Alice", "extra": 1}))
            .unwrap_err();
        assert!(matches!(err, ValidationError::UnknownProperty { .. }));

        // TypeMismatch: object schema receives non-object.
        assert!(matches!(
            schema.validate(&serde_json::json!([1, 2, 3])),
            Err(ValidationError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn enum_anyof_and_ref_arms() {
        // Enum miss.
        let schema = SchemaType::Enum {
            values: vec![serde_json::json!("a"), serde_json::json!("b")],
        };
        assert!(schema.validate(&serde_json::json!("a")).is_ok());
        assert!(matches!(
            schema.validate(&serde_json::json!("c")),
            Err(ValidationError::EnumMismatch { .. })
        ));

        // AnyOf success on second arm + AnyOfFailed when all reject.
        let any = SchemaType::AnyOf {
            schemas: vec![SchemaType::integer(), SchemaType::string()],
        };
        assert!(any.validate(&serde_json::json!("ok")).is_ok());
        assert!(any.validate(&serde_json::json!(42)).is_ok());
        assert!(matches!(
            any.validate(&serde_json::json!(true)),
            Err(ValidationError::AnyOfFailed { .. })
        ));

        // Ref arm at validator level returns Ok — resolution
        // is a registry-level concern (see L699-702).
        let r = SchemaType::Ref {
            schema_ref: "#/definitions/X".into(),
        };
        assert!(r.validate(&serde_json::json!(null)).is_ok());

        // Any matches anything.
        assert!(SchemaType::Any.validate(&serde_json::json!({"x":1})).is_ok());
    }

    // ---------- ApiQuery negative-branch coverage ----------

    #[test]
    fn query_matches_returns_false_on_each_filter_miss() {
        let schema = ApiSchema::new("svc", ApiVersion::new(1, 0, 0))
            .with_tag("gpu")
            .add_endpoint(ApiEndpoint::new("/run", ApiMethod::Post));
        let ann = ApiAnnouncement::new(make_node_id(1), vec![schema]);

        // Wrong api name.
        let q = ApiQuery::new().with_api("other");
        assert_eq!(registry_match_count(&ann, &q), 0);

        // Wrong tag.
        let q = ApiQuery::new().with_tag("cpu");
        assert_eq!(registry_match_count(&ann, &q), 0);

        // Wrong endpoint path.
        let q = ApiQuery::new().with_endpoint("/missing");
        assert_eq!(registry_match_count(&ann, &q), 0);

        // Wrong method on existing path.
        let q = ApiQuery::new()
            .with_endpoint("/run")
            .with_method(ApiMethod::Get);
        assert_eq!(registry_match_count(&ann, &q), 0);
    }

    /// Helper: register an announcement and count matches against a query.
    /// Keeps the test focused on the matcher, not registry plumbing.
    fn registry_match_count(ann: &ApiAnnouncement, q: &ApiQuery) -> usize {
        let r = ApiRegistry::new();
        r.register(ann.clone()).unwrap();
        r.query(q).len()
    }

    // ---------- Expired-entry filtering ----------

    #[test]
    fn find_by_endpoint_skips_expired_entries() {
        let registry = ApiRegistry::new();
        let schema = ApiSchema::new("svc", ApiVersion::new(1, 0, 0))
            .add_endpoint(ApiEndpoint::new("/run", ApiMethod::Post));

        // ttl_secs = 0 → expires the instant `now > timestamp`.
        let ann = ApiAnnouncement::new(make_node_id(7), vec![schema]).with_ttl(0);
        registry.register(ann).unwrap();

        // Give the wall clock a moment to advance past `timestamp`.
        std::thread::sleep(std::time::Duration::from_millis(5));

        assert!(registry.find_by_endpoint("/run", ApiMethod::Post).is_empty());
    }
}
