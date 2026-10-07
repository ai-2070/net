//! Shared fixture for the capability-fold scale benches (`fold_scale`
//! and `fold_scale_report`). See CAPABILITY_FOLD_SCALE_PLAN.md, Slice 0.
//!
//! Every envelope a bench applies is built here, outside any timed
//! region. Payloads come from a fixed pool of translated templates, so
//! building a 1M-entry fold costs one clone per entry instead of one
//! `sample_capability_set` + `translate_announcement` per entry. The pool
//! is the full period of `sample_capability_set`'s variation
//! (lcm(64, 4, 100, 2, 3, 5) = 4800), so the tag distribution matches what
//! building every entry from scratch would give.

#![allow(dead_code)]

use std::time::Duration;

use net::adapter::net::behavior::fold::{
    capability_bridge, ApplyOutcome, CapabilityFold, CapabilityMembership, Fold, NodeId,
    SignedAnnouncement,
};
use net::adapter::net::behavior::{
    CapabilityAnnouncement, CapabilitySet, GpuInfo, GpuVendor, HardwareCapabilities, Modality,
    ModelCapability, ResourceLimits, SoftwareCapabilities, ToolCapability,
};
use net::adapter::net::identity::EntityId;

/// Period of `sample_capability_set`'s per-index variation.
pub const TEMPLATE_PERIOD: u64 = 4800;

/// TTL for entries that must stay live for the whole bench.
pub const LIVE_TTL_SECS: u32 = 3600;

/// The bench workload's capability set: the single definition used by
/// the `net` bench's `capability_fold_*` rows and by the fold-scale
/// benches, so their numbers stay comparable.
pub fn sample_capability_set(node_index: u64) -> CapabilitySet {
    let gpu = GpuInfo::new(GpuVendor::Nvidia, "RTX 4090", 24)
        .with_compute_units(128)
        .with_tensor_cores(512)
        .with_fp16_tflops(82.5);

    let hardware = HardwareCapabilities::new()
        .with_cpu(16, 32)
        .with_memory(64 + (node_index as u32 % 64))
        .with_gpu(gpu)
        .with_storage(2000)
        .with_network(10);

    let software = SoftwareCapabilities::new()
        .with_os("linux", "6.1")
        .add_runtime("python", "3.11")
        .add_framework("pytorch", "2.1")
        .with_cuda("12.1");

    let model = ModelCapability::new(format!("llama-3.1-{}b", 7 + (node_index % 4) * 20), "llama")
        .with_parameters(7.0 + (node_index % 4) as f32 * 20.0)
        .with_context_length(128000)
        .with_quantization("fp16")
        .add_modality(Modality::Text)
        .add_modality(Modality::Code)
        .with_tokens_per_sec(50 + (node_index % 100) as u32)
        .with_loaded(node_index.is_multiple_of(3));

    let tool = ToolCapability::new("python_repl", "Python REPL")
        .with_version("1.0.0")
        .with_estimated_time(100);

    let mut caps = CapabilitySet::new()
        .with_hardware(hardware)
        .with_software(software)
        .add_model(model)
        .add_tool(tool)
        .with_limits(ResourceLimits::new().with_max_concurrent(10));

    if node_index.is_multiple_of(2) {
        caps = caps.add_tag("inference");
    }
    if node_index.is_multiple_of(3) {
        caps = caps.add_tag("training");
    }
    if node_index.is_multiple_of(5) {
        caps = caps.add_tag("gpu-cluster");
    }

    caps
}

/// A legacy announcement for node `i`, as the production ingest path
/// receives it.
pub fn legacy_announcement(i: u64) -> CapabilityAnnouncement {
    CapabilityAnnouncement::new(
        i,
        EntityId::from_bytes([0u8; 32]),
        1,
        sample_capability_set(i),
    )
}

/// Translated payload templates, one per index in the variation period.
pub struct Templates {
    anns: Vec<SignedAnnouncement<CapabilityMembership>>,
}

impl Templates {
    pub fn new() -> Self {
        let anns = (0..TEMPLATE_PERIOD)
            .map(|i| capability_bridge::translate_announcement(&legacy_announcement(i), None))
            .collect();
        Self { anns }
    }

    /// A fresh owned envelope for `node`. `variant` 0 is the template's
    /// payload; variant 1 adds one indexed tag, so applying 1 over 0 (or
    /// back) is an index-changing Replace, while re-applying the same
    /// variant with a higher generation is an index-equivalent refresh.
    pub fn envelope(
        &self,
        node: NodeId,
        generation: u64,
        ttl_secs: u32,
        variant: u8,
    ) -> SignedAnnouncement<CapabilityMembership> {
        let mut ann = self.anns[(node % TEMPLATE_PERIOD) as usize].clone();
        ann.node_id = node;
        ann.generation = generation;
        ann.ttl_secs = Some(ttl_secs);
        if variant == 1 {
            ann.payload.tags.push("bench-variant-b".into());
        }
        ann
    }

    /// Like [`Self::envelope`], but with `unique` extra tags that no
    /// other node carries. Used by the footprint probe's mixed-unique
    /// configuration.
    pub fn envelope_with_unique_tags(
        &self,
        node: NodeId,
        generation: u64,
        unique: usize,
    ) -> SignedAnnouncement<CapabilityMembership> {
        let mut ann = self.envelope(node, generation, LIVE_TTL_SECS, 0);
        for k in 0..unique {
            ann.payload.tags.push(format!("unique-{node}-{k}").into());
        }
        ann
    }
}

/// A fold with no background sweeper, so expiry runs only when a bench
/// calls `sweep_expired_now`.
pub fn empty_fold() -> Fold<CapabilityFold> {
    Fold::<CapabilityFold>::with_sweep_interval(Duration::ZERO)
}

/// Node ids are `1..=n`; 0 is avoided only to keep ids readable in
/// assertion messages.
pub fn node_id(i: u64) -> NodeId {
    i + 1
}

/// Build a fold of `n` live entries, every one at generation 1 and
/// variant 0.
pub fn live_fold(templates: &Templates, n: u64) -> Fold<CapabilityFold> {
    let fold = empty_fold();
    for i in 0..n {
        let outcome = fold
            .apply(templates.envelope(node_id(i), 1, LIVE_TTL_SECS, 0))
            .expect("fixture apply");
        assert_eq!(outcome, ApplyOutcome::Inserted, "fixture apply of node {i}");
    }
    fold
}

/// Every `stride`-th node, the population a mass-expiry bench expires.
pub fn every_nth(n: u64, stride: u64) -> Vec<NodeId> {
    (0..n).step_by(stride as usize).map(node_id).collect()
}

/// Small deterministic PRNG (xorshift64*), so the workloads need no
/// extra dependency and replay identically.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, bound: u64) -> u64 {
        self.next_u64() % bound
    }
}
