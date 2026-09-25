//! The control-plane boundary must stay a boundary.
//!
//! `control_plane.rs` states two rules. Neither is enforced by the
//! compiler, and both fail *silently* — the code keeps working
//! exactly as before and the serverless follow-on quietly stops
//! being an implementation:
//!
//! 1. **No anchor type crosses the trait.** No `PeerAddr`, no
//!    `PeerAddr::Rtc`, no anchor handle, no HTTP or WebSocket type.
//!    An address may appear only as an opaque string the
//!    implementation itself minted.
//! 2. **No Net data packet crosses it.** A control plane carries
//!    SDP, ICE candidates, signed announcements and signed
//!    signalling envelopes. One that forwarded packets would be a
//!    relay wearing a trait, and the anchorless mock would prove
//!    nothing.
//!
//! There is a third property this file exists to keep true, and it
//! is the one a refactor breaks first: **the bindgen surface must
//! not do the control plane's job itself.** Before Stage 5 slice 2
//! `wasm.rs` held the `fetch` calls, the trickle socket and the
//! listener's route strings inline, so the trait existed while
//! nothing used it. One inlined HTTP call is how a boundary stops
//! being one.
//!
//! # Why some of this is a source scan
//!
//! Two of the checks are free and already type-level, and this file
//! says so rather than re-asserting them:
//!
//! - **The trait module compiles natively.** `control_plane.rs` is
//!   not `#[cfg(target_arch = "wasm32")]`, so a trait method that
//!   took a `web_sys::Response`, a `WebSocket` or any other browser
//!   type would fail to compile this very test binary. The native
//!   build IS the assertion.
//! - **A control plane needs no anchor state.** [`NoAnchor`] below
//!   implements the whole trait over a zero-sized type. If a method
//!   ever required something only an anchor can supply, `NoAnchor`
//!   could not implement it and this file would not compile.
//!
//! What remains — that no *nameable* forbidden type appears in a
//! signature, and that the bindgen surface stops doing HTTP — cannot
//! be expressed in the type system, because the failure mode is
//! code that compiles perfectly.

use std::path::{Path, PathBuf};

use net_leaf::control_plane::{
    BootstrapAccepted, ControlEvent, ControlPlane, DialogId, IceCandidate, NodeId, Sdp,
    SignalEnvelope, SignedAnnouncement,
};
use net_leaf::error::LeafError;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(name: &str) -> String {
    let path = manifest_dir().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()))
}

/// A module's source with line comments removed.
///
/// The scans below look for *code*. Every one of these modules
/// explains in prose what it does not do — `control_plane.rs`'s
/// doc-comment is largely about `PeerAddr::Rtc` not crossing it —
/// so a scan over raw text would flag the documentation that exists
/// to prevent the bug.
fn code_only(body: &str) -> String {
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `src/`, recursively, as `(name, source)`.
///
/// The name is the path relative to `src/`, `/`-separated, so a
/// helper dropped in a subdirectory is scanned and blamed under the
/// name it would ship as. `lib.rs` is a module like any other — it
/// is where the type-alias forwarding escape lived while the scans
/// read only the files a flat `read_dir` happened to name.
fn source_tree() -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("{} is readable: {e}", dir.display()))
            .map(|entry| entry.expect("dir entry").path())
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .expect("utf-8 file name")
                .to_string();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if path.is_dir() {
                walk(&path, &rel, out);
            } else if name.ends_with(".rs") {
                out.push((
                    rel,
                    std::fs::read_to_string(&path).expect("module is readable"),
                ));
            }
        }
    }
    let mut out = Vec::new();
    walk(&manifest_dir().join("src"), "", &mut out);
    out
}

/// The `pub trait ControlPlane { … }` block, code only.
fn trait_body() -> String {
    let code = code_only(&source("control_plane.rs"));
    let start = code
        .find("pub trait ControlPlane {")
        .expect("control_plane.rs must declare `pub trait ControlPlane`");
    let tail = &code[start..];
    let end = tail
        .find("\n}")
        .expect("the trait block must close at column zero");
    tail[..end].to_string()
}

/// The `impl ControlPlane for NoAnchor { … }` block in this file.
fn impl_body() -> String {
    let code = code_only(&std::fs::read_to_string(file!()).expect("this file is readable"));
    let start = code
        .find("impl ControlPlane for NoAnchor {")
        .expect("this file must declare the NoAnchor impl");
    let tail = &code[start..];
    let end = tail
        .find("\n}")
        .expect("the impl block must close at column zero");
    tail[..end].to_string()
}

