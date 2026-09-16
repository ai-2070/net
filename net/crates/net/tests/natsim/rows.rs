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
}

/// The six rows, in the brief's order.
pub const ROWS: &[Row] = &[
    Row {
        scenario: "browser_cone_cone",
        nat_a: Nat::ConeAr,
        nat_b: Nat::ConeAr,
        expect: Disposition::Direct,
        why: "both sides admit the peer's check once their own outbound has opened the mapping",
    },
    Row {
        scenario: "browser_cone_portrestricted",
        nat_a: Nat::ConeAr,
        nat_b: Nat::ConePr,
        expect: Disposition::Direct,
        why: "both mappings are endpoint-independent, so each side's check hits the exact tuple \
               the other sent to",
    },
    Row {
        scenario: "browser_portrestricted_portrestricted",
        nat_a: Nat::ConePr,
        nat_b: Nat::ConePr,
        expect: Disposition::Direct,
        why: "simultaneous open: each check matches the conntrack reply tuple the other side's \
               own check created",
    },
    Row {
        scenario: "browser_cone_symmetric",
        nat_a: Nat::ConeAr,
        nat_b: Nat::Symmetric,
        expect: Disposition::Direct,
        why: "the symmetric side's check arrives from an unpredictable port and the \
               address-restricted filter admits it; ICE learns the pair peer-reflexively",
    },
    Row {
        scenario: "browser_portrestricted_symmetric",
        nat_a: Nat::ConePr,
        nat_b: Nat::Symmetric,
        expect: Disposition::Relayed,
        why: "the symmetric side's check arrives from a port the full-tuple filter never sent \
               to and is dropped; the reverse check dies at the symmetric gateway",
    },
    Row {
        scenario: "browser_symmetric_symmetric",
        nat_a: Nat::Symmetric,
        nat_b: Nat::Symmetric,
        expect: Disposition::Relayed,
        why: "neither side can predict the other's mapping; the routed session through the \
               anchor is kept and typed as such",
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
pub const CONTROL: Row = Row {
    scenario: "browser_cone_cone_firefox",
    nat_a: Nat::ConeAr,
    nat_b: Nat::ConeAr,
    expect: Disposition::Direct,
    why: "the cone × cone row on the other engine — the direct path is not Chromium-specific",
};

/// Every scenario this slice defines, rows then control.
pub fn all_scenarios() -> Vec<Row> {
    ROWS.iter()
        .copied()
        .chain(std::iter::once(CONTROL))
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
        self.a.check_exact("side a (leaf)", row.leaf_expectation())?;
        self.b.check_exact("side b (leaf)", row.leaf_expectation())?;
        self.anchor
            .check_exact("anchor (native)", row.anchor_expectation())?;
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GatewayFlows {
    /// UDP conntrack entries involving the peer gateway's public
    /// address.
    pub udp_flows: u64,
    /// How many of those saw traffic in BOTH directions.
    pub udp_replied: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            })
        };
        Ok(Self {
            a: side("a")?,
            b: side("b")?,
        })
    }

    /// Assert the gateways agree with the row's disposition.
    pub fn check(&self, row: &Row) -> Result<(), String> {
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
}

/// Extract the browser scenarios from `run_scenario.sh`'s source.
///
/// The arms are written in a fixed one-line shape precisely so this
/// can read them:
///
/// ```text
///   browser_cone_cone) NAT_A=cone-ar NAT_B=cone-ar MODE=browser EXPECT=direct ENGINE_A=chromium ENGINE_B=chromium ;;
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
        });
    }
    if out.is_empty() {
        return Err("no browser_* case arms found in run_scenario.sh — either the matrix is \
                    gone or the arms no longer have the shape this parser reads"
            .to_owned());
    }
    Ok(out)
}
