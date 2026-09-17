//! The Stage 6 browser NAT conformance matrix, **as data**.
//!
//! Six rows, each with the NAT flavor on both sides, the disposition
//! ICE is expected to reach, and the reason it is expected — plus the
//! counter expectation every row asserts. Nothing in this file touches
//! the network, spawns a process or needs Linux, which is the point:
//! the matrix's *content* and its *acceptance arithmetic* are the two
//! things a Linux-gated test file cannot prove anywhere else, and a
//! conformance suite whose table silently stopped matching its runner
//! is exactly the failure mode Stage 6 exists to close.
//!
//! Compiled by three consumers, so there is one table and no drift:
//!
//! * `tests/natsim.rs` — Linux-gated, root, runs the rows through
//!   `tests/natsim/run_scenario.sh`.
//! * `tests/natsim_browser.rs` — **ungated**, runs everywhere: checks
//!   this table against `run_scenario.sh`'s own case arms and
//!   exercises the checker against every shape it must refuse.
//! * `tests/natsim/browser/src/main.rs` — the runner, which refuses at
//!   run time if the topology it was handed disagrees with the row.
//!
//! # Why the dispositions are what they are
//!
//! All three flavors here have endpoint-independent **mapping** (one
//! public port for every destination). They differ in **filtering**,
//! and that is the whole axis:
//!
//! | | admits inbound from |
//! |---|---|
//! | address-restricted (`cone-ar`) | any port at an address we wrote to |
//! | port-restricted (`cone-pr`) | only the exact tuple we wrote to |
//! | symmetric | only the exact tuple we wrote to, *and* its own mapping differs per destination |
//!
//! A symmetric peer's check therefore arrives from a source port
//! nobody could have predicted: an address-restricted filter admits
//! it and ICE learns the pair peer-reflexively, while a
//! port-restricted filter drops it — and the reverse check dies at the
//! symmetric gateway, whose mapping toward us is not the one it
//! observed against the anchor's STUN. So symmetric interworks with
//! address-restricted cone and with nothing narrower.
//!
//! That makes **four** direct rows and **two** relayed
//! (`port-restricted × symmetric` and `symmetric × symmetric`). The
//! brief's example sentence names only symmetric × symmetric as
//! relayed; its governing clause is "every row where ICE is
//! **expected** to solve lands direct", and port-restricted ×
//! symmetric is not such a row. Confirmed by the owner rather than
//! assumed (Main, 2026-09-16) — the derivation is recorded here so the
//! expectation is reasoned rather than moved.
//!
//! A relayed row is a **PASS**: the routed session through the anchor
//! is the documented disposition (plan §6 "Fallback and its limit"),
//! and the row asserts it is *typed* as such at the page surface
//! rather than reported as a broken direct attempt.
//!
//! # What an attempt is, and why every side expects TWO
//!
//! An attempt is one signalling **dialog** — the offer we sent or the
//! offer we accepted — not one `connectPeer` call and not one ICE
//! restart. A leaf's dialog with its **anchor** is such a dialog and
//! is counted, which is easy to forget and would make every row's
//! arithmetic wrong by one. Verified at the bump sites rather than
//! taken on faith (`leaf/src/wasm.rs`: `ice_attempted` in `connect`
//! at the point the anchor's answer registers the dialog, in
//! `peer_offer`, and in `peer_accept_offer`; the anchor bootstrap
//! settles `IceTerm::Direct` on install).
//!
//! So on one row: A counts its anchor dialog and its peer dialog, B
//! counts the same two, and the anchor counts one bootstrap dialog per
//! leaf. Three distinct dialogs in the system, two attempts on every
//! participant.

#![allow(dead_code, reason = "each consumer uses a different subset")]

use std::fmt;

// =========================================================================
// Flavors, dispositions, rows
// =========================================================================

/// A NAT flavor, spelled exactly as `setup.sh --nat-a/--nat-b` takes
/// it. Only the two ephemeral-port cone modes and `symmetric` appear
/// in the browser matrix: a browser's ICE socket picks its own port,
/// so the pinned-port `cone` mode cannot be used for it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Nat {
    /// `cone-ar` — endpoint-independent mapping, address-restricted
    /// filtering.
    ConeAr,
    /// `cone-pr` — endpoint-independent mapping, port-restricted
    /// filtering. This is what the pre-existing `cone` mode has
    /// always been, despite its name.
    ConePr,
    /// `symmetric` — `masquerade fully-random`, a fresh public port
    /// per destination tuple.
    Symmetric,
}

impl Nat {
    /// The `setup.sh` mode string.
    pub fn mode(self) -> &'static str {
        match self {
            Self::ConeAr => "cone-ar",
            Self::ConePr => "cone-pr",
            Self::Symmetric => "symmetric",
        }
    }

    /// Parse a `setup.sh` mode string. `None` for anything else —
    /// including `cone`, which is deliberately not a browser-matrix
    /// flavor.
    pub fn from_mode(s: &str) -> Option<Self> {
        match s {
            "cone-ar" => Some(Self::ConeAr),
            "cone-pr" => Some(Self::ConePr),
            "symmetric" => Some(Self::Symmetric),
            _ => None,
        }
    }
}

impl fmt::Display for Nat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mode())
    }
}

/// Where a row's peer dialog is expected to end up.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Disposition {
    /// ICE solved: a direct DataChannel session replaced the routed
    /// one.
    Direct,
    /// ICE could not solve: the routed session through the anchor was
    /// kept, and the page surface says so.
    Relayed,
}

impl Disposition {
    /// The verdict/table spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relayed => "relayed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "direct" => Some(Self::Direct),
            "relayed" => Some(Self::Relayed),
            _ => None,
        }
    }

    /// The `type` discriminant `@net-mesh/browser`'s
    /// `PeerConnectOutcome` carries for this disposition (S6Peer,
    /// slice 1, surface frozen 2026-09-16).
    ///
    /// Note the two vocabularies differ, and are NOT unified here:
    /// `relayed` is the *stats* term (`RtcStats::ice_relayed`) and
    /// `iceTimeout` is the *outcome* type. Mapping them in one place
    /// means no row can quietly accept the wrong word.
    ///
    /// `udpBlocked` is deliberately NOT accepted as a relayed
    /// disposition. It is `iceTimeout` narrowed by
    /// `UdpBlockedEvidence`, and on these rows UDP egress
    /// demonstrably works — every row's anchor dialog lands direct
    /// over UDP. A row reporting `udpBlocked` would be claiming a
    /// narrower cause than the evidence supports, which is worse than
    /// no term at all because it will be read as a diagnosis.
    pub fn page_type(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Relayed => "iceTimeout",
        }
    }

    /// Which `RtcStats` term this row's PEER dialog must move.
    pub fn counter(self) -> &'static str {
        match self {
            Self::Direct => "ice_direct",
            Self::Relayed => "ice_relayed",
        }
    }
}

