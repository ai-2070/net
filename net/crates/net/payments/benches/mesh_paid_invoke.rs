//! B3 — paid invocation delta (feature `mesh`).
//!
//! Apples-to-apples: the same application surface both sides — `serve_tool`
//! (unpaid) vs `serve_tool_paid` (paid), identical request/response types,
//! handler body, and transport config; the payment gate is the only
//! difference. `delta = paid − unpaid` is the payment admission cost, at
//! concurrency 1 / 16 / 128, with store cardinality held fixed.
//!
//! Implemented in phase P4 — see
//! `docs/plans/PAYMENTS_BENCHMARKS_PLAN.md`.

fn main() {
    eprintln!(
        "mesh_paid_invoke: not yet implemented (phase P4). \
         See docs/plans/PAYMENTS_BENCHMARKS_PLAN.md"
    );
}
