//! `#[tool]` procedural macro for `net-mesh-sdk`.
//!
//! Lets users declare an AI tool with a single attribute on an
//! existing async function:
//!
//! ```ignore
//! use net_sdk::tool::{self};
//! use net_sdk_macros::tool;
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! #[derive(JsonSchema, Deserialize, Serialize)]
//! struct WebSearchReq { query: String }
//!
//! #[derive(JsonSchema, Deserialize, Serialize)]
//! struct WebSearchResp { results: Vec<String> }
//!
//! #[tool(
//!     description = "Search the web for relevant pages.",
//!     tag = "web",
//!     tag = "research",
//!     stateless = true,
//!     estimated_time_ms = 500,
//! )]
//! async fn web_search(req: WebSearchReq) -> Result<WebSearchResp, String> {
//!     Ok(WebSearchResp { results: vec![format!("hit for {}", req.query)] })
//! }
//! ```
//!
//! The macro keeps the original async function intact AND generates
//! a sibling `<fn_name>_descriptor()` returning a `ToolDescriptor`
//! plus `<fn_name>_register(mesh)` that calls `mesh.serve_tool(...)`
//! atomically.
//!
//! All attribute args are optional:
//! - `name = "..."` — override the tool_id (defaults to the function name).
//! - `description = "..."` — human-readable.
//! - `version = "..."` — defaults to `"1.0.0"`.
//! - `tag = "..."` — repeatable; free-form classification tags.
//! - `stateless = true|false` — defaults to true.
//! - `estimated_time_ms = N` — soft latency hint (defaults to 0).
//!
//! Plan: slice A-7 in `docs/plans/NRPC_AI_TOOL_CALLING_AND_AGENT_DX.md`.

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{
    parse_macro_input, FnArg, ItemFn, LitBool, LitInt, LitStr, MetaNameValue, PathArguments,
    ReturnType, Token, Type,
};

/// Parsed args from `#[tool(...)]`.
struct ToolArgs {
    name: Option<LitStr>,
    description: Option<LitStr>,
    version: Option<LitStr>,
    stateless: Option<LitBool>,
    estimated_time_ms: Option<LitInt>,
    tags: Vec<LitStr>,
}

impl Parse for ToolArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut name = None;
        let mut description = None;
        let mut version = None;
        let mut stateless = None;
        let mut estimated_time_ms = None;
        let mut tags = Vec::new();

        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier key"))?
                .to_string();
            let value = pair.value;
            match key.as_str() {
                "name" => name = Some(parse_lit_str(value, "name")?),
                "description" => description = Some(parse_lit_str(value, "description")?),
                "version" => version = Some(parse_lit_str(value, "version")?),
                "stateless" => stateless = Some(parse_lit_bool(value, "stateless")?),
                "estimated_time_ms" => {
                    estimated_time_ms = Some(parse_lit_int(value, "estimated_time_ms")?);
                }
                "tag" => tags.push(parse_lit_str(value, "tag")?),
                other => {
                    return Err(syn::Error::new(
                        Span::call_site(),
                        format!(
                            "unknown #[tool] arg `{other}`; accepted: name, description, \
                             version, stateless, estimated_time_ms, tag (repeatable)"
                        ),
                    ))
                }
            }
        }

        Ok(Self {
            name,
            description,
            version,
            stateless,
            estimated_time_ms,
            tags,
        })
    }
}

fn parse_lit_str(expr: syn::Expr, key: &str) -> syn::Result<LitStr> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(s),
        ..
    }) = expr
    {
        Ok(s)
    } else {
        Err(syn::Error::new_spanned(
            expr,
            format!("`{key}` must be a string literal"),
        ))
    }
}

fn parse_lit_bool(expr: syn::Expr, key: &str) -> syn::Result<LitBool> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Bool(b),
        ..
    }) = expr
    {
        Ok(b)
    } else {
        Err(syn::Error::new_spanned(
            expr,
            format!("`{key}` must be a boolean literal"),
        ))
    }
}

fn parse_lit_int(expr: syn::Expr, key: &str) -> syn::Result<LitInt> {
    if let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Int(i),
        ..
    }) = expr
    {
        Ok(i)
    } else {
        Err(syn::Error::new_spanned(
            expr,
            format!("`{key}` must be an integer literal"),
        ))
    }
}

