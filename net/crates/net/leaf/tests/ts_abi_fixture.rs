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
        panic!("{span_end} must follow {span_start} in leaf/src/wasm.rs — the extraction span moved")
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