impl fmt::Display for Disposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether the harness grants the page's origin camera/microphone
/// permission before the row runs.
///
/// This is a HARNESS knob and not a product one, which is exactly why
/// it has to be part of the table. Chromium gates local-interface
/// enumeration on media permission: with none,
/// `FilteringNetworkManager` logs `received permission status:
/// denied`, the allocator logs `Allocate ports on any any`, and every
/// port is created on the wildcard network
/// `Net[any:0.0.0.x/0:Wildcard:id=0]` at cost 999. Granting
/// camera/microphone is what a user does before a call, and it is
/// what the six matrix rows do.
///
/// **What that costs is candidate CLASSES, not reachability.** A
/// wildcard port still binds `0.0.0.0` and still receives; measured
/// in run 35182320241, the permission-free pair reached the STUN
/// endpoint, gathered srflx on both sides and landed `direct`
/// (S6_REPORT.md §11.8). What enumeration denial removes is the real
/// host candidate — replaced by an mDNS `<uuid>.local` name — and the
/// IPv6 leg, whose wildcard port logs `STUN server address is
/// incompatible` and has its host candidate discarded by the filter.
/// Neither is a candidate class that solves a NAT'd pair, which is
/// why the boundary is invisible on this topology.
///
/// A row that runs with `None` is therefore measuring something the
/// granted rows cannot: whether the PRODUCT works in an ordinary
/// browsing context that was never asked for a media permission and
/// never given one. The product calls no media API — that is good
/// source evidence and it is not a measurement, which is Kyra's E1
/// third item verbatim. Only a row that withholds the grant can
/// answer it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Media {
    /// `context.grantPermissions(['camera','microphone'])` for the
    /// page's own origin, before the page is opened.
    Granted,
    /// Nothing granted, nothing prompted: product defaults.
    None,
}

impl Media {
    /// The `run_scenario.sh` / verdict spelling.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::None => "none",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "granted" => Some(Self::Granted),
            "none" => Some(Self::None),
            _ => None,
        }
    }
}

impl fmt::Display for Media {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.flag())
    }
}

/// What a row does with **Chromium's interface enumeration**: require
/// it, or record it.
///
/// Derived from `media` rather than stored, because it is not an
/// independent axis: `FilteringNetworkManager` gates the network list
/// on a media permission and nothing else in this harness touches it.
/// Firefox has no enumerator stage and logs no `Net[…]` at all, so
/// this is a Chromium-only field and asserting anything about nICEr
/// here would assert nothing.
///
/// **The two arms are not two strengths of the same check.** They are
/// different KINDS of statement, and keeping them apart is the whole
/// reason this lives in the table:
///
/// * `Required` — the granted rows. A port on a real named network is
///   a *precondition of the environment those rows were built for*,
///   and a Chromium tab that lost enumeration otherwise reports an
///   indistinguishable ICE timeout sixty seconds later. That
///   guardrail is what named §6.12's cause in one line, and it is
///   unchanged, verbatim, here.
///
/// * `Observed` — the permission-free rows. `real == 0` is the
///   *explanatory observation*, not the outcome under investigation.
///   Asserting it would pin a Chromium build's gating policy as
///   though it were a promise of this product, and a row that failed
///   because a future Chromium stopped gating would be reporting a
///   browser change as a product regression. So the counts are
///   recorded into the verdict verbatim and the row is decided by
///   what it is actually about: authenticated application delivery.
///
/// §11.8 is why the second arm is not a relaxation of the first. It
/// measured a pair whose tabs both logged `permission status: denied`
/// and allocated only wildcard ports, and which still reached the
/// STUN endpoint, gathered srflx, solved `direct` and delivered
/// nonce-correlated payloads in both directions. `real == 0` is
/// therefore demonstrably not a necessary condition for that path,
/// and a check that treated it as one would refuse a working
/// measurement.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Enumeration {
    /// At least one port on a real named network
    /// (`Net[eth0:192.168.10x.x/24:Ethernet:id=1]`) is REQUIRED, and
    /// a row without one is refused.
    Required,
    /// Whatever the allocator did is RECORDED into the verdict and
    /// the row is not decided by it.
    Observed,
}

impl Enumeration {
    /// Whether counts read off one Chromium tab's allocator log
    /// satisfy this row. `Observed` is satisfied by anything: it is a
    /// record, not a criterion.
    pub fn satisfied_by(self, real: u64) -> bool {
        match self {
            Self::Required => real > 0,
            Self::Observed => true,
        }
    }
}

/// What the ANCHOR's per-pair application-forwarding counter must do
/// across a row's application exchange.
///
/// The counter is `forwarded_app_packets(src32, dest)` and it
/// EXCLUDES `0x0D02` signalling (`mesh.rs`, the
/// `inner_sub != SUBPROTOCOL_RTC_SIGNAL` arm), so it is a statement
/// about application bytes and nothing else.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Forwarding {
    /// The anchor carried none of these application bytes: the pair
    /// is direct and the bytes went leaf → leaf.
    Flat,
    /// The anchor carried them, in both directions: the pair is
    /// relayed and the routed session is what delivered.
    Carried,
}

/// One matrix row.
#[derive(Copy, Clone, Debug)]
pub struct Row {
    /// The `run_scenario.sh` scenario name.
    pub scenario: &'static str,
    pub nat_a: Nat,
    pub nat_b: Nat,
    pub expect: Disposition,
    /// Why this disposition, in one line, for the failure message.
    pub why: &'static str,
    /// What the harness granted the page before it opened. Part of
    /// the row rather than of the runner's flags because a row that
    /// silently acquired a permission it claims not to need would be
    /// measuring the granted environment under the other name.
    pub media: Media,
}

impl Row {
    /// What each LEAF's ICE ledger must read once the row has
    /// settled: its anchor bootstrap dialog (always direct — the
    /// anchor is on the simulated internet and reachable from behind
    /// every flavor here) plus this row's peer dialog.
    ///
    /// `udp_blocked` is zero on every row by construction: no row
    /// blocks UDP egress. `--drop-direct`, natsim's one UDP-killing
    /// knob, kills the peer-to-peer path and leaves the anchor path
    /// intact, and no browser row uses it.
    pub fn leaf_expectation(&self) -> IceCounters {
        match self.expect {
            Disposition::Direct => IceCounters {
                attempted: 2,
                direct: 2,
                ..IceCounters::default()
            },
            Disposition::Relayed => IceCounters {
                attempted: 2,
                direct: 1,
                relayed: 1,
                ..IceCounters::default()
            },
        }
    }

