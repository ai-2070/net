//! Async stream-based event consumption.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::Stream;
use futures::StreamExt;

use net::consumer::Ordering;
use net::{ConsumeRequest, EventBus, Filter, StoredEvent};

use crate::error::{Result, SdkError};

/// Options for subscribing to events.
#[derive(Clone, Debug)]
pub struct SubscribeOpts {
    pub(crate) limit: usize,
    pub(crate) filter: Option<Filter>,
    pub(crate) ordering: Ordering,
    pub(crate) poll_interval: Duration,
    pub(crate) max_backoff: Duration,
}

impl Default for SubscribeOpts {
    fn default() -> Self {
        Self {
            limit: 100,
            filter: None,
            ordering: Ordering::None,
            poll_interval: Duration::from_millis(1),
            max_backoff: Duration::from_millis(100),
        }
    }
}

impl SubscribeOpts {
    /// Set the maximum number of events per poll.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set an event filter.
    pub fn filter(mut self, filter: Filter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Set the event ordering.
    pub fn ordering(mut self, ordering: Ordering) -> Self {
        self.ordering = ordering;
        self
    }

    /// Set the base poll interval.
    ///
    /// Pre-fix, `Duration::ZERO` was accepted verbatim,
    /// and combined with a zero `max_backoff` the doubling loop
    /// at `current_interval = (current_interval * 2).min(max_backoff)`
    /// resolved to zero forever. The poll-then-zero-sleep-then-
    /// wake_by_ref path then ran at 100 % CPU on an idle stream.
    /// Clamp to a minimum of 1 ns so even pathological inputs stay
    /// out of the spin path; production callers wanting a tight
    /// poll should set `Duration::from_millis(1)` or similar.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(MIN_BACKOFF_INTERVAL);
        self
    }

    /// Set the maximum backoff interval.
    ///
    /// See `poll_interval` — the same zero-collapse hazard
    /// applies. Clamped to a minimum of 1 ns so the doubling loop
    /// always parks the task on a real timer rather than spinning.
    pub fn max_backoff(mut self, max: Duration) -> Self {
        self.max_backoff = max.max(MIN_BACKOFF_INTERVAL);
        self
    }
}

/// Lower bound on `poll_interval` and `max_backoff`. Anything
/// shorter would let the doubling-and-sleep loop in `poll_next`
/// resolve to zero and burn CPU instead of parking on a timer.
/// 1 ns is below any realistic timer resolution but cleanly above
/// `Duration::ZERO`, which is the actual danger.
const MIN_BACKOFF_INTERVAL: Duration = Duration::from_nanos(1);

type PollFuture = Pin<
    Box<
        dyn Future<Output = std::result::Result<net::ConsumeResponse, net::error::ConsumerError>>
            + Send,
    >,
>;

/// An async stream of events from the event bus.
///
/// Internally polls the bus with adaptive backoff — polls tightly when
/// events are flowing, backs off when idle.
pub struct EventStream {
    bus: Arc<EventBus>,
    opts: SubscribeOpts,
    cursor: Option<String>,
    buffer: Vec<StoredEvent>,
    buffer_idx: usize,
    current_interval: Duration,
    sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    inflight: Option<PollFuture>,
}

impl EventStream {
    pub(crate) fn new(bus: Arc<EventBus>, opts: SubscribeOpts) -> Self {
        let interval = opts.poll_interval;
        Self {
            bus,
            opts,
            cursor: None,
            buffer: Vec::new(),
            buffer_idx: 0,
            current_interval: interval,
            sleep: None,
            inflight: None,
        }
    }
}

impl Stream for EventStream {
    type Item = Result<StoredEvent>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        // Return buffered events first.
        if this.buffer_idx < this.buffer.len() {
            let event = this.buffer[this.buffer_idx].clone();
            this.buffer_idx += 1;
            return Poll::Ready(Some(Ok(event)));
        }

