//! The **UDP-blocked profile**, and why it is a firewall rule.
//!
//! Stage 5's last browser witness needs a network in which
//!
//!   * the anchor's HTTPS bootstrap (`POST /rtc/offer`, `wss
//!     /rtc/trickle`) still works, and
//!   * UDP to the anchor's `rtc_addr` does not,
//!
//! because that pair of facts is the ONLY evidence that turns the
//! leaf's failure type from `RtcError::IceTimeout` into
//! `RtcError::UdpBlocked`. Playwright cannot produce it:
//! `context.setOffline` and CDP's `Network.emulateNetworkConditions`
//! act on the fetch/XHR layer, not on the UDP sockets ICE uses, so
//! they would take the bootstrap down and leave ICE alone — exactly
//! backwards.
//!
//! # The two candidates, and the choice
//!
//! **natsim (`tests/natsim/`) — rejected.** Its gateways are
//! netfilter NAT topologies between network namespaces, and its
//! `--drop-direct` switch drops UDP *between the two sides' public
//! IPs* to kill punch paths while leaving the relay intact. To use it
//! here the browser AND the page server would have to be relocated
//! into `nsim_b`, behind a NAT, on a `10.99.0.0/24` bridge. That
//! changes the network every OTHER witness measures — the mDNS
//! candidate pair, the srflx candidate the anchor's STUN responder
//! hands back, the loopback pair — so the merged runner would be
//! measuring two different networks and reporting one ledger. It also
//! models the wrong fault: natsim's drop is a *peer-to-peer* path
//! being dead, not *this client's UDP egress* being dead.
//!
//! **A firewall rule — chosen.** One `nft` (or `iptables`) rule pair
//! that drops UDP to and from the anchor's `rtc_addr`, installed for
//! the duration of one witness and removed afterwards. It is
//! kernel-level, so it hits the browser's real ICE sockets, the STUN
//! binding probe included; it is scoped to one address and port, so
//! DNS, the runner agent and every other witness are untouched; and
//! the block sits in the CLIENT's path while the anchor stays a
//! healthy anchor — which is what a UDP-blocking corporate firewall
//! is, and what `UdpBlocked` is supposed to name.
//!
//! A third option was considered and rejected: advertise an
//! `rtc_addr` pointing at a bound-but-silent UDP socket the harness
//! owns. It needs no privilege and satisfies the leaf's evidence rule
//! — but it makes the ANCHOR broken rather than the client's network,
//! so it cannot distinguish "UDP is blocked" from "that anchor's RTC
//! socket is dead", and telling those apart is the entire reason
//! `UdpBlocked` is a separate variant.
//!
//! # Privilege, and what happens without it
//!
//! Installing the rule needs `CAP_NET_ADMIN` (root, or `sudo -n` on
//! a GitHub-hosted runner). Without it the profile is **refused** and
//! the witness FAILS with the refusal as its detail. There is no
//! simulated fallback: a witness that quietly stopped blocking UDP
//! would report a typed failure the leaf reached for some other
//! reason, which is worse than no witness.
//!
//! On Windows the profile is refused outright, and not for want of
//! privilege: the Windows Filtering Platform does not filter traffic
//! addressed to the host's own addresses, so a same-host anchor —
//! which is what this harness runs — cannot be UDP-blocked by any
//! `netsh advfirewall` rule. `nft`/`iptables` do filter `lo`, which
//! is why this witness is a Linux gate.

use std::net::SocketAddr;
use std::process::Stdio;

use tokio::process::Command;

use crate::browser::Engine;

