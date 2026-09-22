# WebRTC Transport Merge Code Review

**Date:** 2026-09-22
**Repository:** `ai-2070/net`
**Reviewed head:** `801ae18c74304e5f4bb9722de55aa334c5e285b4` (`fix(merge): land post-merge CI fallout`)
**Reviewed base:** `c5a893c1eb90747d8a30b75ec3018b5be397e804` — the `origin/master` state the branch absorbed at merge commit `e3d15e9aa`
**Branch:** `LZL0/webrtc-transport` (landed on `master` by fast-forward; there is no PR merge commit)
**Scope:** 466 branch-owned commits, 562 files, +220,021 / −3,419. The whole browser-native WebRTC transport program, Stages 0 through 7: spikes, the `net-mesh-wire` extraction, the RTC driver / STUN / ICE / NAT matrix, signalling and §12 admission, the anchor and browser bootstrap credential, the `net-leaf` crate, `@net-mesh/browser`, the browser game store, and the CI / natsim / browser witness apparatus.

## Executive summary

This is a program, not a change set, and it is unusually well-built. The generation fences, retirement ordering, identity at rest, replay windows, copy-on-write patch semantics, assembly bounds, AEAD/clock seams and the `PeerAddr`/`PeerSink` refactor all survive direct attack. The evidence base is genuine witness work in the majority, and the code comments narrate real design decisions rather than excusing omissions.

It is nonetheless **incorrect as merged**. Three classes of defect run through it:

1. **Convergence and access contracts break on the failure model the code itself documents.** A single lost owner-initiated MANIFEST wedges a replica forever while it reports `ready`; a handle dies without the replica ever learning; and read revocation never reaches the delta feed, so a revoked peer keeps receiving live projection diffs indefinitely.
2. **Fail-open at one security gate, fail-leaky at several lifecycle seams.** `§12` gates 3/4 are skipped entirely when the peer-map entry is unresolvable — the exact state the ingress documents as reachable — and the witness for that gate cannot fail. In the leaf, a pending handshake is destroyed by the very packet meant to complete it, a borrow conflict silently discards an authenticated packet, and the bootstrap credential's `http://localhost` exception is a URL *prefix* match that routes the bearer to an attacker host.
3. **A meaningful minority of witnesses cannot discriminate their claimed outcome.** Eleven are named below — post-hoc zeros, `X || !X`, an oracle that accepts the loss its name forbids, a probe that prints its verdict without ever verifying the mechanism it needed.

**Recommended disposition:** the 28 High findings are follow-up work that should be scheduled before further build-out on this transport, not left to accumulate; findings 15–16 (credential URL prefix, invite nonce in a redacting `Debug`) are security defects and deserve immediate patches. The Medium witness findings are individually cheap and collectively decide whether the next regression is caught.

---

## Findings at a glance

| # | Finding | Severity | Area | Status |
|---|---|---|---|---|
| 1 | Lost owner-initiated MANIFEST wedges a replica that reports `ready` | High | store | Open |
| 2 | A correlated `no {closed}` is dropped; a dead handle is held forever | High | store | Open |
| 3 | Read revocation never reaches the delta feed | High | store | Open |
| 4 | A failed reply-stream open is cached as a permanent rejection | High | store | Open |
| 5 | `§12` gates 3/4 skip their check when the peer entry is unresolvable | High | anchor | Open |
| 6 | The `§12` provisional-signalling witness cannot fail | High | anchor | Open |
| 7 | Ownership-charge refusal drops acknowledged fragment bytes | High | rtc | Open |
| 8 | A pending handshake is destroyed before msg2 validates | High | leaf | Open |
| 9 | Pending handshakes have no deadline and no release on retirement | High | leaf | Open |
| 10 | A registered call is leaked on the request-encode error path | High | leaf | Open |
| 11 | `unsubscribe` destroys the nRPC plane's membership it does not own | High | leaf | Open |
| 12 | The reply-carrier fence covers Stream owners but not Channel owners | High | leaf | Open |
| 13 | Peer sessions are never released when their transport dies | High | leaf | Open |
| 14 | The membership Ack is decoded and discarded | High | leaf | Open |
| 15 | The credential's `http://localhost` exception is a prefix match | High | leaf | Open |
| 16 | The redacting `Credential` Debug prints the invite nonce | High | leaf | Open |
| 17 | Buffered trickle frames flush with no dialog fence | High | leaf | Open |
| 18 | The inbound sink drops the datagram on borrow conflict | High | leaf | Open |
| 19 | An inbound message 1 destroys our own pending handshake | High | leaf | Open |
| 20 | A post-`close()` attempt registration leaves an orphan ICE agent | High | leaf | Open |
| 21 | `mark_remote_ready` strands the answerer's deferred candidates | High | leaf | Open |
| 22 | The Reject handler tears down a Direct-terminal attempt | High | leaf | Open |
| 23 | `settle_peer_deadline` publishes `iceTimeout` over an installed attempt | High | leaf | Open |
| 24 | `RxStream::promote`'s span-blind handoff stalls a reliable stream | High | leaf | Open |
| 25 | Two opens of one identity alias one node stream | High | leaf | Open |
| 26 | The signal dialog is carried as `f64`, corrupting ~511/512 of minted dialogs | High | leaf | Open |
| 27 | `stand_down` never re-attaches; the tab becomes a permanent zombie | High | leaf | Open |
| 28 | The per-source-order witness never asserts order | High | tests | Open |
| 29–60 | Robustness, lifecycle and contract-drift defects (32) | Medium | mixed | Open |
| 61–91 | Witness non-discrimination (31) | Medium | tests | Open |
| 92–96 | Bindings, ABI and documentation contracts (5) | Medium | mixed | Open |
| 97–119 | Accounting, allocation, stale-doc and nit defects (23) | Low | mixed | Open |
| 120–121 | CI floors and witness pins (2) | High | ci | Open |
| 122–130 | CI selection, checkers and fixture contracts (9) | Medium/Low | ci | Open |

**Severity.** *High* = a shipped contract breaks, a security boundary fails open, or a security gate's witness cannot fail. *Medium* = robustness and lifecycle defects, contract drift, and witnesses that cannot discriminate their claimed outcome. *Low* = accounting, allocation, stale-doc and wording defects.

---

## Resolution status

**Repair pass in progress** on `LZL0/webrtc-fixes` (branched from `0efc4ab39`). The findings below are unedited — they record what was true at the reviewed commit `801ae18c7`, and the values and line numbers they quote are evidence, not claims. This table is updated at each batch boundary.

| # | Finding | Status | Commit |
|---|---|---|---|
| 1 | Lost owner-initiated MANIFEST wedges a replica | Closed | `89f1ed53f` |
| 2 | A correlated `no {closed}` is dropped; a dead handle is held forever | Closed | `89f1ed53f` |
| 3 | Read revocation never reaches the delta feed | Closed | `b0f6ae11c` |
| 4 | A failed reply-stream open is cached as a permanent rejection | Closed | `078e72a41` |
| 5 | `§12` gates 3/4 skip their check when the peer entry is unresolvable | Closed | `f52edc4cf` |
| 6 | The `§12` provisional-signalling witness cannot fail | Closed | `db6dd0a20` |
| 7 | Ownership-charge refusal drops acknowledged fragment bytes | Closed | `f52edc4cf` |
| 29 | Five floor steps pin witnesses by unanchored substring | Closed | `47fb57694` |
| 30 | Stale floors: `MIN=196` (wire) and 31 (deck) | **Partly closed** — wire 196→275 and deck 31→35 re-measured in the repair tree; leaf 308 and `sensing_org_lease_wire --min 9` deferred, because those two count a different surface (the lib plus its integration binaries) than the attribute scan used here | `78880312c` |
| 8–28, 31–130 | — | **In progress** — eight repair streams over disjoint file sets | — |

### What the closures changed, and how they are witnessed

- **#1** A delta for a generation the replica never installed is now split by direction: *below* the installed one is still dropped (`stale-generation`, a retransmission of a document already moved past), *above* it provokes `resync` through the existing `#behind` machinery, coalesced so a flood of deltas for the missed generation asks once. Witness: `replica.test.ts` observes the ask, the coalescing, and that nothing was applied.
- **#2** `closed` is now handle news however it was provoked — `errors.ts` makes it terminal for the handle and for any action in flight on it, and `receive`'s foreign-handle gate already discards any `closed` naming a different handle. The `q`-of-a-dead-question guard is kept for the request refusals, which do answer a question.
- **#3** `propagate` re-consults `permitsRead` per handle per commit and forgets the handle on refusal. The feed *is* the read delivered fresh, so leaving the handle alive would let `alive` renew the lease of a peer the policy now forbids.
- **#4** `replyStream` (host) and `stream()` (join) clear their cached open on failure as well as on success, so a transient `openStream` failure is a retry rather than a verdict on the peer.
- **#5** All three `§12` gate families — forward (1), subscribe (3), announce (4) — now `let Some(endpoint) = … else { return; }`: an unresolvable endpoint is the eviction race the ingress documents as reachable, and gate 5 already failed closed there. `cargo check --lib --features "net cortex webrtc"` clean.
- **#6** The signalling leg's `after >= before` (monotone counters — cannot fail) is now a `wait_for` on a strict `>`, plus the absence the claim is really about: no `ice_pending()` and no `admission_promoted()` movement across the whole witness. Counting a refusal is not the same as not participating, and the absence covers legs (a)–(d) too. Verified: `cargo nextest run --test rtc_admission --features "webrtc fixtures cortex" --no-tests=fail --retries 0` → 1 test run, 1 passed.
- **#7** The charge refusal now emits the `AbandonedGroup` record and the fence the capacity refusal twelve lines below already had. The record carries `charged: false` — no obligation slot was taken there — and `take_terminals` and the coalesce path release only for records that hold one; `Partial::abandoned` and the capacity refusal's record carry `charged: true`. Without that distinction every charge refusal drifted `OwnershipCharge` downward and admitted more groups than the bound allows.
- **#29** All five pins require the witness name as a complete identifier token (not preceded or followed by `[A-Za-z0-9_]`), which defeats the `<required>_extra` and renamed-supersets shapes the file itself documents as spoofable. Anchored by token rather than by line shape because the five steps read three different output formats.

### Witnesses inverted rather than extended

Three existing witnesses encoded the old buggy behavior and had to change sides, which is worth recording because each is now a regression guard against a repair being reverted:

1. `review_repairs.test.ts` — *"does not let a stale refusal tear down a healthy view"* pinned the `closed` drop that **#2** reports as the defect. Its property belongs to `forbidden` (a request refusal answering a dead question), and the `closed` case is now asserted to tear down and rejoin.
2. `replica.test.ts` — *"drops a delta for a generation other than the installed one"* asserted exactly the behaviour **#1** reports as a bug. It is now two tests, one per direction.
3. `hosted.test.ts` — the adoption witness's `successor.counts().handles === 0` was an *incidental* consequence of the replica never recovering from handle death. The claim it was written for — the successor never adopted the stale handle, and its write was not applied — is asserted directly above; the count is now `=== 1`, a handle the successor minted for the replica's rejoin.

### Process notes from the repair pass

- **The silent-no-op trap is real and it bit.** `tests/rtc_admission.rs` is `#![cfg(all(feature = "webrtc", feature = "fixtures"))]`; under `--features "net cortex webrtc"` it compiles to **zero tests**, and `nextest` reported `0 tests run: no tests to run`. Only `--no-tests=fail` turned that into a failure instead of a green. The same feature gap also made an early `cargo check` "pass" without compiling `rtc/fragment.rs` or any `#[cfg(feature = "webrtc")]` gate at all. Every verification in this pass names the feature set it ran under.
- **An early green was retracted.** A 686-test `vitest` run was reported as covering two new witnesses before anyone noticed the edits had landed in the wrong worktree (relative paths resolve to `net-cli`; the repair tree is `webrtc-fixes`). The work was relocated, the claim corrected, and all file access since has used absolute paths. The second run — same 686, then 688 with the new tests — is the one that counts.

---

## Method and limits

Read-only review at a detached worktree pinned to `801ae18c7`; the main working tree is a different branch and was not read. No build, test, lint or format command was run, so this document makes no claim about compilation or test outcomes. Line numbers were read at the reviewed commit.

The range is far too large to line-review exhaustively. It was decomposed into seven slices — the `net-leaf` crate, the browser SDK and game store, the wire/RTC transport core, the anchor and bootstrap security boundary, the test and CI apparatus, the bindings/ABI/docs surface, and a CI-gates re-pass — each reviewed independently, plus a cross-cutting pass over the store's bounds, the patch semantics, the assembly table, and the large-response and chunking seams. Every finding below names the failure mode it enables; three hypotheses raised during the cross-cutting pass were **refuted by checking first** and are recorded in "Explicitly verified sound" rather than filed.

Two process notes. One reviewer stream died before covering CI floors, rosters and the two roster/witness checkers; its four recovered findings are included (88–91) and the uncovered half was re-run as a second pass, whose findings are 120–130. One reviewer's 78 filed findings did not survive into its delivered payload; those were recovered from its scout reports and its transcript, and the reviewer subsequently re-emitted the full set to `local://leaf-findings.md`. Findings 26–27 were restored from that re-emission and independently re-derived against the pinned source — the `dialog as f64` cast and the `stand_down` asymmetry were both read directly.

