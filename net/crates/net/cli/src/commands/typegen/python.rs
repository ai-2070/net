//! Python renderer: `ToolDescriptor` → a per-tool package with Pydantic v2
//! models (`models.py`), type stubs (`models.pyi`), a typed call helper
//! (`call.py`), and a package `__init__.py`, plus a top-level `__init__.py`
//! re-export and `_meta.json`.
//!
//! Schemas are first lowered into a flat ordered list of [`Decl`]s (classes
//! and aliases, dependency-ordered children-first). `models.py` and
//! `models.pyi` both render from that single list, so the runtime models and
//! the stubs can't drift.

use net_sdk::tool::ToolDescriptor;

use super::schema::{self, Additional, ParsedSchema, Primitive, Schema};
use super::{module_basename, pascal_case, GenMeta, GeneratedFile};
use crate::error::CliError;

/// Render every descriptor into Python package files.
pub(super) fn generate(
    descriptors: &[ToolDescriptor],
    meta: &GenMeta,
    skipped: &mut Vec<String>,
) -> Result<Vec<GeneratedFile>, CliError> {
    let mut files = Vec::new();
    let mut modules: Vec<String> = Vec::new();

    for d in descriptors {
        match render_tool(d, meta) {
            Ok(tool) => {
                let base = module_basename(&d.tool_id);
                files.push(file(&format!("{base}/models.py"), tool.models_py));
                files.push(file(&format!("{base}/models.pyi"), tool.models_pyi));
                files.push(file(&format!("{base}/call.py"), tool.call_py));
                files.push(file(&format!("{base}/__init__.py"), tool.init_py));
                modules.push(base);
            }
            Err(reason) => {
                eprintln!("warning: tool `{}` skipped — {reason}", d.tool_id);
                skipped.push(d.tool_id.clone());
            }
        }
    }

    files.push(file("__init__.py", render_root_init(&modules)));
    files.push(file("_meta.json", render_meta_json(meta, &modules)));
    Ok(files)
}

fn file(rel_path: &str, contents: String) -> GeneratedFile {
    GeneratedFile {
        rel_path: rel_path.to_string(),
        contents,
    }
}

struct ToolFiles {
    models_py: String,
    models_pyi: String,
    call_py: String,
    init_py: String,
}

// ── declaration IR ──────────────────────────────────────────────────

/// A lowered Python declaration: a class or a type alias.
enum Decl {
    Class(ClassDecl),
    Alias(AliasDecl),
}

struct ClassDecl {
    name: String,
    doc: Option<String>,
    /// Base classes — `["BaseModel"]` normally, or the components of a
    /// top-level `allOf` for inheritance.
    bases: Vec<String>,
    fields: Vec<FieldDecl>,
    extra: Extra,
}

struct FieldDecl {
    /// Safe Python attribute name.
    name: String,
    /// Original schema property name, when it differs (→ `Field(alias=…)`).
    alias: Option<String>,
    ty: String,
    required: bool,
}

struct AliasDecl {
    name: String,
    doc: Option<String>,
    ty: String,
}

/// `additionalProperties` lowering for a class.
enum Extra {
    /// Absent `additionalProperties` — no `model_config` (Pydantic default).
    Default,
    /// `additionalProperties: false`.
    Forbid,
    /// `additionalProperties: true` / typed.
    Allow,
}

/// Lowers parsed schemas into a dependency-ordered [`Decl`] list, hoisting
/// inline objects into named classes.
struct Builder {
    prefix: String,
    decls: Vec<Decl>,
    needs_any: bool,
    needs_literal: bool,
}

impl Builder {
    fn new(prefix: &str) -> Self {
        Self {
            prefix: prefix.to_string(),
            decls: Vec::new(),
            needs_any: false,
            needs_literal: false,
        }
    }