    /// What the ANCHOR's ledger must read: one bootstrap dialog per
    /// leaf, both direct, on **every** row.
    ///
    /// Row-independent on purpose, and load-bearing because of it.
    /// The plan's "100 % of sessions established" is scoped to pairs
    /// *whose anchors are reachable*, so a relayed row only means
    /// what it claims if the anchor path itself worked. This is the
    /// assertion that establishes it: a relayed row with a broken
    /// anchor dialog fails here rather than passing as a correct
    /// fallback.
    ///
    /// The anchor is not a party to the peer dialog — it forwards the
    /// signalling envelopes blind (§9 step 3) — so the peer dialog
    /// never appears in this ledger.
    pub fn anchor_expectation(&self) -> IceCounters {
        IceCounters {
            attempted: 2,
            direct: 2,
            ..IceCounters::default()
        }
    }

    /// What the ANCHOR's per-pair application-forwarding counter must
    /// do across this row's application exchange.
    ///
    /// This is the witness the NAT rows did not have. Topology,
    /// outcome type, ICE ledgers and conntrack together establish
    /// that a *session* is direct or relayed; none of them observes
    /// an application payload, and conntrack reply traffic can be
    /// ICE or Noise. A relayed DISPOSITION is not observed delivery.
    ///
    /// So each row now exchanges nonce-correlated application
    /// payloads through the public stream surface and reads the
    /// anchor's own per-pair counter either side of it:
    ///
    /// * a direct row must deliver both nonces while the anchor
    ///   forwarded NONE of those application bytes — flat, in both
    ///   directions, which is what "leaf → leaf" physically means;
    /// * a relayed row must deliver both nonces while the anchor
    ///   forwarded them in BOTH directions — the routed session is
    ///   the thing that delivered, and a relayed row whose counter
    ///   never moved would mean the payload arrived some other way.
    ///
    /// Both halves fail independently: delivery without the right
    /// counter disposition, and the right counter disposition without
    /// delivery, are two different refusals.
    pub fn pair_forwarding(&self) -> Forwarding {
        match self.expect {
            Disposition::Direct => Forwarding::Flat,
            Disposition::Relayed => Forwarding::Carried,
        }
    }

    /// What this row does with Chromium's interface enumeration:
    /// `Required` wherever the grant was made, `Observed` on the
    /// permission-free legs. Chromium tabs only.
    ///
    /// See [`Enumeration`] for why those are two different kinds of
    /// statement rather than two strengths of one check.
    pub fn enumeration(&self) -> Enumeration {
        match self.media {
            Media::Granted => Enumeration::Required,
            Media::None => Enumeration::Observed,
        }
    }
}

/// The six rows, in the brief's order.
pub const ROWS: &[Row] = &[
    Row {
        scenario: "browser_cone_cone",
        nat_a: Nat::ConeAr,
        nat_b: Nat::ConeAr,
        expect: Disposition::Direct,
        why: "both sides admit the peer's check once their own outbound has opened the mapping",
        media: Media::Granted,
    },
    Row {
        scenario: "browser_cone_portrestricted",
        nat_a: Nat::ConeAr,
        nat_b: Nat::ConePr,
        expect: Disposition::Direct,
        why: "both mappings are endpoint-independent, so each side's check hits the exact tuple \
               the other sent to",
        media: Media::Granted,
    },
    Row {
        scenario: "browser_portrestricted_portrestricted",
        nat_a: Nat::ConePr,
        nat_b: Nat::ConePr,
        expect: Disposition::Direct,
        why: "simultaneous open: each check matches the conntrack reply tuple the other side's \
               own check created",
        media: Media::Granted,
    },
    Row {
        scenario: "browser_cone_symmetric",
        nat_a: Nat::ConeAr,
        nat_b: Nat::Symmetric,
        expect: Disposition::Direct,
        why: "the symmetric side's check arrives from an unpredictable port and the \
               address-restricted filter admits it; ICE learns the pair peer-reflexively",
        media: Media::Granted,
    },
    Row {
        scenario: "browser_portrestricted_symmetric",
        nat_a: Nat::ConePr,
        nat_b: Nat::Symmetric,
        expect: Disposition::Relayed,
        why: "the symmetric side's check arrives from a port the full-tuple filter never sent \
               to and is dropped; the reverse check dies at the symmetric gateway",
        media: Media::Granted,
    },
    Row {
        scenario: "browser_symmetric_symmetric",
        nat_a: Nat::Symmetric,
        nat_b: Nat::Symmetric,
        expect: Disposition::Relayed,
        why: "neither side can predict the other's mapping; the routed session through the \
               anchor is kept and typed as such",
        media: Media::Granted,
    },
];

/// The Firefox control: **row 1 again**, same NAT pair, the other
/// engine on both sides.
///
/// Same-engine rather than mixed on purpose. A mixed pair that failed
/// would not say which engine's ICE stack was responsible, and this
/// row exists to answer exactly one question — is the direct path a
/// Chromium artifact? — so the only variable is the engine. Firefox
/// runs one row and not the matrix because the matrix's value is the
/// NAT axis, and six rows of a second engine buys a second reading of
/// the same netfilter behaviour at twice the runtime.
///
/// **This row has always been permission-free**, and that is a fact
/// about the harness rather than a choice made here: Firefox has no
/// media gate on interface enumeration and Playwright cannot grant
/// it camera or microphone at all, so the driver never asked. It is
/// recorded as `Media::None` because the alternative — recording the
/// grant the runner *requested* — would be the verdict describing an
/// environment the row did not run in.
pub const CONTROL: Row = Row {
    scenario: "browser_cone_cone_firefox",
    nat_a: Nat::ConeAr,
    nat_b: Nat::ConeAr,
    expect: Disposition::Direct,
    why: "the cone × cone row on the other engine — the direct path is not Chromium-specific",
    media: Media::None,
};

