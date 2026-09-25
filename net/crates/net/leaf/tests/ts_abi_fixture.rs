//! The Rust half of the TypeScript stream ABI pin.
//!
//! `@net-mesh/browser` decodes what a stream's `on_message` callback
//! delivers, and what that callback delivers is
//! [`LeafEvent::to_json`]'s `stream_data` arm — a JSON string, not
//! bytes. Stage 5 shipped a TypeScript declaration that said
//! `Uint8Array` and a package test double that produced `Uint8Array`,
//! so the two sides agreed with each other and disagreed with Rust,
//! and every page reading `stream.onMessage` received a string.
//!
//! The repair is one contract (Rust emits its event JSON, TypeScript
//! decodes it) plus this file, which is the lock:
//!
//! - **This test** rebuilds every vector in
//!   `../browser-ts/test/fixtures/leaf-abi.json` from production
//!   [`LeafEvent`] values and asserts the committed `json` is byte
//!   identical. Change the emitted shape and this goes red, with the
//!   new text in the failure.
//! - **`browser-ts/test/abi.test.ts` and `tests/abi_real_package.mjs`**
//!   read the same file and drive it through the shipped
//!   `LeafStream`, so the TypeScript side cannot be "fixed" by
//!   rewriting a test double: the double is asserted against these
//!   vectors too.
//!
//! The same lock, for the org byte-stream terminal items
//! `@net-mesh/browser`'s `OrgByteStreamHandle.next()` resolves, is the
//! second half of this file: `../browser-ts/test/fixtures/org-abi.json`
//! is rebuilt from the production response codec and from the item
//! keys `leaf/src/wasm.rs`'s `org_stream_end*` renderers set — read
//! out of that source, not restated here.
//!
//! Regenerate deliberately, never by hand:
//!
//! ```text
//! NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture
//! ```

use std::path::PathBuf;

use bytes::Bytes;
use net_leaf::node::LeafEvent;
use net_leaf::rpc_wire::{RpcResponsePayload, RpcStatus};
use serde_json::{json, Value};

/// The fixture's vectors, as Rust values.
///
/// Deliberately constructed with named fields rather than a helper:
/// a new field on [`LeafEvent::StreamData`] must fail to compile
/// here, because a new field is a new wire shape for the TypeScript
/// decoder.
fn vectors() -> Vec<(&'static str, LeafEvent)> {
    vec![
        (
            // The reviewer's own probe payload, so her assertion and
            // this pin are the same bytes.
            "two bytes",
            LeafEvent::StreamData {
                peer_node: 200,
                incarnation: 1,
                stream_id: 9,
                seq: 1,
                payload: Bytes::from_static(&[1, 2]),
            },
        ),
        (
            // Both u64s past 2^53. `JSON.parse` would round them, which
            // is why they cross as strings and why the decoder compares
            // stream ids as BigInt rather than Number.
            "u64 ids beyond the JS safe integer",
            LeafEvent::StreamData {
                peer_node: u64::MAX,
                incarnation: 9_007_199_254_740_995,
                stream_id: u64::MAX,
                seq: 9_007_199_254_740_993,
                payload: Bytes::from_static(b"\x00\xff"),
            },
        ),
        (
            // Empty is a legal payload and must not be confused with
            // "no event".
            "empty payload",
            LeafEvent::StreamData {
                peer_node: 0x0102_0304_0506_0708,
                incarnation: 2,
                stream_id: 255,
                seq: 0,
                payload: Bytes::new(),
            },
        ),
        (
            // Every byte value, so the base64 alphabet's `+`, `/` and
            // padding are all exercised by the decoder.
            "all 256 byte values",
            LeafEvent::StreamData {
                peer_node: 11_696_303_054_639_710_820,
                incarnation: 3,
                stream_id: 0x0102_0304_0506_0708,
                seq: 42,
                payload: Bytes::from_iter((0..=255u8).collect::<Vec<_>>()),
            },
        ),
        (
            // **The R4-10 vector.** One label opened to two peers is
            // one stream id on two sessions, and this is the second
            // of the pair: same `streamId`, different `peerNode`. A
            // decoder that filters on the id alone accepts both, so
            // the TypeScript side's `(peer, streamId)` filter is
            // exactly what this vector distinguishes — the two
            // strings differ in nothing else.
            "same stream id, a different peer",
            LeafEvent::StreamData {
                peer_node: 300,
                incarnation: 1,
                stream_id: 9,
                seq: 1,
                payload: Bytes::from_static(&[1, 2]),
            },
        ),
    ]
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-ts/test/fixtures/leaf-abi.json")
}

