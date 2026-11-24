//! napi bindings for capability sensing (SENSING_OLB_COMPLETION_PLAN B1).
//!
//! Every behaviour here is the Rust SDK's (`net_sdk::sensing`), forwarded:
//!
//! - **Provider.** `NetMesh.sensingProvide(capability, value)` returns a
//!   [`ReadinessProvider`] over the SDK's VALUE-backed provider. JavaScript
//!   sets a value; Rust's evaluator reads it on the attestation path. No JS
//!   callback runs per beat.
//! - **Consumer.** `NetMesh.sensingWatch(capability, options)` returns a
//!   [`SensingWatch`]. `changed()` and `close()` follow the SDK's B1-pre
//!   contract. A `close()` while a `changed()` is pending settles that wait
//!   promptly with a `sensing:watch_closed:` error, never success. The wait
//!   is cancel-safe on the Rust side: an abandoned promise consumes no edge.
//! - **Errors.** Every refusal is a JS `Error` whose message is the SDK's
//!   `sensing:<kind>: <message>` wire string. [`sensing_error_class`] answers
//!   its kind and terminal flag from Rust's own classifier, so this binding
//!   never matches `SensingError` variants (C5).

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use net_sdk::sensing::{
    self as sdk, Coalesced, ReadinessValue, SensedViability, SensingError, SensingQuery,
};

use crate::NetMesh;

/// A refusal as a JS error carrying the SDK's wire string verbatim.
fn sensing_err(error: SensingError) -> Error {
    Error::from_reason(error.to_wire())
}

fn millis(value: Option<f64>, name: &str) -> Result<Option<Duration>> {
    match value {
        None => Ok(None),
        // Checked: a finite value too large for `Duration` (1e308 ms) is a
        // refusal, not a panic inside the binding.
        Some(ms) if ms.is_finite() && ms >= 0.0 => Duration::try_from_secs_f64(ms / 1000.0)
            .map(Some)
            .map_err(|_| {
                Error::from_reason(format!(
                    "{name} is too large to represent as a duration; got {ms}"
                ))
            }),
        Some(ms) => Err(Error::from_reason(format!(
            "{name} must be a finite, non-negative number of milliseconds; got {ms}"
        ))),
    }
}

fn as_ms(value: Option<Duration>) -> Option<f64> {
    value.map(|d| d.as_secs_f64() * 1000.0)
}

/// A provider's readiness as a plain value.
///
/// `state` is `"ready"`, `"not_ready"` or `"unevaluable"`.
/// `estimatedStartMs` applies to `"ready"`, and `reason` to `"not_ready"`.
#[napi(object)]
pub struct ReadinessValueJs {
    pub state: String,
    pub estimated_start_ms: Option<f64>,
    pub reason: Option<u32>,
}

fn value_from_js(value: ReadinessValueJs) -> Result<ReadinessValue> {
    match value.state.as_str() {
        "ready" => Ok(ReadinessValue::Ready {
            estimated_start: millis(value.estimated_start_ms, "estimatedStartMs")?,
        }),
        "not_ready" => {
            let reason = value.reason.unwrap_or(0);
            let reason = u16::try_from(reason).map_err(|_| {
                Error::from_reason(format!("reason must fit in 16 bits; got {reason}"))
            })?;
            Ok(ReadinessValue::NotReady { reason })
        }
        "unevaluable" => Ok(ReadinessValue::Unevaluable),
        other => Err(Error::from_reason(format!(
            "state must be \"ready\", \"not_ready\" or \"unevaluable\"; got {other:?}"
        ))),
    }
}

fn value_to_js(value: ReadinessValue) -> ReadinessValueJs {
    match value {
        ReadinessValue::Ready { estimated_start } => ReadinessValueJs {
            state: "ready".into(),
            estimated_start_ms: as_ms(estimated_start),
            reason: None,
        },
        ReadinessValue::NotReady { reason } => ReadinessValueJs {
            state: "not_ready".into(),
            estimated_start_ms: None,
            reason: Some(u32::from(reason)),
        },
        ReadinessValue::Unevaluable => ReadinessValueJs {
            state: "unevaluable".into(),
            estimated_start_ms: None,
            reason: None,
        },
    }
}