/// Mark an async function as an AI tool. See the crate-level doc
/// for the full attribute surface.
///
/// Expects the function signature to be
/// `async fn <name>(<req>: Req) -> Result<Resp, String>` (or any
/// error type — the `Result<_, E>` Ok type is what the macro
/// extracts for `Resp`). Both `Req` and `Resp` must implement
/// `schemars::JsonSchema` so `metadata_for::<Req, Resp>(name)` can
/// derive their JSON schemas at descriptor build time.
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as ToolArgs);
    let func = parse_macro_input!(item as ItemFn);
    match expand_tool(args, func) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand_tool(args: ToolArgs, func: ItemFn) -> syn::Result<proc_macro2::TokenStream> {
    if func.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &func.sig,
            "#[tool] requires an `async fn` — sync handlers aren't supported (the \
             SDK's `serve_tool` expects `Fn(Req) -> Future<Output = Result<Resp, _>>`)",
        ));
    }

    let fn_name = &func.sig.ident;
    let fn_name_string = fn_name.to_string();
    let tool_id: LitStr = match &args.name {
        Some(n) => n.clone(),
        None => LitStr::new(&fn_name_string, fn_name.span()),
    };

    // Extract Req from the first positional arg.
    let req_ty = extract_req_type(&func)?;
    // Extract Resp from `-> Result<Resp, _>`.
    let resp_ty = extract_resp_type(&func)?;

    // Builder chain — each setter is omitted when the user didn't
    // supply that attribute, keeping the generated code close to a
    // hand-written `metadata_for(...).description(...).build()`.
    let description_setter = args.description.as_ref().map(|d| {
        quote! { let __builder = __builder.description(#d); }
    });
    let version_setter = args.version.as_ref().map(|v| {
        quote! { let __builder = __builder.version(#v); }
    });
    let stateless_setter = args.stateless.as_ref().map(|s| {
        quote! { let __builder = __builder.stateless(#s); }
    });
    let estimated_setter = args.estimated_time_ms.as_ref().map(|e| {
        quote! { let __builder = __builder.estimated_time_ms(#e); }
    });
    let tag_setters: Vec<_> = args
        .tags
        .iter()
        .map(|t| quote! { let __builder = __builder.tag(#t); })
        .collect();

    let descriptor_fn = format_ident!("{}_descriptor", fn_name);
    let register_fn = format_ident!("{}_register", fn_name);

    Ok(quote! {
        #func

        /// Auto-generated by `#[tool]`. Returns the
        /// `ToolDescriptor` for this tool — same shape every
        /// `metadata_for::<Req, Resp>(name).build()` produces.
        #[doc(hidden)]
        #[allow(non_snake_case, dead_code)]
        pub fn #descriptor_fn() -> ::net_sdk::tool::ToolDescriptor {
            let __builder = ::net_sdk::tool::metadata_for::<#req_ty, #resp_ty>(#tool_id);
            #description_setter
            #version_setter
            #stateless_setter
            #estimated_setter
            #(#tag_setters)*
            __builder.build()
        }

        /// Auto-generated by `#[tool]`. Registers this tool on the
        /// given `Mesh`. Returns a `ToolServeHandle`; drop it (or
        /// call `.close()` via the underlying `ServeHandle`) to
        /// unregister.
        #[doc(hidden)]
        #[allow(non_snake_case, dead_code)]
        pub fn #register_fn(
            __mesh: &::net_sdk::mesh::Mesh,
        ) -> ::std::result::Result<
            ::net_sdk::tool::ToolServeHandle,
            ::net_sdk::mesh_rpc::ServeError,
        > {
            __mesh.serve_tool::<#req_ty, #resp_ty, _, _>(
                #descriptor_fn(),
                #fn_name,
            )
        }
    })
}

/// Pull the request type out of the first positional arg.
fn extract_req_type(func: &ItemFn) -> syn::Result<Type> {
    let first = func.sig.inputs.first().ok_or_else(|| {
        syn::Error::new_spanned(
            &func.sig,
            "#[tool] requires exactly one positional argument (`req: Req`)",
        )
    })?;
    match first {
        FnArg::Typed(pat_ty) => Ok((*pat_ty.ty).clone()),
        FnArg::Receiver(_) => Err(syn::Error::new_spanned(
            first,
            "#[tool] doesn't support `self` — declare the tool as a free function",
        )),
    }
}

/// Pull the success type from `-> Result<Resp, _>`.
fn extract_resp_type(func: &ItemFn) -> syn::Result<Type> {
    let ret_ty =
        match &func.sig.output {
            ReturnType::Default => return Err(syn::Error::new_spanned(
                &func.sig,
                "#[tool] requires the function to return `Result<Resp, _>` (no return type found)",
            )),
            ReturnType::Type(_, ty) => ty,
        };
    let path =
        match ret_ty.as_ref() {
            Type::Path(tp) => &tp.path,
            _ => return Err(syn::Error::new_spanned(
                ret_ty,
                "#[tool] requires the return type to be `Result<Resp, _>` (got a non-path type)",
            )),
        };
    let last = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ret_ty, "#[tool] requires a non-empty return type path")
    })?;
    if last.ident != "Result" {
        return Err(syn::Error::new_spanned(
            ret_ty,
            "#[tool] requires the return type to be a `Result<Resp, _>` (use \
             `Result<Resp, String>` for the canonical error contract)",
        ));
    }
    let generics = match &last.arguments {
        PathArguments::AngleBracketed(g) => g,
        _ => {
            return Err(syn::Error::new_spanned(
                &last.arguments,
                "#[tool] requires `Result` to have generic arguments",
            ))
        }
    };
    let first = generics.args.first().ok_or_else(|| {
        syn::Error::new_spanned(generics, "#[tool] requires `Result<Resp, _>` (no args)")
    })?;
    match first {
        syn::GenericArgument::Type(ty) => Ok(ty.clone()),
        _ => Err(syn::Error::new_spanned(
            first,
            "#[tool] requires `Result<Resp, _>`'s first generic to be a type",
        )),
    }
}