Findings are grouped by area within each severity band rather than strictly by severity across the document; 120–130 form their own slice section at the end.

Where the format differs from `SDK_CHANNEL_CONSISTENCY_AUDIT_2026_08_08.md`: High findings carry the full `### Required closure` treatment; Medium and Low findings are given as compact entries carrying the same four facts (where, evidence, failure mode, closure), so that 130 findings stay readable in one document.

---

## Explicitly verified sound

Recorded so the findings are not read as a general indictment of a 220 000-line range.

- **The store's assembly bounds hold.** The total-bytes ceiling is charged against *reservations* (the manifest's declared total), not arrivals, so concurrent near-empty assemblies cannot fill the table past its bound; chunk admission charges before holding; the base64 gate is genuinely canonical (alphabet, `len % 4`, and the pad-bit check at `wire.ts:764-778`); index, total, duplicate and conflicting-chunk paths all terminate with the reservation released; the assembled document is re-parsed under the same depth / duplicate-key / finite-number rules a single frame gets.
- **Atomic patch semantics hold.** `applyPatch` checks every bound over the whole op list before touching anything, applies to a copy-on-write draft so `current` is never mutated on any outcome, refuses root removal, and is prototype-safe twice over: forbidden segments *and* own-property-only descent. The atomicity claim is a property of the function, not a rule its callers must remember.
- **`lost MANIFEST` is repaired the right way.** Orphan chunks are dropped rather than assembled from chunk metadata, a reclaimed assembly admits nothing, and the generation watermark (`#retired`) drops duplicate and superseded manifests before they reach the table.
- **One hypothesis was refuted:** a manifest naming a foreign handle looked like it could key an assembly that every cleanup path (keyed on the learned handle) would miss, leaking its reservation forever. `replica.ts:382` carries a global `foreign-handle` gate ahead of every per-kind handler, so the shape cannot arise. A second — that `propagate` silently drops updates — is refuted by `#delta`'s `base !== #revision` gap check, which resyncs on the next change.
- **Receiving-edge ordering and identity.** Deltas apply only at `base === revision` on the installed generation; the watermark is strictly monotone; generations restart only with a fresh handle; a revoked-then-readmitted member cannot apply a stale old-handle delta.
- **Bounded RTC ingress.** `RtcTransport::submit` charges reserved slots and bytes under the queue lock *before* the copy and re-checks `closed` under the same lock; reassembly refuses `end > MAX_REASSEMBLED_BYTES` before any allocation, caps pieces per group, groups per session and held bytes per session; `checked_add`/`saturating_add` throughout, and `protocol.rs:80` pins the u16 fragment offsets at compile time. No overflow or truncation found.
- **`PeerAddr`/`PeerSink` cutover.** Every mesh send site routes through `PeerSink`'s total `PeerAddr` match; `addr_to_node` is populated only from `PeerTransport::owned_addr()`; and the "an RTC relay is not an RTC target" seam is enforced by construction (`pair_action_for` requires our RTC config *and* the target's own RTC attachment or its announced `transport:rtc`).
- **Clock and AEAD seams.** Every production time read in `wire/src` goes through `Clock`; the AEAD seam is RFC 8439-identical across ring and RustCrypto, pinned by one shared golden vector asserted on both backends; nonces are counter-based off a pool-shared TX counter; `PacketCipher` is deliberately not `Clone`; the replay window rejects forward jumps past the window and the `u64::MAX` ceiling; all `Debug` impls redact key material.
- **Leaf lifecycle invariants.** Generation fencing (`GenerationLease`, revoked-once, refusals counted) is consulted on every reply, broadcast and spawned op; `CallTable` releases every registration exactly once with a four-fact owner check before removal; `fail_incarnation` retires only the predecessor; `retry.rs` holds one owner and one absolute deadline with retire-before-install; identity at rest uses a non-extractable AES-GCM wrapping key with the blob magic as AED and zeroizes on drop; announcement authority binds `node_id` to the signing entity before anything can authorize, filters freshness at read time in both clock directions, and consults the seen-set before any effect; establishment proofs are non-transferable (the transcript binds role, ordered ids and the per-handshake `h`).
- **Cancelled promotion leaves no built state** — the historical defect the branch claims to have repaired is genuinely repaired in its current shape (`Cancellable` polls cancellation first, `abandon_bootstrap` retires before releasing the lock, `take_leadership` re-reads `closed` after every await and publishes lock + server together or not at all).
- **FFI and ABI delta.** Every `extern "C"` signature change reached Rust, both mesh headers and the only in-repo C caller together; the new −117/−118 codes cross Go/Node/Python/TS without taxonomy collapse; `net_mesh_close_stream` frees on every contract-reachable path with no double free; the 8 104 / 64 832 size claims check out against `wire/src/protocol.rs`.
- **Witnesses that are discriminating as written:** all 11 `establishment_identity.rs` proofs (staged refusals, exact counter deltas, in-test honest controls), `wasm_anchorless.rs`, `wasm_rtc_conservation.rs` (non-vacuous precondition), `ts_abi_fixture.rs` (byte-exact), the golden-vector replays in `wasm_leaf.rs`, the `kyra_*` ordered-delivery and exactly-once probes, and the majority of `wasm_leader.rs`'s fencing and restoration witnesses — `BootstrapMarks`' Drop-observed cancellation instrument in particular is the file's strongest oracle, being a signal a late completion cannot forge.

---

# 1. High — A lost owner-initiated manifest wedges the replica permanently

`net/crates/net/browser-ts/src/store/replica.ts:614-623`

The `ready` path for a delta is a bare `return this.#drop('stale-generation')` for any `g !== #installed`. The owner allocates unsolicited, q-less generations exactly when a delta overflows the message budget (`owner.ts:309-310`: *"Too large to carry as a patch: replace the whole view"*) and `install` emits `man` with `q === null`, which the replica admits while `ready`. If that single `man` frame is lost — the failure this codebase itself records at `replica.ts:131-134` (*"the lost datagram was the MANIFEST … far too small to be one of the multi-kilobyte chunks"*) — the generation's chunks are correctly refused as orphans, but every subsequent delta carries the new `g`, takes the `stale-generation` branch and is silently counted. `tick()`'s recovery ladder runs only in `joining`/`installing`, and the generation check fires *before* the `base` gap check that would have sent a resync.

Failure mode: one lost manifest wedges the replica at the old generation permanently while `getStatus()` reports `phase: 'ready', stale: false`, and no later delta can ever recover it. The join direction has `MAX_JOIN_REASKS` and is witnessed (`hosted.test.ts:1424`); the owner-initiated direction has neither a recovery nor a witness.

### Required closure

Fire the existing `#behind`/`#resync` machinery when `message.g > #installed`, as the `gap` branch already does. Witness the owner-initiated direction with a lost-manifest probe.

---

# 2. High — A correlated `no {closed}` is discarded, so a dead handle is held forever

`net/crates/net/browser-ts/src/store/replica.ts:650-659`

Any `closed` whose `q` is not the live slot is dropped: `if (q !== undefined && q !== this.#slot) return this.#drop('stale-correlation');`. But every owner-side handle-death answer carries the provoking request's `q` (`owner.ts:362`: `this.no(peer, message.h, 'closed', requestOf(message))`), so an `alive` or `act` on a dead handle is answered `no {q: <that request's q>, …, closed}` and discarded — a 20 s keepalive's `q` is never the slot. The one shape the replica honours, the q-less expiry notice, is sent once from the sweep and skipped entirely when `replies.has(entry.peer)` is false.

`src/store/errors.ts:44-50` defines `closed` as *"terminal for the handle and for any action in flight on it"*, and the foreign-handle check at `replica.ts:355-358` already discards any `closed` naming a different handle — so a `closed` reaching `#refused` always names the current handle and is always handle news. The guard's stated justification (*"a late frame answered a dead question"*) cannot apply to it.

Failure mode: after one lost or skipped expiry notice the replica holds a dead handle forever; every `alive` gets `no {q: aliveQ, …, closed}`, every answer is counted `stale-correlation`, the view stays published as `ready`, and no rejoin ever happens. `test/store/review_repairs.test.ts:301-318` currently pins the drop — the witness locks in the side of the contradiction that leaves the wedge.

### Required closure

Treat a correlated `closed` as handle death. Keep the stale-correlation guard for the non-terminal refusal codes (`forbidden`, `capacity`, …), which the fall-through at `:672` already handles. Invert the witness at `review_repairs.test.ts:301`.

---

# 3. High — Read revocation never reaches the delta feed

`net/crates/net/browser-ts/src/store/owner.ts:281-296`

`propagate` emits a delta to every live handle with no `authorize` call anywhere in the loop, and `alive` (`owner.ts:366-369`) only refreshes `lastSeen`. Authorization is consulted at join, `aud`/`resume` (`:417-427`), `resync` (`:393-398`, with the comment *"a read the policy has since revoked must not be served because the handle is still warm"*) and per action/input — but the ongoing delta feed, which **is** the read delivered fresh, is never re-authorized. The `resync` refusal deliberately keeps the handle alive, and `alive` then renews its lease indefinitely.

Failure mode: once `authorize` starts returning false for a peer, that peer keeps receiving live projection diffs of its bound audience for as long as it sends a keepalive every 20 s. Given the module's own stated rule — permission does not carry forward across time (`owner.ts:387-392`) — this is the one read path that does.

### Required closure

Consult `permitsRead(handle.peer, handle.audience)` in `propagate` (or `alive`) and `forget` + `no {forbidden}` on refusal. `review_repairs.test.ts:242-262` covers only the resync refusal; the delta feed after revocation is unwitnessed.

---

# 4. High — A failed reply-stream open is cached as a permanent rejection

`net/crates/net/browser-ts/src/store/host.ts:302-321` (and the same shape at `join.ts:198-213`)

In `replyStream`, `pendingReplies.delete(peer)` runs only inside the success `.then` (`:305`); the promise registered at `:320` is never removed when `openStream` rejects. Every later frame for that peer takes `const inFlight = pendingReplies.get(peer); if (inFlight !== undefined) return inFlight;` and receives the same stale rejection. The `await replyStream(frame.peer)` at `:344` sits outside the reopen logic at `:353-380` — the logic that exists precisely because session-replacement churn is routine here. The stale-send reopen path *does* delete both entries; the first-open failure path has no equivalent.

Failure mode: one transient `openStream` failure during the session-replacement window permanently wedges that peer's service for the lifetime of the `hostStore`. Every future commit's frames are counted `send-failed`, the peer never receives another byte even after connectivity recovers, and only `close()` ends it. `joinStore`'s `stream()` has the identical cached-rejection shape.

### Required closure

Delete the pending entry in a `.catch`/`finally` at both sites.

---

# 5. High — `§12` gates 3/4 skip their check when the peer entry is unresolvable

`net/crates/net/src/adapter/net/mesh.rs:34120-34142`, `:39060-39065`, and five gate-1 sites at `:39508-39512`, `:39562-39565`, `:39652-39555`, `:40288-40291`, `:40296-40299`

The subscription and announcement gates run only inside `#[cfg(feature = "webrtc")] if let Some(endpoint) = Self::endpoint_of(from_node, ctx) { … if !Self::admission_gate_subscribe(...) { return; } }`, where `endpoint_of` is `ctx.peers.get(&node_id).map(|e| e.value().addr())`. When the entry is gone — the exact state `ingress_admission` documents as reachable (*"an endpoint with no installed `PeerInfo` … frames still queued behind a peer that has already been removed"*) and deliberately answers `Denied` — the `if let Some` **skips the gate entirely** and dispatch falls through to `ctx.roster.add_with_mode(...)` or to `handle_capability_announcement`'s ingest + re-flood + TOFU `peer_entity_ids` pin. Gate 5 is source-keyed and fails closed on the same state.

*Independently verified against the pinned source at `mesh.rs:34105-34150`.*

