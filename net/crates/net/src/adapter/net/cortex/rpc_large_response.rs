// SPDX-License-Identifier: MIT OR Apache-2.0
//! Opt-in unary response fragments. No change to packet or RESPONSE codecs.
use super::{RpcResponsePayload, RpcStatus, EVENT_META_SIZE, RPC_ROUTE_V1_SIZE};
use bytes::Bytes;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

pub(crate) const FLAG: u16 = 1 << 6;
pub(crate) const HEADER: &str = "nrpc-response-fragment-v1";
pub(crate) const CHUNK: usize = 4096;
pub(crate) const MAX: usize = 1024 * 1024;
const AGGREGATE: usize = 8 * MAX;

/// The native sender owns this logical response until delivery stops. Keeping
/// the token in the server fold makes CANCEL effective after the handler.
pub(crate) struct Transfer {
    pub from_node: u64,
    pub session_id: u64,
    pub caller_origin: u64,
    pub call_id: u64,
    pub deadline_ns: u64,
    pub cancellation: super::RpcCancellationToken,
    pub response: RpcResponsePayload,
}

pub(crate) type Emitter =
    Arc<dyn Fn(Transfer) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

pub(crate) fn needs_transfer(response: &RpcResponsePayload) -> bool {
    let len = response.encoded_len();
    len + EVENT_META_SIZE + RPC_ROUTE_V1_SIZE > net_wire::protocol::MAX_EVENT_SIZE
        && len <= MAX
        && !response.headers.iter().any(|(name, _)| name == HEADER)
}

/// One monotonic deadline covers encoding and every send. The absolute request
/// deadline can shorten, never extend, the 30-second server transfer ceiling.
pub(crate) fn transfer_deadline(deadline_ns: u64) -> tokio::time::Instant {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    let ceiling = Duration::from_secs(30);
    let remaining = if deadline_ns == 0 {
        ceiling
    } else {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Duration::from_nanos(deadline_ns)
            .saturating_sub(now)
            .min(ceiling)
    };
    tokio::time::Instant::now() + remaining
}

pub(crate) fn error(message: &str) -> RpcResponsePayload {
    RpcResponsePayload {
        status: RpcStatus::Internal,
        headers: vec![],
        body: Bytes::from(format!(
            "large RPC response: {message}; handler may have completed; do not automatically retry"
        )),
    }
}

pub(crate) fn emit(
    response: RpcResponsePayload,
    enabled: bool,
    mut send: impl FnMut(RpcResponsePayload),
) {
    if response.headers.iter().any(|(name, _)| name == HEADER) {
        send(error("reserved fragment header in handler response"));
        return;
    }
    if !enabled
        || response.encoded_len() + EVENT_META_SIZE + RPC_ROUTE_V1_SIZE
            <= net_wire::protocol::MAX_EVENT_SIZE
    {
        send(response);
        return;
    }
    let len = response.encoded_len();
    if len > MAX {
        send(error("encoded response exceeds 1048576-byte limit"));
        return;
    }
    let mut encoded = Vec::with_capacity(len);
    response.encode_into(&mut encoded);
    let encoded = Bytes::from(encoded);
    for (index, offset) in (0..len).step_by(CHUNK).enumerate() {
        let mut value = Vec::with_capacity(6);
        value.extend_from_slice(&(len as u32).to_le_bytes());
        value.extend_from_slice(&(index as u16).to_le_bytes());
        send(RpcResponsePayload {
            status: RpcStatus::Ok,
            headers: vec![(HEADER.into(), value)],
            body: encoded.slice(offset..(offset + CHUNK).min(len)),
        });
    }
}

pub(crate) struct Assembly {
    data: Vec<u8>,
    seen: Vec<bool>,
    remaining: usize,
    reserved: usize,
    budget: Arc<AtomicUsize>,
}

impl Drop for Assembly {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.reserved, Ordering::AcqRel);
    }
}

