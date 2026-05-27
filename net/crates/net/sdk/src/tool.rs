//! AI tool-calling surface — SDK-side helpers built atop the
//! substrate's `cortex::tool` module.
//!
//! Gated by the `tool` Cargo feature. Three things land here:
//!
//! 1. Re-exports of every public wire / type primitive from
//!    [`net::adapter::net::cortex::tool`]. Downstream consumers
//!    write `use net_sdk::tool::ToolDescriptor` rather than reaching
//!    deep into the substrate-side module path.
//! 2. [`metadata_for`] — builds a [`ToolDescriptor`] from any pair
//!    of Rust request / response types implementing
//!    [`schemars::JsonSchema`]. The schemas land on the descriptor
//!    as JSON-encoded strings (matching `ToolCapability::input_schema`'s
//!    existing shape).
//! 3. [`ToolMetadataBuilder`] — a fluent builder for the fields
//!    that aren't derivable from the type signature: description,
//!    version, streaming flag, tags, stateless / latency hints.
//!    Callers chain `metadata_for::<Req, Resp>(name).description(...)`
//!    to build the descriptor in one expression.
//!
//! The actual `serve_tool` / `list_tools` / `watch_tools` /
//! `call_tool` SDK methods land in subsequent A-2..A-6 slices; this
//! one just establishes the type re-exports + the schema-derivation
//! helper every later slice composes against.
//!
//! Plan: see `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`,
//! slice A-1.

#[cfg(feature = "cortex")]
pub use net::adapter::net::cortex::tool::{
    description_metadata_key, streaming_metadata_key, tags_metadata_key, ToolDescriptor,
    ToolEvent, ToolMetadataRegistry, ToolMetadataRequest, ToolMetadataResponse,
    TOOL_METADATA_FETCH_SERVICE,
};

/// Builder for a [`ToolDescriptor`] that derives its JSON Schema
/// from Rust type parameters. Construct via [`metadata_for`], then
/// chain setters for the fields that aren't derivable from the
/// type signature (description, version, streaming, etc.).
///
/// Example:
///
/// ```ignore
/// use net_sdk::tool::metadata_for;
///
/// #[derive(schemars::JsonSchema, serde::Deserialize)]
/// struct WebSearchReq { query: String, max_results: u32 }
///
/// #[derive(schemars::JsonSchema, serde::Serialize)]
/// struct WebSearchResp { results: Vec<String> }
///
/// let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
///     .description("Search the web for relevant pages.")
///     .stateless(true)
///     .estimated_time_ms(500)
///     .tag("web")
///     .tag("research")
///     .build();
/// // hand the descriptor to `serve_tool` (lands in A-2).
/// ```
#[must_use = "ToolMetadataBuilder does nothing until `.build()` is called"]
pub struct ToolMetadataBuilder {
    descriptor: ToolDescriptor,
}

impl ToolMetadataBuilder {
    /// Replace the default human-readable description. Mandatory for
    /// any tool an LLM should reason about — the model reads this
    /// field to decide when to call.
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.descriptor.description = Some(description.into());
        self
    }

    /// Override the version (defaults to `"1.0.0"` from
    /// `ToolCapability::new`). Two registrations of the same `name`
    /// at different versions surface as separate descriptors in
    /// `list_tools`.
    pub fn version(mut self, version: impl Into<String>) -> Self {
        self.descriptor.version = version.into();
        self
    }

    /// Mark the tool as server-streaming (lowers into the future
    /// `serve_tool_streaming` rather than the unary `serve_tool`).
    /// Adapters use this flag to decide whether to render progress
    /// + delta envelopes vs. one terminal result.
    pub fn streaming(mut self, streaming: bool) -> Self {
        self.descriptor.streaming = streaming;
        self
    }

    /// Set the `stateless` flag. Pure-function tools (same input →
    /// same output, no session state) get cached, retried in
    /// parallel, etc. Stateful tools opt out.
    pub fn stateless(mut self, stateless: bool) -> Self {
        self.descriptor.stateless = stateless;
        self
    }

    /// Soft latency hint for the model scheduler / UI spinner.
    /// `0` means "no estimate" (the default).
    pub fn estimated_time_ms(mut self, ms: u32) -> Self {
        self.descriptor.estimated_time_ms = ms;
        self
    }

    /// Append one tag. Free-form; adapters surface tags as
    /// provider-specific metadata (e.g. Anthropic `cache_control`
    /// hints).
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.descriptor.tags.push(tag.into());
        self
    }

    /// Replace the tag list wholesale. Useful when the caller has
    /// the tags as a `Vec` already.
    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.descriptor.tags = tags;
        self
    }

    /// Append a required capability / dependency. Mirrors
    /// `ToolCapability::requires`. Adapters can use this to surface
    /// "tool needs X" dependencies (e.g. a `web_search` tool that
    /// depends on a configured API key).
    pub fn requires(mut self, dep: impl Into<String>) -> Self {
        self.descriptor.requires.push(dep.into());
        self
    }

    /// Consume the builder and return the finished [`ToolDescriptor`].
    /// Pass this into the future `serve_tool` / `serve_tool_streaming`
    /// methods (A-2 / A-3).
    pub fn build(self) -> ToolDescriptor {
        self.descriptor
    }
}