    /// A Python type expression for `schema`, hoisting any inline object
    /// into a class named `suggested` (emitted before the referrer).
    fn ty(&mut self, schema: &Schema, suggested: &str) -> String {
        match schema {
            Schema::Primitive(p) => match p {
                Primitive::String => "str".into(),
                Primitive::Integer => "int".into(),
                Primitive::Number => "float".into(),
                Primitive::Boolean => "bool".into(),
                Primitive::Null => "None".into(),
            },
            Schema::Array(inner) => {
                format!("list[{}]", self.ty(inner, &format!("{suggested}Item")))
            }
            Schema::Tuple(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(i, s)| self.ty(s, &format!("{suggested}{i}")))
                    .collect();
                format!("tuple[{}]", parts.join(", "))
            }
            Schema::Object(obj) => {
                self.class_for(suggested, obj, None, vec!["BaseModel".into()]);
                suggested.to_string()
            }
            Schema::Enum(values) => self.literal(values),
            Schema::Const(v) => {
                self.needs_literal = true;
                format!("Literal[{}]", py_literal(v))
            }
            Schema::Union(branches) => {
                if branches.is_empty() {
                    self.needs_any = true;
                    return "Any".into();
                }
                branches
                    .iter()
                    .enumerate()
                    .map(|(i, s)| self.ty(s, &format!("{suggested}{i}")))
                    .collect::<Vec<_>>()
                    .join(" | ")
            }
            // Inline intersections don't lower to a Python type cleanly;
            // top-level allOf is handled as inheritance in `lower_named`.
            Schema::Intersection(_) => {
                self.needs_any = true;
                "Any".into()
            }
            Schema::Ref(name) => format!("{}{}", self.prefix, pascal_case(name)),
            Schema::Unknown => {
                self.needs_any = true;
                "Any".into()
            }
        }
    }

    fn literal(&mut self, values: &[serde_json::Value]) -> String {
        if values.is_empty() {
            self.needs_any = true;
            return "Any".into();
        }
        self.needs_literal = true;
        let lits: Vec<String> = values.iter().map(py_literal).collect();
        format!("Literal[{}]", lits.join(", "))
    }

    /// Build a class for `obj` and push it (children first via `ty`).
    fn class_for(
        &mut self,
        name: &str,
        obj: &schema::ObjectSchema,
        doc: Option<&str>,
        bases: Vec<String>,
    ) {
        let mut fields = Vec::with_capacity(obj.properties.len());
        for (prop, prop_schema) in &obj.properties {
            let ty = self.ty(prop_schema, &format!("{name}{}", pascal_case(prop)));
            let (safe, alias) = py_field_name(prop);
            fields.push(FieldDecl {
                name: safe,
                alias,
                ty,
                required: obj.required.contains(prop),
            });
        }
        let extra = match &obj.additional {
            Additional::Unspecified => Extra::Default,
            Additional::Denied => Extra::Forbid,
            // Typed extras aren't expressible as a Pydantic field; both
            // collapse to "extra allowed" (documented limitation).
            Additional::Allowed | Additional::Typed(_) => Extra::Allow,
        };
        self.decls.push(Decl::Class(ClassDecl {
            name: name.to_string(),
            doc: doc.map(str::to_string),
            bases,
            fields,
            extra,
        }));
    }

    /// Lower a top-level named schema (a `$def`, Request, or Response).
    fn lower_named(&mut self, name: &str, schema: &Schema, doc: Option<&str>) {
        match schema {
            Schema::Object(obj) => self.class_for(name, obj, doc, vec!["BaseModel".into()]),
            // `allOf` of refs → multiple inheritance.
            Schema::Intersection(parts)
                if parts.iter().all(|p| matches!(p, Schema::Ref(_))) && !parts.is_empty() =>
            {
                let bases: Vec<String> = parts
                    .iter()
                    .map(|p| match p {
                        Schema::Ref(n) => format!("{}{}", self.prefix, pascal_case(n)),
                        _ => "BaseModel".into(),
                    })
                    .collect();
                self.decls.push(Decl::Class(ClassDecl {
                    name: name.to_string(),
                    doc: doc.map(str::to_string),
                    bases,
                    fields: Vec::new(),
                    extra: Extra::Allow,
                }));
            }
            other => {
                let ty = self.ty(other, &format!("{name}Inner"));
                self.decls.push(Decl::Alias(AliasDecl {
                    name: name.to_string(),
                    doc: doc.map(str::to_string),
                    ty,
                }));
            }
        }
    }
}