/// The two bounds a watch can name, and its optional coalescing.
/// `withinMs` is the consumer-local end-to-end budget. `startWithinMs` is the
/// provider-start predicate that rides the signed interest. `coalesced`
/// observes THROUGH the organization's elected sensing leader (SDK plan
/// LS-7c); it never widens what this node sees and never falls back to an
/// exact watch.
#[napi(object)]
pub struct SensingQueryOptions {
    pub within_ms: Option<f64>,
    pub start_within_ms: Option<f64>,
    pub coalesced: Option<CoalescedOptions>,
}

/// A coalesced watch's size. `top` absent asks the leader for the best one
/// provider; `top: k` (1..32) for up to `k`. Any other `top` is refused with
/// kind `unsatisfiable_coalescing`.
///
/// `top` is a JS `number` read as `f64`, not `u32`: napi's `u32` conversion
/// wraps modulo 2^32 and truncates fractions, so `4294967297` would silently
/// become `Top(1)` and `2.5` `Top(2)`.
#[napi(object)]
pub struct CoalescedOptions {
    pub top: Option<f64>,
}

/// The JS coalescing as the SDK's. Anything that is not an integer `u16` holds
/// — a fraction, a negative, NaN, a value past 65535 — becomes `Top(0)`, so the
/// SDK refuses it with its OWN kind rather than the binding inventing one.
fn coalesced_from_js(options: &CoalescedOptions) -> Coalesced {
    match options.top {
        None => Coalesced::Best,
        Some(k) if k.fract() == 0.0 && (0.0..=f64::from(u16::MAX)).contains(&k) => {
            Coalesced::Top(k as u16)
        }
        Some(_) => Coalesced::Top(0),
    }
}

/// One authorized provider's row in a snapshot.
#[napi(object)]
pub struct SensedProviderJs {
    pub node_id: BigInt,
    /// `"ready"`, `"not_ready"` or `"unknown"`.
    pub readiness: String,
    /// `"viable"`, `"potential"` or `"not_viable"`.
    pub viability: String,
    pub estimated_start_ms: Option<f64>,
    pub route_estimate_ms: Option<f64>,
}

/// Rust's classification of a sensing wire string.
#[napi(object)]
pub struct SensingErrorClass {
    pub kind: String,
    pub terminal: bool,
}

/// Classify a `sensing:` error message, from Rust's own classifier. `null` when
/// `wire` is not a sensing wire string this build knows.
#[napi]
pub fn sensing_error_class(wire: String) -> Option<SensingErrorClass> {
    sdk::classify_sensing_wire(&wire).map(|class| SensingErrorClass {
        kind: class.kind.to_string(),
        terminal: class.terminal,
    })
}

/// An owning handle on one value-backed readiness registration.
#[napi]
pub struct ReadinessProvider {
    inner: sdk::ValueReadiness,
}

#[napi]
impl ReadinessProvider {
    /// Publish a new value, then wake every live observation path. Returns
    /// whether any observation moved. A set after `close()` stores nothing
    /// and returns `false`.
    #[napi]
    pub fn set(&self, value: ReadinessValueJs) -> Result<bool> {
        Ok(self.inner.set(value_from_js(value)?))
    }

    /// The value currently published.
    #[napi]
    pub fn get(&self) -> ReadinessValueJs {
        value_to_js(self.inner.get())
    }

    /// Stop serving readiness. `true` only for the call that removed it.
    #[napi]
    pub fn close(&self) -> bool {
        self.inner.close()
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[napi(getter)]
    pub fn capability(&self) -> String {
        self.inner.capability().as_str().to_string()
    }
}

/// ONE request-relative capture of an authorized population's readiness.
/// Immutable.
#[napi]
pub struct SensingSnapshot {
    pub(crate) inner: sdk::SensingSnapshot,
}

#[napi]
impl SensingSnapshot {
    #[napi(getter)]
    pub fn capability(&self) -> String {
        self.inner.capability().to_string()
    }

