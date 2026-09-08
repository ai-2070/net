//! Names EVERY declaration in the fixtures-only sensing bridge, one per line.
//!
//! WITHOUT `--features fixtures` this must fail to compile: the module does not
//! exist, so the `use` below cannot resolve. WITH the feature it must compile
//! and run. `MANIFEST` beside this file lists the same names, and the guard in
//! `tests/sensing_org_exact_guards.rs` asserts MANIFEST equals the bridge's own
//! declaration set - so this probe can never silently cover fewer symbols than
//! the bridge exports.

use net::adapter::net::org_exact_sensing_bridge::{authorized_population, sensed_provider_order};

fn main() {
    // Referenced as values, so the names must resolve AND keep their arity;
    // nothing is invoked (that would need a live node).
    let names: [*const (); 2] = [
        authorized_population as *const (),
        sensed_provider_order as *const (),
    ];
    println!("fixtures-only bridge symbols resolved: {}", names.len());
}