impl Assembly {
    pub(crate) fn accept(
        slot: &mut Option<Self>,
        budget: &Arc<AtomicUsize>,
        response: RpcResponsePayload,
    ) -> Result<Option<RpcResponsePayload>, &'static str> {
        let Some((_, value)) = response.headers.iter().find(|(name, _)| name == HEADER) else {
            // A terminal server error can abort a partially emitted response.
            if slot.is_some() && response.status == RpcStatus::Ok {
                return Err("unfragmented success interrupted fragments");
            }
            *slot = None;
            return Ok(Some(response));
        };
        if response.status != RpcStatus::Ok || response.headers.len() != 1 || value.len() != 6 {
            return Err("invalid fragment envelope");
        }
        let total = u32::from_le_bytes([value[0], value[1], value[2], value[3]]) as usize;
        let index = u16::from_le_bytes([value[4], value[5]]) as usize;
        if total == 0 || total > MAX || index >= total.div_ceil(CHUNK) {
            return Err("fragment bounds exceeded");
        }
        let offset = index * CHUNK;
        let end = (offset + CHUNK).min(total);
        if response.body.len() != end - offset {
            return Err("invalid fragment length");
        }
        if slot.is_none() {
            budget
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(total).filter(|n| *n <= AGGREGATE)
                })
                .map_err(|_| "aggregate reassembly budget exhausted")?;
            *slot = Some(Self {
                data: vec![0; total],
                seen: vec![false; total.div_ceil(CHUNK)],
                remaining: total.div_ceil(CHUNK),
                reserved: total,
                budget: budget.clone(),
            });
        }
        let Some(assembly) = slot.as_mut() else {
            return Err("missing assembly");
        };
        if assembly.reserved != total {
            return Err("inconsistent fragment total");
        }
        if assembly.seen[index] {
            if assembly.data[offset..end] != response.body[..] {
                return Err("contradictory duplicate fragment");
            }
            return Ok(None);
        }
        assembly.data[offset..end].copy_from_slice(&response.body);
        assembly.seen[index] = true;
        assembly.remaining -= 1;
        if assembly.remaining != 0 {
            return Ok(None);
        }
        let data = Bytes::from(std::mem::take(&mut assembly.data));
        *slot = None;
        let length = data.len();
        let decoded =
            RpcResponsePayload::decode(data).map_err(|_| "malformed assembled response")?;
        if decoded.encoded_len() != length {
            return Err("trailing assembled response bytes");
        }
        if decoded.headers.iter().any(|(name, _)| name == HEADER) {
            return Err("nested fragment envelope");
        }
        Ok(Some(decoded))
    }
}

#[cfg(test)]
mod tests {
    use super::super::RpcClientPending;
    use super::*;

    #[test]
    fn transfer_deadline_caps_missing_or_far_future_deadlines_and_preserves_expiry() {
        use std::time::Duration;
        for deadline in [0, u64::MAX] {
            let before = tokio::time::Instant::now();
            let end = transfer_deadline(deadline);
            assert!(end >= before + Duration::from_secs(29));
            assert!(end <= tokio::time::Instant::now() + Duration::from_secs(30));
        }
        assert!(transfer_deadline(1) <= tokio::time::Instant::now());
    }

