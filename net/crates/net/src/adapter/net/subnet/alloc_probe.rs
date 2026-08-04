//! Fixtures-only instrumentation for the production protected-relay
//! allocation witness (`tests/subnet_relay_alloc_e2e.rs`).
//!
//! `tests/subnet_route_hop_alloc.rs` proves the sealing *primitive*
//! allocates nothing, and the `protected_forward_allocation_pins`
//! module in `mesh.rs` structurally pins that `relay_protected_hop`
//! still calls it. Neither measures the production branch: the relay
//! runs on a tokio worker thread where *reception* allocates per
//! packet, so a bare process-wide allocation counter cannot attribute
//! anything to the relay.
//!
//! This module gives the witness that attribution. The relay branch
//! opens a [`RelaySection`] for exactly its own extent; the witness's
//! counting `#[global_allocator]` consults [`in_relay_section`] on
//! every allocation and charges only those made *inside* the section,
//! on whichever thread the relay happens to run. [`sections_entered`]
//! exists so the witness can prove the marker actually executed — an
//! allocation count of zero over a section that never ran would be a
//! vacuous pass, which is the exact failure mode this witness was owed
//! to close (SUBNET_AUTH_PLAN.md D9).
//!
//! Everything here must itself be allocation-free on the measured
//! path: the thread-local is const-initialized (no lazy allocation,
//! and `Cell<bool>` has no destructor to register) and the entry
//! counter is a plain atomic. The module is compiled only for tests
//! and the `fixtures` feature; production builds carry neither the
//! marker nor this file's code.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// Whether the current thread is executing the production relay
    /// branch. Const-initialized so that reading it from inside a
    /// global allocator cannot itself allocate or recurse.
    static IN_RELAY_SECTION: Cell<bool> = const { Cell::new(false) };
}

/// Total number of times the relay section was entered, across all
/// threads, since process start. Monotonic; read two samples to
/// attribute a window.
static SECTIONS_ENTERED: AtomicU64 = AtomicU64::new(0);

/// RAII marker for the production relay branch. Constructed at the
/// top of `relay_protected_hop`; the drop covers every early return
/// in the branch.
pub struct RelaySection {
    prev: bool,
}

impl RelaySection {
    /// Enter the measured section on this thread.
    pub fn enter() -> Self {
        SECTIONS_ENTERED.fetch_add(1, Ordering::Relaxed);
        let prev = IN_RELAY_SECTION.with(|flag| flag.replace(true));
        Self { prev }
    }
}

impl Drop for RelaySection {
    fn drop(&mut self) {
        IN_RELAY_SECTION.with(|flag| flag.set(self.prev));
    }
}

/// Is the current thread inside the production relay branch?
///
/// Called from the witness's global allocator: must never allocate.
pub fn in_relay_section() -> bool {
    IN_RELAY_SECTION.with(|flag| flag.get())
}

/// How many times the relay section has been entered process-wide.
pub fn sections_entered() -> u64 {
    SECTIONS_ENTERED.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker nests and restores correctly across early exits —
    /// the shape `relay_protected_hop`'s many `return`s rely on.
    #[test]
    fn section_flag_nests_and_restores() {
        assert!(!in_relay_section());
        let before = sections_entered();
        {
            let _outer = RelaySection::enter();
            assert!(in_relay_section());
            {
                let _inner = RelaySection::enter();
                assert!(in_relay_section());
            }
            assert!(
                in_relay_section(),
                "dropping a nested section must restore the outer one, not clear it",
            );
        }
        assert!(!in_relay_section());
        assert_eq!(sections_entered() - before, 2);
    }
}