/// The permission-free leg: **row 1 again, with nothing granted —
/// and it lands `direct`.**
///
/// Kyra's E1 third item, and now answered by measurement rather than
/// by reading the product's source. The six Chromium matrix rows all
/// grant the page's origin camera and microphone before it opens,
/// because Chromium withholds its interface enumeration from WebRTC
/// until a media permission exists (§6.12). "The product calls no
/// media API" is true, is source evidence, and is NOT a measurement
/// of the ungranted environment: the granted rows cannot tell a
/// product that needs no permission from one whose networking
/// happened to be fixed by the grant.
///
/// So this row is the same NAT pair, the same engine and the same
/// disposition as row 1 — one variable, the grant — and it runs
/// behind the real NATs rather than on loopback, because the
/// enumeration this measures is what a non-loopback candidate needs.
///
/// **What it measured** (run 35182320241, S6_REPORT.md §11.8): both
/// tabs logged `permission status: denied` and `Allocate ports on
/// any any`, `real=0 wildcard=117`/`157`, and both halves of the
/// dialog still typed `direct` — leaf ledgers `attempted=2 direct=2`,
/// both nonces delivered, the anchor's per-pair application
/// forwarding FLAT in both directions, and a replied two-way UDP flow
/// between the two public addresses in BOTH gateways' conntrack. The
/// wildcard port binds `0.0.0.0`, reaches the STUN endpoint and
/// gathers srflx; what the denial costs is the real host candidate
/// (an mDNS `.local` name instead) and the IPv6 leg, and neither of
/// those is what solves a NAT'd pair.
///
/// **Scope.** That is one Chromium build, one IP-handling policy and
/// one topology (two address-restricted cone gateways, srflx against
/// the anchor's announced STUN endpoint). It licenses exactly this:
/// on the tested configuration the media grant is not a prerequisite
/// for a data-only Net application. It does NOT license a universal
/// claim about Chromium data-only WebRTC, and it does not retire the
/// observation that the grant WAS a material part of the environment
/// the six matrix rows were measured in.
///
/// The `real=0` observation is therefore RECORDED into the verdict
/// rather than asserted (see [`Enumeration`]): pinning it would make
/// a future Chromium that stopped gating look like a regression in
/// this product, and §11.8 already shows it is not a necessary
/// condition for the path that worked.
pub const NO_MEDIA: Row = Row {
    scenario: "browser_cone_cone_nomedia",
    nat_a: Nat::ConeAr,
    nat_b: Nat::ConeAr,
    expect: Disposition::Direct,
    why: "row 1 with no camera/microphone grant: enumeration is denied and the ports are \
          wildcard, and the srflx pair a NAT'd row needs is gathered and solved anyway",
    media: Media::None,
};

/// The permission-free **ROUTED** leg: `symmetric × symmetric` again,
/// with nothing granted.
///
/// [`NO_MEDIA`] answers whether an ungranted pair goes DIRECT. It
/// cannot answer whether an ungranted pair can use Net's routed path,
/// and the reason is structural rather than incidental: on a row that
/// solves direct the anchor's per-pair application counter is
/// **flat by design** — that flatness is the direct row's own
/// assertion ([`Row::pair_forwarding`]) — so a direct row is exactly
/// the shape that cannot witness forwarding.
///
/// And the fallback is the half that most needs witnessing, because
/// **Net's routed path is not TURN.** It rides each leaf's own
/// authenticated session with the anchor rather than a relay
/// allocation, so "it falls back to the anchor" is a claim about
/// leaf-to-anchor application delivery. A row that reported it
/// without measuring it would be reporting nothing: if the anchor hop
/// were the thing a denied enumeration broke, there would be no
/// fallback to fall back to.
///
/// `symmetric × symmetric` is the pair ICE cannot solve — neither
/// side can predict the other's mapping — so it drives the routed
/// path deliberately, and it does so with the instrument the granted
/// relayed rows already use and this slice already asserts: both
/// nonces observed by the RECEIVER that did not mint them, and the
/// anchor's own per-pair application counter moving in BOTH
/// directions across the exchange ([`Forwarding::Carried`]). No new
/// witness, no widened deadline, no weakened assertion — the one
/// variable against `browser_symmetric_symmetric` is the grant.
pub const NO_MEDIA_RELAYED: Row = Row {
    scenario: "browser_symmetric_symmetric_nomedia",
    nat_a: Nat::Symmetric,
    nat_b: Nat::Symmetric,
    expect: Disposition::Relayed,
    why: "the relayed row with no camera/microphone grant — whether Net's anchor-routed \
          path, which is not TURN and depends on a working leaf-to-anchor session, carries \
          application bytes for a page that was never asked for a media permission",
    media: Media::None,
};

/// Every scenario this slice defines: the six rows, the Firefox
/// control, then the two permission-free legs — direct and routed.
pub fn all_scenarios() -> Vec<Row> {
    ROWS.iter()
        .copied()
        .chain([CONTROL, NO_MEDIA, NO_MEDIA_RELAYED])
        .collect()
}

// =========================================================================
// The counter identity
// =========================================================================

/// The §10 ICE ledger of ONE side, as read off a leaf's
/// `counters_json()` or an anchor's `RtcStats::ice_snapshot()`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct IceCounters {
    pub attempted: u64,
    pub direct: u64,
    pub relayed: u64,
    pub failed: u64,
    pub udp_blocked: u64,
}

