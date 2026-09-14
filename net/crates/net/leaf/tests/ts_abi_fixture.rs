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
//! Regenerate deliberately, never by hand:
//!
//! ```text
//! NET_ABI_FIXTURE_WRITE=1 cargo test --test ts_abi_fixture
//! ```

use std::path::PathBuf;

use bytes::Bytes;
use net_leaf::node::LeafEvent;
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
                stream_id: 0x0102_0304_0506_0708,
                seq: 42,
                payload: Bytes::from_iter((0..=255u8).collect::<Vec<_>>()),
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
                stream_id,
                seq,
                payload,
            } = &event
            else {
                unreachable!("the vectors are stream_data events")
            };
            json!({
                "note": note,
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
