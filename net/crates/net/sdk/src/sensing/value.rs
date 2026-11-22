//! The value-backed provider (SENSING_OLB_COMPLETION_PLAN B1).
//!
//! [`ReadinessEvaluator::evaluate`](super::ReadinessEvaluator) is synchronous
//! and runs on the attestation path, once per due beat per branch. A language
//! binding must not call back into JavaScript or Python there: the callback
//! would hold the GIL, hop the napi thread boundary, or wait behind a busy
//! event loop, and stall every branch the node signs.
//!
//! So the SDK provides an evaluator whose answer is a stored VALUE. The
//! application sets it whenever its readiness changes, from any thread and in
//! any language. Each set publishes the value first and then wakes the
//! observation paths, which is the order
//! [`ReadinessRegistration::changed`](super::ReadinessRegistration::changed)
//! requires. The evaluator itself only reads.
//!
//! One rule is the evaluator's own: a `Ready` value whose estimated start
//! exceeds the request's provider-start bound answers `NotReady`, with
//! [`START_BOUND_EXCEEDED`]. That bound is the signed predicate
//! ([`SensingQuery::start_within`](super::SensingQuery::start_within)), and a
//! provider that cannot start in time does not hold it. A `Ready` value with
//! no estimate holds every bound; the provider has claimed nothing it could
//! violate.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use super::{
    CapabilityId, EvaluationRequest, ReadinessEvaluation, ReadinessEvaluator,
    ReadinessRegistration, SensingClient, SensingError,
};

/// The `NotReady` reason a value provider reports when its `Ready` estimate
/// cannot meet the request's provider-start bound. Diagnostics only, like
/// every reason code.
pub const START_BOUND_EXCEEDED: u16 = 0xFFF0;

/// A provider's current readiness, as a plain value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ReadinessValue {
    /// Ready, optionally with the provider's own estimate of time-to-start.
    Ready {
        /// When the provider expects it could start new work.
        estimated_start: Option<Duration>,
    },
    /// Not ready. `reason` is a provider-defined diagnostic code.
    NotReady {
        /// Provider-defined detail code; never semantics.
        reason: u16,
    },
    /// The provider cannot currently say. Consumers read this as `Unknown`,
    /// which never prunes.
    Unevaluable,
}

impl ReadinessValue {
    /// What a value evaluator answers for one request.
    fn answer(self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        match self {
            Self::Ready { estimated_start } => {
                let bound = request.work_latency.provider_start_within;
                match (estimated_start, bound) {
                    (Some(start), Some(bound)) if start > bound => ReadinessEvaluation::NotReady {
                        reason: START_BOUND_EXCEEDED,
                    },
                    _ => ReadinessEvaluation::Ready { estimated_start },
                }
            }
            Self::NotReady { reason } => ReadinessEvaluation::NotReady { reason },
            Self::Unevaluable => ReadinessEvaluation::TemporarilyUnevaluable,
        }
    }
}

/// The published value and whether the handle is closed, under ONE lock, so a
/// `set` racing a `close` either lands before it or stores nothing.
struct ValueCell {
    value: ReadinessValue,
    closed: bool,
}

/// The evaluator the node holds: a shared cell, read on every beat.
struct ValueEvaluator {
    value: Arc<RwLock<ValueCell>>,
}

impl ReadinessEvaluator for ValueEvaluator {
    fn evaluate(&self, request: &EvaluationRequest<'_>) -> ReadinessEvaluation {
        self.value.read().value.answer(request)
    }
}

/// An owning handle on one value-backed readiness registration. Obtained from
/// [`SensingClient::provide_value`].
///
/// `Send` and `Sync`: [`Self::set`] may be called from any thread. Closing it
/// (or dropping it) removes the registration exactly as
/// [`ReadinessRegistration::close`] does.
pub struct ValueReadiness {
    value: Arc<RwLock<ValueCell>>,
    registration: ReadinessRegistration,
}

impl ValueReadiness {
    /// Publish a new readiness value, then wake every live observation path
    /// on this capability.
    ///
    /// Returns whether any live observation path moved; the value is stored
    /// either way. A set after [`Self::close`] stores nothing and returns
    /// `false`.
    pub fn set(&self, value: ReadinessValue) -> bool {
        {
            // The closed check and the write are one critical section with
            // `close`, so a set that loses the race stores nothing.
            let mut cell = self.value.write();
            if cell.closed || self.registration.is_closed() {
                return false;
            }
            cell.value = value;
        }
        self.registration.changed()
    }

