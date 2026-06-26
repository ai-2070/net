//! Canonical encoding of task-lifecycle ids into MeshOS `DaemonRef`s.
//!
//! A task's *daemon* is the worker that runs its step. Reconcile keys
//! daemons by `DaemonRef { id, name }`, and some surfaces key on the
//! `id` alone (the MeshOS snapshot's `daemons: BTreeMap<u64, _>`, ICE
//! force-restart-by-id), so task-derived ids are spread pseudo-randomly
//! across the whole `u64` range and decorrelated from the small
//! sequential ids the registry hands to system daemons — a collision with
//! one is therefore vanishingly unlikely (~2⁻⁶⁴ per system daemon), not
//! structurally impossible. These functions are the *one* place that
//! mints task-derived refs — constructing a `DaemonRef` with a
//! task-shaped id anywhere else is a regression (integration plan
//! Resolved Decision 1).
//!
//! The **attempt number is deliberately excluded** from every encoding:
//! a step retry keeps the same `TaskId`, so it maps to the same ref →
//! same desired state → no reconcile diff → no spurious stop/start
//! churn (RD 1).

use crate::adapter::net::behavior::meshos::DaemonRef;
use crate::adapter::net::cortex::workflow::TaskId;

/// Namespace tag for top-level task daemons (`"task_id1"` in ASCII).
/// XOR-mixed into the id before hashing so the task-id space is
/// decorrelated from the small sequential ids the registry assigns to
/// system daemons (collision ~2⁻⁶⁴, not structurally precluded).
const TASK_DOMAIN: u64 = 0x7461_736B_5F69_6431;
/// Namespace tag for shard daemons (`"shard_id"` in ASCII).
const SHARD_DOMAIN: u64 = 0x7368_6172_645F_6964;

/// SplitMix64 finalizer — a bijection on `u64`, so distinct inputs give
/// distinct outputs (the encoding itself introduces no collisions).
const fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Encode a top-level task id into a spread, namespaced daemon id.
/// Injective over task ids — XOR-with-constant and `splitmix64` are
/// both bijections, so distinct tasks never share a daemon id.
fn encode_task_ref(task: TaskId) -> u64 {
    splitmix64(TASK_DOMAIN ^ task)
}

/// Encode a `(parent, shard_index)` pair into a daemon id. Two mixing
/// rounds so neither field can be trivially separated, and a distinct
/// domain tag keeps it out of the top-level task id space. The attempt
/// number is not an input (RD 1); collision probability between any two
/// distinct pairs is ~2⁻⁶⁴.
fn encode_shard_ref(parent: TaskId, shard_index: u16) -> u64 {
    let folded_parent = splitmix64(SHARD_DOMAIN ^ parent);
    splitmix64(folded_parent ^ shard_index as u64)
}

/// The `DaemonRef` for a top-level task's worker daemon. This is the
/// canonical task→daemon mapping the projection uses for *every* task
/// (a shard is itself a standalone `TaskId`, so keying by its own id is
/// maximally stable under retries and sibling churn).
pub fn daemon_ref(task: TaskId) -> DaemonRef {
    DaemonRef {
        id: encode_task_ref(task),
        name: format!("task/{task}"),
    }
}

/// The `DaemonRef` for a shard's worker daemon addressed by its parent
/// and position within the fan-out — the by-`(parent, shard_index)`
/// form for callers that hold a `ShardGroup` and think in indices.
///
/// Note: `project_daemon_intents` does *not* use this; in this codebase
/// every shard already has a standalone `TaskId`, so it keys shards by
/// [`daemon_ref`] on that id, which is stable even if a sibling is
/// deleted or the fan-out is re-derived. Prefer [`daemon_ref`] unless
/// you specifically need the `(parent, index)` addressing and readable
/// `task/<parent>/shard/<index>` name.
pub fn daemon_ref_shard(parent: TaskId, shard_index: u16) -> DaemonRef {
    DaemonRef {
        id: encode_shard_ref(parent, shard_index),
        name: format!("task/{parent}/shard/{shard_index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_refs_are_distinct_namespaced_and_readable() {
        let a = daemon_ref(1);
        let b = daemon_ref(2);
        assert_ne!(a.id, b.id, "distinct tasks → distinct ids");
        assert_eq!(a.name, "task/1");
        assert_eq!(b.name, "task/2");
        // Namespaced: the encoded id is not the raw task id, so it is
        // decorrelated from the small sequential system-daemon ids (a
        // collision is ~2⁻⁶⁴, not structurally impossible — these checks
        // pin representative values).
        assert_ne!(daemon_ref(1).id, 1);
        assert_ne!(daemon_ref(2).id, 2);
    }

    #[test]
    fn task_ref_is_stable_across_calls_so_retries_are_invisible() {
        // No attempt input: the same task id always maps to the same
        // ref, so a retry produces no reconcile diff (RD 1).
        assert_eq!(daemon_ref(7), daemon_ref(7));
        assert_eq!(daemon_ref_shard(7, 3), daemon_ref_shard(7, 3));
    }

    #[test]
    fn shard_refs_distinguish_index_and_parent() {
        let s0 = daemon_ref_shard(10, 0);
        let s1 = daemon_ref_shard(10, 1);
        let other_parent = daemon_ref_shard(11, 0);
        assert_ne!(s0.id, s1.id, "same parent, different shard index");
        assert_ne!(s0.id, other_parent.id, "different parent, same index");
        assert_eq!(s0.name, "task/10/shard/0");
        assert_eq!(s1.name, "task/10/shard/1");
    }

    #[test]
    fn task_and_shard_id_spaces_do_not_overlap_for_related_inputs() {
        // A top-level task id and a shard id built from numerically
        // related inputs don't collide — distinct domain tags + mixing.
        assert_ne!(daemon_ref(10).id, daemon_ref_shard(10, 0).id);
        assert_ne!(daemon_ref(0).id, daemon_ref_shard(0, 0).id);
    }
}
