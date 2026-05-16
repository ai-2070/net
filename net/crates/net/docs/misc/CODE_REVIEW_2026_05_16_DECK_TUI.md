# Deck TUI branch code review — 2026-05-16

Branch: `tui` (~14K LOC; 100+ commits ahead of `master`).
Adds a new `deck` crate (Ratatui-based operator TUI) plus SDK
re-exports (`dataforts`, `meshdb`) and substrate-side
`InventoryProbe` + `DaemonSnapshot.placement/age_ms` +
`PeerSnapshot` capability fields.

Four parallel passes covered deck core (app/runtime/streams),
deck tabs, deck widgets + bookmarks, and SDK + adapter changes.

## Status

**Closed.** 48 items identified: **12 High / 16 Medium / 14 Low / 6 Nit.**
46/48 landed across the same number of commits on `tui`; the
two remaining items (N5 vertical-centering, N6 tabs re-exports)
were verified non-issues and tracked as "no change" below. Per
the "no review-tracking IDs in code or commit messages"
feedback rule, labels (H1-H12, M1-M16, L1-L14, N1-N6) are for
this doc only — code and commit messages stay self-explanatory.

Substrate / SDK items (H1, H11, H12, M3, M4, M5, M6, L12, L13,
L14) shipped with regression tests where reasonable.

## H. High — fix before merge

| ID  | Area      | Title                                                                            | Location                                                                                                  |
|-----|-----------|----------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| H1  | substrate | Postcard wire-format break in `DaemonSnapshot` / `PeerSnapshot` (no `serde(default)`) | `behavior/meshos/snapshot.rs:174-188, 285-330`                                                            |
| H2  | deck      | Terminal not restored on panic (`ratatui::restore` only on happy path)           | `deck/src/main.rs:71-82`                                                                                  |
| H3  | deck      | Hardcoded `this_node = 0x0001` in 9 sites                                        | `deck/src/app.rs:680,727,746,944,985,2037,2058,2429`, `tabs/groups.rs:299`, `tabs/node_page.rs:299`        |
| H4  | deck      | `Esc` quits the app from the top level                                           | `deck/src/app.rs:1117-1120,1225`                                                                          |
| H5  | deck      | Silent admin failures (dispatched detached, errors `let _ =`)                    | `deck/src/app.rs:323-334,2133-2208`                                                                       |
| H6  | deck      | Index OOB / non-char-boundary panic on `entry.hash_hex[..2]`                     | `deck/src/widgets/blob_detail.rs:174`                                                                     |
| H7  | deck      | `u64` overflow in `(used * 100 / total).min(999)` saturation math                | `deck/src/tabs/nodes.rs:120,127`                                                                          |
| H8  | deck      | `LogLevel::Debug` renders as `?    ` (fallthrough in level styling)              | `deck/src/tabs/logs.rs:272-277`                                                                           |
| H9  | deck      | Non-atomic bookmark write (`fs::write` direct)                                   | `deck/src/bookmarks.rs:175-193`                                                                           |
| H10 | deck      | Filter-matches-nothing displays "1/0" chip with empty silent body                | `deck/src/tabs/failures.rs:65-72`, `tabs/blobs.rs:30-34`                                                  |
| H11 | substrate | `MeshBlobAdapter::list` materializes whole refcount table per call               | `adapter/net/dataforts/blob/mesh.rs:1252-1296`                                                            |
| H12 | substrate | `InventoryProbe` samples never GC'd; departed peers leak forever                 | `adapter/net/behavior/meshos/event_loop.rs:781-799`                                                       |

## M. Medium

