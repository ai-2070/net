//! Helpers for the Stage 2 (protected client-streaming + duplex) witnesses.
//!
//! The idiom is the s15 one: REAL registrations through the protected serve
//! seams, openings minted through the REAL caller-side mint helper
//! (`test_sign_admission_proof`), frames injected at the bridge dispatcher's
//! exact hand-off (`ServeHandle::inject_inbound_for_test`), and every
//! response observed at REAL wire endpoints (the caller's reply-channel
//! recorder).

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use parking_lot::Mutex;

use net::adapter::net::behavior::org::OrgId;
use net::adapter::net::behavior::org_call::RpcCallShape;
use net::adapter::net::behavior::org_grant::CapabilityAuthorityId;
use net::adapter::net::cortex::rpc::{
    RequestStream, RpcClientStreamingHandler, RpcDuplexHandler, RpcHandlerError, RpcRequestPayload,
    RpcResponsePayload, RpcResponseSink, RpcStatus, RpcStreamingContext,
    FLAG_RPC_CLIENT_STREAMING_REQUEST, FLAG_RPC_STREAMING_RESPONSE,
    HEADER_NRPC_STREAM_WINDOW_INITIAL,
};
use net::adapter::net::identity::EntityId;
use net::adapter::net::mesh_rpc::{test_sign_admission_proof, OrgProofIntent};

use super::s13;

/// A client-streaming opening payload (CS flags — `ClientStreaming` is
/// `(true, false)` under `RpcCallShape::from_streaming_flags`).
pub(crate) fn cs_opening(service: &str, body: &[u8]) -> RpcRequestPayload {
    RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_CLIENT_STREAMING_REQUEST,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    }
}

/// A duplex opening payload (BOTH streaming flags) with an optional
/// response-direction window (`nrpc-stream-window-initial`; `Some(0)` = the
/// response pump parks until a `STREAM_GRANT`).
pub(crate) fn dx_opening(service: &str, window: Option<u32>, body: &[u8]) -> RpcRequestPayload {
    let mut req = RpcRequestPayload {
        service: service.to_string(),
        deadline_ns: 0,
        flags: FLAG_RPC_CLIENT_STREAMING_REQUEST | FLAG_RPC_STREAMING_RESPONSE,
        headers: Vec::new(),
        body: Bytes::copy_from_slice(body),
    };
    if let Some(w) = window {
        req.headers.push((
            HEADER_NRPC_STREAM_WINDOW_INITIAL.to_string(),
            w.to_string().into_bytes(),
        ));
    }
    req
}

/// Mint the opening's admission proof through the REAL caller-side mint
/// helper (`test_sign_admission_proof`) over the FINALIZED request and
/// return the wire frame (proof header included) — "the caller's bytes".
pub(crate) fn mint_opening(
    intent: &OrgProofIntent,
    shape: RpcCallShape,
    binding: [u8; 32],
    call_id: u64,
    origin: u64,
    req: &RpcRequestPayload,
) -> Bytes {
    let mut req = req.clone();
    let (name, proof) = test_sign_admission_proof(intent, call_id, &req, shape, Some(binding))
        .expect("mint the streaming proof header");
    req.headers.push((name, proof));
    s13::request_frame(origin, call_id, &req)
}

/// Assert the four-party attribution + proof-stripping the protected
/// handler observed (the `fixture::AdmitHandler` probes, CS/DX edition).
pub(crate) fn attribution_ok(
    saw_admission: &AtomicBool,
    attribution_ok: &AtomicBool,
    proof_stripped: &AtomicBool,
    what: &str,
) {
    assert!(
        saw_admission.load(Ordering::SeqCst),
        "{what}: the handler saw `org_admission` on its streaming context",
    );
    assert!(
        attribution_ok.load(Ordering::SeqCst),
        "{what}: the four-party attribution matched",
    );
    assert!(
        proof_stripped.load(Ordering::SeqCst),
        "{what}: the raw proof header was stripped before the handler (E1.6)",
    );
}