#[test]
fn the_typescript_fixture_is_exactly_what_the_leaf_emits() {
    let produced: Vec<Value> = vectors()
        .into_iter()
        .map(|(note, event)| {
            let LeafEvent::StreamData {
                peer_node,
                incarnation,
                stream_id,
                seq,
                payload,
            } = &event
            else {
                unreachable!("the vectors are stream_data events")
            };
            json!({
                "note": note,
                "peerNode": peer_node.to_string(),
                "incarnation": incarnation.to_string(),
                "streamId": stream_id.to_string(),
                "seq": seq.to_string(),
                "payloadHex": payload.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "json": event.to_json(),
            })
        })
        .collect();

    let document = json!({
        "note": "GENERATED. The exact strings net_leaf::node::LeafEvent emits at the \
                 wasm stream callback. Pinned from Rust by leaf/tests/ts_abi_fixture.rs \
                 and consumed from TypeScript by browser-ts/test/abi.test.ts and \
                 browser-ts/tests/abi_real_package.mjs. Regenerate with \
                 NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture.",
        "streamData": produced,
    });
    let serialised = format!(
        "{}\n",
        serde_json::to_string_pretty(&document).expect("serialise")
    );

    let path = fixture_path();
    if std::env::var_os("NET_ABI_FIXTURE_WRITE").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("create");
        std::fs::write(&path, &serialised).expect("write the fixture");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} could not be read ({error}). It is the TypeScript side's copy of this \
             ABI; regenerate it with NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture",
            path.display()
        )
    });

    assert_eq!(
        committed.replace("\r\n", "\n"),
        serialised,
        "the leaf's stream event JSON no longer matches the fixture \
         `@net-mesh/browser` decodes. This is a public ABI change: update the \
         TypeScript decoder in browser-ts/src/stream.ts and the package's test \
         double in browser-ts/test/leaf-abi.ts, then regenerate with \
         NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture"
    );
}

// ───────── the org terminal-items fixture ─────────

/// The org fixture's path — the terminal items
/// `OrgByteStreamHandle.next()` resolves, as `@net-mesh/browser`
/// reads them.
fn org_fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-ts/test/fixtures/org-abi.json")
}

/// The property names one `org_stream_end*` renderer sets on its item
/// object, in order, READ OUT OF `leaf/src/wasm.rs`.
///
/// The fixture's `itemKeys` are the keys the TypeScript wrapper
/// reads off the item `next()` resolves, so the pin extracts them
/// from the source that sets them rather than restating them here: a
/// renamed or reordered key moves the extraction and reddens the
/// fixture comparison. Every property name those renderers set is
/// spelled `JsValue::from_str(..)` in their body; nothing else there
/// is.
fn wasm_item_keys(span_start: &str, span_end: &str) -> Vec<String> {
    let source = include_str!("../src/wasm.rs");
    let start = source.find(span_start).unwrap_or_else(|| {
        panic!("{span_start} must exist in leaf/src/wasm.rs — the fixture's item keys come from it")
    });
    let rest = &source[start + span_start.len()..];
    let end = rest.find(span_end).unwrap_or_else(|| {
        panic!(
            "{span_end} must follow {span_start} in leaf/src/wasm.rs — the extraction span moved"
        )
    });
    let body = &rest[..end];
    let mut keys = Vec::new();
    let mut cursor = 0;
    while let Some(found) = body[cursor..].find("JsValue::from_str(\"") {
        let name_start = cursor + found + "JsValue::from_str(\"".len();
        let Some(len) = body[name_start..].find('"') else {
            panic!("every from_str literal in {span_start} is closed")
        };
        keys.push(body[name_start..name_start + len].to_string());
        cursor = name_start + len;
    }
    keys
}

/// The fixture's vectors, as (note, terminal body) pairs: the three
/// end items the TypeScript side decodes — a small probe body, every
/// byte value, and the empty body that is the clean end.
fn org_vectors() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        (
            "a completion frame with a non-empty final body (the two-byte probe)",
            vec![1, 2],
        ),
        ("a final body of every byte value", (0..=255u8).collect()),
        (
            "a completion frame with an empty body is the clean end",
            Vec::new(),
        ),
    ]
}