impl IceCounters {
    /// Read the five terms out of a JSON object.
    ///
    /// Accepts a **decimal string** (the leaf: every counter is a
    /// `u64` and `JSON.parse` rounds above 2^53) or a JSON number. A
    /// missing or unparseable term is an **error**, never a zero:
    /// "the field is not there" and "the counter did not move" are
    /// opposite diagnoses, and defaulting the first to the second is
    /// how a partition assertion passes vacuously.
    ///
    /// Native `RtcStats` has no `udp_blocked` term and should not — a
    /// node that signals over UDP cannot have UDP blocked — so the
    /// runner emits a constant `"0"` for the anchor rather than
    /// letting this function invent one.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        Ok(Self {
            attempted: term(v, "ice_attempted")?,
            direct: term(v, "ice_direct")?,
            relayed: term(v, "ice_relayed")?,
            failed: term(v, "ice_failed")?,
            udp_blocked: term(v, "udp_blocked")?,
        })
    }

    /// The four outcome terms, checked for overflow.
    pub fn outcomes(&self) -> Result<u64, String> {
        self.direct
            .checked_add(self.relayed)
            .and_then(|s| s.checked_add(self.failed))
            .and_then(|s| s.checked_add(self.udp_blocked))
            .ok_or_else(|| format!("the four outcome terms overflow u64: {self:?}"))
    }

    /// Attempts with no outcome yet — `attempted - (the four)`.
    ///
    /// An outcome total ABOVE `attempted` is not negative pending, it
    /// is a broken partition, and it is reported as one.
    pub fn pending(&self) -> Result<u64, String> {
        let out = self.outcomes()?;
        self.attempted.checked_sub(out).ok_or_else(|| {
            format!(
                "outcome terms sum to {out} but only {} attempts were recorded — the partition \
                 is broken, not pending: {self:?}",
                self.attempted
            )
        })
    }

    /// The whole per-row acceptance for one side: the identity, the
    /// residual, and every term at its exact expected value.
    ///
    /// Four things are asserted together, and the first is the one
    /// that matters most:
    ///
    /// 1. `attempted != 0`. The identity holds trivially at all-zero,
    ///    so a row that passed the sum while nothing ever attempted
    ///    would have proven nothing. That is not hypothetical: the
    ///    leaf's four counters existed with no call site at all until
    ///    Stage 6 slice 1 added the five bump sites, and Stage 5's own
    ///    evidence log shows all four at zero for two leaves that had
    ///    a live direct session.
    /// 2. the partition `direct + relayed + failed + udp_blocked ==
    ///    attempted`, with `pending == 0` **in the same assertion**,
    ///    so an in-flight attempt is a named off-by-one rather than a
    ///    quiescence race somebody waits longer for.
    /// 3. `attempted` exactly — one anchor dialog plus one peer
    ///    dialog. A retry shows up as a third attempt and is a defect,
    ///    not noise.
    /// 4. every outcome term exactly, so a row cannot land its
    ///    expected disposition while something else also failed.
    pub fn check_exact(&self, who: &str, want: IceCounters) -> Result<(), String> {
        if self.attempted == 0 {
            return Err(format!(
                "{who}: ice_attempted == 0. The identity holds vacuously at all-zero, so this \
                 row proves nothing: {self:?}"
            ));
        }
        let pending = self.pending()?;
        if pending != 0 {
            return Err(format!(
                "{who}: {pending} attempt(s) with no outcome (ice_pending != 0): {self:?}"
            ));
        }
        if self.attempted != want.attempted {
            return Err(format!(
                "{who}: expected {} attempt(s) — one anchor bootstrap dialog plus one peer \
                 dialog, an attempt being one DIALOG — but ice_attempted == {}: {self:?}",
                want.attempted, self.attempted
            ));
        }
        for (name, got, expected) in [
            ("ice_direct", self.direct, want.direct),
            ("ice_relayed", self.relayed, want.relayed),
            ("ice_failed", self.failed, want.failed),
            ("udp_blocked", self.udp_blocked, want.udp_blocked),
        ] {
            if got != expected {
                return Err(format!(
                    "{who}: expected {name} == {expected}, got {got}: {self:?} (wanted {want:?})"
                ));
            }
        }
        Ok(())
    }
}

/// One counter term: decimal string or JSON number, both `u64`.
fn term(v: &serde_json::Value, key: &str) -> Result<u64, String> {
    match v.get(key) {
        None => Err(format!(
            "counter {key} is absent. A missing term is not a zero — the surface that should \
             carry it did not, and a partition over absent terms is vacuous."
        )),
        Some(serde_json::Value::String(s)) => s
            .parse::<u64>()
            .map_err(|e| format!("counter {key} = {s:?} is not a u64 decimal string: {e}")),
        Some(serde_json::Value::Number(n)) => n
            .as_u64()
            .ok_or_else(|| format!("counter {key} = {n} is not a u64")),
        Some(other) => Err(format!("counter {key} has non-numeric type: {other}")),
    }
}

// =========================================================================
// The application-delivery witness
// =========================================================================

/// What one row's **application exchange** observed, plus the
/// anchor's own per-pair application-forwarding counter either side
/// of it.
///
/// Nonce-correlated and bidirectional, and both properties are
/// load-bearing:
///
/// * The runner mints two nonces it never lets either page choose.
///   A sends `nonce_a` on a peer-addressed stream and B must decode
///   exactly that nonce; B answers with `nonce_b` and A must decode
///   exactly that. A counting witness — "N payloads arrived" —
///   passes when the frames are the receiver's own echo, when they
///   are a previous row's leftovers, and when the two sides never
///   agreed on a single byte. A nonce the *other* side minted cannot
///   be produced by the side that reports it.
/// * One direction proves a half-duplex path. The NAT flavors under
///   test are asymmetric by construction — the whole reason
///   `cone-ar × symmetric` solves and `cone-pr × symmetric` does not
///   is which side's check the other side's filter admits — so a row
///   that only measured A → B would pass on a pair that can never
///   answer.
///
/// The counters are the anchor's, sampled in the anchor's own
/// process, and `0x0D02` signalling is excluded from them by the
/// anchor: they are a statement about application bytes and nothing
/// else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppExchange {
    /// The nonce the runner minted for A → B, and the one it minted
    /// for B → A.
    pub nonce_a: String,
    pub nonce_b: String,
    /// The nonce each side actually DECODED from the peer's frames.
    /// Compared against the minted values rather than reported as a
    /// boolean, so "B says it saw something" cannot stand in for "B
    /// saw the bytes A sent".
    pub seen_at_b: String,
    pub seen_at_a: String,
    /// Application frames each side delivered to `stream.send` and
    /// each side's receiver observed.
    pub sent_a_to_b: u64,
    pub sent_b_to_a: u64,
    pub received_at_b: u64,
    pub received_at_a: u64,
    /// `anchor.forwarded_app_packets(a32, b)` and `(b32, a)` before
    /// the exchange started and after it finished.
    pub forwarded_pre_ab: u64,
    pub forwarded_pre_ba: u64,
    pub forwarded_post_ab: u64,
    pub forwarded_post_ba: u64,
}