    /// Every authorized provider, one row each, in ascending node-id order.
    #[napi(getter)]
    pub fn providers(&self) -> Vec<SensedProviderJs> {
        self.inner
            .providers()
            .iter()
            .map(|row| SensedProviderJs {
                node_id: BigInt::from(row.node_id()),
                readiness: match row.readiness() {
                    sdk::ProjectedReadiness::Ready => "ready",
                    sdk::ProjectedReadiness::NotReady => "not_ready",
                    sdk::ProjectedReadiness::Unknown => "unknown",
                }
                .to_string(),
                viability: match row.viability() {
                    SensedViability::Viable => "viable",
                    SensedViability::Potential => "potential",
                    SensedViability::NotViable => "not_viable",
                }
                .to_string(),
                estimated_start_ms: as_ms(row.estimated_start()),
                route_estimate_ms: as_ms(row.route_estimate()),
            })
            .collect()
    }

    /// The viable providers in sensed rank order.
    #[napi(getter)]
    pub fn ranked(&self) -> Vec<BigInt> {
        self.inner
            .ranked()
            .iter()
            .map(|id| BigInt::from(*id))
            .collect()
    }

    /// The provider to try first, or `null` when nothing is viable.
    #[napi(getter)]
    pub fn preferred(&self) -> Option<BigInt> {
        self.inner.preferred().map(BigInt::from)
    }

    /// Whether this capture came from a coalesced watch: its rows are the
    /// providers the organization's sensing leader chose to sense, clamped to
    /// what this node sees. The ranking is this node's own either way.
    #[napi(getter)]
    pub fn is_coalesced(&self) -> bool {
        self.inner.is_coalesced()
    }
}

/// An owning observation of one capability's authorized exact-provider
/// readiness.
#[napi]
pub struct SensingWatch {
    inner: Arc<tokio::sync::Mutex<sdk::SensingWatch>>,
    closer: sdk::SensingWatchCloser,
    capability: String,
}

#[napi]
impl SensingWatch {
    /// Take a snapshot. Async because it may wait for a pending `changed()`
    /// to settle; it never blocks the JS thread.
    #[napi]
    pub fn snapshot<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, SensingSnapshot>> {
        let inner = self.inner.clone();
        let closer = self.closer.clone();
        env.spawn_future(async move {
            if closer.is_closed() {
                return Err(sensing_err(SensingError::WatchClosed));
            }
            let mut watch = inner.lock().await;
            watch
                .snapshot()
                .map(|inner| SensingSnapshot { inner })
                .map_err(sensing_err)
        })
    }

    /// Resolve when something that can change the answer moved. Rejects with
    /// `sensing:watch_closed:` when the watch is closed, including by a
    /// `close()` while this wait is pending. Abandoning the promise consumes
    /// no edge.
    #[napi]
    pub fn changed<'env>(&self, env: &'env Env) -> Result<PromiseRaw<'env, ()>> {
        let inner = self.inner.clone();
        let closer = self.closer.clone();
        env.spawn_future(async move {
            if closer.is_closed() {
                return Err(sensing_err(SensingError::WatchClosed));
            }
            let mut watch = inner.lock().await;
            watch.changed().await.map_err(sensing_err)
        })
    }

    /// Stop observing. Ends any pending `changed()`. `true` only for the call
    /// that closed it.
    #[napi]
    pub fn close(&self) -> bool {
        self.closer.close()
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.closer.is_closed()
    }

    #[napi(getter)]
    pub fn capability(&self) -> String {
        self.capability.clone()
    }
}

/// Operator counters for sensed organization call routing.
#[napi(object)]
pub struct OrgSensingRoutingCountersJs {
    pub truncated_total: BigInt,
    pub fallback_total: BigInt,
    pub fallback_contended: BigInt,
    pub fallback_refused: BigInt,
    pub fallback_unretained: BigInt,
}

impl NetMesh {
    fn sensing_client(&self) -> Result<sdk::SensingClient> {
        let node = self.node_arc_clone()?;
        let mesh = net_sdk::mesh::Mesh::from_node_arc(
            node,
            Arc::new(net::adapter::net::ChannelConfigRegistry::new()),
            None,
        );
        mesh.sensing().map_err(sensing_err)
    }
}