/// The UDP-blocked profile, by the mechanism that can actually
/// establish it on THIS host and engine.
///
/// Two mechanisms, and the ledger names which one ran:
///
/// * [`UdpProfile::Firewall`] — a kernel `nft`/`iptables` rule pair
///   scoped to the anchor's `rtc_addr`. The Linux gate, and the CI
///   mechanism. Unchanged.
/// * [`UdpProfile::EngineUdpOff`] — the engine's own
///   no-non-proxied-UDP policy with no proxy configured
///   (`--force-webrtc-ip-handling-policy=disable_non_proxied_udp`;
///   Firefox: `media.peerconnection.ice.proxy_only`). Needed on
///   Windows, where the Filtering Platform does not filter traffic
///   addressed to the host's own addresses and therefore cannot
///   block UDP to a same-host anchor at all.
///
/// Both produce the pair of facts `UdpBlocked` is defined by — HTTPS
/// bootstrap reachable, UDP to the anchor's `rtc_addr` unanswered —
/// and both put the fault in the CLIENT's path while the anchor stays
/// healthy, which is what distinguishes this from a dead anchor
/// socket. They differ in scope, and the difference is stated rather
/// than glossed: the firewall rule kills UDP to one address for the
/// whole host, the engine policy kills WebRTC's UDP for one browser
/// process. The engine policy is not a simulation — it is the
/// deployed configuration (`WebRTCIPHandlingPolicy`) that the
/// UDP-blocking corporate networks `UdpBlocked` names actually use,
/// and its effect was measured here: gathering completes with zero
/// candidates, not even a host one.
///
/// Establishing it requires relaunching the browser, so the caller
/// owns that step; `EngineUdpOff` carries no undo of its own.
pub enum UdpProfile {
    Firewall(UdpBlock),
    EngineUdpOff { how: String },
}

impl UdpProfile {
    /// Pick the mechanism this host and engine can actually
    /// establish, or say exactly why neither can.
    pub async fn install(target: SocketAddr, engine: Engine) -> Result<Self, String> {
        if !cfg!(windows) {
            return UdpBlock::install(target).await.map(Self::Firewall);
        }
        match engine {
            Engine::Chromium | Engine::Firefox => Ok(Self::EngineUdpOff {
                how: format!(
                    "the ENGINE's own UDP policy, because a kernel rule cannot do it here: the \
                     Windows Filtering Platform does not filter traffic addressed to the \
                     host's own addresses, and this anchor's rtc_addr {target} is one of \
                     them, so no `netsh advfirewall` rule can block it. The browser is \
                     relaunched with non-proxied UDP disabled and no proxy configured — \
                     Chrome's own WebRTCIPHandlingPolicy=disable_non_proxied_udp, the \
                     configuration real UDP-blocking networks deploy — which takes WebRTC's \
                     UDP away from the browser process while HTTPS keeps working. Scope \
                     differs from the Linux gate's rule pair (one browser process rather \
                     than one host-wide address), and the evidence pair the typing rests on \
                     is the same"
                ),
            }),
            Engine::Webkit => Err(format!(
                "the UDP-blocked profile cannot be established for WebKit on Windows: the \
                 Filtering Platform cannot block UDP to this host's own {target}, and \
                 Playwright's WebKit exposes no UDP-handling knob to take its UDP away \
                 instead. Run this witness on the Linux job, where the profile is an \
                 `nft`/`iptables` rule pair."
            )),
        }
    }

    /// What was established, for the ledger.
    pub fn how(&self) -> &str {
        match self {
            Self::Firewall(block) => &block.how,
            Self::EngineUdpOff { how } => how,
        }
    }

    /// Remove whatever was installed. A no-op for the engine policy,
    /// which the caller undoes by relaunching the browser without it.
    pub async fn remove(&self) {
        if let Self::Firewall(block) = self {
            block.remove().await;
        }
    }
}

/// A rule pair that is live until [`UdpBlock::remove`] runs.
pub struct UdpBlock {
    /// What was installed, for the ledger and the report.
    pub how: String,
    undo: Vec<Vec<String>>,
}

