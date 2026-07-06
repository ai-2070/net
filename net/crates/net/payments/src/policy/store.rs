//! The payment policy store — Workstream 3 lands the full store here.
//!
//! Pattern to hold (from `sdk/src/pins.rs`, verbatim regime):
//! - one machine-shared JSON file behind [`default_payment_policy_path`],
//!   every consumer funneling through this one resolver;
//! - cross-process advisory lock on a sidecar `.lock` file, acquired with
//!   async exponential backoff (never a blocking acquire — the pin store's
//!   pool-exhaustion regression test is the law here);
//! - atomic persistence: per-pid temp file, 0600 from creation, fsync,
//!   rename;
//! - every read-modify-write (the per-day spend counter above all) runs
//!   inside `mutate(path, f)` with the lock held across load → apply →
//!   save.

use std::path::PathBuf;

/// The per-user default payment policy path:
/// `<local data>/net-mesh/payment-policy.json`.
///
/// Same resolution ladder as `default_pin_store_path` — the CLI-less SDK
/// surfaces, Hermes, and OpenClaw must all converge on one file, or
/// "approved anywhere is approved everywhere" breaks.
pub fn default_payment_policy_path() -> Option<PathBuf> {
    dirs::data_local_dir()
        .or_else(dirs::home_dir)
        .map(|d| d.join("net-mesh").join("payment-policy.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_lands_in_the_net_mesh_dir() {
        let p = default_payment_policy_path().expect("test hosts have a data dir");
        assert!(p.ends_with(PathBuf::from("net-mesh").join("payment-policy.json")));
    }
}