impl AppExchange {
    /// Read the witness out of the verdict's `app` object.
    ///
    /// Every field is REQUIRED. A missing one is an error and never a
    /// default: "the runner did not report what the anchor forwarded"
    /// and "the anchor forwarded nothing" are opposite facts, and a
    /// schema that defaults the first to the second turns a
    /// measurement failure into observed absence — which is the exact
    /// defect the conntrack witness was rejected for one layer down.
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let text = |key: &str| -> Result<String, String> {
            match v.get(key) {
                Some(serde_json::Value::String(s)) => Ok(s.clone()),
                None => Err(format!(
                    "app.{key} is absent. A missing nonce is not an empty one — the side that \
                     should have reported what it decoded did not."
                )),
                Some(other) => Err(format!("app.{key} is not a string: {other}")),
            }
        };
        Ok(Self {
            nonce_a: text("nonce_a")?,
            nonce_b: text("nonce_b")?,
            seen_at_b: text("seen_at_b")?,
            seen_at_a: text("seen_at_a")?,
            sent_a_to_b: term(v, "sent_a_to_b")?,
            sent_b_to_a: term(v, "sent_b_to_a")?,
            received_at_b: term(v, "received_at_b")?,
            received_at_a: term(v, "received_at_a")?,
            forwarded_pre_ab: term(v, "forwarded_pre_ab")?,
            forwarded_pre_ba: term(v, "forwarded_pre_ba")?,
            forwarded_post_ab: term(v, "forwarded_post_ab")?,
            forwarded_post_ba: term(v, "forwarded_post_ba")?,
        })
    }

    /// How far the anchor's per-pair counter moved across the
    /// exchange, A → B and B → A.
    ///
    /// A counter that went DOWN is not a negative delta, it is a
    /// broken reading (the anchor's map is monotonic per pair), and
    /// it is reported as one rather than saturating to zero — a
    /// saturating subtraction here would read as "flat" and pass a
    /// direct row on a nonsense sample.
    pub fn forwarded_delta(&self) -> Result<(u64, u64), String> {
        let one = |what: &str, pre: u64, post: u64| -> Result<u64, String> {
            post.checked_sub(pre).ok_or_else(|| {
                format!(
                    "the anchor's {what} pair counter went {pre} → {post}: it decreased, which \
                     is not a flat path but an unusable reading"
                )
            })
        };
        Ok((
            one("a→b", self.forwarded_pre_ab, self.forwarded_post_ab)?,
            one("b→a", self.forwarded_pre_ba, self.forwarded_post_ba)?,
        ))
    }

    /// The whole application-delivery acceptance for one row.
    pub fn check(&self, row: &Row) -> Result<(), String> {
        if self.nonce_a.is_empty() || self.nonce_b.is_empty() {
            return Err(format!(
                "row {}: the runner minted an empty nonce (a {:?}, b {:?}) — an empty nonce \
                 matches anything, including nothing",
                row.scenario, self.nonce_a, self.nonce_b
            ));
        }
        if self.nonce_a == self.nonce_b {
            return Err(format!(
                "row {}: both directions were given the same nonce {:?}, so a frame looped back \
                 to its own sender would satisfy both halves",
                row.scenario, self.nonce_a
            ));
        }
        if self.seen_at_b != self.nonce_a {
            return Err(format!(
                "row {}: B decoded {:?} from A's peer-addressed stream, but A sent {:?}. No \
                 application payload of A's was observed at B, so this row has a disposition and \
                 no delivery.",
                row.scenario, self.seen_at_b, self.nonce_a
            ));
        }
        if self.seen_at_a != self.nonce_b {
            return Err(format!(
                "row {}: A decoded {:?} from B's peer-addressed stream, but B sent {:?}. The \
                 reverse direction was not observed to deliver.",
                row.scenario, self.seen_at_a, self.nonce_b
            ));
        }
        for (who, sent, received) in [
            ("a→b", self.sent_a_to_b, self.received_at_b),
            ("b→a", self.sent_b_to_a, self.received_at_a),
        ] {
            if sent == 0 {
                return Err(format!(
                    "row {}: no {who} application frame was ever handed to `stream.send`, so the \
                     nonce it reports cannot have crossed the transport",
                    row.scenario
                ));
            }
            if received == 0 {
                return Err(format!(
                    "row {}: {sent} {who} frames were sent and the receiver counted none, while \
                     still reporting a matching nonce — the two reports contradict each other",
                    row.scenario
                ));
            }
        }
        let (ab, ba) = self.forwarded_delta()?;
        match row.pair_forwarding() {
            Forwarding::Flat => {
                if ab != 0 || ba != 0 {
                    return Err(format!(
                        "row {} claims direct, and both nonces arrived — but the ANCHOR forwarded \
                         {ab} a→b and {ba} b→a application packets for this exact pair while they \
                         did ({} → {} and {} → {}). Bytes the anchor carried are not a direct \
                         path, whatever the outcome type says.",
                        row.scenario,
                        self.forwarded_pre_ab,
                        self.forwarded_post_ab,
                        self.forwarded_pre_ba,
                        self.forwarded_post_ba
                    ));
                }
                Ok(())
            }
            Forwarding::Carried => {
                if ab == 0 || ba == 0 {
                    return Err(format!(
                        "row {} is relayed and both nonces arrived — but the anchor's per-pair \
                         application counter moved {ab} a→b and {ba} b→a ({} → {} and {} → {}). \
                         On a relayed row the anchor IS the path, so a direction that delivered \
                         without the anchor forwarding anything means the payload arrived by a \
                         route this row does not model.",
                        row.scenario,
                        self.forwarded_pre_ab,
                        self.forwarded_post_ab,
                        self.forwarded_pre_ba,
                        self.forwarded_post_ba
                    ));
                }
                Ok(())
            }
        }
    }
}

// =========================================================================
// The verdict a row's runner writes
// =========================================================================

/// What `natsim-browser-matrix` writes to
/// `<state>/browser_outcome.json`.
///
/// Parsed rather than indexed so a row test says "the runner did not
/// report `page_type`" instead of panicking inside a `serde_json`
/// index expression.
#[derive(Debug, Clone)]
pub struct RowVerdict {
    pub scenario: String,
    pub nat_a: String,
    pub nat_b: String,
    pub engine_a: String,
    pub engine_b: String,
    /// The RAW `type` of A's `PeerConnectOutcome` — `"direct"`,
    /// `"iceTimeout"`, `"udpBlocked"`, `"noAnnouncement"`,
    /// `"handshakeFailed"` or `"superseded"`.
    pub page_type: String,
    /// The same for B's `acceptPeer`, the answerer's half.
    pub peer_page_type: String,
    pub page_detail: String,
    pub a: IceCounters,
    pub b: IceCounters,
    pub anchor: IceCounters,
    /// What the harness granted the pages, as the DRIVERS reported
    /// doing it rather than as the runner's flag echoed back.
    pub media: String,
    /// The row's application-delivery witness.
    pub app: AppExchange,
    /// Anything the runner could not do. Non-empty fails the row
    /// before any counter is read: a verdict written around an error
    /// is not a measurement.
    pub errors: Vec<String>,
}