| ID  | Area      | Title                                                                            | Location                                                                                                  |
|-----|-----------|----------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| M1  | deck      | `net_map` recomputes `spread_overlaps` (120 × O(n²)) every frame                 | `deck/src/tabs/net_map.rs:64-66,100-110,252-264`                                                          |
| M2  | deck      | `daemon_page::sibling_role` regroups all daemons N+1 times per frame             | `deck/src/tabs/daemon_page.rs:197,286-298`                                                                |
| M3  | substrate | `age_ms` populated from `last_started` even after exit/crash                     | `behavior/meshos/snapshot.rs:185-188,492-495`; `state.rs:320-345`                                          |
| M4  | substrate | `placement` field unenforced; could mis-label remote daemons as local            | `behavior/meshos/snapshot.rs:175-181,491`                                                                 |
| M5  | substrate | `BlobAdapter::list` default returns `Ok(vec![])` — indistinguishable from empty  | `adapter/net/dataforts/blob/adapter.rs:267-269`                                                           |
| M6  | sdk       | `start_with_options` is a strict subset of `start_with_full_extensions`; misnamed | `sdk/src/meshos.rs:621-652` vs runtime extensions                                                         |
| M7  | deck      | `record_matches` allocates `String` per record per frame                         | `deck/src/tabs/audit.rs:261-279`, `tabs/failures.rs:170-178`                                              |
| M8  | deck      | `format_ts_ms` drops the hour component (`(s/60)%60` mod 60m)                    | `deck/src/tabs/logs.rs:348-356`, `widgets/export.rs:184-190`                                              |
| M9  | deck      | `format_id_label` uses `{:04x}` vs `{:x}` elsewhere — same id renders differently | `deck/src/tabs/dataforts.rs:712-717`                                                                      |
| M10 | deck      | Stream loops busy-loop on persistent errors (no backoff)                         | `deck/src/streams.rs:81-97,133-143,184-193`                                                               |
| M11 | deck      | `BookmarkStore::upsert` allows empty / whitespace / dup names                    | `deck/src/bookmarks.rs:148-153`                                                                           |
| M12 | deck      | `param_input::parse_duration` accepts duplicate / out-of-order units; no max len | `deck/src/widgets/param_input.rs:79-116,201`                                                              |
| M13 | deck      | Picker windowing produces empty render when row-height is 0                      | `deck/src/widgets/cluster_picker.rs:88-91`, `widgets/pick_node.rs:171-174`                                |
| M14 | deck      | `local_maint_summary` wildcard `_ => "?"` hides new variants                     | `deck/src/widgets/status_bar.rs:182-192`                                                                  |
| M15 | deck      | Sample seeder spawned detached; harness drop can race partial population         | `deck/src/runtime.rs:440-457`                                                                             |
| M16 | deck      | `build.rs` lacks `cargo:rerun-if-changed=.git/HEAD`; baked SHA drifts            | `deck/build.rs:23-46`                                                                                     |

## L. Low

