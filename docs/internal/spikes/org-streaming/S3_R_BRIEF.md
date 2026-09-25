# S3_R — Stage 3 repair round (ACCEPT findings closure)

**Authorization and limits.** The S3Review ACCEPT packet
(`docs/internal/spikes/org-streaming/S3_REVIEW_PACKET.md`, pinned head
`2225de011`) and this pinned brief. **REPAIR ONLY (3 rows):** two
witness-gap closures and one witness + §4.3-conformance fix. The ACCEPT
stands; these are its named non-blocking findings. NOTHING else: no
features, no refactors, no bindings (Stage 4), no browser work, and the
owner-declined F-S1R-2 rider plus the F-S3.1-2 Stage-4 rider stay out of
scope. Row 3 authorizes exactly ONE production change —
`OrgStreamRaw::poll_next`'s `Err` arm surfacing `Ready(Some(Err(_)))`
(§4.3's `Stream<Item = Result<Resp, OrgSdkError>>` semantics; the current
`Ready(None)` swallow is the forbidden "false clean end") — it is a
contract fix; anything beyond it is a finding, STOP.

## Rows (citations → executed evidence → VERBATIM closure property)

**Row 1 — F-S3R-1 (P2, witness gap): the Granted facade serve arms.**
Cite: `sdk/src/org/serve.rs:648-651` (+ the client-stream/duplex siblings at
`:675`, `:700`). Executed: the reviewer's M6 (each Granted arm resolving to
`serve_rpc_owner_scoped_*`) leaves the landed suite 10/10 GREEN while its
temporary probe — all three shapes round-tripped through
`serve_org_*(.., Granted, ..)` with a real cross-org grant — REDS at the
granted admission (probe green at pristine: the capability is real; the arms
work today; a regression would ship silently). **This hole propagates: Stage
4's binding serve rows dispatch through these same arms.**
**Closure (packet §8 verbatim): "a named witness in which a granted caller
completes streaming, client-streaming and duplex calls through handlers
registered with `OrgAccess::Granted` asserting exact payloads and
four-party attribution, whose inverse (each Granted arm resolving to
`serve_rpc_owner_scoped_*`) reddens it at the named assertion."** Witness
name: `granted_facade_streaming_serve_rows_complete_cross_org` (all three
shapes in the one named witness; each arm's flip must redden it).

**Row 2 — F-S3R-2 (P2, witness gap): the Q1 facade default deadline.**
Cite: `sdk/src/org/call.rs:175-184` (the `deadline_ms == 0` →
`DEFAULT_LIFETIME_MS = 300_000` mapping). Executed: the reviewer's M5 (the
0 arm producing NO CallOptions deadline — the forbidden "none" semantics)
leaves 10/10 and 338/338 GREEN. The property is observationally equivalent
to core's own default under default configs — a discriminating witness must
run a provider whose `default_live` is materially shorter than 300 s.
**Closure (packet §8 verbatim): "a named witness in which a streaming call
issued through a facade verb (which passes `deadline_ms == 0`) against a
provider whose `default_live` is materially shorter than 300 s keeps
delivering past that shorter bound — the facade's 300 s lifetime in force —
and the inverse (the `deadline_ms == 0` arm producing no deadline) reddens
that witness at its named assertion."** Witness name:
`facade_default_deadline_at_zero_outlives_a_shorter_provider_default`.

**Row 3 — F-S3R-3 (P3 + one §4.3 fix): the bytes rows' behaviour.**
Cite: `sdk/src/org/call.rs:232-255` (`OrgStreamRaw`) and the CS seam at
`:801-812`. Executed: the reviewer's M8 (the `Err` arm swallowing errors to
`Ready(None)` — a false clean end) leaves the full ten GREEN.
**Closure (packet §8 verbatim): "a named witness that drains `OrgStreamRaw`
through a midstream retirement and observes the final
`Err(AdmissionDenied(Denied))` item (never a swallowed clean end), plus one
drive of `call_client_stream_bytes_deadline` to a typed terminal; the
inverse (`Ready(Some(Err(_)))` → `Ready(None)` in `OrgStreamRaw::poll_next`)
reddens the first at its named assertion."** Witness names:
`org_stream_raw_surfaces_midstream_errors_as_items` (the drain) and
`call_client_stream_bytes_deadline_reaches_a_typed_terminal` (the drive).
The ONE authorized production change: the `Err` arm surfaces
`Ready(Some(Err(_)))` per §4.3's item-shape; the swallow form IS the named
inverse.

## Preserved credit (packet §9 — do not rework)

The 10/10 + 338/338 + 42/42 estate; the §4.3 verb-table conformance
(line-by-line verified); the frozen-type diff-empty claim; F-S3.2-1's
complete ruling compliance (exactly the five `pub(crate)` constructors);
all ten named witnesses discriminating (M2/M3/M4); the REQUIRED pin receipt;
the F-S3.1-2 disclosure (executed and structural).

## Ownership, evidence rules, acceptance

Files (exclusive): `net/crates/net/sdk/src/org/**` (Row 3's `Err`-arm fix at
`call.rs:239-241` + any test seams), `net/crates/net/sdk/tests/org_streaming.rs`
(plus small `tests/` helpers if needed), and
`docs/internal/spikes/org-streaming/S1_REPORT.md` (§6 corrections + your
`## 7. Repair round (S3_R)` record). Everything else is Main's or verified
work — a needed change beyond Row 3's one-line arm = finding, STOP. Evidence
rules verbatim (raw inverse receipts at the production site — Row 1's
per-arm flips and Row 2's and Row 3's inverses REQUIRED; four weakenings;
rosters from source; `--retries 0 --no-tests=fail`; the SDK's named feature
set; `cargo fmt -p net-mesh-sdk -- --check`; executed vs source-established;
never-executed lists; disk discipline — verify size+sha after every write
under 5 GB free). Commit prefixes `S3R:` (implementation + receipts +
record pairs). Report the `org_streaming` binary's new count + full name
list to Main for a same-commit CI floor re-pin (floor 10 → expected 13).
Owner-pending: none. Report only on green at your exact head. NO stage n+1.