impl RowVerdict {
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let s = |key: &str| -> Result<String, String> {
            v.get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("verdict field {key} missing or not a string"))
        };
        let errors: Vec<String> = v
            .get("errors")
            .and_then(serde_json::Value::as_array)
            .map(|a| a.iter().map(ToString::to_string).collect())
            .unwrap_or_default();
        // A verdict the runner wrote around an ERROR has no ledgers to
        // parse — it died before any side reported counters, so they
        // are `null`. Parsing them strictly here would replace the
        // runner's actual reason ("the page server in nsim_b never
        // came up") with a complaint about a missing counter field,
        // which is the least useful sentence available. Measured on
        // the real refusal paths: `--scenario` not in the table,
        // a mis-wired topology, and a wrong `--expect` all produce
        // exactly this shape. So when `errors` is non-empty the
        // ledgers default and `check` reports the errors first, which
        // it does unconditionally.
        let side = |key: &str| -> Result<IceCounters, String> {
            let Some(obj) = v.get(key).and_then(|s| s.get("counters")) else {
                if errors.is_empty() {
                    return Err(format!("verdict field {key}.counters missing"));
                }
                return Ok(IceCounters::default());
            };
            if obj.is_null() && !errors.is_empty() {
                return Ok(IceCounters::default());
            }
            IceCounters::from_json(obj).map_err(|e| format!("{key}.counters: {e}"))
        };
        let text = |key: &str| -> Result<String, String> {
            if errors.is_empty() {
                s(key)
            } else {
                Ok(v.get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned())
            }
        };
        Ok(Self {
            scenario: s("scenario")?,
            nat_a: s("nat_a")?,
            nat_b: s("nat_b")?,
            engine_a: s("engine_a")?,
            engine_b: s("engine_b")?,
            page_type: text("page_type")?,
            peer_page_type: text("peer_page_type")?,
            page_detail: v
                .get("page_detail")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned(),
            a: side("a")?,
            b: side("b")?,
            anchor: side("anchor")?,
            media: text("media")?,
            // The application witness, on the same rule as the
            // ledgers: REQUIRED on a measuring verdict, defaulted
            // only when the runner already said why there is no
            // measurement at all. A verdict that simply omitted `app`
            // would otherwise present as "no payload was forwarded",
            // which on a direct row is indistinguishable from the
            // property the row exists to prove.
            app: match v.get("app") {
                Some(obj) if !obj.is_null() => {
                    AppExchange::from_json(obj).map_err(|e| format!("app: {e}"))?
                }
                _ if !errors.is_empty() => AppExchange::default(),
                Some(_) => {
                    return Err(
                        "verdict field app is null and the runner reported no error, so the \
                         application exchange was neither measured nor refused"
                            .to_owned(),
                    )
                }
                None => {
                    return Err(
                        "verdict field app missing. The row's application-delivery witness is \
                         not optional: without it the row observes a disposition and no payload."
                            .to_owned(),
                    )
                }
            },
            errors,
        })
    }

    /// The disposition one side's `PeerConnectOutcome` reports, or the
    /// typed outcome that means this row has no disposition at all.
    pub fn disposition_of(&self, who: &str, page_type: &str) -> Result<Disposition, String> {
        match page_type {
            "direct" => Ok(Disposition::Direct),
            "iceTimeout" => Ok(Disposition::Relayed),
            "udpBlocked" => Err(format!(
                "{who} settled as udpBlocked. On this row UDP egress demonstrably works — the \
                 leaf's own anchor dialog is a UDP DataChannel that landed direct — so this is a \
                 narrower cause than the evidence supports, not a disposition. ({})",
                self.page_detail
            )),
            other => Err(format!(
                "{who} settled as {other:?} ({}), which is neither a direct session nor a kept \
                 routed one — this row reached no disposition",
                self.page_detail
            )),
        }
    }

    /// Everything a row asserts about its verdict, in the order a
    /// reader wants it: the row really ran what it claims, both sides
    /// agree on the disposition, the disposition is the expected one,
    /// then the ledgers.
    pub fn check(&self, row: &Row) -> Result<(), String> {
        if !self.errors.is_empty() {
            return Err(format!(
                "the runner reported errors, so nothing below is a measurement: {:?}",
                self.errors
            ));
        }
        if self.scenario != row.scenario {
            return Err(format!(
                "verdict is for scenario {:?}, expected {:?}",
                self.scenario, row.scenario
            ));
        }
        if self.nat_a != row.nat_a.mode() || self.nat_b != row.nat_b.mode() {
            return Err(format!(
                "row {} ran behind {} x {} but the table says {} x {} — the scenario is mis-wired",
                row.scenario, self.nat_a, self.nat_b, row.nat_a, row.nat_b
            ));
        }
        let offerer = self.disposition_of("the offerer (connectPeer)", &self.page_type)?;
        let answerer = self.disposition_of("the answerer (acceptPeer)", &self.peer_page_type)?;
        if offerer != answerer {
            return Err(format!(
                "row {}: the offerer settled {offerer} and the answerer settled {answerer}. One \
                 pair has one disposition; two different ones means one side installed a session \
                 the other does not have.",
                row.scenario
            ));
        }
        if offerer != row.expect {
            return Err(format!(
                "row {} landed {offerer}, expected {} ({})",
                row.scenario, row.expect, row.why
            ));
        }
        self.a
            .check_exact("side a (leaf)", row.leaf_expectation())?;
        self.b
            .check_exact("side b (leaf)", row.leaf_expectation())?;
        self.anchor
            .check_exact("anchor (native)", row.anchor_expectation())?;
        // The application exchange, last, because it is the witness
        // that only means something once the disposition above is
        // established: "the anchor forwarded nothing" is the direct
        // claim and "the anchor forwarded both ways" is the relayed
        // one, and which of them is being asserted comes from the row.
        if self.media != row.media.flag() {
            return Err(format!(
                "row {} ran with media {:?} but the table says {} — a row that acquired a \
                 permission it claims not to need is measuring the granted environment",
                row.scenario, self.media, row.media
            ));
        }
        self.app.check(row)?;
        Ok(())
    }
}

// =========================================================================
// The NAT-level disposition witness
// =========================================================================

/// Per-gateway conntrack facts `run_scenario.sh` writes to
/// `<state>/nat_flow.json` after the verdict.
///
/// This is the row's **independent** witness, and the only one that
/// lives entirely inside this slice: it reads the gateway's own
/// conntrack table rather than any counter either endpoint reports.
///
/// Mere existence of a flow to the peer's public address proves
/// nothing — ICE sends checks on every row, including the relayed
/// ones, so the outbound entry is always there. What discriminates is
/// whether that flow was ever **replied** to: a conntrack entry
/// without `[UNREPLIED]` means packets crossed between the two
/// gateways in both directions, which is what "direct" physically
/// means here.
///
/// A read that FAILED is not a gateway that saw nothing. The script
/// reads conntrack through a ladder (`conntrack -L`, then
/// `/proc/net/nf_conntrack`) and a namespace that can produce
/// neither used to yield `{"udp_flows":0,"udp_replied":0}` — which
/// is exactly the shape a relayed row wants to see, so a kernel
/// without `CONFIG_NF_CONNTRACK_PROCFS` and no `conntrack` binary
/// would have PASSED both relayed rows while observing nothing at
/// all. Each side now reports which reader produced its numbers, and
/// an unreadable side is a refusal rather than an absence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayFlows {
    /// UDP conntrack entries involving the peer gateway's public
    /// address.
    pub udp_flows: u64,
    /// How many of those saw traffic in BOTH directions.
    pub udp_replied: u64,
    /// How the table was read: `conntrack` (the CLI), `procfs`, or
    /// `unreadable` when neither worked. `unreadable` is the whole
    /// reason this field exists.
    pub source: String,
}