/// Build a [`ToolMetadataBuilder`] for the given `(Req, Resp)` pair.
/// Both types must implement [`schemars::JsonSchema`]; the helper
/// derives JSON Schema (draft 2020-12) for each and stores them on
/// the descriptor as JSON-encoded strings.
///
/// The `name` parameter is the nRPC service name; same string the
/// caller will pass to `serve_tool` and the agent will see in
/// `list_tools`.
///
/// Description defaults to an empty string; callers should chain
/// [`ToolMetadataBuilder::description`] to set it before `.build()`.
/// `version` defaults to `"1.0.0"`; `stateless` defaults to `true`
/// (matching `ToolCapability::new`'s defaults); `streaming`
/// defaults to `false`.
pub fn metadata_for<Req, Resp>(name: impl Into<String>) -> ToolMetadataBuilder
where
    Req: schemars::JsonSchema,
    Resp: schemars::JsonSchema,
{
    let name = name.into();
    let input_schema = schemars::schema_for!(Req);
    let output_schema = schemars::schema_for!(Resp);
    let input_schema_json = serde_json::to_string(&input_schema)
        .expect("schemars output is always valid JSON");
    let output_schema_json = serde_json::to_string(&output_schema)
        .expect("schemars output is always valid JSON");
    ToolMetadataBuilder {
        descriptor: ToolDescriptor {
            tool_id: name.clone(),
            name,
            version: "1.0.0".to_string(),
            description: None,
            input_schema: Some(input_schema_json),
            output_schema: Some(output_schema_json),
            requires: Vec::new(),
            estimated_time_ms: 0,
            stateless: true,
            streaming: false,
            tags: Vec::new(),
            node_count: 0,
        },
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(JsonSchema, Deserialize, Serialize)]
    #[allow(dead_code)]
    struct WebSearchReq {
        /// The query string.
        query: String,
        /// Maximum results to return.
        max_results: u32,
    }

    #[derive(JsonSchema, Deserialize, Serialize)]
    #[allow(dead_code)]
    struct WebSearchResp {
        results: Vec<String>,
    }

    #[test]
    fn metadata_for_derives_schemas_and_sets_defaults() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search").build();
        assert_eq!(descriptor.tool_id, "web_search");
        assert_eq!(descriptor.name, "web_search");
        assert_eq!(descriptor.version, "1.0.0");
        assert!(descriptor.description.is_none());
        assert!(descriptor.stateless);
        assert!(!descriptor.streaming);
        assert_eq!(descriptor.estimated_time_ms, 0);
        assert_eq!(descriptor.node_count, 0);
        assert!(descriptor.tags.is_empty());
        assert!(descriptor.requires.is_empty());

        // Schemas must be present + parse as valid JSON.
        let input = descriptor.input_schema.as_ref().expect("input schema present");
        let parsed: serde_json::Value =
            serde_json::from_str(input).expect("input schema must be valid JSON");
        // Object with `query` + `max_results` properties.
        let props = parsed.get("properties").expect("object schema has properties");
        assert!(props.get("query").is_some());
        assert!(props.get("max_results").is_some());

        let output = descriptor
            .output_schema
            .as_ref()
            .expect("output schema present");
        let _: serde_json::Value =
            serde_json::from_str(output).expect("output schema must be valid JSON");
    }

    #[test]
    fn builder_setters_apply_in_chain() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
            .description("Search the web.")
            .version("2.1.0")
            .streaming(true)
            .stateless(false)
            .estimated_time_ms(500)
            .tag("web")
            .tag("research")
            .requires("api_key:tavily")
            .build();
        assert_eq!(descriptor.description.as_deref(), Some("Search the web."));
        assert_eq!(descriptor.version, "2.1.0");
        assert!(descriptor.streaming);
        assert!(!descriptor.stateless);
        assert_eq!(descriptor.estimated_time_ms, 500);
        assert_eq!(descriptor.tags, vec!["web", "research"]);
        assert_eq!(descriptor.requires, vec!["api_key:tavily"]);
    }

    #[test]
    fn builder_tags_replaces_wholesale() {
        let descriptor = metadata_for::<WebSearchReq, WebSearchResp>("web_search")
            .tag("first")
            .tags(vec!["replaced".into(), "second".into()])
            .build();
        // `tags(...)` wholesale replaces — `first` is gone.
        assert_eq!(descriptor.tags, vec!["replaced", "second"]);
    }
}