/// The `org-abi.json` half of the pin: every vector rebuilt from the
/// production codec (`RpcResponsePayload::encode_into` for the
/// completion frame) and from the item keys read out of
/// `leaf/src/wasm.rs`. Drift on either side — a codec change, a
/// renamed item key, a fixture rewritten to match — goes red here.
#[test]
fn the_org_terminal_fixture_is_exactly_what_the_leaf_renders() {
    // The two `next()` renderers' key sets, from the source that sets
    // them: an empty body ends through `org_stream_end`, a final body
    // through `org_stream_end_value`. Both extractions must find keys,
    // so a broken read cannot pass against a fixture that went empty
    // in the same drift.
    let end_keys = wasm_item_keys("fn org_stream_end(", "fn org_stream_end_value(");
    let end_value_keys = wasm_item_keys("fn org_stream_end_value(", "fn org_stream_end_error(");
    assert!(
        !end_keys.is_empty() && !end_value_keys.is_empty(),
        "the item keys must be read out of org_stream_end/org_stream_end_value"
    );

    let end_items: Vec<Value> = org_vectors()
        .into_iter()
        .map(|(note, body)| {
            // The completion frame the terminal body rides:
            // `rpc_stream::terminal_of` folds exactly this `Ok`
            // response to `StreamTerminal::Completed { body }`.
            let payload = RpcResponsePayload {
                status: RpcStatus::Ok,
                headers: Vec::new(),
                body: Bytes::copy_from_slice(&body),
            };
            let mut frame = Vec::new();
            payload
                .encode_into(&mut frame)
                .expect("a completion frame encodes");
            let item_keys = if body.is_empty() {
                &end_keys
            } else {
                &end_value_keys
            };
            json!({
                "note": note,
                "frameHex": frame.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "bodyHex": body.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "itemKeys": item_keys,
            })
        })
        .collect();

    let document = json!({
        "note": "GENERATED. The org byte-stream terminal items `@net-mesh/browser`'s \
                 `OrgByteStreamHandle.next()` resolves. `frameHex` is \
                 net_leaf::rpc_wire::RpcResponsePayload::encode_into's exact output for a \
                 completion frame whose terminal body is `bodyHex` \
                 (rpc_stream::terminal_of folds it to StreamTerminal::Completed); `itemKeys` \
                 are the property names org_stream_end_value/org_stream_end in \
                 leaf/src/wasm.rs set on the item object, in order, extracted from that \
                 source. Consumed by browser-ts/test/leaf-abi.ts and pinned by \
                 browser-ts/test/org.test.ts. The Rust half (assert + regenerate, \
                 NET_ABI_FIXTURE_WRITE=1) belongs beside leaf/tests/ts_abi_fixture.rs; this \
                 document was generated from the same production values that pin asserts \
                 will replay.",
        "endItems": end_items,
    });
    let serialised = format!(
        "{}\n",
        serde_json::to_string_pretty(&document).expect("serialise")
    );

    let path = org_fixture_path();
    if std::env::var_os("NET_ABI_FIXTURE_WRITE").is_some() {
        std::fs::create_dir_all(path.parent().expect("a parent directory")).expect("create");
        std::fs::write(&path, &serialised).expect("write the fixture");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} could not be read ({error}). It is the TypeScript side's copy of the org \
             terminal-item ABI; regenerate it with NET_ABI_FIXTURE_WRITE=1 cargo test \
             --test ts_abi_fixture",
            path.display()
        )
    });

    assert_eq!(
        committed.replace("\r\n", "\n"),
        serialised,
        "the org terminal items no longer match the fixture `@net-mesh/browser` decodes. \
         A terminal body the leaf can emit or an item key it sets is a public ABI change: \
         update browser-ts/src/org.ts and browser-ts/test/leaf-abi.ts, then regenerate \
         with NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture"
    );
}

// ───────────── the `sink_error` wording and retire vocabulary pins ─────────────
//
// `@net-mesh/browser` re-types the leaf's `sink_error` rejections by
// their TEXT (`parseOrgError`'s closed-refusal family and the pinned
// `ORG_*_REFUSAL` constants in `browser-ts/src/errors.ts`), and it
// types the `retired()` verdicts by their spelling. Neither side can
// import the other, so these pins read both sources: wording drift on
// EITHER side — a reworded `sink_error` arm, a retyped TS constant, a
// regex that no longer matches the leaf's template, a retire spelling
// the TS switch does not name — goes red here.

/// `browser-ts/src/errors.ts`, read from disk (not `include_str!`: a
/// cross-package include breaks an unpacked `cargo package`).
fn ts_errors_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../browser-ts/src/errors.ts");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} could not be read ({error})", path.display()))
        .replace("\r\n", "\n")
}

/// The text between `start` and the first `end` after it.
fn span<'a>(source: &'a str, start: &str, end: &str, what: &str) -> &'a str {
    let from = source
        .find(start)
        .unwrap_or_else(|| panic!("{start} must exist in {what} — the extraction span moved"));
    let rest = &source[from + start.len()..];
    let to = rest.find(end).unwrap_or_else(|| {
        panic!("{end:?} must follow {start} in {what} — the extraction span moved")
    });
    &rest[..to]
}