Failure mode: a Subscribe or announcement frame dispatched concurrently with the peer-map eviction — deterministic at the 30 s `PROVISIONAL_TTL` reclaim — lands past its `§12` gate: an unenrolled session obtains a roster subscription judged only by the ordinary ACL, or its announcement is ingested and flooded mesh-wide (gate 4's exact prohibition), reinstating the identity pin the eviction just cleared.

### Required closure

Refuse when the endpoint cannot be resolved instead of skipping the gate, or consult the source-keyed `admission_allows`. Apply the same to the five gate-1 forwarding sites.

---

# 6. High — The `§12` provisional-signalling witness cannot fail

`net/crates/net/tests/rtc_admission.rs:236-243`

`every_denied_action_is_refused_at_its_named_gate_and_counted` claims *"the same provisional session is refused at each named gate"*, but its signalling leg asserts `assert!(after >= before_deliver, …)` where both sides are sums of the monotone counters `admission_refused_deliver() + admission_refused_forward()` — a comparison that holds even when the Offer is acted on and nothing is refused. Legs (a) and (d) assert only `counter > before`, which cannot distinguish "refused" from "counted the refusal and did it anyway"; legs (b) and (c) call the pure gate predicate `allow_provisional_action(...)` and never drive the anchor at all.

Failure mode: the regression this witness exists for — a provisional peer's `0x0D02` Offer reaching the engine and allocating an ICE agent (the defect the R1 comment at `mesh.rs:29480-29483` describes) — leaves every assertion in the file green.

### Required closure

Assert a negative observable (no dialog row, clean fold, `ice_allocations` unchanged) alongside each counter, and drive the anchor for legs (b) and (c).

---

# 7. High — The ownership-charge refusal destroys acknowledged fragment bytes with no record and no fence

`net/crates/net/src/adapter/net/rtc/fragment.rs:1232-1234`

The new-group arm refuses a first piece when the global obligation charge is exhausted with `if !charge.try_charge() { return Err(FragmentOutcome::Refused); }` — no `AbandonedGroup` record and no abandonment fence. The adjacent capacity refusal twelve lines below does both, with the comment: *"refusing to open its group loses acknowledged bytes exactly as destroying a live group does — and pre-fix this branch produced neither a record nor a fence. The consequence was worse than the silence: once the capacity pressure eased, a TAIL of the refused group opened a fresh headless group that could never complete."* The charge refusal is that identical situation in its pre-fix shape, and it is the only path in the module that destroys acknowledged bytes with no disposition — contradicting the module's own X11 invariant (*"Every destruction produces an `AbandonedGroup` record … on every path"*).

Reachability is ordinary fan-in, not a pathological case: `MAX_OUTSTANDING_GROUPS` is 512 = `MAX_RETIRED_SESSIONS * MAX_GROUPS_PER_SESSION`, sized on the premise that live session entries are bounded by `MAX_RETIRED_SESSIONS` — but `bound_retired` evicts only *retired* markers, so live entries run to `max_peers` (256), and 65+ sessions holding their honest 8-group concurrency exhaust the charge.

*Independently verified against the pinned source at `fragment.rs:1215-1270`: the two arms are adjacent and asymmetric exactly as described.*

Failure mode: those bytes were already acknowledged and credited at the ingress, so they are permanently lost with no terminal raised — `dispose_abandoned_rtc_groups` never runs `reset_rx_stream` for that stream — and once a slot frees, the group's tail opens a fresh headless group.

### Required closure

Install the same record and fence the sibling branch installs, plus an accounting path for a terminal whose obligation slot could not be charged (today `take_terminals` releases one slot per drained record, so a record pushed without a charge would drift `OwnershipCharge::charged` downward).

---

# 8. High — A pending handshake is destroyed before message 2 validates

`net/crates/net/leaf/src/node.rs:899-904`

```rust
let pending = self.handshakes.remove(&peer).ok_or_else(...)?;
let session = pending.read_msg2(msg2)?;
```

The entry is taken at `:902` and only then validated at `:904`, so any message failing `read_msg2` destroys the in-flight handshake as a side effect of an error return. A delayed message 2 from a superseded attempt — or any garbage a relayed packet claims under a peer's 32-bit `src_id` projection — lands while a newer attempt's pending is installed: `:902` removes the **new** pending, `:904` errors, and when the real message 2 arrives `is_handshaking` is false, so the driver routes it to `accept_handshake` as a message 1 and `settle(attempt, IceTerm::Failed)` kills the live attempt.

The file already knows the correct rule: `CallTable::deliver` (`rpc.rs:212-238`) inspects before removal so a wrong-owner frame *"neither completes the call nor takes its slot"*.

Failure mode: one out-of-order or spoofed handshake packet burns a whole connect attempt.

### Required closure

Validate before removal, as `CallTable::deliver` does.

---

# 9. High — Pending handshakes have no deadline and no release on retirement

`net/crates/net/leaf/src/node.rs:883` (insert), `:902`, `:1226` (the only releases)

`tick` (`:1272-1296`) sweeps call deadlines, reassemblies and provisionals but has **no handshake sweep and no handshake deadline**. The driver's attempt retirement (`wasm.rs::settle`) revokes its own `HandshakeOwner` and calls `retire_provisional` without touching the node's `handshakes` entry; production `drop_session` runs only at node close and leadership release.

Failure mode — a pending that ends only on a message a dead peer will never send, and worse when the peer does send: after one initiator attempt whose message 2 never arrives, `is_handshaking(peer)` is stuck `true`, so every later handshake packet from that peer is steered into the message-2 branch where `handshake_may_install` is false and is refused **without clearing the node entry** — the peer can never initiate again. Since `RetryPolicy` makes the offerer of the lost session the pair's only repair owner, a peer-owned repair burns its one re-attempt per network change forever.

### Required closure

Give pending handshakes a deadline swept from `tick`, and release the entry when the owning attempt is retired.

---

# 10. High — A registered call is leaked on the request-encode error path

`net/crates/net/leaf/src/node.rs:1788-1806`

`CallTable::register` has already taken the slot when `encode_request_frame` runs at `:1789`, and the `?` on it returns without `self.calls.take(call_id)` — which only the *send*-failure path does (`:1804`). The trigger is ordinary application input: `RpcRequestPayload::validate` refuses a service name over 255 bytes or a body over 4 MiB, but only after registration.

Failure mode: the pending entry and its oneshot leak until the deadline sweep, when `tick` fails it `Timeout` and emits a CANCEL for a REQUEST that never left the leaf. A caller looping with an oversized body exhausts `MAX_IN_FLIGHT_CALLS` (256) and every subsequent call is refused `Backpressure` until the leaked deadlines expire.

### Required closure

Take the slot on both error paths; the asymmetry at `:1796` vs `:1801-1806` is the unintentional shape.

---

# 11. High — `unsubscribe` destroys the nRPC plane's membership it does not own

`net/crates/net/leaf/src/node.rs:1520-1541`

The wire `Unsubscribe` goes out unconditionally at `:1525-1534` **before** the guarded local deregistration, with no check of `reply_subscriptions` / `rpc_reply_carriers`. The doc at `:1512-1519` claims *"unsubscribing an application channel must not strand a reply carrier that happens to hash to the same id"* — but only the local `stream_kinds` registration is spared; the anchor-side membership is destroyed regardless. There is no shared refcount for one `(peer, channel)` membership across the application and RPC planes.

Failure mode: unsubscribe a name the nRPC plane rides (the reply-channel name is an ordinary application channel in this crate's own tests) and the anchor's roster entry dies while `reply_subscriptions` still reports it subscribed. `ensure_reply_subscription` then answers `Ok(false)` forever and never re-subscribes, so every later call to that service runs its handler at the anchor, strands the reply, and fails with a bare *"the call's deadline elapsed"* — no event, no counter.

### Required closure

Refcount `(peer, channel)` membership across both planes, or refuse the unsubscribe when the channel is a live reply carrier.

---

# 12. High — The reply-carrier ownership fence covers Stream owners but not Channel owners

`net/crates/net/leaf/src/node.rs:1839-1859`

The ownership fence refuses only `StreamKind::Stream` (`:1840`); `subscribe` and `publish` register `StreamKind::Channel` and are neither refused here nor checked at their own registration (`open_stream` checks `rpc_reply_carriers` at `:1616`; `subscribe`/`publish` insert blindly). Failure mode — the very conflict the doc at `:1820-1829` says registration settles *"in both directions"*: an application `subscribe`/`publish` on an id that later becomes a reply carrier is silently taken over when `:1859` runs, after which the application's own inbound payloads on its own channel are dispatched to the RPC plane and come back `Dropped { UnknownCall }` with no refusal at registration time. The symmetric half of `a_call_cannot_take_a_carrier_an_application_stream_already_owns` is simply missing for Channel registrations.

### Required closure

Extend the fence to `StreamKind::Channel` in both directions.

---

# 13. High — Peer sessions are never released when their transport dies

`net/crates/net/leaf/src/node.rs:1222-1247`

A session entry taken at `install_session:1189` is released only by `drop_session` or a later replacement — and enumerating the callers, production `drop_session` runs exactly twice, both node-wide and for the anchor only (node close, leadership release). The transport-loss path never reaches it.

Failure mode: a peer session whose DataChannel dies is never released — no `Disconnected` event is ever emitted for a peer, `has_session` stays true, `take_outbound` keeps queueing to a dead transport, and in-flight calls burn their full deadline and end `RpcError::Timeout` (*"the call's deadline elapsed"* — the local-deadline case where *"nothing happened"*) instead of the `SessionLost` the disposition table promises for *"the session carrying it goes away"*, which `CallTable::fail_peer` exists to produce and nothing reaches.

### Required closure

Wire the transport-loss path to `drop_session`, and fence it by incarnation (see M29).

---

# 14. High — The membership Ack is decoded and discarded

`net/crates/net/leaf/src/node.rs:2636-2638`

```rust
// A leaf serves no membership requests.
Decoded::Membership(_) => {}
```

Every `0x0A00` membership message — **including the Ack answering each Subscribe/Unsubscribe this leaf sends** — is decoded and dropped in a no-op arm. The correlation was designed and never wired: `subscribe` returns *"the nonce the Ack will echo"* and `Channel::subscribe_payload`'s contract is that the echoed nonce *"is how a leaf knows which subscribe was admitted or refused"*.

Failure mode: an anchor-side refusal of a Subscribe (which exists — `authorize_subscribe` binds a subscriber to its own-origin name) is indistinguishable from success. `stream_kinds`/`reply_subscriptions`/`rpc_reply_carriers` record the claim locally, `call()` proceeds, and the reply strands to a bare deadline with no event, no counter, and the one message that says "refused" landing in `{}`. Every membership outcome in the protocol is silently discarded.

### Required closure

Correlate the Ack by nonce and surface admission and refusal distinctly.

---

# 15. High — The credential's `http://localhost` exception is a URL prefix match

`net/crates/net/leaf/src/bootstrap.rs:171-175`

```rust
if !bootstrap_url.starts_with("https://") && !bootstrap_url.starts_with("http://localhost")
```

A **prefix match on the URL string**, not a host check. A credential with `bootstrap_url = http://localhost.attacker.example/rtc` — or `http://localhost@evil.example/`, where `localhost` is merely userinfo — passes `Credential::decode`, which verifies no issuer signature (*"the anchor verifies the issuer signature, the leaf's job is to present the string unmodified"*), and is used verbatim: `anchor_control_plane.rs:308-312` posts the whole bearer to `{bootstrap_url}/rtc/offer`.

Failure mode: a phished or config-supplied `net-bootstrap:` string defeats the module's own asserted rule (*"a plain-http bootstrap URL a browser could not use must be refused"*) and the entire bearer credential — PSK, invite nonce, issuer signature — travels over cleartext HTTP to an attacker-controlled host.

### Required closure

Parse the URL and match the **host** against `localhost`/`127.0.0.1`/`[::1]` with an explicit port policy. This is the same defect class as a hostname prefix check in TLS validation.

---

# 16. High — The redacting `Credential` Debug prints the invite nonce

`net/crates/net/leaf/src/bootstrap.rs:100-105`

The `Debug` impl redacts `encoded` and `psk` — its own rationale (`:94-99`) being that a bearer secret has no business in a log line — and then formats `self.invite` through `Invite`'s **derived** `Debug` (`enroll.rs:122-135`), which prints `pub nonce: [u8; 16]` in cleartext. `Credential` is also `Clone` and is cloned into `State`, multiplying copies and formatting opportunities.

Failure mode: the nonce is proof-of-invite for a single-use invite. Anyone with the log line can `build_join_request` echoing the stolen nonce and root under their own device key and redeem first; the authority's single-use replay accounting then burns the victim's only attempt. Exactly the exposure this `Debug` exists to prevent, one field below the redaction.

### Required closure

Give `Invite` a redacting `Debug` (or format only its non-secret fields). See also M20, the same problem on `OfferAccepted`.

---

# 17. High — Buffered trickle frames flush with no dialog fence and are never cleared

`net/crates/net/leaf/src/anchor_control_plane.rs:262-272`, `:296`, `:350-362`

The dialog fence at `:283` protects only the **enqueue** moment; the flush is unfenced. `pending_flush` is one shared queue per `State`, filled at `:296`, and the `on_open` `drain(..)` is the only removal in the file — `end_attempt` never clears it. The buffer is keyed to no dialog and flushed with no dialog check while every other effect is dialog-fenced.

Failure mode: `offer()` → D1, a candidate buffered while the socket is CONNECTING, `end_attempt(D1)` closes the socket mid-CONNECTING (the frame stays queued), then a successor `offer()` → D2 installs a new `on_open` over the same queue: D2's socket flushes D1's stale candidate frames — or in the overlapping-offer variant, S1's still-live `on_open` drains D2's freshly buffered frames onto the abandoned D1 socket. Either way a candidate is trickled on the wrong dialog, which is the exact ICE failure the buffer's own doc says leaves nothing naming the cause.

### Required closure

Key the buffer to its dialog (or drain-and-discard in `end_attempt`) and fence the flush.

---

# 18. High — The inbound sink drops the datagram on borrow conflict

`net/crates/net/leaf/src/wasm.rs:1596-1605`

```rust
let Ok(mut guard) = inner.try_borrow_mut() else { return; };
guard.inbox.push_back((peer, bytes));
```

On `try_borrow_mut` failure the `return` fires **before** `push_back`, so `bytes` is discarded, not deferred — while the sink's own construction comment promises a datagram arriving while the pump holds the node *"is left for the pump that is running"*, and `Inner::inbox`'s doc says this re-entrancy is real.

Failure mode: an authenticated Net packet — a Noise message 2, an ack — is silently lost with no counter and no retransmit below this layer. Per the RTO arithmetic in the same comment, one lost ack collapses the sender's window and stalls the stream.

### Required closure

Queue first, outside the borrowed cell, then deliver — the shape the comment already describes.

---

# 19. High — An inbound message 1 destroys our own pending handshake

`net/crates/net/leaf/src/wasm.rs:643-660`

The case discriminator is *"do I have a handshake in flight with this peer"* and cannot tell the peer's message 1 from its message 2 (the `Inbound::Handshake` payload carries no kind). When this leaf has its own outbound handshake pending and the peer's message 1 arrives — two pages racing `peer_handshake` in both directions — the message 1 is fed to `self.node.complete_handshake(from, packet)`, which (finding 8) removes the pending **before** validating: the transcript mismatch destroys this leaf's own handshake state and the peer's establishment is refused.

Failure mode: both sides fail with *"did not complete the Noise handshake over the direct channel inside the deadline"* while both channels are open; a later real message 2 gets *"no handshake in flight"*. The operation whose handshake the effect destroyed is not the one the packet belongs to.

### Required closure

Discriminate message 1 from message 2 at the boundary, or have `complete_handshake` inspect before removal (which finding 8 also requires).

---

# 20. High — A post-`close()` attempt registration leaves an unowned dialog and an orphan ICE agent

`net/crates/net/leaf/src/wasm.rs:2277-2305`

`offer_peer`'s registration block runs after `ensure_relayed_session(...)` (up to 5 s of awaits) and re-checks nothing: there is no `guard.admit()` here (contrast `service_peer`'s `:2567`), so if the page calls `close()`/`retire()` during the wait the continuation inserts a `PeerDialog` into a closed node, moves `ice_attempted`, and `create_offer` at `:2308` builds a **new** `RTCPeerConnection` after `close()` already ran `transport.close_all()`. `retire_attempts` has already run and the ticker has exited, so nothing ever settles this attempt.

Failure mode: the §10 partition `direct + relayed + failed + udp_blocked == attempted` drifts by one forever, and the orphan ICE agent keeps gathering — precisely the leak `ConnectGuard::Drop` exists to prevent on the bootstrap path. `peer_accept_offer`'s identical block at `:2407-2433` is exposed to the same race.

### Required closure

`admit()` at both registration sites and re-check `closed` after every await before registering.

---

# 21. High — `mark_remote_ready` strands the answerer's deferred candidates

`net/crates/net/leaf/src/wasm.rs:1142-1150`

The flag flips but `dialog.deferred` is **not drained** — `promote_deferred`'s drain runs only in the `SignalKind::Answer if role == PeerRole::Offerer` arm, which an answerer never executes. Candidates captured while `remote_ready` is false go to `hold.push(payload)` and are parked by `defer_candidates`, and `deferred` is write-only from then on.

Failure mode: the window is `peer_accept_offer`'s registration (`remote_ready: false`, with the offerer's early candidates seeded into `inbox: early`) to `mark_remote_ready(attempt)` — across `transport.accept_offer(...)`'s three `JsFuture` suspensions, during which any concurrent `peer_candidate` drains that inbox and holds it. Those early candidates are exactly the host addresses `PendingOffer::early` was built to preserve (*"the first candidates — the host ones a same-network pair actually connects on"*); stranding them reproduces the bug that mechanism fixed: the answerer applies no remote candidates from the pre-answer window and ICE has nothing to pair with.

