# Deck TUI branch — independent second-pass review — 2026-05-16

Branch: `tui`. A previous 48-item review (see
`CODE_REVIEW_2026_05_16_DECK_TUI.md`) closed every item it
opened; this is an independent re-read that flags issues the
first pass missed.

## Status

**Closed.** 17 items identified: **1 High / 6 Medium / 9 Low**,
plus 5 nits. 17/17 landed across the same number of commits
on `tui`. Per the "no review-tracking IDs in code or commit
messages" feedback rule, labels (H1, M1-M6, L1-L9, N1-N5) are
for this doc only — code and commit messages stay
self-explanatory.

Substrate / SDK items (M4, M5, L9) shipped with regression
tests.

## H. High — fix before merge

| ID  | Area | Title                                                                                  | Location                                  |
|-----|------|----------------------------------------------------------------------------------------|-------------------------------------------|
| H1  | deck | `cursor_to_bottom` for DATAFORTS uses `blob_adapters.len()`; rendered list is `1 local + N remote dataforts`. `G` lands on the wrong row whenever remote dataforts exist. | `deck/src/app.rs:1787-1789` |

## M. Medium

| ID  | Area      | Title                                                                                  | Location                                  |
|-----|-----------|----------------------------------------------------------------------------------------|-------------------------------------------|
| M1  | deck      | `clamp_blobs_cursor` clamps against unfiltered `blobs_tail.records.len()`, but render + `open_blob_detail` apply `record_matches`. With an active `/` filter the cursor + Enter target rows past the visible tail. | `deck/src/app.rs:1726-1733` vs `1020-1043` |
| M2  | deck      | BLOBS poll only refreshes `blob_adapters[0]`. Operator cursoring to a different DATAFORTS adapter sees stale entries from adapter 0 on BLOBS and via the `b` cross-link. | `deck/src/streams.rs:268-285`, `main.rs:62-69` |
| M3  | deck      | BLOBS poll swallows `adapter.list()` errors silently (`Err(_) => continue`). Persistent failure gives stale data with no toast / footer signal — H5/M10 pattern wasn't applied here. | `deck/src/streams.rs:280-283` |
| M4  | substrate | Multi-probe partial-panic wipes peers from the non-panicking probe. With disjoint per-probe coverage, a panic in B leaves `peers_seen_inventory` containing only A's peers — `retain` drops B's. | `behavior/meshos/event_loop.rs:794-823` |
| M5  | substrate | Empty `Ok(vec![])` from any inventory probe wipes `actual.inventory`. Trait doc allows transient empty returns; H12 conflates "ran successfully" with "authoritatively saw everyone." | `behavior/meshos/event_loop.rs:794-823`, `probes.rs:103-113` |
| M6  | deck      | `tabs::logs::matches_ci` lowercases the haystack per record per render — reintroduces the per-frame `String` alloc that M7 (first pass) closed for audit/failures. | `deck/src/tabs/logs.rs:321-326` |

## L. Low

| ID  | Area      | Title                                                                                  | Location                                  |
|-----|-----------|----------------------------------------------------------------------------------------|-------------------------------------------|
| L1  | deck      | Two divergent `short_id` shapes: `daemon_page::short_id` is `0xXXXXXX` (6-padded); `groups::short_id` is `0x{id:x}` (variable). Same id reads differently on neighbouring tabs. | `deck/src/tabs/daemon_page.rs:375-381` vs `tabs/groups.rs:235-237` |
| L2  | deck      | `net_map` legend advertises `◇` for UNREACHABLE but `glyph_for` only ever returns `◆` / `■` — unreachable peers render as a red `◆`. | `deck/src/tabs/net_map.rs:349-361, 437-438` |
| L3  | deck      | `tabs::nodes` re-iterates every daemon per row to count placements — O(peers × daemons) per frame. Pre-aggregate once. | `deck/src/tabs/nodes.rs:144-148` |
| L4  | deck      | `node_page::pressure_color` / `tabs::nodes::pressure_style` hardcode `0.85` / `0.95`; dataforts already exposes shared `HEALTH_GATE_*_THRESHOLD` constants. Drift risk. | `deck/src/tabs/node_page.rs:801-809`, `tabs/nodes.rs` |
| L5  | deck      | `lineage::role_for` casts `index as u8` — wraps mod 256 for groups with >256 members; `REP m[0]` for member 256. | `deck/src/lineage.rs:116-129` |
| L6  | deck      | `g` / `G` keys swallowed in `daemon_focus` / `node_focus` (trailing `else { return; }`); other cursor tabs honour them. | `deck/src/app.rs:1184-1192, 1226-1267` |
| L7  | deck      | `groups.rs` renders `mbr 1/0` when the cursored group has no members. Same off-by-one pattern H10 (first pass) fixed for BLOBS / FAILURES. | `deck/src/tabs/groups.rs:104-106` |
| L8  | deck      | `confirm::render_blast_radius` silently truncates the warning list to 3. For ICE break-glass the warnings *are* the rationale. | `deck/src/widgets/confirm.rs:472` |
| L9  | substrate | No postcard legacy-byte regression test for `MeshOsSnapshot`. H1 (first pass) added JSON forward-compat + same-binary postcard round-trip but didn't pin a captured legacy byte string. | `behavior/meshos/snapshot.rs:858-981` |