impl UdpBlock {
    /// Block UDP to and from `target`, leaving TCP alone.
    pub async fn install(target: SocketAddr) -> Result<Self, String> {
        if cfg!(windows) {
            return Err(
                "the UDP-blocked profile is a Linux gate: the Windows Filtering Platform does \
                 not filter traffic addressed to the host's own addresses, so no `netsh \
                 advfirewall` rule can block UDP to a same-host anchor. Run this witness on \
                 the Linux CI job."
                    .into(),
            );
        }
        let ip = target.ip().to_string();
        let port = target.port().to_string();

        // nftables first: it is what a current Linux runner has, and
        // its own table means removal cannot disturb a rule the
        // runner image installed.
        let nft_add: Vec<Vec<String>> = vec![
            vec![
                "nft".into(),
                "add".into(),
                "table".into(),
                "inet".into(),
                TABLE.into(),
            ],
            vec![
                "nft".into(),
                "add".into(),
                "chain".into(),
                "inet".into(),
                TABLE.into(),
                "out".into(),
                "{ type filter hook output priority 0; policy accept; }".into(),
            ],
            vec![
                "nft".into(),
                "add".into(),
                "chain".into(),
                "inet".into(),
                TABLE.into(),
                "in".into(),
                "{ type filter hook input priority 0; policy accept; }".into(),
            ],
            vec![
                "nft".into(),
                "add".into(),
                "rule".into(),
                "inet".into(),
                TABLE.into(),
                "out".into(),
                "udp".into(),
                "dport".into(),
                port.clone(),
                "drop".into(),
            ],
            vec![
                "nft".into(),
                "add".into(),
                "rule".into(),
                "inet".into(),
                TABLE.into(),
                "in".into(),
                "udp".into(),
                "sport".into(),
                port.clone(),
                "drop".into(),
            ],
        ];
        let nft_undo: Vec<Vec<String>> = vec![vec![
            "nft".into(),
            "delete".into(),
            "table".into(),
            "inet".into(),
            TABLE.into(),
        ]];

        if run_all(&nft_add).await.is_ok() {
            return Ok(Self {
                how: format!(
                    "nft table inet {TABLE}: drop udp dport/sport {port} (the anchor's \
                     rtc_addr {target}); TCP, DNS and every other flow untouched"
                ),
                undo: nft_undo,
            });
        }
        // Undo any partial nft state before trying the other tool.
        let _ = run_all(&nft_undo).await;

        let ipt_add: Vec<Vec<String>> = vec![
            vec![
                "iptables".into(),
                "-I".into(),
                "OUTPUT".into(),
                "-p".into(),
                "udp".into(),
                "-d".into(),
                ip.clone(),
                "--dport".into(),
                port.clone(),
                "-j".into(),
                "DROP".into(),
            ],
            vec![
                "iptables".into(),
                "-I".into(),
                "INPUT".into(),
                "-p".into(),
                "udp".into(),
                "-s".into(),
                ip.clone(),
                "--sport".into(),
                port.clone(),
                "-j".into(),
                "DROP".into(),
            ],
        ];
        let ipt_undo: Vec<Vec<String>> = vec![
            vec![
                "iptables".into(),
                "-D".into(),
                "OUTPUT".into(),
                "-p".into(),
                "udp".into(),
                "-d".into(),
                ip.clone(),
                "--dport".into(),
                port.clone(),
                "-j".into(),
                "DROP".into(),
            ],
            vec![
                "iptables".into(),
                "-D".into(),
                "INPUT".into(),
                "-p".into(),
                "udp".into(),
                "-s".into(),
                ip.clone(),
                "--sport".into(),
                port.clone(),
                "-j".into(),
                "DROP".into(),
            ],
        ];
        match run_all(&ipt_add).await {
            Ok(()) => Ok(Self {
                how: format!(
                    "iptables OUTPUT/INPUT DROP for udp {target} only; TCP, DNS and every \
                     other flow untouched"
                ),
                undo: ipt_undo,
            }),
            Err(e) => {
                let _ = run_all(&ipt_undo).await;
                Err(format!(
                    "the UDP-blocked profile could not be installed: neither `nft` nor \
                     `iptables` would take the rule ({e}). It needs CAP_NET_ADMIN — run the \
                     job as root or with passwordless sudo. There is no simulated fallback."
                ))
            }
        }
    }

    pub async fn remove(&self) {
        let _ = run_all(&self.undo).await;
    }
}

const TABLE: &str = "netmesh_stage5";

/// Run each command, elevating with `sudo -n` when this process is
/// not already root. The first failure aborts and is reported.
async fn run_all(commands: &[Vec<String>]) -> Result<(), String> {
    for argv in commands {
        let (program, args) = elevate(argv);
        let out = Command::new(&program)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| format!("{program}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "`{} {}` exited {:?}: {}",
                program,
                args.join(" "),
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(())
}

fn elevate(argv: &[String]) -> (String, Vec<String>) {
    let root = std::env::var("USER").map(|u| u == "root").unwrap_or(false)
        || std::env::var("HOME").map(|h| h == "/root").unwrap_or(false);
    if root {
        (argv[0].clone(), argv[1..].to_vec())
    } else {
        let mut args = vec!["-n".to_string()];
        args.extend_from_slice(argv);
        ("sudo".to_string(), args)
    }
}