### Required closure

Drain `deferred` in `mark_remote_ready`, or route the answerer through `promote_deferred`.

---

# 22. High — The Reject handler tears down a Direct-terminal attempt

`net/crates/net/leaf/src/wasm.rs:2709-2724`

```rust
if guard.live(attempt) {
    guard.settle(attempt, IceTerm::Failed, "failed");
    guard.peers.remove(&peer);
}
```

`live()` compares the dialog but not `terminal`, and `peers.remove` runs even when `settle` **refused**. A Reject drained in the same batch as work that already settled the attempt therefore tears down a successful one: `service_peer`'s send loop runs `guard.pump()`, whose verified-admission promotion can settle this very attempt synchronously there. The kind loop then reaches the Reject with `terminal == Some(Direct)`: the settle is refused (the counter partition survives) but the Direct-terminal record is removed anyway and the call returns *"rejected the attempt"*.

Failure mode: the page is told its attempt was rejected while the direct session is installed and open; a parked `wait_for_direct_session` then reports `Retired` and `run_handshake` says *"no live attempt"* for a handshake that succeeded.

### Required closure

Test `active` (not `live`) and do not remove on a refused settle.

---

# 23. High — `settle_peer_deadline` publishes `iceTimeout` over an attempt that installed

`net/crates/net/leaf/src/wasm.rs:3392-3406`

The function returns `state` **unconditionally**, including when its `settle` was refused. The STUN probe is a network round trip; during it the ticker's `pump` can promote a verified admission and settle the attempt `Direct`. The fenced `settle` correctly refuses the second term, but `service_peer` still builds the reading from the stale pre-probe facts, and `require_live` is terminal-blind, so the publication passes.

Failure mode: a Direct-installed attempt is published `state: iceTimeout|udpBlocked, direct:false`. `re_attempt_once` reads `"iceTimeout" => return "iceTimeout"` and aborts a repair that actually connected, and a page's drive loop classifies a working pair as UDP-blocked — corrupting the §10 partition's meaning.

### Required closure

Re-read `installed`/`direct` after the probe, and make the reading reflect whether the settle applied.

---

# 24. High — `RxStream::promote`'s boundary handoff is span-blind and stalls a reliable stream

`net/crates/net/leaf/src/stream.rs:328-348`

The concession treats every record as one sequence, but a record's `span` (*"a reassembled group consumes the sequences its fragments arrived on"*) can be > 1. A held record with key < `boundary` whose span **straddles** the boundary is released whole by `split_off(&boundary)` while `next_expected` lands inside its coverage: `drain` can never fire for it, the covered tail is no longer protected by `already_owned`, and every later arrival is either mis-owned or held behind a gap the sender already filled.

Failure mode: the reliable stream stalls at the boundary and terminally fails `ReorderOverflow` on payloads it already delivered; a retransmitted fragment in the released tail passes `already_owned` and can be delivered twice. `conceded.saturating_sub(out.len())` separately over-counts `FireAndForgetGap` by `sum(span-1)`. The file's own unit tests cover every path except this one.

### Required closure

Make the boundary handoff span-aware (`accept` already is) and split straddling records.

---

# 25. High — Two opens of one identity alias a single node stream

`net/crates/net/leaf/src/stream_ownership.rs:221-225` with `resolve_identity` at `:53-63`

The identity is `(wire_id, peer, incarnation)`. Two opens that share all three — the same label opened twice to one peer through the proxy, which `answer_stream_request`'s `StreamOpen` does not refuse — get two distinct map handles but **one** identity: the open reply echoes the same triple twice and `LeafStream::on_message`'s filter is byte-identical for both wrappers, so both consumers receive every payload of the one wire stream.

The close side then breaks the sibling: `answer_stream_request`'s `StreamClose` reaches `node.rs::close_stream`, whose `stream_kinds.remove` and `rx_closed.insert` are keyed on the **shared** `(peer, stream_id)` — closing one open silently kills the survivor's receive half (arrivals counted `StreamClosed`) while its `stream_send` keeps working.

Failure mode: a half-dead stream with no error. The module's stated composition covers *"one label opened to two peers"*; the same-peer duplicate is an aliasing case the rule does not refuse.

### Required closure

Refuse a second open of a live `(peer, incarnation, wire_id)`, or make the two opens share one ownership entry with a refcount.

---

# 26. High — The signal dialog is carried as `f64`, corrupting almost every minted dialog

`net/crates/net/leaf/src/leader_session.rs:1973-1974`, `:2413`; `net/crates/net/leaf/src/wasm.rs:2131`, `:2152`, `:2254-2257`

`DialogId` is `u64` (`control_plane.rs:40`) and the production mint is a **full-range random u64** (`wasm.rs:2254-2257`: `let dialog = u64::from_le_bytes(random32().map_err(js)?[..8].try_into()…)`). Yet the `signal` surface takes its dialog as an `f64` — `dialog: f64` at `wasm.rs:2131` and `leader_session.rs:2413` — and the proxied path deliberately degrades an exact `u64` to one, with the lint that would have named it suppressed:

```rust
#[allow(clippy::cast_precision_loss)]
(dialog as f64),
```

(`leader_session.rs:1973-1974`.) An `f64` is exact only to 2^53, so the round trip `dialog as f64 as u64` (`wasm.rs:2152`) rounds for the great majority of minted dialogs — the reviewing agent measured **~511/512**.

*Independently verified against the pinned source; the cast, the `f64` parameters and the random-u64 mint were all read directly.*

Failure mode: `sign_signal` signs an envelope whose `dialog` names no attempt. The receiver files it against nothing (`signal_unknown_dialog`), and the attempt never receives its offer, answer or candidate and dies at the ICE deadline. Because every dialog-fenced effect compares `dialog == attempt.dialog` at the effect site, the fence rejects almost everything it is handed.

This file has already been repaired for this exact class one field over — `wasm.rs:4170-4173`: *"`channelHash` used to be read with `as_f64()` and cast. Two silent failures came out of that: the **string** the published TypeScript type asked callers for read as `None` and became hash 0 — a different channel — and `70_000` saturated to `65_535`."*

### Required closure

Carry the dialog as `u64` end to end, or as a hex/decimal **string** across the JS boundary — the discipline the store already applies to its `Decimal`/`Hex` wire spellings and the one `channelHash` was moved to. Drop the `cast_precision_loss` allow.

---

# 27. High — `stand_down` never re-attaches, and the tab becomes a permanent zombie

`net/crates/net/leaf/src/leader_session.rs:1652-1657` versus `:1048`, `:1063`

`stand_down` retires the server and then does `state.role = Role::Follower; state.subscribed.clear(); state.announced.clear(); state.lock = None;` — but, unlike `fall_back_to_follower`, which calls `attach_as_follower(shared)?` and re-queues `await_promotion`, it creates no `ProxyClient` and queues no lock waiter. `attach_as_follower` is the only place `shared.client` is ever set, and it has exactly two call sites — neither of them here.

Failure mode: exactly the scenario `guard_lease` exists for — *a tab frozen while holding its lock, whose successor bumped the IndexedDB counter*. After standing down, the tab reports `role() == "follower"` while every `Lifecycle::request` takes the client-less arm and fails `NotLeader` **forever**; `dispatch` piles every inbound proxy message into `shared.queued` (replayed only by `take_leadership`, which this tab will never run); and no `await_promotion` waiter exists to win the lock when the successor closes. A zombie tab until the page is reloaded, with no event telling the application anything is wrong.

### Required closure

`stand_down` must perform the same recovery `fall_back_to_follower` does: `attach_as_follower(shared)` and re-queue `await_promotion`. Factor the two paths together so the recovery cannot drift again.

---

# 28. High — The per-source-order witness never asserts order

`net/crates/net/tests/rtc_loopback.rs:164-170`

`rtc_ingress_preserves_per_source_order_through_one_owner`'s doc claims *"Ordering is asserted per input … the sequence numbers a single RTC stream delivers must be contiguous and ascending"*, but the only delivery oracle is `assert!(seen >= N, …)` where `seen` is a **total event count summed across polls**.

Failure mode: the single-owner ingress reordering or duplicating one stream's events (two dispatch owners, an unordered hop between `send_to_peer_node` and the shard queue) leaves the counts unchanged and the test green — and duplicate delivery also satisfies `>= N`. The named ordering property for `PeerAddr::Rtc` ingress is unwitnessed while the test counts toward the `rtc_loopback=5` floor.

### Required closure

Assert contiguity and ascending sequence per stream, which is what the doc already describes.

---

# Medium — robustness, lifecycle and contract drift

Each entry: **title** — `where`. Evidence → failure mode → closure.

**Store**

