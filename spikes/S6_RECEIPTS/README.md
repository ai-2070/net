# Stage 6 — the receipt index

**What this file is.** A per-witness statement of what inverse-receipt
evidence exists for each Stage 6 witness, where it is, and — where
none exists — that none exists. `S6_REPORT.md` §8 previously claimed
that *every* row in the stage had a raw bounded-mutation → RED →
revert → GREEN chain. That claim was not traceable: the reviewer's
filename search found no Stage 6 chains in the tree, and neither did
mine (§1 below records exactly where I looked). The claim is now
corrected in the report and replaced by this index.

**An honest gap is the point of an index.** A row marked *no receipt*
is not a hidden failure; it is a witness whose ability to fail has not
been demonstrated by mutation, and the difference between that and a
demonstrated one is exactly what the reviewer asked to be able to see
per witness rather than per stage.

---

## 1. Inventory: what receipt evidence exists in this repository

Searched, on 2026-09-17, at the Stage 6 repair working tree:

| Location | What is there | Stage 6 receipts? |
|---|---|---|
| `spikes/S5_R3_NATIVE_RECEIPTS/` | Stage 5 round-3 native inverse receipts (X9, X10, X11, ACK oracle), with bounded diffs, commands, exits, verbatim RED and restored GREEN | **No** — Stage 5 |
| `spikes/s5r3_inverse/` | 6 raw logs (P1a–c, P2, X1, X3) in the same shape, plus `README-LeafP12.md` | **No** — Stage 5 |
| `spikes/s5r3-leadership-inverse.md` | one Stage 5 leadership chain | **No** — Stage 5 |
| `spikes/S5_R3_EVIDENCE/` | two Chromium browser-matrix run logs + an anchorless fixture causality log | **No** — Stage 5, and green runs rather than receipts |
| `spikes/kyra/` | reviewer probe sources retained from rounds 4a/4b/5 | **No** — reviewer probes, not receipts |
| `docs/internal/spikes/S6_REPORT.md` §8 | two receipts *described* in prose (the §3 part 2 relay re-pin, and the demo flat row with `peer:` dropped from `openStream`) | **Described, not captured** — no log, diff, command or exit anywhere in the tree |
| `spikes/S6_CHROMIUM_NAT_NOTE.md` | the §6.12 Chromium/NAT investigation note | **No** — analysis |
| `~/AppData/Local/hermes/cache/webrtc-stage6-9ef5b6a03/` | the **reviewer's** evidence directory (parent probes, baselines, CI json) | **No** — review artifacts, and counterexamples rather than receipts |
| `~/AppData/Local/hermes/cache/stage6-nat-35056816488/` | 13 natsim state directories + `natsim.log` from CI run 35056816488 | **No** — and it is a **failing** run (`natsim_browser_cone_cone_is_direct` panicked; `nat_flow.json` = `{"a":{"udp_flows":0,"udp_replied":0},…}`). It is the only retained natsim artifact archive in existence here, which corroborates the reviewer's point that *successful* natsim artifacts are not uploaded |
| `spikes/S6_RECEIPTS/` (this directory) | **9 raw Stage 6 receipts**, taken 2026-09-17, listed in §3 | **Yes** |

So: before this round, the stage had **zero** captured inverse
receipts in the repository. Two were described in prose. The rest of
the §8 claim rested on green CI logs, which establish that a witness
*passed*, never that it *can fail*.

---

## 2. Per-witness index

Columns, per the reviewer's requirement: source SHA (the base a diff
applies to), the selective diff, the executed witness and engine, the
command and its exit, the intended assertion failure, the restoration
identity, and the restored positive output. For a row with a receipt
those eight live in the named log; the table gives the locator and the
one-line mutation so the log is findable without opening nine files.
For a row without one, the table says so and says why.

`WT` below means the Stage 6 repair working tree; each receipt log
carries the exact `sha256` of the file it mutated, before, during and
after, so the base is pinned per receipt rather than per round.

### 2.1 The natsim row table and its checker — `tests/natsim/rows.rs`, `tests/natsim_browser.rs`

Ungated, runs on every platform, and it is where this round's new
acceptance arithmetic lives. **38 witnesses** (was 22).

