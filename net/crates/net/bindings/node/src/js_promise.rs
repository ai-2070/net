//! Telling an **abandoned** JS Promise apart from a rejected one.
//!
//! Every bridge in this binding that calls a JS callback and awaits the
//! Promise it returns (A2A executors and preflights, nRPC and org handlers,
//! local tool handlers, payment signers, JS blob adapters) goes through
//! napi's `Promise<T>`. That type keeps only the receiving end of a channel;
//! its `then` / `catch` callbacks live on the JS Promise object. When a
//! Promise is pending and nothing references it — so nothing can ever call its
//! `resolve` or `reject` — V8 may collect it. The callbacks are finalized, the
//! sender drops, and napi reports `GenericFailure` with the futures-oneshot
//! reason `"oneshot canceled"` (`napi`'s `Promise::poll`), folded into the
//! same `Result` a JS rejection arrives in.
//!
//! Such a Promise can never settle, so a bridge says what happened — the
//! Promise was dropped before it settled — at once, instead of calling it a
//! rejection or waiting out a deadline. Each bridge keeps its own error
//! variant and status — only the reason changes — so nothing about how a
//! caller classifies the failure moves.
//!
//! **The one ambiguity, kept deliberately.** The detection is the exact
//! status and reason napi uses. napi's public `Error` does not say whether
//! it came from a JS rejection (that error holds a reference to the JS error
//! object, but the field is crate-private) or from the dropped channel, so a
//! JS rejection whose message is literally `oneshot canceled` is classified
//! the same way. That is why the reason states only what the bridge knows —
//! the result channel was lost — and names V8 collecting an unreferenced
//! Promise as the usual cause rather than asserting it. Telling the two
//! apart structurally would mean re-implementing napi's Promise bridging
//! (our own `then`/`catch` on a `PromiseRaw`) in every bridge; the
//! trade-off is pinned by `test/js_promise_abandoned.test.ts`.

/// Whether awaiting a JS Promise failed because the Promise was abandoned
/// (collected while pending) rather than rejected.
pub(crate) fn is_abandoned(e: &napi::Error) -> bool {
    e.status == napi::Status::GenericFailure && e.reason == "oneshot canceled"
}

/// The reason a bridge gives when [`is_abandoned`] holds, after the name of
/// what returned the Promise ("a2a task handler", "JS handler", …).
pub(crate) const NEVER_SETTLES: &str = "returned a Promise that was dropped before it \
     settled (napi lost its result channel — what happens when V8 collects a pending \
     Promise that nothing references)";

/// `"{subject} {NEVER_SETTLES}"` for an abandoned Promise, otherwise the
/// bridge's own rejection text — the one-line form every bridge uses.
pub(crate) fn failure_reason(
    subject: &str,
    e: &napi::Error,
    rejected: impl FnOnce(&napi::Error) -> String,
) -> String {
    if is_abandoned(e) {
        format!("{subject} {NEVER_SETTLES}")
    } else {
        rejected(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_channel_is_abandoned_and_a_rejection_is_not() {
        let dropped = napi::Error::new(napi::Status::GenericFailure, "oneshot canceled");
        assert!(is_abandoned(&dropped));
        for other in [
            napi::Error::new(napi::Status::GenericFailure, "handler threw"),
            napi::Error::new(napi::Status::GenericFailure, "oneshot canceled!"),
            napi::Error::new(napi::Status::InvalidArg, "oneshot canceled"),
            napi::Error::from_reason("oneshot"),
        ] {
            assert!(!is_abandoned(&other), "{other}");
        }
    }

    #[test]
    fn the_reason_names_the_subject_or_keeps_the_rejection() {
        let dropped = napi::Error::new(napi::Status::GenericFailure, "oneshot canceled");
        assert_eq!(
            failure_reason("JS handler", &dropped, |e| format!("rejected: {e}")),
            format!("JS handler {NEVER_SETTLES}")
        );
        let thrown = napi::Error::new(napi::Status::GenericFailure, "boom");
        assert!(
            failure_reason("JS handler", &thrown, |e| format!("rejected: {e}"))
                .starts_with("rejected: ")
        );
    }
}