- **29. An equal in-flight audience transition resolves at t0** — `browser-ts/src/store/join.ts:507-516`. `setAudience` returning no request neither registers a `readyWaiter` nor a correlation, and the `#waiters` counter it bumps has no consumer anywhere in `src/` (`cancelWaiter` is test-only) → two rapid `setAudience` calls leave the second resolving over the already-cleared `empty()` view while the install is still on the wire, so `await setAudience(...); getState()` returns the empty world. Contradicts `BROWSER_GAME_STORE_API_DESIGN.md:375` (*"an equal pending request awaits that transition"*). → Await the in-flight transition.
- **30. The audience correlation settles at the request deadline, not on installation** — `join.ts:514-523`. Nothing resolves the `aud`/`resume` correlation on success (`onEvent` settles only `res`/`ok`/`no`; the answering `man` matches none), so it survives until `stopDeadlines` rejects it at 10 s → any transition still installing at 10 s gets `setAudience` rejected `indeterminate` although it later installs, so the caller reports failure for a success; and every call holds one of 64 `MAX_OUTSTANDING` slots for the full 10 s. → Settle on installation.
- **31. `host.setState` inside a handler bypasses the open transaction** — `browser-ts/src/store/core.ts:240-243`. `applySnapshot` commits unconditionally, with no check for the open `#transaction` that `applyOwnerUpdate` honours → a handler that calls `host.setState` and then throws publishes beside the transaction while `transact` reports `action-rejected` and discards its staged writes, bypassing *"either commits one revision or discards everything"*; on the non-throwing path the later commit silently reverts the nested write after replicas were told. Contradicts `BROWSER_GAME_STORE_API_DESIGN.md:419-420`. → Route it through the open transaction.
- **32. The admissible-value scan skips snapshots** — `browser-ts/src/store/chunker.ts:103-112`. `inadmissibleValue` covers only `act`/`in`/`res`/`delta` payloads, never the snapshot document → a projection containing `NaN`/`Infinity` ships as `null` and an `undefined`-valued own key is silently dropped, and the delta-overflow fallback is exactly this path. Owner and replica hold different values with no error anywhere, and a no-op commit leaves them diverged because `Object.is(NaN, NaN)` suppresses the diff. → Run the scan over the snapshot before chunking.
- **33. An unencodable action input rejects with `RangeError`, not `StoreError`** — `join.ts:473-482`. `encodeMessage` runs outside any try/catch, so `{n: NaN}` rejects `act()` with an error carrying no `code` → every `error.code === …` branch in application code misses it. Contradicts `BROWSER_GAME_STORE_API_DESIGN.md:374` (*"Invalid data still throws `StoreError`"*). → Wrap and throw `StoreError('invalid-data', …, { cause })`.
- **34. Design doc §2/§4 store contracts drift from the shipped handle APIs** — `BROWSER_GAME_STORE_API_DESIGN.md:315-316, 372, 385, 96-104`. The documented `host.setState(state => ({...}))` shallow-merge exists only as `StoreCore.applyOwnerUpdate`, which no shipped host path exposes (`HostedStoreHandle.setState` full-replaces); `OperationOptions`, `StoreLimits`, `HostedStore` and `JoinedStore` have no implementor anywhere in `src/`, so `timeoutMs`/`signal`/`limits` are unimplementable — while `src/store/types.ts:8-13` claims these were *"moved into the package so they are compiled rather than merely written down"*. → Implement the declared surface or rewrite §2/§4 and `types.ts` to the shipped shape.
- **35. `assertChunkingFits` does not enforce the invariant its own comment promises** — `browser-ts/src/store/chunker.ts:63-83`. The guard's stated purpose is *"Refuse to start rather than discover the numbers later"*, but it enforces only `MIN_CHUNK_BYTES` (1024) and not `chunkBytes × MAX_SNAPSHOT_CHUNKS ≥ MAX_SNAPSHOT_BYTES` (which needs ≥ 4113 B/chunk) → a transport reporting `maxEventBytes` in [1557, 5676] starts the store and then fails on its first large projection. Not reachable with today's transport (`maxEventBytes ≈ 8108` → 5937 B/chunk → 1.51 MiB capacity) but `OwnerDeps.maxEventBytes` is a runtime seam. → Add the product check to the startup guard.
- **36. `propagate` silently skips a handle whose projection returns null** — `browser-ts/src/store/owner.ts` (`before === null || after === null` → `continue`). Every sibling path refuses typed (`join` → `forget` + `capacity`; oversized delta → whole-view replacement); only this one says nothing → the replica stays at the old revision reporting `ready, stale: false` until the next change to that audience triggers `#resync('gap')`. → Emit a replacement or a typed refusal.

**Wire / RTC**

- **37. The reassembly byte budget caps honest peers at one near-full fragment group** — `src/adapter/net/rtc/fragment.rs:1235-1237`. `MAX_PROVISIONAL_STREAM_BYTES` (64 KiB) is scoped to *provisional* sessions in its own doc but applied to every session, while `MAX_FRAGMENTED_EVENT_SIZE` is ~64.8 KiB → any second concurrent group above ~672 B of slack is refused and `reset_rx_stream` kills an honest peer's stream because two legitimate messages overlapped, despite `MAX_GROUPS_PER_SESSION` documenting 8 as reachable by an honest peer. → Split the budget by admission class, or size it to `MAX_GROUPS_PER_SESSION × MAX_FRAGMENTED_EVENT_SIZE`.
- **38. A duplicate Offer/Answer corrupts the `ice_*` ledger and strands dialog state** — `src/adapter/net/rtc/engine.rs:165-174` and `:196-206`; `signal.rs:270-277`. `SignalBudget::admit` answers `OpenDialog` for a second Offer on a live dialog id and `DialogTable::insert` is a plain `HashMap::insert` that silently replaces the live row; `note_ice_attempted` moves the denominator again. A mirror-case duplicate `Answer` is terminal in the engine, removing the row and counting `ice_failed` for a dialog whose channel is opening. *(Found independently by two reviewers.)* → one retransmitting or hostile peer inflates `ice_attempted`, permanently strands `ice_pending()` above zero (breaking the documented *"`ice_pending() == 0` after quiesce"* every conformance surface relies on), leaks one str0m session per duplicate, and can kill an in-flight upgrade with one stray Answer. → Make duplicate Offer/Answer idempotent; see also finding 26's dialog-id handling.
- **39. `MeshNode::on_batch` fails on an RTC-first peer map** — `src/adapter/net/mesh.rs:48807-48812`. The publish destination is `self.peers.iter().next().…addr().udp()`; after the `PeerAddr` refactor `.udp()` is `None` for both new shapes and DashMap iteration order is unspecified → event-bus publishes flakically fail with *"no peers connected"* on any node holding an RTC peer even when other UDP peers are connected, and can never deliver to an RTC peer at all. → `find_map(|e| e.value().addr().udp())` or route through `PeerSink`.
- **40. `has_username` treats a malformed attribute walk as uncredentialed** — `src/adapter/net/rtc/stun.rs:55-63`. The doc promises *"an unparsable one is treated as credentialed (the conservative answer)"*; the walk `break`s and `return false`, and both callers treat `false` as *"unsolicited gathering request"* and answer it **blind** → the ICE-check-vs-gathering discriminator is evadable by corrupting one attribute length field, and the documented conservative default is inverted. → Return `true` on a walk that did not complete cleanly.

**Anchor / bootstrap**

- **41. A stale trickle socket's close retires its successor's attempt** — `sdk/src/rtc_bootstrap.rs:1408-1415`. `Attempts::authorize` does not consume the token, so a reconnecting browser is re-upgraded with the same token while its attempt is live; but `trickle_socket`'s exit retires on whichever socket closes **first**, and `end_bootstrap_dialog` unconditionally `release_signal_budget`s even when its `table.remove` found nothing → a WebSocket drop + reconnect mid-ICE is then killed by the old socket's delayed TCP close. → Consume the token at upgrade, or end the attempt only for the current socket generation.
- **42. ACME publication can destroy the working pair; the claimed self-healing is absent** — `sdk/src/rtc_bootstrap_acme.rs:188-197`. `write_cache`'s doc claims *"the next order replaces"* a mismatched pair, but `read_cache` never checks the key matches the leaf, and `write_cache` renames the cert first and the key second, deleting `key_tmp` on the key-rename error arm while the old cert is already overwritten → an I/O failure or crash between the two renames bricks every subsequent start until an operator deletes the cache directory. → Verify the pair matches in `read_cache`, and publish key-first or atomically.
- **43. A refused enrollment REQUEST still consumes one of its four charges** — `src/adapter/net/mesh.rs:37893-37902`. `charge_enroll_request().and_then(reserve_enrollment)` composes two fallible steps, so when the reservation refuses the REQUEST count is already incremented and never rolled back, contradicting the invariant documented on the charge (*"R3-A … a refused REQUEST does not consume one of the four"*). → Reserve first, or roll back the charge.
- **44. `§12`'s allow-list has no arm for identity-proof** — `src/adapter/net/rtc/admission.rs:84-91` with `mesh.rs:29504-29508`. The contract says *"everything else is denied before effects"*, yet `BootstrapAction` models no identity-proof frame and the `SUBPROTOCOL_IDENTITY_PROOF` dispatch arm runs ungated: a provisional session's `ChallengeRequest` allocates state and mints a signed `Challenge`, and a `Proof` installs `peer_entity_ids[from_node]` — the pinned identity the invariant says cannot exist — which then **wins** over the session-bound origin in `provisional_reply_origin`. → Add the arm to the allow-list, or gate the dispatch.
- **45. The token-holder witness exercises neither trickling nor abandoning** — `sdk/tests/rtc_bootstrap_listener.rs:927-936`. After `post_offer` it only compares request statuses in a `oneshot` harness its own comment says cannot complete an upgrade → no candidate is ever trickled, no socket ever closes, and no retirement is asserted, while the green test advertises exactly those. The abandonment path is the lifecycle code most exposed to finding 41. → Drive a real upgrade, then close and assert retirement.
- **46. The bootstrap budget probe cannot distinguish budgeting from total refusal** — `sdk/tests/rtc_bootstrap_listener.rs:910-918`. `assert!(bootstrap_ok < MAX_FRAMES_PER_WINDOW, …)` is upper-bound-only and passes when `bootstrap_ok == 0`; the companion positive control covers only the native path → an over-refusing or unwired `admit_signal_frame` on the HTTP/WebSocket ingress keeps the R2 witness green while legitimate trickling is dead. → Assert admission below the cap too.

**Leaf lifecycle and robustness**

