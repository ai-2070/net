//! The C ABI's half of durable issuer state (decision 4b).
//!
//! Driven through the raw exports rather than a wrapper, because the
//! exports are what Go and C consumers see, and the handle lifecycle —
//! a rotation produces a second independently-freeable owning pointer —
//! is where this can go wrong in a way no Rust-side test would notice.

use std::ffi::CString;

use crate::adapter::net::identity::{PermissionToken, IDENTITY_STATE_SIZE};
use crate::ffi::mesh::{
    net_free_bytes, net_identity_at_generation, net_identity_entity_id, net_identity_free,
    net_identity_from_state, net_identity_generate, net_identity_generation,
    net_identity_issue_token, net_identity_state_size, net_identity_to_state, IdentityHandle,
};

/// `NET_ERR_IDENTITY`. Restated here because the constant is
/// `pub(crate)` to `ffi::mesh`; the value is pinned by the header.
const NET_ERR_IDENTITY: std::os::raw::c_int = -120;

fn generate() -> *mut IdentityHandle {
    let mut h: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(unsafe { net_identity_generate(&mut h) }, 0);
    h
}

fn entity_id(h: *mut IdentityHandle) -> [u8; 32] {
    let mut out = [0u8; 32];
    assert_eq!(unsafe { net_identity_entity_id(h, out.as_mut_ptr()) }, 0);
    out
}

/// Round-trip through `to_state` / `from_state`, and prove the
/// restored issuer comes back on the persisted epoch.
#[test]
fn state_round_trip_preserves_the_generation() {
    let h = generate();
    assert_eq!(unsafe { net_identity_generation(h) }, 0);

    let mut rotated: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(unsafe { net_identity_at_generation(h, 6, &mut rotated) }, 0);
    assert_eq!(unsafe { net_identity_generation(rotated) }, 6);
    assert_eq!(
        unsafe { net_identity_generation(h) },
        0,
        "rotation must leave the source handle alone"
    );

    let mut state = [0u8; IDENTITY_STATE_SIZE];
    assert_eq!(
        unsafe { net_identity_to_state(rotated, state.as_mut_ptr()) },
        0
    );

    let mut restored: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { net_identity_from_state(state.as_ptr(), state.len(), &mut restored) },
        0
    );
    assert_eq!(unsafe { net_identity_generation(restored) }, 6);
    assert_eq!(
        entity_id(restored),
        entity_id(rotated),
        "same key, not just the same epoch"
    );

    unsafe {
        net_identity_free(h);
        net_identity_free(rotated);
        net_identity_free(restored);
    }
}

/// A minted token carries the handle's generation.
///
/// Without this the whole surface is decorative — which is what it
/// was: the field existed, and nothing a C caller could do would set
/// it to anything but zero.
#[test]
fn issued_tokens_carry_the_handles_generation() {
    let signer = generate();
    let mut rotated: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { net_identity_at_generation(signer, 11, &mut rotated) },
        0
    );

    let subject = generate();
    let subject_id = entity_id(subject);

    let scope = CString::new(r#"["publish"]"#).unwrap();
    let channel = CString::new("issuer/rotation").unwrap();
    let mut token_ptr: *mut u8 = std::ptr::null_mut();
    let mut token_len: usize = 0;
    assert_eq!(
        unsafe {
            net_identity_issue_token(
                rotated,
                subject_id.as_ptr(),
                subject_id.len(),
                scope.as_ptr(),
                channel.as_ptr(),
                3600,
                0,
                &mut token_ptr,
                &mut token_len,
            )
        },
        0
    );
    let token =
        PermissionToken::from_bytes(unsafe { std::slice::from_raw_parts(token_ptr, token_len) })
            .expect("parse token");
    assert_eq!(token.issuer_generation, 11);
    token.verify().expect("token must verify");

    unsafe {
        net_free_bytes(token_ptr, token_len);
        net_identity_free(signer);
        net_identity_free(rotated);
        net_identity_free(subject);
    }
}

/// Backwards and past-the-ceiling rotations are refused, and refusal
/// hands back nothing to free.
#[test]
fn rotation_is_monotonic_and_capped() {
    let h = generate();
    let mut at5: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(unsafe { net_identity_at_generation(h, 5, &mut at5) }, 0);

    let mut out: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { net_identity_at_generation(at5, 4, &mut out) },
        NET_ERR_IDENTITY
    );
    assert!(
        out.is_null(),
        "a refused rotation must not hand back a handle"
    );

    // Re-applying the persisted generation on restart is not an error.
    let mut same: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(unsafe { net_identity_at_generation(at5, 5, &mut same) }, 0);
    assert_eq!(unsafe { net_identity_generation(same) }, 5);

    let mut ceiling: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { net_identity_at_generation(at5, u32::MAX, &mut ceiling) },
        0
    );
    let mut past: *mut IdentityHandle = std::ptr::null_mut();
    assert_eq!(
        unsafe { net_identity_at_generation(ceiling, u32::MAX, &mut past) },
        NET_ERR_IDENTITY,
        "at the ceiling the answer is a new key, not a new generation"
    );

    unsafe {
        net_identity_free(h);
        net_identity_free(at5);
        net_identity_free(same);
        net_identity_free(ceiling);
    }
}

/// Malformed state is refused, not partially parsed.
#[test]
fn from_state_rejects_wrong_length_and_future_versions() {
    let mut out: *mut IdentityHandle = std::ptr::null_mut();

    // A bare 32-byte seed is not identity state. Reading one as state
    // would put the generation-zero trap back through the versioned
    // door.
    let seed = [0u8; 32];
    assert_eq!(
        unsafe { net_identity_from_state(seed.as_ptr(), seed.len(), &mut out) },
        NET_ERR_IDENTITY
    );
    assert!(out.is_null());

    let h = generate();
    let mut state = [0u8; IDENTITY_STATE_SIZE];
    assert_eq!(unsafe { net_identity_to_state(h, state.as_mut_ptr()) }, 0);
    state[0] = 2;
    assert_eq!(
        unsafe { net_identity_from_state(state.as_ptr(), state.len(), &mut out) },
        NET_ERR_IDENTITY
    );
    assert!(out.is_null());

    unsafe { net_identity_free(h) };
}

/// The header's `NET_IDENTITY_STATE_SIZE` and this build agree.
///
/// Read out of the header text rather than restated: a constant
/// restated in a test is a constant that agrees with itself. The nRPC
/// ABI sat two versions stale for exactly that reason.
#[test]
fn header_state_size_matches_the_implementation() {
    assert_eq!(net_identity_state_size(), IDENTITY_STATE_SIZE);

    let header = include_str!("../../include/net.go.h");
    let line = header
        .lines()
        .find(|l| l.contains("#define NET_IDENTITY_STATE_SIZE"))
        .expect("the header must define NET_IDENTITY_STATE_SIZE");
    let declared: usize = line
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .expect("numeric");
    assert_eq!(
        declared, IDENTITY_STATE_SIZE,
        "net.go.h declares {declared}, the implementation writes {IDENTITY_STATE_SIZE}"
    );
}