/// The `fn …` signatures inside a block, bodies excluded: each is
/// cut at its body's `{` or its terminating `;`, both at type depth
/// (the `;` inside `[u8; 32]` is not a terminator).
fn signatures(block: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("fn ") {
        let tail = &rest[at..];
        let chars: Vec<char> = tail.chars().collect();
        let mut depth = 0i32;
        let mut end = tail.len();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '(' | '<' | '[' => depth += 1,
                ')' | '>' | ']' => depth -= 1,
                // `->` is not a generic's `>`.
                '-' if i + 1 < chars.len() && chars[i + 1] == '>' => i += 1,
                '{' | ';' if depth == 0 => {
                    end = tail
                        .char_indices()
                        .nth(i)
                        .map(|(idx, _)| idx)
                        .unwrap_or(tail.len());
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        found.push(tail[..end].to_string());
        rest = &tail[end.max(1)..];
    }
    found
}

/// Every capitalized identifier in `text` — the type vocabulary a
/// signature uses.
fn capitalized_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for character in text.chars() {
        if character.is_ascii_uppercase()
            || (!current.is_empty() && (character.is_ascii_alphanumeric() || character == '_'))
        {
            current.push(character);
        } else if !current.is_empty() {
            tokens.push(core::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Every identifier in `text`, at any case, lifetimes skipped.
///
/// `capitalized_tokens` above is the vocabulary scan's first cut.
/// The type scan below needs the lowercase half too: `type h =
/// ::crate::session::LeafSession;` is a forbidden type that names no
/// capitalized identifier at all.
fn identifiers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // A lifetime (`'a`, `'static`), not a type name.
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            tokens.push(chars[start..i].iter().collect());
        } else {
            i += 1;
        }
    }
    tokens
}

/// The type names a type expression mentions: its identifiers minus
/// path segments (`crate::session::LeafSession` mentions only
/// `LeafSession`) and language keywords.
fn tail_types(text: &str) -> Vec<String> {
    const KEYWORDS: [&str; 12] = [
        "dyn", "impl", "mut", "const", "for", "where", "async", "fn", "pub", "use", "type",
        "extern",
    ];
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
        } else if c.is_ascii_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let token: String = chars[start..i].iter().collect();
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_segment = j + 1 < chars.len() && chars[j] == ':' && chars[j + 1] == ':';
            if !is_segment && !KEYWORDS.contains(&token.as_str()) {
                tokens.push(token);
            }
        } else {
            i += 1;
        }
    }
    tokens
}

/// What a type binding does. `type X = …` and `use … as X` exist
/// purely to rename — banned outright when they rename a node type.
/// `struct X(…)` exists to wrap — a real type a module may own, but
/// one whose name still names its fields' types wherever it appears.
#[derive(PartialEq, Eq, Clone, Copy)]
enum BindingKind {
    Rename,
    Wrapper,
}