/// The first Rust string literal after `marker` in `body`, with
/// `\`-newline continuations folded exactly as rustc folds them.
fn rust_literal_after(body: &str, marker: &str) -> String {
    let at = body
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} must exist in sink_error"));
    let rest = &body[at + marker.len()..];
    let open = rest.find('"').expect("a string literal follows the arm") + 1;
    let mut out = String::new();
    let mut chars = rest[open..].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => return out,
            '\\' => match chars.next() {
                Some('\n') => {
                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                        chars.next();
                    }
                }
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                other => panic!("unexpected escape {other:?} in a sink_error literal"),
            },
            c => out.push(c),
        }
    }
    panic!("the literal after {marker} is unterminated")
}

/// `export const NAME = '...'` (or `"..."`) from the TS source.
fn ts_const(source: &str, name: &str) -> String {
    let decl = format!("export const {name} =");
    let at = source
        .find(&decl)
        .unwrap_or_else(|| panic!("browser-ts/src/errors.ts must export {name}"));
    let rest = source[at + decl.len()..].trim_start();
    let quote = rest.chars().next().expect("a string initializer");
    assert!(
        quote == '\'' || quote == '"',
        "{name} must be a plain string literal"
    );
    let body = &rest[1..];
    let close = body.find(quote).expect("a closed string literal");
    body[..close].to_string()
}

/// Escape a literal text for a JS regex literal.
fn js_regex_escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        if "\\^$.|?*+()[]{}/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Every `"the <name> sink"` literal in a source — the `what` the leaf
/// hands `sink_error`.
fn sink_names_in(source: &str) -> std::collections::BTreeSet<String> {
    let mut names = std::collections::BTreeSet::new();
    let mut cursor = 0;
    while let Some(found) = source[cursor..].find("\"the ") {
        let start = cursor + found + 1;
        let Some(len) = source[start..].find('"') else {
            break;
        };
        let text = &source[start..start + len];
        if text.ends_with(" sink") && text.split(' ').count() == 3 {
            names.insert(text.to_string());
        }
        cursor = start + len + 1;
    }
    names
}

/// The leaf's `sink_error` closed and byte-budget refusals ARE the
/// `ORG_*_REFUSAL` constants `@net-mesh/browser` pins, for every sink
/// name the leaf passes, and `parseOrgError`'s two refusal regexes are
/// exactly the leaf's templates, generic in the sink name only.
#[test]
fn sink_error_wording_is_exactly_the_typescript_refusal_constants() {
    let wasm = include_str!("../src/wasm.rs");
    let leader = include_str!("../src/leader_session.rs");
    let ts = ts_errors_source();

    let body = span(
        wasm,
        "pub(crate) fn sink_error(",
        "\n}\n",
        "leaf/src/wasm.rs",
    );
    let closed = rust_literal_after(body, "SinkError::Closed =>");
    let budget = rust_literal_after(body, "SinkError::ResourceExhausted =>");
    for template in [&closed, &budget] {
        assert_eq!(
            template.matches("{what}").count(),
            1,
            "a sink_error template names its sink exactly once: {template:?}"
        );
    }

    // One TS constant per sink per verdict.
    let table = [
        (
            "the response sink",
            "ORG_SINK_CLOSED_REFUSAL",
            "ORG_SINK_BUDGET_REFUSAL",
        ),
        (
            "the upload sink",
            "ORG_UPLOAD_SINK_CLOSED_REFUSAL",
            "ORG_UPLOAD_SINK_BUDGET_REFUSAL",
        ),
    ];
    // Every sink name the leaf actually passes has its constants: a new
    // sink the TS side has not pinned reddens here, by name.
    let mut spoken = sink_names_in(wasm);
    spoken.extend(sink_names_in(leader));
    let pinned: std::collections::BTreeSet<String> =
        table.iter().map(|(name, _, _)| name.to_string()).collect();
    assert_eq!(
        spoken, pinned,
        "the sink names the leaf hands sink_error must be exactly the ones \
         browser-ts/src/errors.ts pins an ORG_*_REFUSAL pair for"
    );
    for (name, closed_const, budget_const) in table {
        assert_eq!(
            closed.replace("{what}", name),
            ts_const(&ts, closed_const),
            "sink_error(SinkError::Closed, {name:?}) no longer spells {closed_const}"
        );
        assert_eq!(
            budget.replace("{what}", name),
            ts_const(&ts, budget_const),
            "sink_error(SinkError::ResourceExhausted, {name:?}) no longer spells {budget_const}"
        );
    }

    // `parseOrgError`'s regexes are the templates, generic in the sink
    // name only, each re-typing as its class.
    for (template, class) in [
        (&closed, "OrgCancelledError"),
        (&budget, "OrgAdmissionDeniedError"),
    ] {
        let regex = format!(
            "/^{}$/.test(message)",
            template
                .split("{what}")
                .map(js_regex_escape)
                .collect::<Vec<_>>()
                .join(".+")
        );
        let at = ts.find(&regex).unwrap_or_else(|| {
            panic!(
                "browser-ts/src/errors.ts must match the leaf's sink_error template with \
                 exactly {regex} — the leaf spells {template:?}"
            )
        });
        let arm = &ts[at..];
        let arm = &arm[..arm.find(';').expect("a return statement")];
        assert!(
            arm.contains(&format!("new {class}(")),
            "{regex} must re-type as {class}: {arm}"
        );
    }

    // Any hard-coded closed refusal in the leaf (the handler-completion
    // arm spells one) is a pinned text, not a third spelling.
    let mut cursor = 0;
    let mut literals = 0;
    while let Some(found) = wasm[cursor..].find("\"org: the ") {
        let start = cursor + found + 1;
        let len = wasm[start..].find('"').expect("closed literal");
        let text = &wasm[start..start + len];
        if text.contains(" is closed: ") {
            literals += 1;
            assert!(
                table
                    .iter()
                    .any(|(_, c, b)| text == ts_const(&ts, c) || text == ts_const(&ts, b)),
                "leaf/src/wasm.rs spells a closed refusal no TS constant pins: {text:?}"
            );
        }
        cursor = start + len + 1;
    }
    assert!(
        literals >= 1,
        "the handler-completion closed refusal literal moved"
    );
}