    /// The value currently published.
    pub fn get(&self) -> ReadinessValue {
        self.value.read().value
    }

    /// The capability this registration serves.
    pub fn capability(&self) -> &CapabilityId {
        self.registration.capability()
    }

    /// Stop serving readiness. `true` only for the call that removed it.
    pub fn close(&self) -> bool {
        // Mark the cell closed FIRST, under the lock every `set` takes, so no
        // set can publish once this returns.
        self.value.write().closed = true;
        self.registration.close()
    }

    /// Whether this handle has been closed.
    pub fn is_closed(&self) -> bool {
        self.registration.is_closed()
    }
}

impl std::fmt::Debug for ValueReadiness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueReadiness")
            .field("capability", &self.capability().as_str())
            .field("value", &self.get())
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl SensingClient {
    /// Serve readiness for one capability from a stored value (B1). This is
    /// the provider shape for callers that cannot run a synchronous evaluator
    /// on the attestation path: every language binding, and any Rust
    /// application whose readiness is simply a state it already tracks.
    ///
    /// The same refusals as [`Self::provide`], and the same no-steal rule: an
    /// incumbent registration is never replaced.
    pub fn provide_value(
        &self,
        capability: impl Into<CapabilityId>,
        initial: ReadinessValue,
    ) -> Result<ValueReadiness, SensingError> {
        let value = Arc::new(RwLock::new(ValueCell {
            value: initial,
            closed: false,
        }));
        let registration = self.provide(
            capability,
            Arc::new(ValueEvaluator {
                value: Arc::clone(&value),
            }),
        )?;
        Ok(ValueReadiness {
            value,
            registration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use net::adapter::net::behavior::sensing::{CanonicalConstraints, WorkLatencyEnvelope};

    fn ask(value: ReadinessValue, bound: Option<Duration>) -> ReadinessEvaluation {
        let capability = CapabilityId::new("gpu.infer");
        let constraints = CanonicalConstraints::default();
        let latency = WorkLatencyEnvelope {
            provider_start_within: bound,
            first_event_after_admission: None,
        };
        value.answer(&EvaluationRequest {
            capability_id: &capability,
            constraints: &constraints,
            work_latency: &latency,
        })
    }

    /// The value maps onto the frozen evaluation model one-to-one, except
    /// that a Ready estimate over the signed start bound is NotReady.
    #[test]
    fn a_value_answers_the_signed_predicate() {
        let ms = Duration::from_millis;
        assert_eq!(
            ask(
                ReadinessValue::Ready {
                    estimated_start: Some(ms(50))
                },
                Some(ms(100))
            ),
            ReadinessEvaluation::Ready {
                estimated_start: Some(ms(50))
            }
        );
        assert_eq!(
            ask(
                ReadinessValue::Ready {
                    estimated_start: Some(ms(150))
                },
                Some(ms(100))
            ),
            ReadinessEvaluation::NotReady {
                reason: START_BOUND_EXCEEDED
            },
            "a provider that cannot start in time does not hold the predicate"
        );
        assert_eq!(
            ask(
                ReadinessValue::Ready {
                    estimated_start: Some(ms(100))
                },
                Some(ms(100))
            ),
            ReadinessEvaluation::Ready {
                estimated_start: Some(ms(100))
            },
            "the bound is inclusive"
        );
        assert_eq!(
            ask(
                ReadinessValue::Ready {
                    estimated_start: None
                },
                Some(ms(1))
            ),
            ReadinessEvaluation::Ready {
                estimated_start: None
            },
            "no estimate claims nothing a bound can refute"
        );
        assert_eq!(
            ask(
                ReadinessValue::Ready {
                    estimated_start: Some(ms(150))
                },
                None
            ),
            ReadinessEvaluation::Ready {
                estimated_start: Some(ms(150))
            }
        );
        assert_eq!(
            ask(ReadinessValue::NotReady { reason: 7 }, None),
            ReadinessEvaluation::NotReady { reason: 7 }
        );
        assert_eq!(
            ask(ReadinessValue::Unevaluable, None),
            ReadinessEvaluation::TemporarilyUnevaluable
        );
    }

    #[test]
    fn the_value_handle_is_send_and_sync() {
        fn require<T: Send + Sync>() {}
        require::<ValueReadiness>();
    }
}
