// SPDX-License-Identifier: MIT OR Apache-2.0
//! SDK audit 2026-10-11: a zero heartbeat interval or session timeout is
//! refused at node construction. Only the C ABI refused them; the Rust SDK
//! (and Node / Python) passed them straight through — a zero heartbeat
//! busy-loops the heartbeat task and a zero session timeout expires every
//! session at once.
#![cfg(feature = "net")]

use net_sdk::mesh::MeshBuilder;

const PSK: [u8; 32] = [0x2C; 32];

#[tokio::test]
async fn a_zero_heartbeat_interval_is_refused() {
    let built = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .heartbeat_ms(0)
        .build()
        .await;
    let err = built.err().expect("a zero heartbeat must be refused");
    assert!(err.to_string().contains("heartbeat"), "{err}");
}

#[tokio::test]
async fn a_zero_session_timeout_is_refused() {
    let built = MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .session_timeout_ms(0)
        .build()
        .await;
    let err = built.err().expect("a zero session timeout must be refused");
    assert!(err.to_string().contains("session timeout"), "{err}");
}

#[tokio::test]
async fn non_zero_timings_still_build() {
    MeshBuilder::new("127.0.0.1:0", &PSK)
        .unwrap()
        .heartbeat_ms(50)
        .session_timeout_ms(500)
        .build()
        .await
        .expect("non-zero timings build");
}