// ── per-tool rendering ──────────────────────────────────────────────

fn render_tool(d: &ToolDescriptor, meta: &GenMeta) -> Result<ToolFiles, String> {
    let prefix = pascal_case(&d.tool_id);
    let req_name = format!("{prefix}Request");
    let resp_name = format!("{prefix}Response");

    let input = schema::parse(d.input_schema.as_deref().unwrap_or("{}"))
        .map_err(|e| format!("input schema: {e}"))?;
    let output = match &d.output_schema {
        Some(s) => Some(schema::parse(s).map_err(|e| format!("output schema: {e}"))?),
        None => None,
    };

    let mut b = Builder::new(&prefix);
    emit_defs(&mut b, &input);
    if let Some(o) = &output {
        emit_defs(&mut b, o);
    }
    b.lower_named(
        &req_name,
        &input.root,
        Some(
            &input
                .doc
                .clone()
                .unwrap_or_else(|| format!("Request body for `{}`.", d.tool_id)),
        ),
    );
    let has_request_model = matches!(input.root, Schema::Object(_));

    let has_response_model = match &output {
        Some(o) => {
            b.lower_named(
                &resp_name,
                &o.root,
                Some(
                    &o.doc
                        .clone()
                        .unwrap_or_else(|| format!("Response body for `{}`.", d.tool_id)),
                ),
            );
            matches!(o.root, Schema::Object(_))
        }
        None => {
            b.needs_any = true;
            b.decls.push(Decl::Alias(AliasDecl {
                name: resp_name.clone(),
                doc: Some(format!(
                    "Response body for `{}`. The descriptor carried no output schema.",
                    d.tool_id
                )),
                ty: "Any".into(),
            }));
            false
        }
    };

    let models_py = render_models_py(d, meta, &b);
    let models_pyi = render_models_pyi(&b);
    let call_py = render_call_py(
        d,
        &req_name,
        &resp_name,
        has_request_model,
        has_response_model,
    );
    let init_py = render_tool_init(&req_name, &resp_name, &module_basename(&d.tool_id));

    Ok(ToolFiles {
        models_py,
        models_pyi,
        call_py,
        init_py,
    })
}

fn emit_defs(b: &mut Builder, parsed: &ParsedSchema) {
    for (name, schema) in &parsed.defs {
        let ty_name = format!("{}{}", b.prefix, pascal_case(name));
        b.lower_named(&ty_name, schema, None);
    }
}

fn render_models_py(d: &ToolDescriptor, meta: &GenMeta, b: &Builder) -> String {
    let mut out = String::new();
    out.push_str(&docstring_header(d, meta));
    out.push_str("from __future__ import annotations\n");

    let needs_configdict = b
        .decls
        .iter()
        .any(|decl| matches!(decl, Decl::Class(c) if class_needs_config(c)));
    let needs_field = b
        .decls
        .iter()
        .any(|decl| matches!(decl, Decl::Class(c) if c.fields.iter().any(|f| f.alias.is_some())));

    let mut pydantic = vec!["BaseModel"];
    if needs_configdict {
        pydantic.push("ConfigDict");
    }
    if needs_field {
        pydantic.push("Field");
    }
    out.push_str(&format!("from pydantic import {}\n", pydantic.join(", ")));
    if let Some(line) = typing_import(b) {
        out.push_str(&line);
    }
    out.push('\n');

    for decl in &b.decls {
        match decl {
            Decl::Class(c) => out.push_str(&render_class_py(c)),
            Decl::Alias(a) => out.push_str(&render_alias_py(a)),
        }
        out.push('\n');
    }
    out
}