| Witness | Receipt | Mutation |
|---|---|---|
| `a_direct_row_whose_payload_the_anchor_forwarded_fails` | `S6A-01-direct-row-flat-forwarding.log` | the direct row's flat-forwarding assertion is disabled |
| `a_relayed_row_whose_payload_the_anchor_never_forwarded_fails` | `S6A-02-relayed-row-carried-forwarding.log` | the relayed row's carried-forwarding assertion is disabled |
| `frames_that_arrived_with_the_wrong_nonce_fail_the_row` | `S6A-03-nonce-correlation.log` | the `seen_at_b == nonce_a` comparison is disabled |
| `a_missing_application_term_is_an_error_and_never_a_zero` | `S6A-04-missing-app-term-is-not-a-zero.log` | a missing `forwarded_post_ab` defaults to `0` |
| `a_verdict_with_no_application_witness_is_refused` | `S6A-05-missing-app-witness-is-refused.log` | an absent `app` object defaults instead of refusing |
| `an_unreadable_gateway_fails_instead_of_confirming_a_relayed_row` | `S6A-06-unreadable-gateway-is-not-absence.log` | `GatewayFlows::measured()` returns `true` unconditionally |
| `a_flow_witness_that_cannot_say_where_its_numbers_came_from_is_refused` | `S6A-07-flow-source-is-required.log` | an absent `source` defaults to `"conntrack"` |
| `a_permission_free_leg_that_was_granted_media_fails` | `S6A-08-media-grant-is-part-of-the-row.log` | the row/verdict media comparison is disabled |
| `run_scenario_matrix_matches_the_rust_table` | `S6A-09-media-seam-cannot-drift.log` | the Firefox arm's `MEDIA=none` becomes `MEDIA=granted` in `run_scenario.sh` |
| the other 29 (`the_matrix_is_the_six_derived_rows`, the counter-identity rows, the verdict-parser rows, `the_gateways_discriminate_direct_from_relayed`, …) | **no receipt taken this round** | Most are themselves negative rows — they assert that a malformed shape is *refused* — and the nine above cover every new assertion this round added. The pre-existing 22 were earned under Stage 6's original round and were not re-mutated here; they were all executed green (§4) |

### 2.2 The natsim rows themselves — `tests/natsim.rs`

**Linux netns + root only.** They do not run on this host at all and
cannot: they provision network namespaces with nftables masquerade and
launch two headless browsers inside them. Every row below runs in the
dedicated `natsim` CI job.

| Witness | Receipt | Status |
|---|---|---|
| `natsim_browser_cone_cone_is_direct` and the other five rows | **no receipt** | The row's *checker* is receipted above (§2.1); the row's own execution is CI-only. A mutation receipt would need a netns host |
| `natsim_browser_cone_cone_is_direct_on_firefox` | **no receipt** | same; Firefox has never run on the implementation host |
| `natsim_browser_cone_cone_is_direct_without_media_permission` (**new**) | **no receipt — unexecuted** | Added this round. It has never been run anywhere yet; its first execution is Main's CI run. Stated here rather than implied: this leg's result is not yet known, and a `direct` result is the claim, not an observation |
| `natsim_natted_anchor_publishes_both_mapped_endpoints` (**new**) | **no receipt — unexecuted** | Same. Needs netns + root + `--stun-port-a`'s forwarded mapping |
| `natsim_natted_anchor_publishes_a_reachable_rtc_addr` and the five native punch/upgrade scenarios | **no receipt** | Pre-existing, CI-only |
| the four `setup.sh` validation guards + `helper_rejects_joiner_without_publics` | **no receipt** | Ungated and green (§4); they assert refusals, and their inverse is a mis-provisioned topology which `setup.sh` would then build |

### 2.3 The browser matrix — `tests/rtc_browser/runner/src/stage6.rs`