/// Every retire spelling the leaf can put in front of `retired()` —
/// `RetireReason::as_str` and the boundary's `retire_reason_text` —
/// is an `OrgRetireReason` member AND a named case in
/// `orgRetireReason`, so none of them falls to the unknown-verdict
/// arm (which reports `replaced`).
#[test]
fn every_leaf_retire_spelling_is_a_typescript_retire_verdict() {
    use net_leaf::rpc_stream::RetireReason;

    let every = [
        RetireReason::Timeout,
        RetireReason::Cancelled,
        RetireReason::Revoked,
        RetireReason::SessionLost,
        RetireReason::LeaderLost,
        RetireReason::NodeClosed,
        RetireReason::Replaced,
        RetireReason::ResourceExhausted,
    ];
    // Exhaustive: a new variant fails to compile here, so it cannot be
    // missing from `every`.
    for reason in every {
        match reason {
            RetireReason::Timeout
            | RetireReason::Cancelled
            | RetireReason::Revoked
            | RetireReason::SessionLost
            | RetireReason::LeaderLost
            | RetireReason::NodeClosed
            | RetireReason::Replaced
            | RetireReason::ResourceExhausted => {}
        }
    }

    let ts = ts_errors_source();
    let union = span(
        &ts,
        "export type OrgRetireReason =",
        ";",
        "browser-ts/src/errors.ts",
    );
    let cases = span(
        &ts,
        "export function orgRetireReason(raw: string): OrgRetireReason {",
        "default:",
        "browser-ts/src/errors.ts",
    );
    let wasm = include_str!("../src/wasm.rs");
    let text_fn = span(wasm, "fn retire_reason_text(", "\n}\n", "leaf/src/wasm.rs");

    let mut spellings: Vec<String> = every.iter().map(|r| r.as_str().to_string()).collect();
    let mut cursor = 0;
    let mut arms = 0;
    while let Some(found) = text_fn[cursor..].find("=> \"") {
        let start = cursor + found + 4;
        let len = text_fn[start..].find('"').expect("closed literal");
        spellings.push(text_fn[start..start + len].to_string());
        arms += 1;
        cursor = start + len + 1;
    }
    assert_eq!(
        arms,
        every.len(),
        "retire_reason_text has one arm per RetireReason"
    );

    for spelling in spellings {
        assert!(
            union.contains(&format!("'{spelling}'")),
            "the leaf spells retire verdict {spelling:?} but OrgRetireReason does not name it"
        );
        assert!(
            cases.contains(&format!("case '{spelling}':")),
            "the leaf spells retire verdict {spelling:?} but orgRetireReason has no case for \
             it — it would be reported as `replaced`"
        );
    }
}