| ID  | Area      | Title                                                                            | Location                                                                                                  |
|-----|-----------|----------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| L1  | deck      | `format_age` divergence: `500ms` vs `0s` for sub-second across tabs              | `tabs/migrations.rs:121-131` vs `tabs/daemons.rs:161-172` vs `tabs/daemon_page.rs:370-381`                |
| L2  | deck      | `MESH.EVENTS` records w/ `node_id, daemon_id=None` mis-labeled as "substrate"    | `deck/src/tabs/net_map.rs:386-389`                                                                        |
| L3  | deck      | `level_rank` fallback `_ => 1` (Info) defeats filters on new variants            | `deck/src/tabs/logs.rs:144-152`                                                                           |
| L4  | deck      | `blob_detail.rs` `host_id:04x` drops bits relative to `{:x}` elsewhere           | `deck/src/widgets/blob_detail.rs:90`                                                                      |
| L5  | deck      | `node_card` `(r * 100.0) as u16` truncates; 99.9% → 99                           | `deck/src/widgets/node_card.rs:121`                                                                       |
| L6  | deck      | `export::open_unique` writes to CWD; no explicit dir arg, no `BufWriter`         | `deck/src/widgets/export.rs:155-182`                                                                      |
| L7  | deck      | ICE confirm variants missing the `fires …` detail line routine variants have    | `deck/src/widgets/confirm.rs:233-329`                                                                     |
| L8  | deck      | `format!("{w:?}")` renders `Warning` enum Debug into the confirm modal           | `deck/src/widgets/confirm.rs:465`                                                                         |
| L9  | deck      | Corrupt bookmark file gives hard `Parse` error — no recovery path                | `deck/src/bookmarks.rs:114-118`                                                                           |
| L10 | deck      | Audit composition: three `.iter().filter().count()` passes over same slice       | `deck/src/tabs/audit.rs:79-86`                                                                            |
| L11 | deck      | `Tab::next/prev` `unwrap()` panics if current tab not in `Tab::all()`            | `deck/src/app.rs:69-78`                                                                                   |
| L12 | sdk       | `sdk::dataforts::OverflowMetricsSnapshot` re-exported from internal `blob::metrics` | `sdk/src/dataforts.rs:19-32`                                                                              |
| L13 | sdk       | `full` feature omits new `dataforts` and `meshdb` re-exports — undocumented      | `sdk/Cargo.toml:113`                                                                                      |
| L14 | substrate | ICE fixture literals don't exercise new `placement` / `age_ms` fields            | `adapter/net/behavior/meshos/ice.rs:1988-2048`                                                            |

## N. Nits

| ID  | Area      | Title                                                                            | Location                                                                                                  |
|-----|-----------|----------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| N1  | deck      | 8 copies of `center()` rect helper across widgets                                | `deck/src/widgets/*.rs`                                                                                   |
| N2  | deck      | `short_id` dead `.min(s.len())` after `{id:016x}`                                | `deck/src/tabs/daemon_page.rs:367`                                                                        |
| N3  | deck      | `daemon.placement == 0x0001 → local_node.clone()` (same as H3) on tabs          | covered by H3                                                                                             |
| N4  | deck      | `splitmix64` derivation drops upper 32 bits of entropy                           | `deck/src/tabs/net_map.rs:235-240`                                                                        |
| N5  | deck      | `empty.rs` `Length(3)` block has an empty trailing line — half-off centering    | `deck/src/widgets/empty.rs:22-29`                                                                         |
| N6  | deck      | `tabs/mod.rs` no `pub use` re-exports — consumers spell `tabs::audit::render`   | `deck/src/tabs/mod.rs`                                                                                    |

---

## Closed

### Substrate / SDK (regression tests included where reasonable)

| ID  | Commit (short title)                                                                                  |
|-----|-------------------------------------------------------------------------------------------------------|
| H1  | `MeshOS: serde(default) on new DaemonSnapshot / PeerSnapshot fields.`                                 |
| H11 | `Dataforts: push BLOB list prefix filter into refcount iteration.`                                    |
| H12 | `MeshOS: GC actual.inventory for peers no probe samples anymore.`                                     |
| M3  | `MeshOS: daemon age_ms anchors on the lifecycle that's current.`                                      |
| M4  | `MeshOS: pin DaemonSnapshot.placement contract for every this_node.`                                  |
| M5  | `Dataforts: add BlobAdapter::supports_list capability bit.`                                           |
| M6  | `MeshOS: rename MeshOsDaemonSdk::start_with_options to clarify scope.`                                |
| L12 | `Dataforts: re-export OverflowMetricsSnapshot via blob::mod.`                                         |
| L13 | `SDK: document why 'full' omits dataforts + meshdb.`                                                  |
| L14 | `MeshOS: ICE simulator fixtures use ..Default::default().`                                            |

### Deck (no regression tests — TUI render code)