        // If we have a sleep pending, wait for it.
        if let Some(sleep) = &mut this.sleep {
            match Pin::new(sleep).poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(()) => {
                    this.sleep = None;
                }
            }
        }

        // If we have an in-flight poll, resume it.
        if this.inflight.is_none() {
            let mut request = ConsumeRequest::new(this.opts.limit);
            if let Some(cursor) = &this.cursor {
                request = request.from(cursor);
            }
            if let Some(filter) = &this.opts.filter {
                request = request.filter(filter.clone());
            }
            request = request.ordering(this.opts.ordering);

            let bus = this.bus.clone();
            this.inflight = Some(Box::pin(async move { bus.poll(request).await }));
        }

        let fut = this.inflight.as_mut().unwrap();
        match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => {
                this.inflight = None;
                Poll::Ready(Some(Err(SdkError::from(e))))
            }
            Poll::Ready(Ok(response)) => {
                this.inflight = None;
                if response.events.is_empty() {
                    // Backoff only when the poll made no forward
                    // progress. Pre-fix this branch fired on
                    // `events.is_empty()` regardless of
                    // `response.has_more` / `next_id`: a poll that
                    // advanced the cursor past records this shard's
                    // filter didn't match returned an empty batch
                    // AND a fresh `next_id`, but the doubling fired
                    // anyway and the wait grew exponentially even
                    // though forward progress was happening. The
                    // cursor's advance is the right "made progress"
                    // signal; reset backoff when next_id changed.
                    let progressed = response
                        .next_id
                        .as_ref()
                        .map(|new| Some(new) != this.cursor.as_ref())
                        .unwrap_or(false);
                    if progressed {
                        this.cursor = response.next_id;
                        this.current_interval = this.opts.poll_interval;
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                    // Backoff: double the interval, up to max.
                    // `current_interval * 2` panics on
                    // Duration overflow if a caller passed a
                    // pathological `poll_interval` (close to
                    // `Duration::MAX`). `saturating_mul` clamps to
                    // `Duration::MAX` so the bound is the
                    // `min(max_backoff)` clamp on the next line.
                    this.current_interval = this
                        .current_interval
                        .saturating_mul(2)
                        .min(this.opts.max_backoff);
                    let mut sleep = Box::pin(tokio::time::sleep(this.current_interval));
                    // Poll the sleep once now so the timer registers
                    // its waker with the executor. Returning Pending
                    // here parks the task on the timer directly,
                    // rather than paying an extra round-trip through
                    // the scheduler (the old code did
                    // `cx.waker().wake_by_ref()` immediately after
                    // creating the sleep, forcing one wasted re-poll
                    // per idle backoff tick).
                    //
                    // If the sleep resolves immediately (zero / already-
                    // elapsed duration), re-wake the task so the next
                    // `poll_next` kicks off a fresh poll instead of
                    // silently parking without a wake (cubic code
                    // review P2).
                    match sleep.as_mut().poll(cx) {
                        Poll::Pending => {
                            this.sleep = Some(sleep);
                            Poll::Pending
                        }
                        Poll::Ready(()) => {
                            // Don't stash the fired sleep; let the
                            // next poll build a fresh one.
                            cx.waker().wake_by_ref();
                            Poll::Pending
                        }
                    }
                } else {
                    // Reset backoff on activity.
                    this.current_interval = this.opts.poll_interval;
                    this.cursor = response.next_id;
                    this.buffer = response.events;
                    this.buffer_idx = 1;
                    Poll::Ready(Some(Ok(this.buffer[0].clone())))
                }
            }
        }
    }
}

/// A typed async stream that deserializes events into `T`.
pub struct TypedEventStream<T> {
    inner: EventStream,
    _marker: std::marker::PhantomData<T>,
}

impl<T: serde::de::DeserializeOwned> TypedEventStream<T> {
    pub(crate) fn new(bus: Arc<EventBus>, opts: SubscribeOpts) -> Self {
        Self {
            inner: EventStream::new(bus, opts),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: serde::de::DeserializeOwned + Unpin> Stream for TypedEventStream<T> {
    type Item = Result<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Some(Ok(event))) => {
                let parsed =
                    serde_json::from_slice(event.raw.as_ref()).map_err(SdkError::Serialization);
                Poll::Ready(Some(parsed))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SubscribeOpts::default().poll_interval(ZERO)`
    /// must not store `Duration::ZERO`. Pre-fix the doubling
    /// loop at `current_interval * 2` would resolve to zero
    /// forever, the sleep would resolve immediately, and the
    /// task would re-wake itself in a tight loop at 100% CPU.
    #[test]
    fn poll_interval_clamps_zero_to_minimum() {
        let opts = SubscribeOpts::default().poll_interval(Duration::ZERO);
        assert!(
            opts.poll_interval > Duration::ZERO,
            "poll_interval(ZERO) must clamp above zero; got {:?}",
            opts.poll_interval
        );
    }

    /// Same clamp on `max_backoff`.
    #[test]
    fn max_backoff_clamps_zero_to_minimum() {
        let opts = SubscribeOpts::default().max_backoff(Duration::ZERO);
        assert!(
            opts.max_backoff > Duration::ZERO,
            "max_backoff(ZERO) must clamp above zero; got {:?}",
            opts.max_backoff
        );
    }

    /// Setting both to zero (the worst case from the
    /// audit) must still produce a non-zero effective interval.
    #[test]
    fn both_zero_still_has_nonzero_intervals() {
        let opts = SubscribeOpts::default()
            .poll_interval(Duration::ZERO)
            .max_backoff(Duration::ZERO);
        assert!(opts.poll_interval > Duration::ZERO);
        assert!(opts.max_backoff > Duration::ZERO);
        // The min() of the doubling loop would clamp current_interval
        // to max_backoff each tick — confirming that, post-clamp,
        // the result is still non-zero.
        let doubled = opts.poll_interval.saturating_mul(2).min(opts.max_backoff);
        assert!(
            doubled > Duration::ZERO,
            "post-clamp doubled interval must be > 0 to avoid spin; got {:?}",
            doubled
        );
    }

    /// `current_interval * 2` panics on Duration
    /// overflow. `saturating_mul(2)` clamps to `Duration::MAX`.
    #[test]
    fn saturating_mul_does_not_panic_on_huge_interval() {
        // Use the largest Duration that, when doubled, would
        // overflow `*` (panic) but stay well-defined under
        // `saturating_mul`.
        let huge = Duration::from_secs(u64::MAX);
        // Pre-fix this would have been `huge * 2` and panicked
        // on overflow when invoked from inside the poll loop.
        let doubled = huge.saturating_mul(2);
        assert_eq!(
            doubled,
            Duration::MAX,
            "saturating_mul must clamp to Duration::MAX, not panic"
        );
    }

    /// Defaults must remain in the safe range and never trigger
    /// the bug — guards against future default-tweaks.
    #[test]
    fn defaults_are_safe() {
        let opts = SubscribeOpts::default();
        assert!(opts.poll_interval > Duration::ZERO);
        assert!(opts.max_backoff > Duration::ZERO);
        assert!(opts.poll_interval <= opts.max_backoff);
    }
}