/// Every name a type can hide behind, across EVERY module including
/// `lib.rs`: line-initial `type X = …;` aliases (any visibility,
/// `impl` blocks' associated types included), `use … as X` import
/// renames, and `struct` wrappers, each mapped to the type names its
/// declaration mentions. This is the type normalization the scans
/// resolve through — a deny-list of spellings is defeated by
/// `type PeerTable = crate::node::LeafNode;` the moment the alias
/// lives where the scan does not look, so the scans look everywhere
/// and resolve instead.
fn type_bindings(tree: &[(String, String)]) -> Vec<(BindingKind, String, Vec<String>)> {
    let mut bindings: Vec<(BindingKind, String, Vec<String>)> = Vec::new();
    for (_, body) in tree {
        let code = code_only(body);
        for line in code.lines() {
            let line = line.trim();
            let words: Vec<&str> = line.split_whitespace().collect();
            // `type X = …;` (or `pub [crate] type X = …;`).
            let is_alias = words.first() == Some(&"type")
                || (words.first().is_some_and(|w| w.starts_with("pub"))
                    && words.get(1) == Some(&"type"));
            if is_alias {
                if let Some(eq) = line.find(" = ") {
                    let name = line[..eq]
                        .split_whitespace()
                        .next_back()
                        .unwrap_or("")
                        .split('<')
                        .next()
                        .unwrap_or("");
                    if !name.is_empty() {
                        bindings.push((
                            BindingKind::Rename,
                            name.to_string(),
                            tail_types(&line[eq + 3..]),
                        ));
                    }
                }
                continue;
            }
            // `use …::X as Y;` and `use …::{X as Y, …}`.
            let is_import = words.first() == Some(&"use")
                || (words.first().is_some_and(|w| w.starts_with("pub"))
                    && words.get(1) == Some(&"use"));
            if is_import {
                let parts: Vec<&str> = line.split(" as ").collect();
                for (prev, next) in parts.iter().zip(parts.iter().skip(1)) {
                    let alias: String = next
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let original: String = prev
                        .chars()
                        .rev()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect();
                    if !alias.is_empty() && alias != "_" && !original.is_empty() {
                        bindings.push((BindingKind::Rename, alias, vec![original]));
                    }
                }
            }
        }
        // `struct X(…)` / `struct X { … }`: a wrapper names its
        // fields' types without naming them at the use site.
        let chars: Vec<char> = code.chars().collect();
        let mut i = 0;
        while i + 6 <= chars.len() {
            let keyword = chars[i..i + 6] == ['s', 't', 'r', 'u', 'c', 't'];
            let bounded = i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
            if keyword && bounded {
                i += 6;
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                // Skip the generic parameters, if any.
                if i < chars.len() && chars[i] == '<' {
                    let mut depth = 0i32;
                    while i < chars.len() {
                        match chars[i] {
                            '<' => depth += 1,
                            '>' => {
                                depth -= 1;
                                if depth == 0 {
                                    i += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                }
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                if i >= chars.len() {
                    break;
                }
                let (open, close) = match chars[i] {
                    '(' => ('(', ')'),
                    '{' => ('{', '}'),
                    ';' => {
                        i += 1;
                        continue;
                    }
                    _ => {
                        i += 1;
                        continue;
                    }
                };
                let inner_start = i + 1;
                let mut depth = 0i32;
                let mut j = i;
                while j < chars.len() {
                    match chars[j] {
                        c if c == open => depth += 1,
                        c if c == close => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner: String = chars[inner_start..j.min(chars.len())].iter().collect();
                let mut targets = Vec::new();
                for field in split_top_level(&inner) {
                    let ty = match open {
                        '(' => field,
                        _ => field
                            .split_once(':')
                            .map(|(_, t)| t.to_string())
                            .unwrap_or(field),
                    };
                    targets.extend(tail_types(&ty));
                }
                if !name.is_empty() {
                    bindings.push((BindingKind::Wrapper, name, targets));
                }
                i = j + 1;
                continue;
            }
            i += 1;
        }
    }
    bindings
}

/// Split `text` at top-level commas; nested `<>`, `()` and `[]` stay
/// together.
fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' | '<' | '[' | '{' => depth += 1,
            ')' | '>' | ']' | '}' => depth -= 1,
            '-' if chars.peek() == Some(&'>') => {
                chars.next();
                current.push_str("->");
                continue;
            }
            ',' if depth == 0 => {
                parts.push(core::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    parts
}

/// Resolve `name` to the plain type names it finally stands for.
/// Renames chain arbitrarily deep (`type A = B; type B = LeafNode;`);
/// a `struct` wrapper is unwrapped at most once along the way — the
/// escapes this closes are the rename (`type PeerTable =
/// crate::node::LeafNode;`) and one wrapper hop (`struct
/// PeerTable(crate::node::LeafNode);`).
fn expand_type(bindings: &[(BindingKind, String, Vec<String>)], name: &str) -> Vec<String> {
    fn go(
        bindings: &[(BindingKind, String, Vec<String>)],
        name: &str,
        hops: u8,
        depth: u8,
        out: &mut Vec<String>,
    ) {
        if depth > 32 {
            // An alias cycle cannot compile in the crate; fail closed
            // by keeping the name itself.
            out.push(name.to_string());
            return;
        }
        // A node type is a terminus: `type PeerTable = LeafNode;`
        // resolves to `LeafNode`, not to `LeafNode`'s own fields —
        // unwrapping it would erase the very name being searched for.
        if DATA_PATH_TYPES.contains(&name) {
            out.push(name.to_string());
            return;
        }
        let mut followed = false;
        for (kind, bound, targets) in bindings {
            if bound != name {
                continue;
            }
            match kind {
                BindingKind::Rename => {
                    followed = true;
                    for target in targets {
                        go(bindings, target, hops, depth + 1, out);
                    }
                }
                BindingKind::Wrapper if hops > 0 => {
                    followed = true;
                    for target in targets {
                        go(bindings, target, hops - 1, depth + 1, out);
                    }
                }
                BindingKind::Wrapper => {}
            }
        }
        if !followed {
            out.push(name.to_string());
        }
    }
    let mut out = Vec::new();
    go(bindings, name, 1, 0, &mut out);
    out
}

/// The node and data-path types a control-plane implementation must
/// never name — spelled as themselves or reached through anything.
const DATA_PATH_TYPES: [&str; 6] = [
    "LeafNode",
    "LeafSession",
    "RtcLeafTransport",
    "ParsedPacket",
    "NetSession",
    "PacketBuilder",
];

/// Whether `name` is a node type: named directly, aliased to one, or
/// a `struct` wrapper around one.
fn resolves_to_data_path(bindings: &[(BindingKind, String, Vec<String>)], name: &str) -> bool {
    DATA_PATH_TYPES.contains(&name)
        || expand_type(bindings, name)
            .iter()
            .any(|n| DATA_PATH_TYPES.contains(&n.as_str()))
}

/// The type text a signature mentions: each parameter's type after
/// its `:`, and the return type after `->`. Receivers (`&self`) are
/// not types this vocabulary is about.
fn signature_types(signature: &str) -> Vec<String> {
    let chars: Vec<char> = signature.chars().collect();
    let mut depth = 0i32;
    let mut params: Option<(usize, usize)> = None;
    let mut ret_start: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' | '<' | '[' => {
                if chars[i] == '(' && params.is_none() && ret_start.is_none() && depth == 0 {
                    // Opening of the parameter list.
                    params = Some((i + 1, usize::MAX));
                }
                depth += 1;
            }
            ')' | '>' | ']' => {
                depth -= 1;
                if chars[i] == ')' && depth == 0 {
                    if let Some((start, end)) = params {
                        if end == usize::MAX {
                            params = Some((start, i));
                        }
                    }
                }
            }
            '-' if i + 1 < chars.len() && chars[i + 1] == '>' => {
                if params.is_some() && ret_start.is_none() {
                    ret_start = Some(i + 2);
                }
                i += 2;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let mut types = Vec::new();
    if let Some((start, end)) = params {
        if end != usize::MAX {
            for part in split_top_level(&chars[start..end].iter().collect::<String>()) {
                if let Some((name, ty)) = part.split_once(':') {
                    if !name.trim().ends_with("self") {
                        types.push(ty.to_string());
                    }
                }
                // A receiver without a colon (`&self`) names no type.
            }
        }
    }
    if let Some(start) = ret_start {
        let ret: String = chars[start..].iter().collect();
        types.push(ret.split(" where ").next().unwrap_or(&ret).to_string());
    }
    types
}

/// Beyond `VOCABULARY`, the names a signature may use: primitives
/// and type-system keywords. The lowercase entries matter — a
/// lowercase name is exactly what the capitalized scan could not see.
const PRIMITIVES: [&str; 15] = [
    "str", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "usize", "isize",
    "bool", "f64",
];
const KEYWORDS: [&str; 13] = [
    "dyn", "impl", "mut", "const", "for", "where", "async", "fn", "pub", "use", "type", "extern",
    "Self",
];

/// Every type the trait and its implementations may name. Adding one
/// is a deliberate act here — that is the closure.
const VOCABULARY: [&str; 15] = [
    "Sdp",
    "BootstrapAccepted",
    "LeafError",
    "DialogId",
    "IceCandidate",
    "SignedAnnouncement",
    "SignalEnvelope",
    "ControlEvent",
    "Vec",
    "Result",
    "Future",
    "Output",
    "Self",
    "ControlPlane",
    "NoAnchor",
];

/// Rule 1, on the signatures: nothing an anchor owns is nameable in
/// the trait.
///
/// The list is not decoration. `PeerAddr`/`RtcPeerId` is the session
/// table's key and a browser has none; `Credential`, `AnchorInfo`
/// and `OfferAccepted` are the 4b listener's wire shapes; `Request`,
/// `Response`, `WebSocket` and `Url` are the transport that happens
/// to be HTTP today; `RtcPeerConnection` and `RtcDataChannel` are
/// the browser's. A Tier A control plane backed by a signed object
/// in blob storage has none of them, and each one that appeared here
/// would be a thing it had to fake.
#[test]
fn no_anchor_type_is_nameable_in_the_trait() {
    let body = trait_body();
    for forbidden in [
        "PeerAddr",
        "RtcPeerId",
        "Credential",
        "AnchorInfo",
        "OfferAccepted",
        "Request",
        "Response",
        "WebSocket",
        "Url",
        "RtcPeerConnection",
        "RtcDataChannel",
        "JsValue",
        "web_sys",
        "js_sys",
        "attempt_token",
        "bootstrap_url",
    ] {
        assert!(
            !body.contains(forbidden),
            "`{forbidden}` appears in the ControlPlane trait. The boundary's \
             first rule is that no anchor type crosses it — an address may \
             appear only as an opaque string the implementation minted.\n\
             trait body:\n{body}"
        );
    }

    // The named-half escapes, closed. Two were open here: a
    // fully-qualified path (`session: crate::session::LeafSession`)
    // contains no `use crate::session` and no bare `LeafSession`, and
    // a type neither deny-list has heard of (`AnyTypeAtAll`) matches
    // nothing at all. So the trait may contain no path separator, and
    // every capitalized identifier in it must be the known carrier
    // vocabulary — a new one fails closed until it is added here
    // consciously.
    assert!(
        !body.contains("::"),
        "`::` appears in the ControlPlane trait: a fully-qualified path is how a \
         forbidden type gets past a name scan (`crate::session::LeafSession`).\n\
         trait body:\n{body}"
    );
    for token in capitalized_tokens(&body) {
        assert!(
            VOCABULARY.contains(&token.as_str()),
            "`{token}` is a type name the boundary's vocabulary does not know. \
             Rule 1 is only as strong as its lists; this one is exhaustive over \
             the names the trait may use.\ntrait body:\n{body}"
        );
    }

    // The same two rules on the zero-sized impl's SIGNATURES — the
    // second escape: an `impl` must name the TYPES its parameters
    // have, so whatever the trait forces every implementor to write
    // appears in `NoAnchor`'s signatures and is caught by the same
    // exhaustive scan (a trait parameter of `AnyTypeAtAll` cannot be
    // implemented without naming it).
    for signature in signatures(&impl_body()) {
        assert!(
            !signature.contains("::"),
            "the NoAnchor impl's signature names a path: {signature}"
        );
        for token in capitalized_tokens(&signature) {
            assert!(
                VOCABULARY.contains(&token.as_str()),
                "`{token}` appears in a NoAnchor impl signature — a type the trait \
                 forces every implementor to name, and the vocabulary does not \
                 know it.\nsignature: {signature}"
            );
        }
    }

    // …and the same rule type-normalized, which is what closes the
    // lowercase escape: `type h = ::crate::session::LeafSession;` at
    // module level names no capitalized identifier and puts its `::`
    // outside the trait body, so both halves above walked past it.
    // The scan is now over the TYPE TEXT of every signature (each
    // parameter after its `:`, the return after `->`) at every
    // spelling, resolved through the crate's `type`-alias, import and
    // struct-wrapper chains before it is judged — and a name that
    // resolves to nothing the vocabulary knows fails closed.
    let bindings = type_bindings(&source_tree());
    for signature in signatures(&trait_body())
        .into_iter()
        .chain(signatures(&impl_body()))
    {
        for ty in signature_types(&signature) {
            for token in identifiers(&ty) {
                if VOCABULARY.contains(&token.as_str())
                    || PRIMITIVES.contains(&token.as_str())
                    || KEYWORDS.contains(&token.as_str())
                {
                    continue;
                }
                let expanded = expand_type(&bindings, &token);
                let known = expanded.iter().all(|n| {
                    VOCABULARY.contains(&n.as_str())
                        || PRIMITIVES.contains(&n.as_str())
                        || KEYWORDS.contains(&n.as_str())
                });
                assert!(
                    known,
                    "`{token}` in a ControlPlane signature resolves to {expanded:?} — \
                     types outside the carrier vocabulary, whatever spelling they \
                     arrived under (the lowercase `type h = …` escape). Rule 1 is \
                     only as strong as its lists; this one is exhaustive over the \
                     names the trait may use, alias-normalized.\nsignature: {signature}"
                );
            }
        }
    }
}

/// Rule 2, on the signatures: no Net packet type is nameable either.
///
/// `Bytes` is the wire crate's packet payload type and
/// `ParsedPacket`/`NetSession`/`PacketBuilder` are the data path. The
/// trait's byte-carrying types are `SignedAnnouncement` and
/// `SignalEnvelope::payload`, both `Vec<u8>`, both signed by the leaf
/// — which is exactly the difference between carrying an
/// authenticated blob and relaying a session's traffic.
///
/// Enforced on names AND on shape. A name-level ban cannot see
/// `blob: Vec<u8>`: a neutral parameter carrying net-packet bytes
/// past every deny-list above. A raw byte-buffer type in any
/// signature is refused whatever it is called.
#[test]
fn no_net_packet_type_is_nameable_in_the_trait() {
    let body = trait_body();
    for forbidden in [
        "ParsedPacket",
        "NetSession",
        "PacketBuilder",
        "Outbound",
        "packet",
        "datagram",
        "Bytes",
    ] {
        assert!(
            !body.contains(forbidden),
            "`{forbidden}` appears in the ControlPlane trait. A control plane \
             that carried Net packets would be a relay wearing a trait, and \
             the anchorless mock would prove nothing.\ntrait body:\n{body}"
        );
    }
    // The shape half: a raw byte buffer IS a packet payload under any
    // name, so the parameter and return types may not be one.
    for signature in signatures(&trait_body())
        .into_iter()
        .chain(signatures(&impl_body()))
    {
        let flat: String = signature.chars().filter(|c| !c.is_whitespace()).collect();
        for shape in ["Vec<u8>", "[u8]", "[u8;", "*constu8", "*mutu8"] {
            assert!(
                !flat.contains(shape),
                "`{shape}` appears in a ControlPlane signature — a raw \
                 byte-buffer type is a Net packet payload under any name \
                 (the neutral `blob: Vec<u8>` escape), and a control plane \
                 that carried one would be a relay wearing a trait.\n\
                 The trait's byte-carrying types are `SignedAnnouncement` and \
                 `SignalEnvelope::payload` — named, signed carriers.\n\
                 signature: {signature}"
            );
        }
    }
}

/// The trait's module imports nothing from the anchor, the browser
/// or the wire.
///
/// A signature can stay clean while the module grows a
/// `use crate::bootstrap::Credential` for a helper — and the next
/// person puts it in a signature.
#[test]
fn the_trait_module_imports_nothing_from_the_anchor_or_the_wire() {
    let code = code_only(&source("control_plane.rs"));
    for forbidden in [
        "use net_wire",
        "use crate::bootstrap",
        "use crate::rtc",
        "use crate::session",
        "use crate::node",
        "web_sys",
        "js_sys",
        "wasm_bindgen",
    ] {
        assert!(
            !code.contains(forbidden),
            "control_plane.rs references `{forbidden}`. The trait is the one \
             module that must stay implementable by something with no anchor, \
             no browser and no session"
        );
    }
}

/// **The boundary is actually used.** `wasm.rs` performs no HTTP, no
/// WebSocket and knows none of the listener's routes.
///
/// This is the check that would have failed before slice 2 while
/// every other one passed: the trait existed and the connect
/// sequence ignored it.
#[test]
fn the_bindgen_surface_does_no_transport_of_its_own() {
    let code = code_only(&source("wasm.rs"));
    for forbidden in [
        "fetch_with_str",
        "fetch_with_request",
        "RequestInit",
        "WebSocket",
        "web_sys::Response",
        "/rtc/offer",
        "/rtc/anchor",
        "/rtc/trickle",
        "wss://",
        "attempt_token",
        "AnchorInfo",
        "OfferAccepted",
        // The named escape: moving the calls into a helper module —
        // `gloo_net::http`, an `XMLHttpRequest`, a global `fetch` —
        // matched none of the spellings above.
        "gloo_net",
        "XMLHttpRequest",
        "EventSource",
    ] {
        assert!(
            !code.contains(forbidden),
            "wasm.rs contains `{forbidden}`. The bindgen surface drives the \
             ControlPlane trait; the moment it speaks the anchor's transport \
             itself, the trait is decoration and the serverless follow-on is \
             a leaf refactor again"
        );
    }
    // And the same rule over EVERY module that is not the transport
    // owner — the escape this closes twice over: the calls moved to
    // a helper module `wasm.rs` merely invokes (and spelled
    // `web_sys::WebSocket::new("wss://…")`, matched nothing here),
    // and a helper dropped under `src/<subdir>/` was outside a flat
    // `read_dir` of `src/` entirely. The walk is now recursive over
    // every `.rs` below `src/`, floored so a walk that silently sees
    // nothing fails, and the old four-module exemption is down to
    // one: `rtc.rs`, `storage.rs` and `bootstrap.rs` are browser
    // modules, not HTTP owners, and an exemption is exactly where a
    // fetch hides. The one exemption left, `anchor_control_plane.rs`,
    // is fenced — it may speak the network only through the audited
    // call spellings, and it must still be speaking them, so the
    // exemption cannot outlive the code it was granted for.
    let tree = source_tree();
    let mut checked = 0;
    for (name, body) in &tree {
        if name == "anchor_control_plane.rs" {
            continue;
        }
        checked += 1;
        let helper = code_only(body);
        for forbidden in [
            "gloo_net",
            "XMLHttpRequest",
            "EventSource",
            "fetch_with_",
            ".fetch(",
            "::fetch(",
            "WebSocket::new(",
            "wss://",
            "ws://",
        ] {
            assert!(
                !helper.contains(forbidden),
                "{name} references `{forbidden}`. HTTP and WebSocket belong to \
                 the one transport owner (`anchor_control_plane`); a helper \
                 module that speaks them is the bindgen surface speaking them"
            );
        }
    }
    assert!(checked > 10, "only {checked} modules were inspected");
    // The fence around the single exemption.
    let owner = code_only(&source("anchor_control_plane.rs"));
    for forbidden in [
        "gloo_net",
        "XMLHttpRequest",
        "EventSource",
        ".fetch(",
        "::fetch(",
        "WebSocket::new(",
    ] {
        assert!(
            !owner.contains(forbidden),
            "anchor_control_plane.rs references `{forbidden}` — its exemption \
             is fenced to the audited spellings `fetch_with_str`, \
             `fetch_with_request` and `WebSocket::new_with_str`, and this is \
             not one of them"
        );
    }
    for audited in [
        "fetch_with_str",
        "fetch_with_request",
        "WebSocket::new_with_str(",
    ] {
        assert!(
            owner.contains(audited),
            "anchor_control_plane.rs no longer uses `{audited}`. Its exemption \
             from the module-wide transport ban exists for exactly these call \
             sites; when they are gone the exemption must go too"
        );
    }
    // And it does drive the trait.
    assert!(
        code.contains("AnchorControlPlane::attach"),
        "wasm.rs must build its control plane through `AnchorControlPlane::attach`"
    );
    for method in [
        "control.offer(",
        "control.trickle(",
        "control.signal(",
        "control.end_attempt(",
        "control.drain_events()",
    ] {
        assert!(
            code.contains(method),
            "wasm.rs never calls `{method}` — the boundary is not carrying \
             that job, so something else must be"
        );
    }
}

/// Neither implementation reaches the data path.
///
/// The two `impl ControlPlane` modules must not touch the node's
/// packet surface. A control plane that called `take_outbound`,
/// `on_datagram`, `stream_send` or the transport's `send` would be
/// forwarding Net packets no matter what its trait signatures say —
/// and one that forwarded publish/call/subscribe through the node
/// must name its node, one way or the other.
///
/// "One way or the other" is enforced, not assumed. The spellings
/// below are the direct names; behind them, every `type X = …` alias
/// chain and `use … as X` rename in the crate (ALL modules,
/// `lib.rs` included) is resolved back to its type names — the
/// root-module `type PeerTable = crate::node::LeafNode;` escape — and
/// renaming a node type is banned outright, while `struct` wrappers
/// are unwrapped so no name that resolves to a node type may appear
/// in either implementation.
#[test]
fn no_control_plane_implementation_touches_the_data_path() {
    let bindings = type_bindings(&source_tree());
    for (kind, name, _) in &bindings {
        // A `type`/`use … as` binding exists purely to rename; one
        // that stands for a node type is the forwarding vehicle and
        // cannot be allowed to exist. (`struct` wrappers are real
        // types a module may own — their NAMES are policed where they
        // are used, below.)
        if *kind != BindingKind::Rename {
            continue;
        }
        assert!(
            !resolves_to_data_path(&bindings, name),
            "the crate renames `{name}` (`type … = …` or `use … as …`), and it \
             resolves to {:?} — a node type. An alias is how a control-plane \
             implementation holds its node under a name no deny-list knows \
             (`type PeerTable = crate::node::LeafNode;` + `table.publish(…)`), \
             so the node types must be named as themselves, where this test \
             bans them",
            expand_type(&bindings, name)
        );
    }
    for module in ["anchor_control_plane.rs", "mock_control_plane.rs"] {
        let code = code_only(&source(module));
        // The tests at the foot of the mock construct a packet on
        // purpose, to prove the tripwire fires. The rule is about
        // the implementation.
        let implementation = code.split("mod tests").next().unwrap_or(&code).to_string();
        for forbidden in [
            "take_outbound",
            "on_datagram",
            "stream_send",
            "open_stream",
            "send_subprotocol",
            "announce_to_peer",
            "RtcLeafTransport",
            "LeafSession",
            "complete_handshake",
            // The forwarding escape: an implementation that reaches
            // the node and forwards publish/call/subscribe through it
            // names none of the above — but it must name its node, one
            // way or the other.
            "LeafNode",
            "crate::node",
            "node.publish",
            "node.call",
            "node.subscribe",
            "publish_stream_id",
            "classify_datagram",
            "drop_session",
        ] {
            assert!(
                !implementation.contains(forbidden),
                "{module} references `{forbidden}`, which is the data path. \
                 A control plane carries signalling and nothing else"
            );
        }
        // …whatever the node type is spelled as here: directly (the
        // list above), or under any name the crate's aliases,
        // renames or struct wrappers resolve to one.
        for token in identifiers(&implementation) {
            assert!(
                !resolves_to_data_path(&bindings, &token),
                "{module} names `{token}`, which resolves to {:?} — a node type \
                 reached through an alias, a rename or a struct wrapper. A \
                 control plane carries signalling and nothing else",
                expand_type(&bindings, &token)
            );
        }
    }
}

/// The anchor implementation does not hand its own types outward.
///
/// Its `pub` surface is what `wasm.rs` can reach, and it is allowed
/// two things beyond the trait: the anchor's node id and the
/// `rtc_addr` the STUN probe aims at (a string the implementation
/// minted, which rule 1 permits explicitly). A `pub fn` returning a
/// `WebSocket`, a `Response` or the credential would re-open the
/// boundary from the other side.
#[test]
fn the_anchor_implementations_public_surface_leaks_nothing() {
    let code = code_only(&source("anchor_control_plane.rs"));
    let signatures: Vec<&str> = code
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("pub fn") || line.starts_with("pub async fn"))
        .collect();
    assert!(
        signatures.len() >= 3,
        "expected the anchor implementation to have a public surface to check, \
         found {signatures:?}"
    );
    for signature in &signatures {
        for forbidden in [
            "WebSocket",
            "Response",
            "Request",
            "JsValue",
            "Credential",
            "PeerAddr",
            "AnchorInfo",
            "OfferAccepted",
        ] {
            // `attach` TAKES a credential — the page's own input,
            // travelling inward. It is the outward direction the
            // rule is about.
            if signature.starts_with("pub async fn attach") && forbidden == "Credential" {
                continue;
            }
            assert!(
                !signature.contains(forbidden),
                "`{signature}` mentions `{forbidden}`: the anchor's types must \
                 not reach the leaf, whatever the trait says"
            );
        }
    }
}

/// The mock holds no key material.
///
/// This is what makes the anchorless witness mean something. If the
/// mock could sign, it could mint an offer, and "the carrier is not
/// trusted" would be untested — the leaf would be relying on a
/// trustworthy mock instead of on the envelope's signature.
#[test]
fn the_anchorless_mock_holds_no_keys() {
    let path = manifest_dir().join("src").join("mock_control_plane.rs");
    let Ok(body) = std::fs::read_to_string(&path) else {
        panic!("{} is readable", path.display());
    };
    let code = code_only(&body);
    let implementation = code.split("mod tests").next().unwrap_or(&code).to_string();
    for forbidden in [
        "SigningKey",
        "StaticKeypair",
        "EntityKeypair",
        "LeafIdentity",
        "psk",
        "verify_entity_signature",
        "signal::verify",
        "signal::decode",
    ] {
        assert!(
            !implementation.contains(forbidden),
            "mock_control_plane.rs references `{forbidden}`. A carrier that \
             held a key, or that verified what it carries, would be trusted — \
             and the envelope exists so that it need not be"
        );
    }
}

// ───────────────────────── the type-level half ─────────────────────────

/// A control plane with **no anchor, no transport and no state**.
///
/// Zero-sized on purpose. It is the compile-time half of rule 1: if
/// a trait method ever needed something only an anchor could supply
/// — a socket, a credential, a session — this type could not
/// implement it and this test binary would not build.
///
/// Its bodies are refusals because there is nothing behind it; what
/// is being asserted is the *shape*, not the behaviour.
struct NoAnchor;

impl ControlPlane for NoAnchor {
    async fn offer(&self, _offer: Sdp) -> Result<BootstrapAccepted, LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn trickle(&self, _dialog: DialogId, _candidate: IceCandidate) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn end_attempt(&self, _dialog: DialogId) -> Result<(), LeafError> {
        Ok(())
    }

    async fn publish_announcement(
        &self,
        _announcement: SignedAnnouncement,
    ) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    async fn query_capability(
        &self,
        _capability: &str,
    ) -> Result<Vec<SignedAnnouncement>, LeafError> {
        Ok(Vec::new())
    }

    async fn signal(&self, _envelope: SignalEnvelope) -> Result<(), LeafError> {
        Err(LeafError::ControlPlane("nothing is behind me".into()))
    }

    fn drain_events(&self) -> Vec<ControlEvent> {
        Vec::new()
    }
}

/// The trait is implementable by a zero-sized type, and its
/// vocabulary is exactly the carrier vocabulary.
///
/// `size_of::<NoAnchor>() == 0` is the assertion that `NoAnchor` is
/// not quietly holding an anchor; the rest of this test is the
/// compiler having accepted the `impl` above.
#[test]
fn a_control_plane_needs_no_anchor_to_exist() {
    assert_eq!(core::mem::size_of::<NoAnchor>(), 0);

    // Every type a trait method mentions is constructible here, with
    // no anchor, no browser and no session — which is the property
    // the serverless follow-on depends on.
    let _: NodeId = 7;
    let _ = Sdp("v=0\r\n".into());
    let _ = IceCandidate {
        candidate: "candidate:1 1 udp 1 127.0.0.1 1 typ host".into(),
        mid: "0".into(),
    };
    let _ = SignedAnnouncement(vec![1, 2, 3]);
    let accepted = BootstrapAccepted {
        dialog: 1,
        answer: Sdp("v=0\r\n".into()),
        peer_static: [0u8; 32],
        peer_node: 9,
    };
    // The one key that crosses is the peer's Noise static, pinned by
    // whatever authenticated the attempt. It is 32 bytes, not a
    // handle to the thing that pinned it.
    assert_eq!(accepted.peer_static.len(), 32);
}