/// Does this class need a `model_config = ConfigDict(...)` line? True when
/// `additionalProperties` was specified (`extra=`) or any field carries an
/// alias (`populate_by_name=True`, so the model can be constructed by the
/// safe attribute name — what the `.pyi` stub advertises — not just the
/// alias).
fn class_needs_config(c: &ClassDecl) -> bool {
    !matches!(c.extra, Extra::Default) || c.fields.iter().any(|f| f.alias.is_some())
}

fn render_class_py(c: &ClassDecl) -> String {
    let mut s = format!("class {}({}):\n", c.name, c.bases.join(", "));
    if let Some(doc) = &c.doc {
        s.push_str(&format!("    \"\"\"{}\"\"\"\n", doc_safe(doc)));
    }
    let mut config_args: Vec<&str> = Vec::new();
    match c.extra {
        Extra::Allow => config_args.push("extra=\"allow\""),
        Extra::Forbid => config_args.push("extra=\"forbid\""),
        Extra::Default => {}
    }
    // A non-identifier / keyword property name is exposed as a safe attr +
    // `Field(alias=...)`; without `populate_by_name` Pydantic would reject
    // construction by that attr name at runtime, contradicting the stub.
    if c.fields.iter().any(|f| f.alias.is_some()) {
        config_args.push("populate_by_name=True");
    }
    let has_config = !config_args.is_empty();
    if has_config {
        s.push_str(&format!(
            "    model_config = ConfigDict({})\n",
            config_args.join(", ")
        ));
    }
    let mut body = false;
    for f in &c.fields {
        body = true;
        let value = field_default(f);
        s.push_str(&format!(
            "    {}: {}{}\n",
            f.name,
            field_annotation(f),
            value
        ));
    }
    // A class with no docstring, no config, and no fields still needs a body.
    if !body && !has_config && c.doc.is_none() {
        s.push_str("    pass\n");
    }
    s
}

fn render_alias_py(a: &AliasDecl) -> String {
    let mut s = String::new();
    if let Some(doc) = &a.doc {
        // A `#` comment can't span lines; flatten any newlines.
        s.push_str(&format!("# {}\n", doc.replace('\n', " ")));
    }
    s.push_str(&format!("{} = {}\n", a.name, a.ty));
    s
}

fn render_models_pyi(b: &Builder) -> String {
    let mut out = String::from("from __future__ import annotations\n");
    out.push_str("from pydantic import BaseModel\n");
    if let Some(line) = typing_import(b) {
        out.push_str(&line);
    }
    out.push('\n');
    for decl in &b.decls {
        match decl {
            Decl::Class(c) => {
                out.push_str(&format!("class {}({}):\n", c.name, c.bases.join(", ")));
                if c.fields.is_empty() {
                    out.push_str("    ...\n");
                } else {
                    for f in &c.fields {
                        // Optional fields get a real `= None` default. (`= ...`
                        // would be read as Pydantic's *required* Ellipsis
                        // sentinel by the mypy plugin, forcing callers to pass
                        // every optional field.) Every optional field's
                        // annotation already includes `| None`, so `None` is
                        // assignable.
                        let default = if f.required { "" } else { " = None" };
                        out.push_str(&format!(
                            "    {}: {}{}\n",
                            f.name,
                            field_annotation(f),
                            default
                        ));
                    }
                }
            }
            Decl::Alias(a) => out.push_str(&format!("{} = {}\n", a.name, a.ty)),
        }
        out.push('\n');
    }
    out
}

/// The annotation after the field name (`T` or `T | None`).
fn field_annotation(f: &FieldDecl) -> String {
    if f.required {
        f.ty.clone()
    } else {
        format!("{} | None", f.ty)
    }
}

/// The ` = …` value for a `.py` field (default / `Field(alias=…)`).
fn field_default(f: &FieldDecl) -> String {
    match (&f.alias, f.required) {
        (Some(alias), true) => format!(" = Field(alias=\"{alias}\")"),
        (Some(alias), false) => format!(" = Field(default=None, alias=\"{alias}\")"),
        (None, true) => String::new(),
        (None, false) => " = None".into(),
    }
}