impl GatewayFlows {
    /// Whether these numbers are a measurement at all.
    pub fn measured(&self) -> bool {
        self.source == "conntrack" || self.source == "procfs"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatFlows {
    pub a: GatewayFlows,
    pub b: GatewayFlows,
}

impl NatFlows {
    pub fn from_json(v: &serde_json::Value) -> Result<Self, String> {
        let side = |key: &str| -> Result<GatewayFlows, String> {
            let o = v
                .get(key)
                .ok_or_else(|| format!("nat_flow.json has no {key} side"))?;
            let n = |k: &str| -> Result<u64, String> {
                o.get(k)
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| format!("nat_flow.json {key}.{k} missing or not a number"))
            };
            Ok(GatewayFlows {
                udp_flows: n("udp_flows")?,
                udp_replied: n("udp_replied")?,
                // REQUIRED, like every other term here: a witness
                // that cannot say where its numbers came from is not
                // a witness, and defaulting this to "readable" would
                // restore the exact collapse it exists to prevent.
                source: o
                    .get("source")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "nat_flow.json {key}.source missing or not a string — a conntrack \
                             read that failed and a gateway that saw no flow are opposite facts"
                        )
                    })?
                    .to_owned(),
            })
        };
        Ok(Self {
            a: side("a")?,
            b: side("b")?,
        })
    }

    /// Assert the gateways agree with the row's disposition.
    pub fn check(&self, row: &Row) -> Result<(), String> {
        // FIRST, on every row: were these numbers read at all?
        //
        // A measurement failure must not count as observed absence.
        // This is checked before the disposition arms because it
        // applies to both of them — a direct row would already fail
        // loudly on zeros, but a relayed row reads zeros as its own
        // confirmation, so an unreadable gateway would confirm it.
        for (who, side) in [("a", &self.a), ("b", &self.b)] {
            if !side.measured() {
                return Err(format!(
                    "row {}: gateway {who}'s conntrack table was not read (source {:?}); it \
                     reported {}/{} flows. No reading is not an absence of flow, and on a \
                     relayed row it would pass as one.",
                    row.scenario, side.source, side.udp_replied, side.udp_flows
                ));
            }
        }
        match row.expect {
            Disposition::Direct => {
                if self.a.udp_replied == 0 || self.b.udp_replied == 0 {
                    return Err(format!(
                        "row {} claims direct, but the gateways saw no two-way UDP flow between \
                         the two public addresses (a: {}/{} replied, b: {}/{} replied). A direct \
                         session that no NAT gateway saw is not a direct session.",
                        row.scenario,
                        self.a.udp_replied,
                        self.a.udp_flows,
                        self.b.udp_replied,
                        self.b.udp_flows,
                    ));
                }
                Ok(())
            }
            Disposition::Relayed => {
                if self.a.udp_replied != 0 || self.b.udp_replied != 0 {
                    return Err(format!(
                        "row {} claims relayed, but a gateway saw a two-way UDP flow to the \
                         peer's public address (a: {}/{} replied, b: {}/{} replied) — something \
                         crossed directly, so the NAT flavor is not what this row models.",
                        row.scenario,
                        self.a.udp_replied,
                        self.a.udp_flows,
                        self.b.udp_replied,
                        self.b.udp_flows,
                    ));
                }
                Ok(())
            }
        }
    }
}

// =========================================================================
// The bash ↔ Rust consistency seam
// =========================================================================

/// One `browser_*` case arm of `run_scenario.sh`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptArm {
    pub scenario: String,
    pub nat_a: String,
    pub nat_b: String,
    pub expect: String,
    pub engine_a: String,
    pub engine_b: String,
    /// `MEDIA=granted|none`. In the seam because the grant is the
    /// only variable of the permission-free leg: an arm that dropped
    /// it would provision the granted environment under the
    /// ungranted row's name, and no counter in the verdict could
    /// tell.
    pub media: String,
}

/// Extract the browser scenarios from `run_scenario.sh`'s source.
///
/// The arms are written in a fixed one-line shape precisely so this
/// can read them:
///
/// ```text
///   browser_cone_cone) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium MEDIA=granted ;;
/// ```
///
/// The alternative — a second copy of the matrix in bash, trusted to
/// stay in step with `ROWS` — is the drift this parser exists to make
/// impossible. If the shape ever changes, the consistency test fails
/// loudly (zero arms parsed is an error, not an empty pass).
pub fn parse_script_arms(script: &str) -> Result<Vec<ScriptArm>, String> {
    let mut out = Vec::new();
    for line in script.lines() {
        let line = line.trim();
        if !line.starts_with("browser_") {
            continue;
        }
        let (scenario, rest) = line
            .split_once(')')
            .ok_or_else(|| format!("browser case arm with no `)`: {line}"))?;
        let field = |name: &str| -> Result<String, String> {
            rest.split_whitespace()
                .find_map(|tok| tok.strip_prefix(&format!("{name}=")))
                .map(str::to_owned)
                .ok_or_else(|| format!("case arm {scenario} has no {name}=: {line}"))
        };
        if field("MODE")? != "browser" {
            return Err(format!(
                "case arm {scenario} is named browser_* but is not MODE=browser: {line}"
            ));
        }
        out.push(ScriptArm {
            scenario: scenario.trim().to_owned(),
            nat_a: field("NAT_A")?,
            nat_b: field("NAT_B")?,
            expect: field("EXPECT")?,
            engine_a: field("ENGINE_A")?,
            engine_b: field("ENGINE_B")?,
            media: field("MEDIA")?,
        });
    }
    if out.is_empty() {
        return Err(
            "no browser_* case arms found in run_scenario.sh — either the matrix is \
                    gone or the arms no longer have the shape this parser reads"
                .to_owned(),
        );
    }
    Ok(out)
}