## N. Nits

| ID  | Title                                                                                  | Location                                  |
|-----|----------------------------------------------------------------------------------------|-------------------------------------------|
| N1  | Three identical `unix_now_ms()` copies — companion hoist alongside `format_age_ms`.    | `audit.rs:343-348`, `blobs.rs:222-227`, `failures.rs:204-209` |
| N2  | Three identical `fmt_ts` (HH:MM:SS.mmm) copies across daemon_page / groups / net_map.  | `daemon_page.rs:385`, `groups.rs:443`, `net_map.rs:418` |
| N3  | `hex_encode` uses `format!("{b:02x}")` per byte (~2 small allocs per byte).            | `adapter/net/dataforts/blob/mesh.rs:1374-1379` |
| N4  | Dataforts overflow aggregation uses plain `+=` u64; H7 (first pass) standardised on saturating. | `deck/src/tabs/dataforts.rs:668-707` |
| N5  | Bookmark corrupt-aside filename uses `unix_ms` granularity — same-ms cycles overwrite the prior aside. | `deck/src/bookmarks.rs:124` |

## Verified no-change

| ID  | Title                                                          | Why                                                                                           |
|-----|----------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| —   | `PeerSnapshot` dropped `Copy` / `Eq` from its derive set       | The substrate's docstring at `snapshot.rs:307-313` already documents this as a deliberate trade-off (`capability_set: BTreeSet<String>` + `software_version: Option<String>` are heap-owned; `cpu_load_1m: Option<f64>` can't derive `Eq`). External SDK consumers cloning instead of copying is the correct fix on their side. Surfaced for completeness — no code change. |

---

## Closed

### Substrate / SDK (regression tests included)

| ID  | Commit (short title)                                                                                                                              |
|-----|---------------------------------------------------------------------------------------------------------------------------------------------------|
| M4  | `MeshOS: inventory GC requires every probe to succeed AND at least one to return non-empty samples.` (covers M5 as well)                          |
| M5  | (batched in M4)                                                                                                                                   |
| L9  | `MeshOS: pin DaemonSnapshot + PeerSnapshot postcard wire bytes so accidental field reorder / type change trips a regression test.`                |

### Deck (no regression tests — TUI render code)

| ID  | Commit (short title)                                                                                                                              |
|-----|---------------------------------------------------------------------------------------------------------------------------------------------------|
| H1  | `Deck: DATAFORTS cursor_to_bottom uses collect_dataforts().len() so G lands on the last visible row when remote dataforts exist.`                 |
| M1  | `Deck: clamp + cursor_to_bottom for FAILURES + BLOBS use the filtered count so an active / search keeps the cursor on a visible row.`             |
| M2  | `Deck: BLOBS poll unions every wired adapter and surfaces per-adapter list errors as footer toasts on each transition.` (covers M3)               |
| M3  | (batched in M2)                                                                                                                                   |
| M6  | `Deck: logs record_matches reuses audit::ascii_icontains so the per-frame search drops the haystack lowercasing allocation.`                      |
| L1  | `Deck: hoist canonical short_id helper into tabs::short_id so daemon_page and groups render the same id with the same shape.`                     |
| L2  | `Deck: net_map renders unreachable peers as the hollow diamond the legend already advertises.`                                                    |
| L3  | `Deck: pre-aggregate daemon placement counts once per NODES render; drops the O(peers × daemons) per-frame scan.`                                 |
| L4  | `Deck: NODES + NODE.PAGE pressure bands wire to the dataforts HEALTH_GATE_*_THRESHOLD constants instead of hardcoded 0.85 / 0.95.`                |
| L5  | `Deck: lineage role index widens to u16 + saturates; >256-member groups no longer wrap to REP m[0].`                                              |
| L6  | `Deck: daemon-focus / node-focus honour g / G top + bottom keys; consistent with every other cursor tab.`                                         |
| L7  | `Deck: GROUPS title chip shows 0/0 instead of 1/0 when the cursored group is empty or no groups exist.`                                           |
| L8  | `Deck: blast-radius confirm modal shows '… +N more (see AUDIT)' when warnings exceed the 3-row cap.`                                              |
| N1  | `Deck: hoist shared tabs helpers (fmt_ts_hms_ms / unix_now_ms), saturate overflow aggregation, write! hex_encode, bookmark corrupt-aside resolves collisions.` (batched N1-N5) |
| N2  | (batched in N1)                                                                                                                                   |
| N3  | (batched in N1)                                                                                                                                   |
| N4  | (batched in N1)                                                                                                                                   |
| N5  | (batched in N1)                                                                                                                                   |