- **47. `end_attempt` on a replaced dialog no-ops while the predecessor socket stays open** — `leaf/src/anchor_control_plane.rs:350-362`. Idempotency for the *current* dialog is right, but the stale-dialog branch conflates already-gone with replaced → a superseded dialog's trickle socket stays open and keeps pushing `ControlEvent`s while the anchor-side attempt is never handed back. → Close the socket `end_attempt` names.
- **48. `OfferAccepted`'s derived `Debug` prints the attempt token** — `leaf/src/bootstrap.rs:286-290`. The token is the trickle socket's WebSocket subprotocol, and anyone holding it can inject `type:"candidate"` frames into the leaf's ICE → the first `map_err`/debug-log/unwrap message that formats this struct hands a per-dialog bearer to a log line anyone with the page can read. Stage 4b redacted `OfferRequest`/`OfferResponse` for exactly this reason. → Give it a redacting `Debug`.
- **49. Dialog id 0 is accepted but used as the no-attempt sentinel** — `leaf/src/bootstrap.rs:309-315`. `DialogId = u64` reserves no value and this parser accepts `dialog: 0`, but the driver uses 0 as its nothing-to-hand-back sentinel (`if dialog != 0 { end_attempt(dialog) }`, in two places) → a listener numbering its first dialog 0 gets `end_attempt` silently skipped on every cancel, abandon and close: the trickle socket stays open and the anchor holds the attempt to its own deadline. → Reserve 0, or make the sentinel an `Option`.
- **50. `Credential`'s `psk`, `encoded` and invite nonce are never zeroized** — `leaf/src/bootstrap.rs:84-85`. Three live secrets with no `Drop`/`Zeroize`, while the crate takes `zeroize` as a direct dependency and does so for `IdentitySecrets` two modules away → on drop, the domain PSK and bearer sit in freed wasm linear memory (browsers never return it to the OS) for the page's remaining life. → Zeroize on drop.
- **51. `accept_answer` is applied before `require_active`** — `leaf/src/wasm.rs:2684-2700`. `rtc.rs`'s `accept_answer` resolves `self.peers.get(&peer)` **by peer alone** and installs the SDP; the dialog comparison happens afterwards, against the module's own claim that the dialog is *"compared before any mutation"* → with two Answer envelopes and a supersession interleaved at that await, the second application targets the successor's fresh `RTCPeerConnection`, corrupting its negotiation with the predecessor dialog's SDP. → Fence before the effect.
- **52. The pending offer is consumed before the fallible ICE-server check** — `leaf/src/wasm.rs:2365-2386`. `take_pending_offer` removes the verified offer and its `early` candidates before `ice_servers_for` can refuse → a retry gets *"no verified offer … is waiting"* and the host candidates are dropped. → Run the fallible reads before the take.
- **53. `PeerLink` drops live handler Closures without detaching them** — `leaf/src/rtc.rs:240-245`. `set_onmessage`/`set_onicecandidate`/… are never cleared and the `Closure`s drop on every link replacement and close → any event task already queued dispatches into a dropped Closure: wasm-bindgen throws *"closure invoked recursively or after being dropped"* as an uncaught handler error on every replacement, and the payload the handler carried is lost with it. → Detach the four handlers before the fields drop.
- **54. `on_message`'s `filter.forget()` leaks every registration** — `leaf/src/wasm.rs:3576-3586`. `forget()` retains the closure and its captured consumer callback forever; `close`/`remove_listener` drop only the `js_sys::Function` → a page following the documented open/on_message/close lifecycle accumulates one leaked closure plus one retained consumer per stream generation for the node's whole life, while `listener_count` reports flat. → Retire the Closure once no snapshot references it.
- **55. The drained inbox is dropped on error, and a lost Answer strands the attempt** — `leaf/src/wasm.rs:2590-2642`. The capture `drain(..)`s the dialog's queues and every subsequent `?` drops what was not yet processed (a send-queue-full refusal is a documented, expected error under load) → if the batch contained the peer's Answer it is gone: `file_signal` consumed the verified envelope exactly once and nothing re-files it, so the attempt has no remote description and silently dies at its deadline even though the answer arrived and was verified. → Push the remainders back on error, or drain only as each item commits.
- **56. `StreamOwnership::with` holds the map borrow across the backend action** — `leaf/src/stream_ownership.rs:251-253`. The `RefCell` borrow is live for the whole of `action`, and `answer_stream_request`'s send arm passes `backend.send(...)` — which runs `dispatch_events` → synchronous JS listeners that are documented to call straight back in → any re-entry into `StreamOwnership` traps with *"already borrowed"*, killing the request mid-flight. → Release the borrow before the action.
- **57. `drop_session` is keyed by peer alone, not by incarnation** — `leaf/src/node.rs:1222-1226`. Every other teardown in the file is incarnation-fenced (`check_handle`, `fail_incarnation`); this one removes whichever session is current → wired per the crate's own test pattern (*"the DataChannel closed"*), a predecessor channel's late `close` removes the **successor**'s entry and fails the successor's in-flight calls. → Fence by incarnation.
- **58. A `Dropped` event names `UnknownSubprotocol` for `Unparsable` drops** — `leaf/src/node.rs:2544-2551`. `dispatch_event` returns `None` for both an unknown subprotocol and a payload that fails decode, and the `else` arm labels both `UnknownSubprotocol` → the counter and the event name different drops, so a field report cannot be reconciled with a snapshot and mixed-version degradation is indistinguishable from corruption. → Label the arm from the classification that was made.
- **59. The overflow break discards acknowledged records uncounted** — `leaf/src/node.rs:2319-2322`. The `break` leaves not-yet-iterated records — assembled from this packet's events and already acknowledged — to drop with no counter, while the delivery loop counts every suppressed payload → a batched packet whose later record trips the reorder bound loses acknowledged payloads uncounted, and §10 drop totals under-report exactly the losses the ledger exists to name. → Count them as the sibling loop does.
- **60. The seen-set key is documented as `(from, dialog, kind)` but is `(from, dialog, kind, digest)`** — `leaf/src/control_plane.rs:131-132` (and two stale copies in `signal.rs:16-17`, `:201`). The documented key made the second legitimate ICE candidate of a dialog a replay of the first (the module's own note on the fix) → the follow-on serverless/native receiver — the audience this boundary doc exists for — implements the documented key and reintroduces that defect. → Correct the three doc sites to the implemented key.

---

# Medium — witnesses that cannot discriminate their claimed outcome

Eleven of these are load-bearing; each names the bug that currently passes.

**Store and leaf witnesses**

- **61. The stale-servicing oracle reads per-call counts** — `leaf/src/wasm_witnesses.rs:578-606`. `AttemptReading.sent` is rebuilt per `service_peer` call, so the assertion compares two independent calls' trickle output, and nothing is pending at either read (`0 == 0`) → `attempt_for` mutating the live replacement and *then* returning its stale-dialog error passes. → Compare cumulative state (and false-alarms if a host candidate lands between the reads).
- **62. The routed-retirement witness performs the retirement itself** — `leaf/src/wasm_witnesses.rs:1157-1162`. The test hand-invokes `revoke_routed_handshake` + `clear_peer_relay` instead of driving the production failure path → a regression to the historical clear-only behaviour passes because the revocation was performed on its behalf. → Drive the 5 s-wait failure path.
- **63. "Installs nothing" is read off `provisional-none`, the state an install also leaves** — `leaf/src/wasm_witnesses.rs:1325-1337`. Promotion consumes the provisional keys, so `provisional_attempt(..).is_none()` is equally true after a successful install → a queued proof installing a session without charging a term or clearing the relay passes; the sibling W11 knows to assert `!has_session`. → Assert `!has_session` and a drained inbox.
- **64. The stale-generation injection has no positive control** — `leaf/tests/wasm_leader.rs:1344-1369`. A post-hoc zero across a timing window, with an injected sender the successor has never registered and nothing injecting the *current* generation to prove the path serves when it should → `ProxyServer::on_message` dropping unknown senders (or `from_json` rejecting the body) before the generation check passes. → Add the current-generation positive control.
- **65. The promotion-close test's assertions are satisfied by the sibling's cancellation mechanism** — `leaf/tests/wasm_leader.rs:1962-1987`. The `Delay::Park` schedule is the one the sibling at `:2030` witnesses as pure cancellation, so *"performed nothing"* is satisfied by "no backend was ever built" and there is no `BootstrapMarks` here to show the factory future resumed → deleting the post-await `closed` check while keeping `close()`'s cancellation leaves every assertion green and reintroduces a lock-holding leader that refuses every operation. → Use `observed_factory`'s entered/resumed/abandoned triple.
- **66. The stale-handle refusal check is structurally dead** — `leaf/tests/wasm_leader.rs:2524-2536`. `Reflect::get(&JsValue::from_str(""), …)` reads `message` off a primitive empty string — always `undefined`, because `refused` was already moved into a `JsValue` on the preceding line — and the second stage accepts the resulting `""` → any failure at any stage satisfies "the refusal must be the typed stale-generation one". → Inspect the real error object.
- **67. The fragment-group probe binds one of three co-varying dimensions** — `leaf/tests/kyra_followup.rs:113-162`. The two `PieceMeta` literals vary `stream_id`, `origin_hash` **and** `channel_hash` at once, and the only oracle is `is_none()` → a group key binding **any single one** of the three sees a mismatch and stays green while the other two are unbound: same-stream pieces from a different `origin_hash` complete each other (provenance forgery through reassembly) with this probe green. The parent-added header overclaims the opposite. → Three probes varying one dimension each.
- **68. The `|| StreamFailed` oracle accepts the loss the probe's name forbids** — `leaf/tests/kyra_round3_review.rs:129-153`. Both reliable fragments are delivered and the only admissible outcome is delivery, yet the oracle is `payload == body || matches!(StreamFailed)` → an implementation whose reassembly terminal-fails a complete group whenever FAF tail traffic interleaves goes green without ever delivering. (`promotion_boundary`'s `delivers or fails typed` is a *different*, legitimate disjunction — a fragment is genuinely missing there.) → Drop the `|| StreamFailed` branch.
- **69. The frozen-tab probe prints its KEEPS verdict without ever verifying the freeze** — `leaf/tests/leader_e2e/frozen_tab_probe.mjs:138-184`. The only actuator is fire-and-forget CDP `Page.setWebLifecycleState` with no read-back, and the file's own comment concedes *"the probe must actually freeze to mean anything"* → on an engine where the freeze is refused, the page runs unfrozen and the probe manufactures the exact D2-correction evidence (*"the lock alone cannot fence it"*) from an instrument that never acted. → Verify a tick counter actually stopped.
- **70. The frozen-tab fence row is an echo, not an observation** — `leaf/tests/leader_e2e/frozen_tab_probe.mjs:169-180`. `resumedLeaderBelievesGeneration` copies the probe's own pre-freeze reading and `resumedTabIsStale` compares 1 against the increment of 1 → true by construction; Chromium may discard a frozen page's JS state and the probe still reports a stale-generation fence that did not occur. → Read the resumed page's state.

**Structural tripwires (name deny-lists, not type-level enforcement)**

- **71. Rule 1's scan misses fully-qualified paths and unlisted type names** — `leaf/tests/control_plane_boundary.rs:101-186`. `session: crate::session::LeafSession` contains no `use crate::session` and `LeafSession` is in neither trait list → an anchor-owned session type becomes a required trait parameter with every scan green, while the assert message claims *"no anchor type crosses it"*.
- **72. The ZST `NoAnchor` witness cannot fail on parameter types** — `leaf/tests/control_plane_boundary.rs:405-433`. A zero-sized impl satisfies `async fn offer(&self, offer: Sdp, anchor: AnyTypeAtAll)` by ignoring the parameter → the compile-time half of rule 1 does not backstop the blacklists, though the module doc claims it would.
- **73. The data-path deny-list misses `LeafNode::publish`/`call` forwarding** — `leaf/tests/control_plane_boundary.rs:245-280`. `LeafNode`, `publish`, `call`, `subscribe` are all absent from the list and nothing forbids the impl modules from importing `crate::node` → `mock_control_plane` or `anchor_control_plane` growing a `LeafNode` and forwarding application payloads is *"a relay wearing a trait"* — the exact sentence the test exists for — with all scans green.
- **74. The bindgen transport scan is name-based and `wasm.rs`-local** — `leaf/tests/control_plane_boundary.rs:195-243`. HTTP moving to a helper module via `gloo_net::http` matches none of the 12 spellings here nor `dependency_boundary`'s 3 → the file's own stated failure mode (*"One inlined HTTP call is how a boundary stops being one"*) recurs uncaught; and the positive half is call-string **presence**, satisfied by a dead call site.
- **75. The manifest deny-list misses renamed dependencies** — `leaf/tests/dependency_boundary.rs:89-112`. `rt = { package = "tokio", … }` contains no forbidden substring → the runtime the test says must never reach a browser tab enters the wasm build under an alias with all five tests green.
- **76. The conflict-refusal escape takes any `open_stream` error** — `leaf/tests/kyra_round4_parent.rs:258-289`. `if registered_rpc && admitted.is_err() { return; }` treats *any* error as the sanctioned conflict refusal → an RPC registration corrupting the stream table so every subsequent `open_stream` errors passes while admitted application traffic is silently refused. → Assert the failure stage.

**Witness-shape defects in `wasm_leader.rs` / `wasm_witnesses.rs`**

- **77. The consumer-stops claim runs over an event source that never fires** — `leaf/tests/wasm_leader.rs:2562-2566`. `TestBackend` emits no node events and nothing injects data for the successor → `delivered == 0` is true by construction. → Drive the production event sink with distinct payloads, as `two_peers_own_their_streams` does.
- **78. "Streams are not resurrected" is vacuous** — `leaf/tests/wasm_leader.rs:1254-1259`. The test opens no stream, so `!matches!(StreamOpen)` is true before and after any restoration logic. → Open one and assert it is not replayed.
- **79. "A promotion that failed must have published nothing" is true by construction** — `leaf/tests/wasm_leader.rs:2415-2419`. `Delay::Fail` returns `Err` before constructing a `TestBackend`, so `performed` cannot be non-empty, and it only sees `LeaderBackend::perform` → a half-built promotion composing an announcement before the connect fails passes. → Observe the composed work too.
- **80. Runtime-cap restoration is pinned with `any(..contains..)`** — `leaf/tests/wasm_leader.rs:2624-2628`. The claim is *"re-publish the announcement that was in force"*, which needs the **last** `Announce`; `any + contains` passes when the cap is published and then narrowed away by a later reconcile — the exact *"silently reverted at the next handoff"* defect, deferred one step. → Use `last_announcement`, as the siblings do.
- **81. "Applied once" is witnessed with a single read** — `leaf/src/wasm_witnesses.rs:781-785`. `applied` is a per-call counter → a service step that re-applies the retained lines on every later call passes; no second read checks `applied == 0`. → Read twice.
- **82. The retention-order claim is pinned at one position in one queue** — `leaf/src/wasm_witnesses.rs:1385-1415`. Only the head of the offer queue is pinned and the live-attempt queue by cardinality alone → a `deferred` that evicts the **oldest** on overflow (keeping the newest N) passes — exactly the entries this bound exists to preserve. The third claimed fate (the drop is warned) is asserted nowhere.
- **83. The parked-noise-wait refusal has no positive control on `run_handshake` success** — `leaf/src/wasm_witnesses.rs:1076-1107`. Nothing in the file shows `run_handshake` completing for a live attempt → a `run_handshake` that refuses on every outcome after its first await leaves the §9-step-4 initiator path dead while this witness stays green. → Add the positive control.
- **84. The disjunctive refusal hides the stale gate** — `leaf/tests/wasm_leader.rs:1378-1385`. `Typed(Session(_)) | Typed(NotLeader {..})` has one arm guaranteed — `suspended.close()` already ran → the claim *"a resumed session is refused rather than served"* cannot fail on the fence axis. → Drop the guaranteed arm.
- **85. The future-generation fence is asserted as any-error** — `leaf/tests/wasm_leader.rs:1046-1049`. `fence(5).await.is_err()` passes on a storage failure or an unrelated `Identity` error, while the stale-generation sibling pins the exact typed refusal. → Pin the type and fields.
- **86. The unnamed-open resolution rule is duplicated in the fixture** — `leaf/tests/wasm_leader.rs:793-808`. *Which* peer an unnamed open learns is decided by the fixture's own `unwrap_or` copying production → the real backend's resolution regressing passes while a page's unnamed stream reports `peer_node: 0`.
- **87. `fixture_parity` walks the hand-maintained `ALL` registry, never the directories** — `leaf/tests/fixture_parity.rs:61-82`. `checked == ALL.len()` counts the registry, not `src/test_vectors/` nor `../tests/cross_lang_wire/` → a vector added but unregistered is invisible while the two copies drift, which is the bug the function names. → Enumerate both trees.