| ID  | Commit (short title)                                                                                  |
|-----|-------------------------------------------------------------------------------------------------------|
| H2  | `Deck: install a panic hook that restores the terminal.`                                              |
| H3  | `Deck: plumb this_node from harness; drop 0x0001 literals.`                                           |
| H4  | `Deck: top-level Esc is a no-op; quit stays on q / Ctrl-C.`                                           |
| H5  | `Deck: surface admin / ICE dispatch errors as footer toasts.`                                         |
| H6  | `Deck: guard blob_detail hash_hex slice against short / non-ASCII inputs.`                            |
| H7  | `Deck: u128-promote nodes saturation math to avoid u64 overflow.`                                     |
| H8  | `Deck: render LogLevel::Debug as DEBUG instead of the unknown-level glyph.`                           |
| H9  | `Deck: atomic bookmark write via sibling tmp + rename.`                                               |
| H10 | `Deck: empty-filter hint in FAILURES + BLOBS instead of '1/0' silent body.`                           |
| M1  | `Deck: compute net_map layout + edges once per frame; HashSet edge dedup.`                            |
| M2  | `Deck: cache daemon_page lineage grouping once per render.`                                           |
| M7  | `Deck: audit + failures search uses allocation-free icontains.`                                       |
| M8  | `Deck: format_ts_ms includes the hours component (HH:MM:SS.mmm).`                                     |
| M9  | `Deck: format_id_label uses {:x} so dataforts ids match the rest of the deck.`                        |
| M10 | `Deck: backoff on stream errors so a persistently failing tail doesn't peg a worker.`                 |
| M11 | `Deck: BookmarkStore::upsert trims + rejects empty name / endpoint.`                                  |
| M12 | `Deck: param_input parse_duration rejects duplicate / out-of-order units + caps buffer length.`       |
| M13 | `Deck: cluster_picker + pick_node bail out when the row window is zero-height.`                       |
| M14 | `Deck: status_bar maint-summary surfaces unknown variants explicitly with a debug_assert.`            |
| M15 | `Deck: sample seeder runs inline so the App starts against a fully populated snapshot.`               |
| M16 | `Deck: build.rs reruns when .git/HEAD moves so the baked SHA stays fresh.`                            |
| L1  | `Deck: hoist format_age into tabs::format_age_ms; remove the four divergent copies.`                  |
| L2  | `Deck: MESH.EVENTS labels node-only records as node.0x.. instead of substrate.`                       |
| L3  | `Deck: unknown LogLevel ranks at 0 so the level filter doesn't silently pass new variants as Info.`   |
| L4  | (batched) `Deck: blob_detail host_id format matches the rest of the deck; node_card rounds saturation.` |
| L5  | (batched in L4)                                                                                       |
| L6  | `Deck: export writes via BufWriter against the startup cwd, with an explicit flush.`                  |
| L7  | (batched) `Deck: ICE confirm gains 'fires' line for every variant + operator-facing warning labels.`  |
| L8  | (batched in L7)                                                                                       |
| L9  | `Deck: BookmarkStore renames corrupt toml aside and recovers with an empty store.`                    |
| L10 | (batched) `Deck: audit outcome counting + Tab::next/prev no longer panic on off-wheel variants.`      |
| L11 | (batched in L10)                                                                                      |
| N1  | `Deck: hoist center() rect helper to widgets/mod.rs; drop the 7 copies.`                              |
| N2  | (batched) `Deck: drop dead .min(s.len()) in short_id; use full 53-bit entropy in net_map angle hash.` |
| N4  | (batched in N2)                                                                                       |

### Verified no-change

| ID  | Title                                                                                  | Why                                                                                                                                |
|-----|----------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------------------------|
| N5  | `empty.rs` centering correction                                                        | The `Min(0) / Length(3) / Min(0)` layout centers symmetrically; the 3-line block contains headline / blank-spacer / hint which IS the visual content. Not off-by-half. |
| N6  | `tabs/mod.rs` re-export render fns                                                     | `tabs::audit::render` is the canonical spelling; the prior review surfaced this as a nit and `tabs/mod.rs` now hosts the shared `format_age_ms` helper. Adding `pub use` aliases would just duplicate the surface without changing the per-tab call sites. |