    #[test]
    fn shared_large_response_wire_vector() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/cross_lang_nrpc/golden_vectors_large_response.json"
        ))
        .unwrap();
        assert_eq!(fixture["request_flag"], FLAG);
        assert_eq!(fixture["header_name"], HEADER);
        assert_eq!(fixture["chunk_bytes"], CHUNK);
        assert_eq!(fixture["max_response_bytes"], MAX);
        assert_eq!(fixture["max_fragments"], MAX.div_ceil(CHUNK));
        let response = RpcResponsePayload {
            status: RpcStatus::from_wire(fixture["response"]["status"].as_u64().unwrap() as u16),
            headers: vec![],
            body: Bytes::from(vec![
                fixture["response"]["body_byte"].as_u64().unwrap() as u8;
                fixture["response"]["body_length"].as_u64().unwrap()
                    as usize
            ]),
        };
        assert_eq!(fixture["encoded_response_bytes"], response.encoded_len());
        let encoded = response.encode();
        let prefix = hex::decode(fixture["encoded_prefix_hex"].as_str().unwrap()).unwrap();
        assert_eq!(
            &encoded[..prefix.len()],
            &prefix[..],
            "response byte layout drifted: encoded prefix != fixture encoded_prefix_hex"
        );
        let pieces = fragments(response);
        let expected = fixture["fragments"].as_array().unwrap();
        assert_eq!(pieces.len(), expected.len());
        for (piece, expected) in pieces.iter().zip(expected) {
            assert_eq!(
                hex::encode(&piece.headers[0].1),
                expected["header_value_hex"]
            );
            assert_eq!(
                piece.body.len(),
                expected["body_bytes"].as_u64().unwrap() as usize
            );
            assert_eq!(
                RpcResponsePayload::decode(Bytes::from(piece.encode())).unwrap(),
                *piece
            );
        }
    }

    fn response(size: usize) -> RpcResponsePayload {
        RpcResponsePayload {
            status: RpcStatus::Application(0x8123),
            headers: vec![("application-detail".into(), vec![1, 2, 3])],
            body: Bytes::from(vec![b'x'; size]),
        }
    }

    fn fragments(response: RpcResponsePayload) -> Vec<RpcResponsePayload> {
        let mut result = Vec::new();
        emit(response, true, |part| result.push(part));
        result
    }

    #[test]
    fn reordered_duplicate_fragments_preserve_status_headers_and_body() {
        let expected = response(22_000);
        let mut pieces = fragments(expected.clone());
        assert!(pieces.len() > 1);
        let budget = Arc::new(AtomicUsize::new(0));
        let mut slot = None;
        pieces.reverse();
        for piece in &pieces[..pieces.len() - 1] {
            assert!(
                piece.encoded_len() + EVENT_META_SIZE + RPC_ROUTE_V1_SIZE
                    <= net_wire::protocol::MAX_EVENT_SIZE
            );
            assert!(Assembly::accept(&mut slot, &budget, piece.clone())
                .unwrap()
                .is_none());
            assert!(Assembly::accept(&mut slot, &budget, piece.clone())
                .unwrap()
                .is_none());
        }
        assert_eq!(budget.load(Ordering::Acquire), expected.encoded_len());
        let actual = Assembly::accept(&mut slot, &budget, pieces.last().unwrap().clone())
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(budget.load(Ordering::Acquire), 0);
        assert!(slot.is_none());
    }

    #[test]
    fn unchanged_small_and_legacy_responses_and_over_limit_refusal() {
        for (size, enabled) in [(10, true), (22_000, false)] {
            let expected = response(size);
            let mut output = Vec::new();
            emit(expected.clone(), enabled, |part| output.push(part));
            assert_eq!(output, vec![expected]);
        }
        let output = fragments(response(MAX));
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].status, RpcStatus::Internal);
        assert!(String::from_utf8_lossy(&output[0].body).contains("1048576-byte limit"));
    }

    #[test]
    fn invalid_claims_are_rejected_before_allocation() {
        let piece = fragments(response(22_000)).remove(0);
        let budget = Arc::new(AtomicUsize::new(0));
        for total in [0, MAX + 1, usize::try_from(u32::MAX).unwrap()] {
            let mut invalid = piece.clone();
            invalid.headers[0].1[..4].copy_from_slice(&(total as u32).to_le_bytes());
            assert!(Assembly::accept(&mut None, &budget, invalid).is_err());
            assert_eq!(budget.load(Ordering::Acquire), 0);
        }
        let mut invalid = piece.clone();
        invalid.headers[0].1[4..].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(Assembly::accept(&mut None, &budget, invalid).is_err());
        let mut invalid = piece;
        invalid.body = invalid.body.slice(1..);
        assert!(Assembly::accept(&mut None, &budget, invalid).is_err());
        assert_eq!(budget.load(Ordering::Acquire), 0);
    }

    #[test]
    fn aggregate_budget_is_bounded_and_reusable_after_cleanup() {
        let size = MAX - response(0).encoded_len();
        let piece = fragments(response(size)).remove(0);
        let budget = Arc::new(AtomicUsize::new(0));
        let mut slots = Vec::new();
        for _ in 0..8 {
            let mut slot = None;
            assert!(Assembly::accept(&mut slot, &budget, piece.clone())
                .unwrap()
                .is_none());
            slots.push(slot);
        }
        assert_eq!(budget.load(Ordering::Acquire), AGGREGATE);
        assert!(Assembly::accept(&mut None, &budget, piece.clone()).is_err());
        slots.pop();
        let mut slot = None;
        assert!(Assembly::accept(&mut slot, &budget, piece)
            .unwrap()
            .is_none());
        drop(slot);
        drop(slots);
        assert_eq!(budget.load(Ordering::Acquire), 0);
    }

    #[test]
    fn wrong_peer_call_and_session_cannot_allocate_or_finish_pending_call() {
        let pending = RpcClientPending::new();
        let mut rx = pending.register_large(1, 2, 3);
        let pieces = fragments(response(22_000));
        for (call, peer, session) in [(9, 2, 3), (1, 9, 3), (1, 2, 9)] {
            pending.deliver_session(call, peer, session, pieces[0].clone());
            assert_eq!(pending.fragment_bytes.load(Ordering::Acquire), 0);
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
        }
        for piece in pieces {
            pending.deliver_session(1, 2, 3, piece);
        }
        assert_eq!(rx.try_recv().unwrap(), response(22_000));
        assert_eq!(pending.fragment_bytes.load(Ordering::Acquire), 0);
    }

    #[test]
    fn missing_fragments_cancel_and_contradictory_fragments_release_budget() {
        for mode in ["cancel", "duplicate", "total", "unfragmented"] {
            let pending = RpcClientPending::new();
            let mut rx = pending.register_large(1, 2, 3);
            let piece = fragments(response(22_000)).remove(0);
            pending.deliver_session(1, 2, 3, piece.clone());
            assert!(pending.fragment_bytes.load(Ordering::Acquire) > 0);
            assert!(matches!(
                rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty)
            ));
            if mode == "cancel" {
                pending.cancel(1);
                assert!(rx.try_recv().is_err());
            } else {
                let mut invalid = piece;
                match mode {
                    "duplicate" => invalid.body = Bytes::from(vec![0; CHUNK]),
                    "total" => invalid.headers[0].1[..4].copy_from_slice(&30_000u32.to_le_bytes()),
                    _ => {
                        invalid = RpcResponsePayload {
                            status: RpcStatus::Ok,
                            headers: vec![],
                            body: Bytes::new(),
                        }
                    }
                }
                pending.deliver_session(1, 2, 3, invalid);
                assert_eq!(rx.try_recv().unwrap().status, RpcStatus::Internal);
            }
            assert_eq!(pending.fragment_bytes.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn legacy_waiter_rejects_unsolicited_fragments() {
        let pending = RpcClientPending::new();
        let mut rx = pending.register(1, 2);
        pending.deliver_session(1, 2, 3, fragments(response(22_000)).remove(0));
        assert_eq!(rx.try_recv().unwrap().status, RpcStatus::Internal);
        assert_eq!(pending.fragment_bytes.load(Ordering::Acquire), 0);
    }
}