#[napi]
impl NetMesh {
    /// Serve readiness for `capability` from a stored value. Refuses with
    /// `sensing:disabled:` / `sensing:durable_identity_required:` /
    /// `sensing:incarnation_required:` / `sensing:already_providing:` exactly
    /// as the Rust SDK does.
    #[napi]
    pub fn sensing_provide(
        &self,
        capability: String,
        value: ReadinessValueJs,
    ) -> Result<ReadinessProvider> {
        let initial = value_from_js(value)?;
        let _enter = self.runtime_handle().enter();
        let inner = self
            .sensing_client()?
            .provide_value(capability, initial)
            .map_err(sensing_err)?;
        Ok(ReadinessProvider { inner })
    }

    /// Observe the readiness of this node's authorized own-organization
    /// providers of `capability`.
    #[napi]
    pub fn sensing_watch(
        &self,
        capability: String,
        options: Option<SensingQueryOptions>,
    ) -> Result<SensingWatch> {
        let mut query = SensingQuery::new(capability.clone());
        if let Some(options) = options {
            if let Some(within) = millis(options.within_ms, "withinMs")? {
                query = query.within(within);
            }
            if let Some(start) = millis(options.start_within_ms, "startWithinMs")? {
                query = query.start_within(start);
            }
            if let Some(coalesced) = &options.coalesced {
                query = query.coalesced(coalesced_from_js(coalesced));
            }
        }
        let _enter = self.runtime_handle().enter();
        let watch = self.sensing_client()?.watch(query).map_err(sensing_err)?;
        let closer = watch.closer();
        Ok(SensingWatch {
            inner: Arc::new(tokio::sync::Mutex::new(watch)),
            closer,
            capability,
        })
    }

    /// The operator counters for sensed organization call routing.
    #[napi]
    pub fn org_sensing_routing_counters(&self) -> Result<OrgSensingRoutingCountersJs> {
        let c = self.node_arc_clone()?.org_sensing_routing_counters();
        Ok(OrgSensingRoutingCountersJs {
            truncated_total: BigInt::from(c.truncated_total),
            fallback_total: BigInt::from(c.fallback_total),
            fallback_contended: BigInt::from(c.fallback_contended),
            fallback_refused: BigInt::from(c.fallback_refused),
            fallback_unretained: BigInt::from(c.fallback_unretained),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{coalesced_from_js, millis, CoalescedOptions};
    use net_sdk::sensing::Coalesced;

    /// `top` absent is Best, an integer `u16` is Top(k), and anything else —
    /// past 65535, past 2^32 (which a `u32` read would wrap to 1), a fraction,
    /// a negative, NaN — becomes Top(0), which the SDK refuses with its own kind.
    #[test]
    fn the_js_coalescing_maps_onto_the_sdk() {
        let map = |top: Option<f64>| coalesced_from_js(&CoalescedOptions { top });
        assert_eq!(map(None), Coalesced::Best);
        assert_eq!(map(Some(3.0)), Coalesced::Top(3));
        assert_eq!(map(Some(0.0)), Coalesced::Top(0));
        assert_eq!(map(Some(65_535.0)), Coalesced::Top(65_535));
        for refused in [
            70_000.0,
            4_294_967_297.0,
            2.5,
            -1.0,
            f64::NAN,
            f64::INFINITY,
        ] {
            assert_eq!(map(Some(refused)), Coalesced::Top(0), "{refused}");
        }
    }

    /// A finite value too large for `Duration` is refused with an error, never a
    /// panic in the binding; ordinary and invalid inputs keep their meaning.
    #[test]
    fn millis_refuses_overflow_instead_of_panicking() {
        assert!(millis(Some(1e308), "timeoutMs").is_err());
        assert!(millis(Some(-1.0), "timeoutMs").is_err());
        assert!(millis(Some(f64::NAN), "timeoutMs").is_err());
        assert_eq!(
            millis(Some(1500.0), "timeoutMs").expect("valid"),
            Some(std::time::Duration::from_millis(1500))
        );
        assert_eq!(millis(None, "timeoutMs").expect("absent"), None);
    }
}