fn typing_import(b: &Builder) -> Option<String> {
    let mut names = Vec::new();
    if b.needs_any {
        names.push("Any");
    }
    if b.needs_literal {
        names.push("Literal");
    }
    if names.is_empty() {
        None
    } else {
        Some(format!("from typing import {}\n", names.join(", ")))
    }
}

fn render_call_py(
    d: &ToolDescriptor,
    req_name: &str,
    resp_name: &str,
    has_request_model: bool,
    has_response_model: bool,
) -> String {
    let fn_name = format!("call_{}", module_basename(&d.tool_id));
    let dump = if has_request_model {
        "input.model_dump(exclude_none=True, by_alias=True)".to_string()
    } else {
        "input".to_string()
    };
    let body = if has_response_model {
        format!("    raw = await mesh.call(TOOL_ID, {dump})\n    return {resp_name}.model_validate(raw)\n")
    } else {
        // Response type is `Any`, so returning the raw dict needs no cast —
        // and a `# type: ignore` here would be flagged unused under
        // `mypy --strict`.
        format!("    return await mesh.call(TOOL_ID, {dump})\n")
    };
    format!(
        "from __future__ import annotations\n\
         from typing import Any, Protocol\n\
         from .models import {req_name}, {resp_name}\n\
         \n\
         \n\
         class _MeshLike(Protocol):\n\
         \x20   async def call(self, tool_id: str, input: dict[str, Any]) -> dict[str, Any]: ...\n\
         \n\
         \n\
         TOOL_ID = {tool_id}\n\
         VERSION = {version}\n\
         \n\
         \n\
         async def {fn_name}(mesh: _MeshLike, input: {req_name}) -> {resp_name}:\n\
         {body}",
        tool_id = py_str(&d.tool_id),
        version = py_str(&d.version),
    )
}

fn render_tool_init(req_name: &str, resp_name: &str, base: &str) -> String {
    format!(
        "from .models import {req_name}, {resp_name}\n\
         from .call import call_{base}, TOOL_ID, VERSION\n\
         \n\
         __all__ = [\n\
         \x20   \"{req_name}\",\n\
         \x20   \"{resp_name}\",\n\
         \x20   \"call_{base}\",\n\
         \x20   \"TOOL_ID\",\n\
         \x20   \"VERSION\",\n\
         ]\n"
    )
}

fn render_root_init(modules: &[String]) -> String {
    let mut s = String::from("# Auto-generated by `net-mesh typegen`. Do not edit by hand.\n");
    for m in modules {
        s.push_str(&format!("from . import {m} as {m}\n"));
    }
    s
}

fn render_meta_json(meta: &GenMeta, modules: &[String]) -> String {
    let value = serde_json::json!({
        "format_version": meta.format_version,
        "source": meta.source_label,
        "captured_at": meta.captured_at,
        "language": "python",
        "modules": modules,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into())
}

fn docstring_header(d: &ToolDescriptor, meta: &GenMeta) -> String {
    let version = if d.version.is_empty() {
        String::new()
    } else {
        format!(" v{}", doc_safe(&d.version))
    };
    format!(
        "\"\"\"\nAuto-generated by `net-mesh typegen`. Do not edit by hand.\n\
         Source: tool `{}`{}\n\
         Generated from {} @ {}\n\"\"\"\n",
        doc_safe(&d.tool_id),
        version,
        doc_safe(&meta.source_label),
        doc_safe(&meta.captured_at)
    )
}

/// Make a string safe to embed inside a Python `"""..."""` docstring:
/// backslashes (e.g. Windows paths — `\t`, `\U`…) become literal, and a
/// `"""` run can't close the docstring early.
fn doc_safe(s: &str) -> String {
    s.replace('\\', "\\\\").replace("\"\"\"", "\\\"\\\"\\\"")
}

// ── value / identifier helpers ──────────────────────────────────────

/// A JSON literal as a Python `Literal[...]` member.
fn py_literal(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => py_str(s),
        serde_json::Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        serde_json::Value::Null => "None".into(),
        other => other.to_string(),
    }
}

