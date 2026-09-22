// SPDX-License-Identifier: MIT OR Apache-2.0
// Marker child: writes its marker file as its first action on startup, then
// exits. A wrapped run that must not execute the untrusted program is
// witnessed by the marker's absence; a spawn is witnessed by its presence.
// Compiled by the driving tests with rustc, like wrap_startup.rs, without
// external dependencies.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let marker = args
        .get(1)
        .expect("marker_child: pass the marker file path as argv[1]");
    std::fs::write(marker, b"started\n").expect("marker_child: write marker file");
}
