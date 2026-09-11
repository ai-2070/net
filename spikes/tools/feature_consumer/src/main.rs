//! R5-A probe: does the core compile when a *downstream* consumer
//! turns on `net-mesh-wire/webrtc` by itself?
//!
//! Three configurations, all of which must build:
//!   1. default            — no RTC anywhere
//!   2. `--features wire-only` — wire variant present, core feature off
//!   3. `--features core-rtc`  — the ordinary full transport
//!
//! (Crate names: `net-mesh` builds the lib `net`, `net-mesh-wire`
//! builds `net_wire` — the published names and the lib names differ.)
//!
//! Configuration 2 is the one that used to fail, and enabling the
//! full transport in the control would mask it.

fn main() {
    // Touch both crates so neither is optimized out of the graph.
    let udp: net::adapter::net::PeerAddr =
        net::adapter::net::PeerAddr::Udp("127.0.0.1:1".parse().expect("addr"));
    println!("peer addr: {udp:?}");
    #[cfg(feature = "wire-only")]
    {
        let rtc = net_wire::peer_addr::PeerAddr::Rtc(net_wire::peer_addr::RtcPeerId {
            slot: 1,
            generation: 0,
        });
        println!("wire-only rtc endpoint: {rtc:?}");
    }
}