15 Stage 6 witnesses (the job's 41 per engine includes Stage 4b/5).
Owned by the peer-establishment lane this round, not by this one.

| Witness | Receipt | Status |
|---|---|---|
| `stage6_direct_peer_app_data_leaves_that_counter_flat_while_the_anchor_is_live` | **described, not captured** | This is §8's first named receipt — the pair put back on the relay just before the flat window. `S6_REPORT.md` describes the observation (`7 → 13` each way); no diff, command, exit or captured output exists in the tree. It is credited as a described mutation |
| `stage6_forcing_the_direct_channel_down_moves_the_counter_again` | **earned, not a mutation receipt** | The forced-channel-down phase is a real negative-to-positive transition inside the witness itself, executed hosted. That is legitimate earned credit and is *not* an inverse receipt |
| the other 13 | **no receipt** | Hosted green only (§4). Any receipt for them belongs to the lane that owns `stage6.rs` |

### 2.4 The demo — `examples/browser-demo/`

| Witness | Receipt | Status |
|---|---|---|
| `demo_the_pair_counter_is_flat_while_the_pair_is_direct` | **described, not captured** | §8's second named receipt: `peer:` dropped from `openStream`, so positions addressed the anchor — counter flat at `1+1` while `0/0` arrived. Described in the report; no log in the tree |
| `demo_the_pair_counter_moves_while_the_anchor_carries_the_pair`, `demo_positions_sustain_60_hz_over_the_direct_path`, `demo_announcements_keep_arriving_while_the_counter_is_flat` | **no receipt** | Hosted green only |
| `demo_public_signalling_moves_the_anchor_signal_counter_in_the_flat_window` (**new**) | **no receipt — unexecuted** | Added this round for E2. The demo needs Playwright Chromium, a built wasm leaf bundle and minutes of runtime; it was not executed on this host, and that is stated rather than implied |

### 2.5 Everything else claimed by §8

Leaf native tests, the TypeScript package, the R11 ABI probes and the
WASM compile check: **no Stage 6 inverse receipts**. They were
executed green (§4) and several are owned by other lanes this round.
§8's universal claim covered them; it should not have.

---

## 3. The receipts taken this round

Nine, all in this directory, all executed on the Windows 11
implementation host with native `cargo test`
(`x86_64-pc-windows-msvc`). Each log holds, in order: the mutated
file's `sha256` before / during / after, the bounded mutation as a
unified diff, the exact command, both exit codes, the verbatim RED
output including the intended assertion failure, and the verbatim
restored GREEN output.

| Log | Witness | Mutated exit | Restored exit | Restore |
|---|---|---|---|---|
| `S6A-01-direct-row-flat-forwarding.log` | `a_direct_row_whose_payload_the_anchor_forwarded_fails` | 101 | 0 | sha256-identical |
| `S6A-02-relayed-row-carried-forwarding.log` | `a_relayed_row_whose_payload_the_anchor_never_forwarded_fails` | 101 | 0 | sha256-identical |
| `S6A-03-nonce-correlation.log` | `frames_that_arrived_with_the_wrong_nonce_fail_the_row` | 101 | 0 | sha256-identical |
| `S6A-04-missing-app-term-is-not-a-zero.log` | `a_missing_application_term_is_an_error_and_never_a_zero` | 101 | 0 | sha256-identical |
| `S6A-05-missing-app-witness-is-refused.log` | `a_verdict_with_no_application_witness_is_refused` | 101 | 0 | sha256-identical |
| `S6A-06-unreadable-gateway-is-not-absence.log` | `an_unreadable_gateway_fails_instead_of_confirming_a_relayed_row` | 101 | 0 | sha256-identical |
| `S6A-07-flow-source-is-required.log` | `a_flow_witness_that_cannot_say_where_its_numbers_came_from_is_refused` | 101 | 0 | sha256-identical |
| `S6A-08-media-grant-is-part-of-the-row.log` | `a_permission_free_leg_that_was_granted_media_fails` | 101 | 0 | sha256-identical |
| `S6A-09-media-seam-cannot-drift.log` | `run_scenario_matrix_matches_the_rust_table` | 101 | 0 | sha256-identical |

Two of them are worth naming, for the same reason the reviewer named
her two findings:

- **`S6A-06`** disables nothing but `GatewayFlows::measured()`, and a
  relayed row then passes with `{"udp_flows":0,"udp_replied":0,
  "source":"unreadable"}` — a gateway whose conntrack table was never
  read confirming that no packets crossed it. That is the defect
  E1's witness-hardening item names, executed rather than argued.
- **`S6A-01`** leaves both nonces arriving and only stops reading the
  anchor's per-pair counter. The row still has its typed outcome on
  both halves, its exact ICE ledgers on both leaves and the anchor,
  and a two-way replied flow on both gateways — and it is a relayed
  path. **Nothing else in the row notices**, which is why the
  application witness had to exist.

---

## 4. Executed green, this round, on this host

```
$ cd net/crates/net
$ cargo test --test natsim_browser
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Compile checks (no runtime available for the gated rows on this host):

```
$ cd net/crates/net && cargo check --example natsim_node --features net,nat-traversal,webrtc   → 0
$ cd net/crates/net/tests/natsim/browser && cargo check                                        → 0
$ "…/Git/bin/bash.exe" -n tests/natsim/setup.sh tests/natsim/run_scenario.sh                   → 0
```

`tests/natsim.rs` is `#![cfg(target_os = "linux")]` and compiles to an
empty binary here; `cargo check --test natsim --target
x86_64-unknown-linux-gnu` fails in `cc-rs` (`x86_64-linux-gnu-gcc:
program not found`), so **the new assertions in that file are not
type-checked on this host** — the `natsim` CI job is their first
compile. Said plainly rather than left for someone to discover.