/// The attribution probe fields shared by the CS/DX handlers below.
pub(crate) struct AttributionProbes {
    pub(crate) saw_admission: Arc<AtomicBool>,
    pub(crate) attribution_ok: Arc<AtomicBool>,
    pub(crate) proof_stripped: Arc<AtomicBool>,
    pub(crate) expected_caller: EntityId,
    pub(crate) expected_acting_org: OrgId,
    pub(crate) expected_provider_org: OrgId,
    pub(crate) expected_provider: EntityId,
    pub(crate) expected_capability: CapabilityAuthorityId,
}

impl AttributionProbes {
    pub(crate) fn observe(&self, ctx: &RpcStreamingContext) {
        if let Some(admitted) = ctx.org_admission.as_ref() {
            self.saw_admission.store(true, Ordering::SeqCst);
            if admitted.caller == self.expected_caller
                && admitted.acting_org == self.expected_acting_org
                && admitted.provider_org == self.expected_provider_org
                && admitted.provider == self.expected_provider
                && admitted.capability == self.expected_capability
            {
                self.attribution_ok.store(true, Ordering::SeqCst);
            }
        }
        let stripped = !ctx
            .headers
            .iter()
            .any(|(name, _)| name == super::fixture::ORG_ADMISSION_HEADER);
        self.proof_stripped.store(stripped, Ordering::SeqCst);
    }
}

/// A client-streaming handler that AGGREGATES every request body and
/// returns them joined as its single response — the CS aggregate witness.
pub(crate) struct AggregateCS {
    pub(crate) entries: Arc<AtomicUsize>,
    pub(crate) probes: AttributionProbes,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for AggregateCS {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        self.probes.observe(&ctx);
        let mut joined: Vec<u8> = Vec::new();
        while let Some(chunk) = requests.next().await {
            joined.extend_from_slice(&chunk);
        }
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(joined),
        })
    }
}

/// A client-streaming handler that collects bodies until input EOF and then
/// PARKS on `release` before returning the aggregate — the half-close
/// witnesses' stable record (its input half stays observable while the call
/// is alive). The release is a semaphore so N parked calls wake race-free
/// (`add_permits(n)` stores permits even before the waiters park).
pub(crate) struct HoldAfterEof {
    pub(crate) collected: Arc<Mutex<Vec<Bytes>>>,
    pub(crate) entries: Arc<AtomicUsize>,
    pub(crate) release: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl RpcClientStreamingHandler for HoldAfterEof {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
    ) -> Result<RpcResponsePayload, RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        let mut joined: Vec<u8> = Vec::new();
        while let Some(chunk) = requests.next().await {
            joined.extend_from_slice(&chunk);
        }
        let permit = self
            .release
            .clone()
            .acquire_owned()
            .await
            .expect("the release semaphore stays open");
        permit.forget();
        Ok(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![],
            body: Bytes::from(joined),
        })
    }
}

/// A duplex handler that records every DELIVERED request body and echoes it
/// to the response sink — the wrong-session probes' input observation.
pub(crate) struct EchoDX {
    pub(crate) seen: Arc<Mutex<Vec<Bytes>>>,
    pub(crate) entries: Arc<AtomicUsize>,
    pub(crate) finished: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for EchoDX {
    async fn call(
        &self,
        _ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        while let Some(chunk) = requests.next().await {
            self.seen.lock().push(chunk.clone());
            responses.send(chunk);
        }
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// A duplex handler for the exchange witness: echoes each request body as
/// a response chunk, emits a content-labelled TAIL after input EOF, and
/// completes — with the four-party attribution probes.
pub(crate) struct ExchangeDX {
    pub(crate) entries: Arc<AtomicUsize>,
    pub(crate) tail: &'static [u8],
    pub(crate) probes: AttributionProbes,
}

#[async_trait::async_trait]
impl RpcDuplexHandler for ExchangeDX {
    async fn call(
        &self,
        ctx: RpcStreamingContext,
        mut requests: RequestStream,
        responses: RpcResponseSink,
    ) -> Result<(), RpcHandlerError> {
        self.entries.fetch_add(1, Ordering::SeqCst);
        self.probes.observe(&ctx);
        while let Some(chunk) = requests.next().await {
            responses.send(chunk);
        }
        responses.send(Bytes::from_static(self.tail));
        Ok(())
    }
}
