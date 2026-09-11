//! `connect_rtc_loopback` — two native nodes, one DataChannel, no
//! signalling subprotocol.
//!
//! Stage 3 has no `0x0D02` and no bootstrap listener (both Stage 4),
//! so the offer/answer/candidate exchange happens in-process: this
//! function is the signalling channel. Everything **below** it is the
//! production path — str0m through the driver, the Noise handshake
//! through `PeerSink` and the dispatch loop, and `install_direct` for
//! the peer record.
//!
//! Test and fixtures only. It exists so the existing witness
//! scenarios can be re-run against a peer on `PeerAddr::Rtc`, which
//! is the Stage 3 exit criterion that matters most.

use std::sync::Arc;
use std::time::Duration;

use str0m::Candidate;

use crate::adapter::net::MeshNode;
use crate::error::AdapterError;

use super::RtcPeerId;

/// Establish a DataChannel between `a` and `b` and run the Noise
/// handshake over it, leaving both sides with a direct session whose
/// endpoint is `PeerAddr::Rtc`.
///
/// `a` offers and is the Noise initiator; `b` answers and responds.
/// Both nodes must already be `start()`ed: the handshake rides the
/// dispatch loop's single ingress owner on both sides, which is the
/// property the harness is meant to exercise.
///
/// Returns the two endpoint handles, `(a's handle for b, b's handle
/// for a)`.
pub async fn connect_rtc_loopback(
    a: &Arc<MeshNode>,
    b: &Arc<MeshNode>,
) -> Result<(RtcPeerId, RtcPeerId), AdapterError> {
    let driver_a = a
        .rtc_driver()
        .ok_or_else(|| AdapterError::Connection("node a has no rtc driver".into()))?;
    let driver_b = b
        .rtc_driver()
        .ok_or_else(|| AdapterError::Connection("node b has no rtc driver".into()))?;

    let err = |e: String| AdapterError::Connection(e);

    // 1. Offer / answer, in-process.
    let (id_a, offer) = driver_a.create_offer().await.map_err(err)?;
    let (id_b, answer) = driver_b.accept_offer(offer).await.map_err(err)?;
    driver_a.accept_answer(id_a, answer).await.map_err(err)?;

    // 2. Host candidates. Both drivers added their own before
    //    producing SDP; on loopback the peers' addresses are known
    //    here, so trickle them across rather than waiting for a
    //    gathering round trip (S0b: trickle is 6.6x faster at the
    //    floor, and this is the same shape).
    let candidate_a = Candidate::host(driver_a.local_addr(), "udp")
        .map_err(|e| AdapterError::Connection(format!("local candidate: {e}")))?
        .to_sdp_string();
    let candidate_b = Candidate::host(driver_b.local_addr(), "udp")
        .map_err(|e| AdapterError::Connection(format!("local candidate: {e}")))?
        .to_sdp_string();
    driver_a
        .remote_candidate(id_a, candidate_b)
        .await
        .map_err(err)?;
    driver_b
        .remote_candidate(id_b, candidate_a)
        .await
        .map_err(err)?;

    // 3. Wait for the channel on both sides.
    driver_a.await_open(id_a).await.map_err(err)?;
    driver_b.await_open(id_b).await.map_err(err)?;

    // 4. Noise over the DataChannel, through the production paths.
    //    The responder registers first: its inbox has to exist before
    //    msg1 lands, exactly as in the UDP case.
    let b_task = {
        let b = Arc::clone(b);
        let a_node_id = a.node_id();
        tokio::spawn(async move { b.accept_rtc(id_b, a_node_id).await })
    };
    // Give the responder a moment to register its inbox before msg1
    // is sent; the initiator retries on timeout anyway, but this
    // keeps the happy path to one attempt.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let b_pubkey = *b.public_key();
    a.connect_rtc(id_a, &b_pubkey, b.node_id()).await?;
    b_task
        .await
        .map_err(|e| AdapterError::Connection(format!("rtc accept task: {e}")))??;

    Ok((id_a, id_b))
}