/// A Rust string as a Python double-quoted string literal.
fn py_str(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Map a schema property name to a `(safe_attr, alias_if_changed)`. Invalid
/// identifiers / Python keywords get a sanitized attr + the original kept as
/// a Pydantic `alias`.
fn py_field_name(name: &str) -> (String, Option<String>) {
    let valid = !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !is_py_keyword(name);
    if valid {
        return (name.to_string(), None);
    }
    let mut safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe.chars().next().is_none_or(|c| c.is_ascii_digit()) {
        safe.insert(0, '_');
    }
    if is_py_keyword(&safe) {
        safe.push('_');
    }
    (safe, Some(name.to_string()))
}

fn is_py_keyword(s: &str) -> bool {
    matches!(
        s,
        "False"
            | "None"
            | "True"
            | "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "break"
            | "class"
            | "continue"
            | "def"
            | "del"
            | "elif"
            | "else"
            | "except"
            | "finally"
            | "for"
            | "from"
            | "global"
            | "if"
            | "import"
            | "in"
            | "is"
            | "lambda"
            | "nonlocal"
            | "not"
            | "or"
            | "pass"
            | "raise"
            | "return"
            | "try"
            | "while"
            | "with"
            | "yield"
            | "match"
            | "case"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(input: &str, output: Option<&str>) -> ToolDescriptor {
        ToolDescriptor {
            tool_id: "acme/web_search".into(),
            name: "Web Search".into(),
            version: "1.2.0".into(),
            description: Some("Search the web".into()),
            input_schema: Some(input.into()),
            output_schema: output.map(str::to_string),
            requires: vec![],
            estimated_time_ms: 800,
            stateless: true,
            streaming: false,
            tags: vec!["search".into()],
            node_count: 1,
        }
    }

    fn meta() -> GenMeta {
        GenMeta {
            source_label: "snapshot tools.snapshot".into(),
            captured_at: "2026-06-04T10:00:00Z".into(),
            format_version: 1,
        }
    }

    #[test]
    fn models_py_emits_pydantic_classes() {
        let d = descriptor(
            r#"{"type":"object","properties":{"query":{"type":"string"},"max_results":{"type":"integer"}},"required":["query"]}"#,
            Some(
                r##"{"type":"object","properties":{"results":{"type":"array","items":{"$ref":"#/$defs/Result"}}},"$defs":{"Result":{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}}}"##,
            ),
        );
        let t = render_tool(&d, &meta()).expect("render");
        let py = &t.models_py;
        assert!(py.contains("from pydantic import BaseModel"), "{py}");
        assert!(py.contains("class AcmeWebSearchResult(BaseModel):"), "{py}");
        assert!(py.contains("url: str\n"), "{py}");
        assert!(
            py.contains("class AcmeWebSearchRequest(BaseModel):"),
            "{py}"
        );
        assert!(py.contains("query: str\n"), "{py}");
        assert!(py.contains("max_results: int | None = None\n"), "{py}");
        assert!(
            py.contains("results: list[AcmeWebSearchResult] | None = None\n"),
            "{py}"
        );
        // $def class precedes the Request that references it (forward-ref safe).
        let def_at = py.find("class AcmeWebSearchResult").unwrap();
        let resp_at = py.find("class AcmeWebSearchResponse").unwrap();
        assert!(def_at < resp_at, "Result must precede Response");
    }

    #[test]
    fn pyi_mirrors_classes() {
        let d = descriptor(
            r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#,
            None,
        );
        let t = render_tool(&d, &meta()).expect("render");
        assert!(
            t.models_pyi
                .contains("class AcmeWebSearchRequest(BaseModel):"),
            "{}",
            t.models_pyi
        );
        assert!(t.models_pyi.contains("q: str\n"), "{}", t.models_pyi);
        // No output schema → Response alias to Any.
        assert!(
            t.models_pyi.contains("AcmeWebSearchResponse = Any"),
            "{}",
            t.models_pyi
        );
    }

    #[test]
    fn pyi_optional_field_defaults_to_none_not_ellipsis() {
        let d = descriptor(
            r#"{"type":"object","properties":{"q":{"type":"string"},"limit":{"type":"integer"}},"required":["q"]}"#,
            None,
        );
        let t = render_tool(&d, &meta()).expect("render");
        // `= ...` is Pydantic's *required* sentinel to the mypy plugin; an
        // optional field must use a real `= None` default so callers can omit it.
        assert!(t.models_pyi.contains("limit: int | None = None"), "{}", t.models_pyi);
        assert!(!t.models_pyi.contains("= ..."), "{}", t.models_pyi);
        assert!(t.models_pyi.contains("q: str\n"), "{}", t.models_pyi);
    }

    #[test]
    fn enum_and_invalid_field_name() {
        let d = descriptor(
            r#"{"type":"object","properties":{"mode":{"enum":["fast","slow"]},"weird-name":{"type":"string"}},"required":["mode","weird-name"]}"#,
            None,
        );
        let t = render_tool(&d, &meta()).expect("render");
        let py = &t.models_py;
        assert!(py.contains("from typing import"), "{py}");
        assert!(py.contains("Literal"), "{py}");
        assert!(py.contains(r#"mode: Literal["fast", "slow"]"#), "{py}");
        // Invalid identifier → sanitized attr + Field(alias=...).
        assert!(py.contains("from pydantic import BaseModel, ConfigDict, Field"), "{py}");
        assert!(
            py.contains(r#"weird_name: str = Field(alias="weird-name")"#),
            "{py}"
        );
        // populate_by_name so the model is constructible by the safe attr
        // name the .pyi advertises (not only by the alias).
        assert!(
            py.contains("model_config = ConfigDict(populate_by_name=True)"),
            "{py}"
        );
    }

    #[test]
    fn alias_config_merges_with_extra() {
        // additionalProperties:false (→ extra="forbid") plus an aliased field
        // must yield a single merged ConfigDict.
        let d = descriptor(
            r#"{"type":"object","properties":{"weird-name":{"type":"string"}},"required":["weird-name"],"additionalProperties":false}"#,
            None,
        );
        let t = render_tool(&d, &meta()).expect("render");
        assert!(
            t.models_py
                .contains(r#"model_config = ConfigDict(extra="forbid", populate_by_name=True)"#),
            "{}",
            t.models_py
        );
    }

    #[test]
    fn call_helper_unary_and_no_output() {
        let with_out = descriptor(
            r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#,
            Some(r#"{"type":"object","properties":{"ok":{"type":"boolean"}}}"#),
        );
        let t = render_tool(&with_out, &meta()).expect("render");
        assert!(
            t.call_py.contains("async def call_acme_web_search("),
            "{}",
            t.call_py
        );
        assert!(
            t.call_py
                .contains("AcmeWebSearchResponse.model_validate(raw)"),
            "{}",
            t.call_py
        );
        assert!(
            t.call_py.contains("TOOL_ID = \"acme/web_search\""),
            "{}",
            t.call_py
        );

        let no_out = descriptor(
            r#"{"type":"object","properties":{"q":{"type":"string"}}}"#,
            None,
        );
        let t2 = render_tool(&no_out, &meta()).expect("render");
        assert!(
            t2.call_py.contains("return await mesh.call(TOOL_ID"),
            "{}",
            t2.call_py
        );
    }

    #[test]
    fn unsupported_schema_is_skipped() {
        let bad = descriptor(r#"{"not":{"type":"string"}}"#, None);
        let mut skipped = Vec::new();
        let files = generate(&[bad], &meta(), &mut skipped).expect("generate");
        assert_eq!(skipped, vec!["acme/web_search".to_string()]);
        assert!(files.iter().any(|f| f.rel_path == "__init__.py"));
        assert!(!files.iter().any(|f| f.rel_path.contains("/models.py")));
    }
}