**Test-harness and probe defects**

- **88. A tautological `X || !X` stands in for the far-side cleanup leg** — `net/crates/net/tests/rtc_signalling.rs:644-650`. `assert!(wait_for(|| …is_none()).await || …is_some(), "either state is acceptable here")` cannot fail, in a REQUIRED-pinned witness where the adjacent A-side leg asserts properly → *"B eventually cleans up"* has no witness at all while the roster implies the whole sequence is checked. *(Found independently by two reviewers.)*
- **89. `run_scenario.sh`'s runner-death diagnostic kills itself under its own `errexit`** — `net/crates/net/tests/natsim/run_scenario.sh:449-455`. The block's first command is `wait "$RUNNER_PID"; rc=$?` under `set -euo pipefail`, so a non-zero wait terminates the script before `rc=`, before the `echo`, before the `tail -n 60 runner.log` → the row fails naming nothing, which is precisely the outcome the block exists to prevent. → `rc=0; wait "$RUNNER_PID" || rc=$?`.
- **90. The browser matrix job carries 75 min of step caps inside a 45-min job** — `.github/workflows/ci.yml:6296-6299`. `timeout-minutes: 45` against four sequential engine legs of 20+20+20+15 plus preamble, with the floor check and the log upload both `if: ${{ !cancelled() }}` → one hung engine (and `driver.mjs`'s `page.evaluate` has no timeout) destroys the completed other-engine evidence with the VM, exactly the failure commit `9f8bde0c7` claims to have closed. → Make the job budget the sum, or upload/verify unconditionally.
- **91. Three witness-shape defects in `rtc_signalling.rs`** — `:645` (see 88), `:803` (`unknown_before` captured **after** the send it bounds, so a fast refusal turns the later `>` into a false red), `:1016-1019` (`assert_eq!(slots, 0, "ignored unknown candidates … retain reservations")` — inverted prose on a real regression). → Reorder the capture; fix the message.

**Bindings, ABI and documentation contracts**

- **92. The stale "not in-order" reliable-stream contract** — `.claude/skills/net-event-bus/streams.md:33-36` and `net/crates/net/wire/src/stream.rs:41-49`. Both teach *"the substrate does NOT reorder for you … Reliable here means 'no loss', not 'delivered in order'"*; the receive path added in this range **does** reorder (`session.rs:2099-2104`, `InOrderBuffer`), and `docs/TRANSPORT.md:134` states the live contract as FIFO. This range's own plan amendment names the wording as *"the transport as it was before that work, not as it is"* and neither site was updated → consumers hand-roll redundant reordering, and a maintainer later *"restores"* arrival-order delivery to match the shipped rustdoc, silently breaking the FIFO guarantee. → Update the skill and the rustdoc.
- **93. Re-anchored source pointers land on the wrong code** — `.claude/skills/net-event-bus/observability.md:101` and `streams.md:252` (both cite `stream.rs:194-228`, which is the `StreamError` enum; `StreamStats` is at `:255`) and `observability.md:125` (`reliability.rs:304`, which is the `StreamMode` default-method region; `untracked_evictions` is at `:586`). → Exactly the readers these docs exist for are sent to unrelated code. The same wrong-anchor class was found in these files before.
- **94. `check-ffi-exports` cannot see a second cdylib** — `.github/scripts/check-ffi-exports.py:58-62`. Its `CANDIDATES` are three filenames of one library in one directory and `--artifact` takes one path, so *"the single libnet cdylib"* is a **premise, not a check** → a second cdylib exporting `net::ffi`'s `#[no_mangle]` set is never opened and the run still prints *"✓ export set matches"*. The stated consumer premise is also wrong (Python and Node compile the `net` crate in-process and never load `libnet`), and `exports.baseline` is a `test-helpers` build whose required set includes fixtures-only symbols, so a default build always fails and a test seam leaking into shipped builds can never be detected — the inverse of `net-ffi/Cargo.toml`'s stated intent.
- **95. The plan's `rtc_addr` `iceServers` claims contradict the shipped `rtc_stun_addr` split** — `docs/internal/plans/BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md:328-330` vs `:908`, `:932-933`. §6 still says *"Browsers get the anchor's `rtc_addr` as their `iceServers` entry"* while the shipped code splits the roles (`rtc_stun_addr` beside `rtc_addr` in `CapabilityAnnouncement` and the signed transcript) and the plan's own Stage-6 record says *"the leaf reads `stun_addr`"* → an implementer wiring `iceServers` from `rtc_addr` reproduces the libwebrtc collision the plan documents at `:2668-2688`, and the tab looks UDP-blocked when it is not.
- **96. The AEAD headline presents a microbenchmark extrapolation as a main-thread cost** — `docs/internal/performance/WEBRTC_DOUBLE_AEAD.md:12-13`. The up-front answer quotes *"+0.21 ms of main-thread time per second"* (the batched hot-loop marginal extrapolated) while the once-per-frame measurement of the same workload is *"+0.64 to +1.72 ms/s"* (`:172`) and §3's own verdict says *"0.6–1.7 ms … of which only ~0.2 ms/s is the cipher itself"* → a reader sizing a frame budget from the headline under-allocates by 3–8×. The body is honest; the number most likely to be quoted is not.

---

# Low — accounting, allocation, stale docs and nits

- **97.** `base64`/`fromBase64` build their binary strings one byte at a time — `browser-ts/src/store/chunker.ts`. `binary += String.fromCharCode(byte)` over up to 1 MiB, with the JSON string, the raw bytes and all base64 pieces live at once (~7× the snapshot size transiently), against *"never avoidably allocate, copy, or compute"*. → Chunked `fromCharCode.apply`.
- **98.** `fromBase64`'s doc claims *"refusing anything non-canonical"* but only calls `atob` — `chunker.ts`. The refusal lives in the caller's `isCanonicalBase64`; a future direct caller inherits `atob`'s laxity. → Fix the doc or move the check.
- **99.** `AssemblyTable.open` reclaims the predecessor **before** the `assembly-too-large` check — `browser-ts/src/store/assembly.ts:327-337`. The refusal is destructive; in-tree callers are shielded by the `#retired` watermark, but the class is re-exported from `owner.ts` for hosts. → Check first, then replace.
- **100.** `AssemblyTable.sweep` is never called by the store — `assembly.ts:363`. The replica enforces deadlines in its own tick and clears via `reclaimHandle`; a host holding its own table must remember to call the bound the class documents. → Call it, or say so on the type.
- **101.** Unclaimed ICE checks are counted as STUN gathering traffic — `src/adapter/net/rtc/driver.rs:2007-2016`, against `stats.rs`'s *"ICE checks … are not counted here"* → the "somebody used us as their STUN server" telemetry over-counts in proportion to failed ICE attempts. → Bump only when `!has_username`.
- **102.** An uncounted drop in `submit_rtc_with_one_retry`'s no-transport branch — `src/adapter/net/rtc/router.rs:701-706`. The comment claims the drop is counted; the code returns silently → the exact R1 class (19 476 packets lost under one reported refusal) is again invisible. → Count it, or reword the comment.
- **103.** The pending-eviction mark saturates at `generation == u32::MAX` — `src/adapter/net/rtc/transport.rs:575-580`. `saturating_add(1)` then decodes to `u32::MAX - 1`, an already-spent incarnation → after 2³² recycles a deferred close names the wrong incarnation and falls back to the failure detector, exactly what H3 prevents. → A checked encoding.
- **104.** `propagate` re-projects and re-serializes the whole world per handle per commit — `browser-ts/src/store/owner.ts:295-304`. Both projections are freshly built so `shallowDiff`'s identity test can never fire → the 60 Hz path is O(handles × world) JSON work per tick. → Cache projections per distinct audience.
- **105.** Ledger entries are measured in UTF-16 code units, not bytes — `browser-ts/src/store/ledger.ts:273-276` against `LEDGER_MAX_BYTES`'s *"Bytes retained per handle"* → the 64 KiB bound under-enforces exactly for unicode-heavy inputs (by ~2× for CJK). `canonicalBytes` already shows the right idiom. → Use `utf8Length`.
- **106.** `signing_bytes`' capacity hint omits the 14-byte magic — `leaf/src/control_plane.rs:193-194` → one avoidable heap growth + memcpy per signal envelope on the wasm heap.
- **107.** `closure.forget()` leaks the probe closure while its comment claims *"both are dropped"* — `leaf/src/bootstrap.rs:549-551`. → Say leak, or scope the closure to the probe handle.
- **108.** `delegation_chain` is not released by `drop_session` — `leaf/src/node.rs:1916-1929` → after a session loss the leaf reports `is_enrolled() == true` with no session, masking *"the single fact that explains a `call` dying on its deadline with the service's handler never having run"*.
- **109.** `peer_rtc_addr` is not cleared by `drop_session` — `leaf/src/node.rs:1199-1203` → a `Connected` event after a reconnect reports the previous establishment's RTC socket, so the STUN probe aims at a stale address and can mint a false `udp_blocked` term.
- **110.** Saturating handle allocation reuses `u64::MAX` and overwrites — `leaf/src/stream_ownership.rs:222-224` → the "never reused" invariant breaks at the counter's end.
- **111.** Replay is classified by an error-text substring — `leaf/src/node.rs:2063-2066`. `reason.contains("replay")` is the only discrimination between `Replay` and `Unparsable` → a wire-crate error-text refactor silently erases the operator's replay-attack signal.
- **112.** The refusal-kind discriminator is error message text — `leaf/src/wasm_witnesses.rs:592-596`, plus three exact-message pins at `leaf/tests/wasm_leader.rs:1770`, `:3157`, `:3272` → reworded correct refusals alarm; phrased-identically wrong-stage failures pass. The typed variants are right to pin; the sentences inside them are not.
- **113.** A tautological `< u128::MAX` clock assertion — `leaf/tests/wasm_leaf.rs:359-360`. Cannot fail for any clock (≈10²⁰ years); witness padding in the shape the doctrine names. The rest of the test carries the real claims. → Delete it.
- **114.** The misquoted run-1 inline cost in the AEAD threshold arithmetic — `docs/internal/performance/WEBRTC_DOUBLE_AEAD.md:221-222`. The doc's own §2.1 table reports 1.911 / 1.721 / 2.744 and the argument quotes *"1.72 / 1.72 / 2.74"*; the conclusion survives but the evidence contradicts the table it summarizes.
- **115.** `StreamConfig`'s fifth knob `close_behavior` is missing from `streams.md:36-38` (which says *"four knobs"*, citing a range that contains the fifth) → a reader misses the knob deciding whether unacked outbound packets are drained or dropped, which the same doc warns about two paragraphs later.
- **116.** A non-discriminating backpressure-absorption assertion — `go/stream_close_test.go:106-108`. `errors.Is(ErrBackpressure, ErrSessionSuperseded)` compares two distinct `errors.New` sentinels and is false by construction → a retry wrapper folding −117 into the backpressure loop stays green.
- **117.** The plan's `SignedPayloadCanonical` anchors are stale — `BROWSER_NATIVE_WEBRTC_TRANSPORT_PLAN.md:2020-2023` cite `capability.rs:2402-2463` / `:2634`; the struct is at `:2527` and the signing call at `:2816`, moved by this range's own field insertions — against the plan's own *"`path:line` … re-derived at that commit"* doctrine.
- **118.** The −117 ABI constant landed across three commits — `go/net.h:64-73` (3c9fa8bde → 70b008909 → 0aa63e69f) with no commit touching the Go ABI stability tests, against the stated one-commit rule → a bisect or revert inside those windows yields a header pair that silently drifts. (The −118 work did co-commit correctly.)
- **119.** The phantom `NET_ERR_BLOB_VTABLE_INVALID` is still asserted — `.claude/skills/net-event-bus/dataforts.md:218` (plus two release-note copies); the partial-vtable return is `NET_ERR_BLOB_BACKEND`. Carried unchanged from the previous review round and still open at this head.

---

# 120–130 — CI floors, witness pins, checkers and fixture contracts

The second pass over the evidence apparatus. Its overall shape: the machinery is strong where it uses the JUnit checker or **anchored** matching, and the gaps are all in the places that fall back to unanchored substring matching or to counts nobody re-measured.

**Verified sound within this slice** (this is why the two High findings are worth reading as the exception rather than the rule):

- Five floors — `MIN=93` (`ci.yml:241`), `MIN=24` (`:351`), `REG_MIN=62` / `STATE_MIN=41` (`:411-412`), `GATE_MIN=60` (`:637`) — **exactly equal** the current static test-fn counts of their modules. The same counting method reproduces all five, which is what validates the method used to find the stale floors at 121.
- **Zero dead pins.** An exhaustive sweep of all 613 by-name roster tokens extracted from `ci.yml` against the whole tree found every one declared.
- `check-witness-results.py` is genuinely fail-closed: missing/unparseable artifact, absent or ambiguous suite, a zero-case suite, `tests=`-vs-elements disagreement, suite-level failures, `<failure>/<error>/<skipped>/<rerunFailure>/<flakyFailure>` children, duplicate pins, wrong-suite pins and floor breach all reject; name matching is exact and exactly-once (so `<name>_extra` is a different string); `--self-test` drives every rejection path. It is not tautological — the roster is hand-pinned in `ci.yml` and the verdict comes from the run's own artifact.
- `check-roster.py` is fail-closed on missing source and cannot be given an empty roster; it is lexical by design and its docstring honestly bounds both limits rather than over-claiming.
- The Selector-target org intake step (`ci.yml:643-808`) is the file's strongest machinery: anchored `^<fqn>: test$` inventory from non-executing `-- --list`, exactly-once existence, one `-- --exact` batch run checked on status + `running N tests` + `N passed; 0 failed; 0 ignored`, all proven by an adversarial self-test.
- Every one of ~65 `cargo nextest run` invocations carries `--no-tests=fail`; the documented doctrine is 100% honored. The `integration-guard` pin scraper is non-tautological (expectation is `ls tests/*.rs` versus scraped pins).
- The RTC, browser and demo inventories use exact-name matching (`^RTCB PASS $name `, `^test $name \.\.\. ok$`) with per-engine floors equal to the pinned-name counts.

One exposure is **enumerated rather than filed** because it is deliberate and documented: the 13 multi-`--test` union groups (`ci.yml:1295-1302`, `:1309-1360`, `:1415-1430`, `:1435-1438`, `:1443-1447`, `:1492-1523`, `:1540-1555`, `:1800-1803`, `:1861-1869`, `:1889-1893`, `:1924-1927`, `:1945-1950`, `:1974-1977`) share the run-wide `--no-tests=fail` hole the file documents at `:1558-1562` (*"`--no-tests=fail` is RUN-WIDE, not per-binary … invisible as long as any sibling still runs something"*). Per-arm drift is real — e.g. `stream_config_and_error_display`, the sole gate for the wire `stream` surface, dropping out of the 51-target group green — but two binaries already got solo runs for exactly this reason.

---

## 120. High — Four floor steps pin security witnesses by unanchored substring

`.github/workflows/ci.yml:325-329` (routing-plane wiring), with identical loops at `:375`, `:474`, `:519` and `:6981`

```sh
if ! printf "%s" "$out" | grep -q "$required"; then
```

An unanchored substring hit **anywhere** in the run transcript satisfies the pin — 68 security witnesses are gated this way at `:326`, plus the routing supervisor, `org_routing_registry`, `org_routing_state` and the ACME cold-start step.

Trigger: a pinned witness renamed to a name that *contains* the old one (e.g. `steady_poison_settles_current_over_an_unserved_source` → `…_source_with_no_reader`), or any test that merely prints the old name. The count floors cannot catch it, because a rename leaves counts unchanged.

The file itself documents this exact shape as spoofable and says it was replaced — *"checked required names with a substring `grep -q`. Both were spoofable … and `<required>_extra` satisfied a substring match for `<required>`"* (`ci.yml:548-551`) — and every sibling roster already uses anchored matching (`^test $name \.\.\. ok$`, `^RTCB PASS $name `, `-- --exact`). These five loops are the survivors of that cleanup.

Failure mode: the gate named as *"the only proof that ONE global drain is minted…"* (`ci.yml:185-187`) passes while the property is unwitnessed.

### Required closure

Anchor all five loops as the sibling rosters do — `grep -q "^test $required \.\.\. ok$"` or the JUnit checker.

---

## 121. High — The wire floor is stale-low by 79 tests, and three more floors never followed their suites

`.github/workflows/ci.yml:128-137` (`MIN=196`), `:953` (deck 31), `:5945` (leaf 308), `:1619` (`--min 9`)

`MIN=196` counts every test `cargo test -p net-mesh-wire --features json` runs, but the crate now compiles **275** `#[test]`-family fns (12 batch + 25 channel/membership + 17 channel/name + 32 crypto + 28 pool + 14 protocol + 47 reliability + 10 route_codec + 18 route_hop + 59 session + 13 stream_window; every `mod tests` is plain `#[cfg(test)]`, no feature gates). The comment says *"MIN is the post-move inventory"* — it was set once in `3f22e5443` and never moved while `wire/src` changed in 23 later commits.

The same staleness, smaller: deck floor 31 vs 35 tests; leaf native floor 308 vs 324; `sensing_org_lease_wire --min 9` vs 11.

The discipline elsewhere is explicit — `6d6724fbb` is a floor-only commit touching no test files (*"ci: floors to this round's measured counts - leaf 266 -> 308, rtc_repairs 49 -> 56"*) — and nothing binds floor and witness movement together.

Failure mode: deleting up to 79 wire witnesses passes the guard whose stated purpose is *"a suite that silently shrinks … would otherwise still exit 0"* (`ci.yml:116-118`). The counting method is validated by its exact reproduction of the five floors recorded as sound above.

### Required closure

Re-measure and raise the four floors to their suites' real inventories, and require floor changes to land in the same commit as the witness changes they count.

---

**122. [Medium] The Windows org+authority union filter can lose one arm silently** — `ci.yml:5101-5104`. `test(/^adapter::net::behavior::org/) + test(/^adapter::net::org_admission_gate/)` under `--no-tests=fail` is run-wide, so an arm that stops matching is invisible while the other runs; the comment above records that `org_admission_gate` *"was silently excluded"* once already for sitting outside the `behavior::org` prefix → a rename or move of either module silently retires its Windows run while ubuntu's `--lib` step keeps Unix-path coverage green. → One invocation per arm, or a post-run per-arm count.

**123. [Medium] The Go ABI-stability `-run` filter passes on zero matches** — `ci.yml:4148-4152`. `go test … -run TestABIStabilityU64FFIRoundTrip ./...` exits 0 when the filter matches nothing (*"no tests to run"*), and the comment notes this witness *"was silently excluded from CI"* once already (`:4145-4146`) → rename, move or re-tag the test and the step goes green while the cgo u64 ABI round-trip runs in no job. → Assert its PASS line appears in the output, as `:4937-4945` does for the net-cli pair.

**124. [Medium] The two new checkers are absent from `on.push.paths`** — `ci.yml:22-31`. The rule right there is *"Editing a checker itself has to re-run it, or a loosened pattern lands green"* (`:25-26`), but only `check-one-library-docs.py` and `check-header-count.py` are listed → a commit that only edits `check-witness-results.py` or `check-roster.py` matches no path filter, so a loosened predicate (dropping the `<flakyFailure>` rejection, or the exactly-once check) merges with zero evidence. `AGENTS.md:185` meanwhile claims the checker scripts trigger the workflow. → Add both.

**125. [Medium] `--run-marker` is silently ignored under `--multi`** — `.github/scripts/check-witness-results.py:209-218`. `check_multi` never consults `args.run_marker`; only `check()` does (`:290-296`), yet one parser declares the flag and the docstring promises it (*"`--run-marker` makes the second part enforceable here too"*, `:39`) → `--multi --run-marker X` performs **no** freshness check while the caller believes the artifact is bound to this run. This is the only freshness mechanism anywhere (an mtime comparison — no hash binding), and no `ci.yml` caller passes it at all; every step instead relies on its own `rm -f` first, so one forgotten `rm -f` verifies an earlier run's artifact as current with no signal. → Enforce it in `check_multi` or reject it there; ideally bind a run id into the artifact.

**126. [Medium] `enroll_exchange.json` is a one-directional pin over the production codec** — `net/crates/net/leaf/src/enroll.rs:45-49`. The fixture claims to be *"a deliberate mirror of `InviteToken` / `JoinRequest` / `JoinOutcome` from `sdk/src/enrollment.rs`"*, but nothing asserts the mirror matches the production codec — only the leaf's own mirror encoder is checked — and the source admits it: *"the SDK-side half of the pin needs a test in `sdk/tests/`, which is outside this slice's ownership"* → a `JoinRequest` layout change (field order, length-prefix width, magic) leaves every fixture consumer green while the anchor's real `net.mesh.enroll` provider starts refusing every leaf request and sessions stay `PROVISIONAL`, with the fixture description actively misleading a reviewer into thinking this is pinned. → Land the deferred `sdk/tests/` half that round-trips the fixture bytes through the production codec.

**127. [Low] `TESTS.md` and the perf audit claim a mechanism was removed while 16 of them gate financial-property witnesses** — `TESTS.md:139-142`, `PERF_AUDIT_2026_09_13_TEST_EXECUTION.md:166-172,178-179,274-275`. Both say the per-name re-run fan-out was *"removed 2026-09-13"* and the graphs collapsed *"now to 4 (one per family)"*; ~16 `for required …; cargo nextest run -E "test(=$required)"` loops survive in `rust-sdk-tests` (`ci.yml:2554-2558` and 15 identical shapes) and **five** integration families exist (`:1211, 1363, 1450, 1812, 1991`) → an evidence-integrity audit trusting the "removed" claim misses the weaker mechanism still gating the payments/A2A witnesses, whose `test(=name)` loops cannot observe retries the way the JUnit check can.

**128. [Low] Stale test counts quoted in `ci.yml` comments** — `:2273-2277` says *"(14 tests)"* beside `--min 22`; `:2494` says *"`cargo nextest list` said 41 when it landed"* beside `>= 43`; `:619-620`'s mesh arithmetic sums to 68 *"net of 3 retired"* against `MESH_MIN=67` and 70 static attrs (on lines that themselves warn *"The arithmetic drifted from the pin"*); the browser inventory echoes 41 against a 47-name roster; the wasm step echoes 27 against 29 → each stale number is what the next editor calibrates a floor change against, which is finding 121's mechanism.

**129. [Low] `cross_lang_wire` README claims and decorative fixture fields** — `net/crates/net/tests/cross_lang_wire/README.md:8-24`. The README says the Rust consumer *"encodes AND decodes each one"* (false for `enroll_exchange.json`, which `cross_lang_wire.rs` never reads) and tabulates 7 of 11 fixtures; `leaf/src/enroll.rs:35` and `leaf/src/rpc_wire.rs:36` point at `tests/enroll_parity.rs` / `tests/nrpc_frame_parity.rs`, which do not exist. In `net_header`/`routing_header`/`nack_payload`/`stream_window` the `fields` block is decorative — tests hard-code Rust literals and compare only `hex` — so a `fields`↔`hex` disagreement drifts green forever. An exhaustive tree search also found **zero** Go/TS/Python readers of any `cross_lang_wire` fixture (`README.md:26-27` says they are *"not part of Stage 2"*, deliberate) while the directory name and `leaf/src/test_vectors.rs:13-14` imply parity the sibling `cross_lang_capability` set actually has. Two reader traps sit on top: u64s as bare JSON numbers above 2^53 beside decimal-string u64s in the same fixture, and contradictory magic-byte descriptions (`net_header.json:2` says `0x4E45 ('NE')` over hex `454e`; `routing_header.json:2` uses the opposite convention).

**130. [Low] `AGENTS.md`'s `MIN=93` coordinate is stale** — `AGENTS.md:167` says *"`MIN=93` at `ci.yml:175`"*; it now lives at `ci.yml:241`. The pointer went stale in-range (commit `3f22e5443` inserted exactly 66 lines above the step; 175 + 66 = 241). The five floor **values** quoted in the same sentence are all correct — only the one hard coordinate that sends a reader to "the source of truth" is wrong.

---

## Outside this diff

Noted for awareness, not counted above: a `SIGINT` delivered before the serve loop first polls `ctrl_c()` terminates without `Drop` and orphans the wrapped child (pre-existing in the MCP/wrap lifecycle) — the same "cleanup reachable only after the first poll of a future" shape as findings 9, 20 and 55.

## Disposition summary

| Group | Count | Disposition |
|---|---|---|
| High — convergence and access contracts (1–4) | 4 | Repair before further store build-out |
| High — security boundary and its witness (5–7, 15–16) | 5 | Patch promptly; 15–16 first |
| High — leaf lifecycle and state (8–14, 17–27) | 18 | Schedule as one lifecycle pass |
| High — witness (28) | 1 | One-line fix to an oracle |
| High — CI floors and witness pins (120–121) | 2 | Anchor the five loops; re-measure four floors |
| Medium — robustness, lifecycle, contract drift (29–60) | 32 | Schedule |
| Medium — witness non-discrimination (61–91) | 31 | Decide per item; each names the bug that currently passes |
| Medium — bindings, ABI, doc contracts (92–96) | 5 | Documentation sweep |
| Medium — CI selection, checkers, fixtures (122–126) | 5 | Schedule with the CI sweep |
| Low (97–119, 127–130) | 27 | Opportunistic |

Totals: 30 High, 73 Medium, 27 Low — 130 filed findings (numbered 1–130 with no gaps) plus one enumerated-but-unfiled exposure (the 13 union groups at 120's note).

Two structural observations, offered as the reason to treat this as follow-up work rather than a defect list. First, most High findings share one shape: **a cleanup, release or fence that is present and correct on the happy branch and absent on an edge branch** — the same shape as the lifecycle work this branch did well elsewhere (findings 8–14, 17–25). Second, the witness findings cluster where a property is *absence* (nothing installed, nothing delivered, nothing resurrected): absence is exactly what a count or a `is_none()` cannot distinguish from "never happened", and 20 of the 31 Medium witness rows are that shape. A house rule that every absence claim carries a positive control would have caught most of them.
