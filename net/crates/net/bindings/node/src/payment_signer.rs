//! JS signer callbacks bridged into the payments `SchemeSigner` seam — the
//! non-custodial settlement path for real networks. A JS callback
//! `(typedIntentJson: string) => Promise<string>` is converted to a
//! `ThreadsafeFunction` and wrapped in `ExternalSigner` / `ExternalSvmSigner` /
//! `ExternalXrplSigner`; the payments flow calls it with a typed intent and
//! gets the signed artifact back. Typed intent in, artifact out: no raw-bytes
//! path, key material unrepresentable — the napi analog of the Python
//! `spawn_blocking` + `Python::attach` signer bridge, using the same
//! TSFN→Promise pattern as `blob.rs`'s async blob adapter.

#![cfg(feature = "payments")]

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::Promise;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use serde_json::Value;

use net_payments::flow::signer::{
    ExternalSigner, ExternalSvmSigner, ExternalXrplSigner, SchemeSigner, SignerError,
};
use net_payments::x402::schemes::exact_svm::SvmTransferIntent;
use net_payments::x402::schemes::exact_xrpl::XrplPaymentIntent;

/// The bridged JS signer callback: `(typedIntentJson: string) => Promise<string>`.
pub(crate) type SignerTsfn =
    ThreadsafeFunction<String, Promise<String>, String, napi::Status, false>;

/// Total budget across both stages (JS returning the Promise + the Promise
/// resolving), matching the blob adapter's async bridge — the worst case is
/// `SIGNER_TIMEOUT`, not `2×`.
const SIGNER_TIMEOUT: Duration = Duration::from_secs(60);

/// Call the JS signer with the typed intent JSON, await its Promise, return the
/// signature/artifact string. The TSFN enqueues onto the JS event loop; the
/// Promise resolution drives back to this tokio task.
async fn call_js_signer(
    tsfn: &SignerTsfn,
    label: &'static str,
    intent_json: String,
) -> Result<String, SignerError> {
    let (tx, rx) = tokio::sync::oneshot::channel::<napi::Result<Promise<String>>>();
    let status = tsfn.call_with_return_value(
        intent_json,
        ThreadsafeFunctionCallMode::NonBlocking,
        move |ret, _env| {
            let _ = tx.send(ret);
            Ok(())
        },
    );
    if status != napi::Status::Ok {
        return Err(SignerError::new(format!(
            "{label}: signer TSFN enqueue status {status:?}"
        )));
    }
    // Total-budget both stages against one deadline.
    let deadline = tokio::time::Instant::now() + SIGNER_TIMEOUT;
    let promise = match tokio::time::timeout_at(deadline, rx).await {
        Ok(Ok(Ok(p))) => p,
        Ok(Ok(Err(e))) => {
            return Err(SignerError::new(format!(
                "{label}: signer threw before returning a Promise: {e}"
            )))
        }
        Ok(Err(_)) => {
            return Err(SignerError::new(format!(
                "{label}: signer callback channel disconnected"
            )))
        }
        Err(_) => {
            return Err(SignerError::new(format!(
                "{label}: signer did not return a Promise within {} ms",
                SIGNER_TIMEOUT.as_millis()
            )))
        }
    };
    match tokio::time::timeout_at(deadline, promise).await {
        Ok(Ok(sig)) => Ok(sig),
        Ok(Err(e)) => Err(SignerError::new(format!(
            "{label}: signer Promise rejected: {e}"
        ))),
        Err(_) => Err(SignerError::new(format!(
            "{label}: signer Promise did not resolve within {} ms",
            SIGNER_TIMEOUT.as_millis()
        ))),
    }
}

/// eip155: the EIP-712 typed-data document (JSON) in, the 0x-hex signature out.
pub(crate) fn eip155_signer(address: String, tsfn: SignerTsfn) -> Arc<dyn SchemeSigner> {
    // `ThreadsafeFunction` isn't `Clone`, but each returned future must own the
    // handle ('static); share it behind an `Arc` and clone that per call.
    let tsfn = Arc::new(tsfn);
    Arc::new(ExternalSigner::new(address, move |typed: Value| {
        let tsfn = tsfn.clone();
        Box::pin(
            async move { call_js_signer(&tsfn, "eip155 payment signer", typed.to_string()).await },
        )
    }))
}

/// solana: the `SvmTransferIntent` (JSON) in, the base64 partially-signed
/// versioned transaction out.
pub(crate) fn svm_signer(address: String, tsfn: SignerTsfn) -> Arc<dyn SchemeSigner> {
    let tsfn = Arc::new(tsfn);
    Arc::new(ExternalSvmSigner::new(
        address,
        move |intent: SvmTransferIntent| {
            let tsfn = tsfn.clone();
            Box::pin(async move {
                let json = serde_json::to_string(&intent)
                    .map_err(|e| SignerError::new(format!("svm transfer intent serialize: {e}")))?;
                call_js_signer(&tsfn, "svm payment signer", json).await
            })
        },
    ))
}

/// xrpl: the `XrplPaymentIntent` (JSON) in, the hex presigned Payment blob out.
pub(crate) fn xrpl_signer(address: String, tsfn: SignerTsfn) -> Arc<dyn SchemeSigner> {
    let tsfn = Arc::new(tsfn);
    Arc::new(ExternalXrplSigner::new(
        address,
        move |intent: XrplPaymentIntent| {
            let tsfn = tsfn.clone();
            Box::pin(async move {
                let json = serde_json::to_string(&intent)
                    .map_err(|e| SignerError::new(format!("xrpl payment intent serialize: {e}")))?;
                call_js_signer(&tsfn, "xrpl payment signer", json).await
            })
        },
    ))
}
