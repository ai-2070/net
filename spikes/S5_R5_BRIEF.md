# Stage 5 fifth round — the four owner questions, ruled (2026-09-16)

Round 4 (`ae690a8c5`…`e342007a0`) closed every executed and
source-established item from Kyra's third review and stopped, correctly,
at four questions only the owner could settle (`S5_REPORT.md` §13.7).
The owner has now ruled on all four. This round implements the rulings
and nothing else, stacking additively on round 4.

**Correction (2026-09-17, Fable):** the original text of this paragraph
said Kyra had not yet reviewed round 4. That was false. Her fourth
review (HOLD at `516a45c33`, 2026-09-16,
`hermes/cache/webrtc-stage5-round4-516a45c33/KYRA_STAGE5_ROUND4_REVIEW.md`)
existed before this brief was written and was never forwarded — a
record-keeping failure on my side, not the implementer's. It names five
executed defects (R4-1..R4-5) and five source-established ones
(R4-6..R4-10); none of them is addressed by this round, and her fifth
review (HOLD at `60e120609`) reproduced all five executed ones again.
They are the subject of `S5_R6_BRIEF.md`.

**The rulings, verbatim in substance:**

1. **Announcement expiry → match native.** Nanosecond precision, expire
   at `age >= ttl`, including TTL-zero and fractional cases. One expiry
   rule for one announcement type.
2. **N4 → confirmed; versioning left to the release process.**
   `#[non_exhaustive]` on `StreamError` and `StreamStats` plus
   `StreamStats::empty()` stand as policy. No version bump or release
   note in this round; the release owner handles both when the branch
   ships. Correct §12.2/§13.7 from "pending owner confirmation" to
   "owner-confirmed 2026-09-16; versioning deferred to release" — no
   other change.
3. **Direct `BrowserNode.close` → end iterators on parent close.**
   Consistent with the leader-proxied path (L2). This is a behaviour
   change for anyone relying on iterators outliving the node; say so in
   the package docs and the report.
4. **Multi-fragment interoperability → supply native reassembly now.**
   The native stream receive path reassembles leaf fragments; the
   bound-as-contract option is rejected.

One commit per ruling, prefix `fix(net): stage 5 —` (1–3) /
`feat(net): stage 5 —` (4); `S5_REPORT.md` §14 with the ruling, the
change, the witness, the raw inverse receipt (diff, command, exit,
restored output) per row; full AGENTS.md pre-push checklist including
the narrow-feature matrix and `leaf/` fmt; report only on all-green
exact-head CI with both browser engines. No Stage 6/7 work.

## 1. Expiry — match native

`leaf/src/announce.rs:113–123, 144–146, 421–436` (leaf, inclusive,
second granularity) vs `behavior/capability.rs:2830–2841, 3689–3699`
(native, `age_secs >= ttl`, nanosecond, documented as matching
`PermissionToken::is_valid`). Closure: the leaf's freshness predicate is
the native one — compute `age` from `timestamp_ns` at nanosecond
precision and expire when `age >= ttl`; TTL-zero is expired at age
zero; fractional-second cases follow. Move the two leaf tests that pin
the TTL-zero-within-its-second case to the native semantics (they are
not weakened — they now assert the opposite, correct, outcome). Add the
production authority-lookup witness Kyra asked for (expired →
not discoverable and not authority, via the real lookup, not a helper).
Cross-check: the `cross_lang_wire` fixture that carries an announcement
with a TTL must still decode on both sides.

## 2. N4 — record the confirmation

No code. `S5_REPORT.md` §12.2 and §13.7 item 2: status becomes
"owner-confirmed 2026-09-16; version bump and release note deferred to
the release process". The commit message says the same. Nothing about
the types moves.

## 3. Direct-node iterators end on parent close

`browser-ts/src/*` direct wrappers (the non-proxied `BrowserNode` path)
and the wasm surface behind them. Closure: `BrowserNode.close()` ends
every stream iterator the node handed out — the same terminal the
proxied path emits on generation change (L2) — so a consumer awaiting
`for await` completes rather than hangs; a subsequent `openStream` on a
closed node is a typed refusal. Witness through the **real built
package** (no hand-driven inner stream objects — Kyra's standing rule):
open a stream, park an iterator, close the node, the iterator completes
with the typed terminal; inverse: remove the end-on-close and the
witness hangs to its deadline (assert with a bounded race, not a
timeout widen). Package docs: state the behaviour change and the
symmetry with the proxied path.

## 4. Native reassembly of leaf fragments

This is core work inside a Stage 5 round, by owner decision; the report
says so and Kyra reviews it as a core change. Scope: the native
**stream receive path** reassembles the leaf's `frag_flags` groups so a
leaf → native reliable stream payload above `MAX_EVENT_SIZE` (8 104 B)
and up to the leaf's ceiling (64 832 B) arrives as one event; and the
native **sender** fragments a stream payload above `MAX_EVENT_SIZE` for
a peer that can reassemble, so native → leaf works in the other
direction. Constraints, all from earlier rounds and to be preserved:

- Reuse the RTC fragment reassembler's ownership model as repaired in
  rounds 3–4 (`rtc/fragment.rs`: group-wide plane/mode/stream/origin/
  channel/sequence provenance; a second piece on a held sequence is a
  contradiction; ACKed groups are complete-or-terminal; retirement with
  the session and on `StreamReset`; markers not load-bearing) — do not
  write a second reassembler beside it. If the stream path needs a
  seam, add it as a new function and name it in the report.
- Bounded per session by the existing byte budget; a group over the
  ceiling is a typed refusal at the first piece, never a partial.
- Reliable semantics unchanged: fragment sequences are consumed by the
  sequence-ownership rule from round 4's mode boundary (the head's
  sequence is the group's; the tail's sequences are consumed by
  reassembly); FAF fragments remain lossy and cannot kill the stream.
- No native ↔ native behaviour change: a native peer announcing no
  reassembly capability still gets the typed `EventTooLarge` refusal
  at `MAX_EVENT_SIZE`. Advertise the capability on the announcement
  (a tag, not a new canonical field — the tag set is already signed)
  so the sender fragments only for peers that reassemble.
- Consumer-tree diff file by file; the export checker on CI's build; if
  the `EventTooLarge` limit accessor changes meaning, the Go/Node/
  Python typed errors and parity tests move in the same commit.

Witnesses, both directions, through the public APIs (leaf `openStream`
+ native `send_on_stream`/`open_stream`), with nonce-correlated
payloads at the receiver: (a) leaf → native, a 40 000-byte reliable
payload arrives as one event, byte-identical; (b) native → leaf, the
same; (c) a lost middle fragment on a reliable stream is recovered by
the existing reliability machinery and the payload still arrives once;
(d) a group over the ceiling is refused typed at the first piece on
both sides; (e) native → a native peer without the capability tag still
gets `EventTooLarge` at 8 104 B. Inverses: reassembly disabled (a/b
deliver N partial events); the capability gate removed (e becomes
partial events at the peer); the ceiling check removed (d accepts).
Pin all five by name; raise the RTC floors accordingly.

Correct §13.7 item 4 and the large-message witness's comment: the
witness now proves multi-fragment interoperability in both directions,
not near-ceiling delivery only.

## Validation

All 42 reviewer probes and every prior suite green and unchanged in
count except the new names; the browser matrix on both engines with the
new iterator witness; the native RTC binaries with the five reassembly
witnesses; narrow matrix; bindings; `--lib` floors; export checker;
consumer diff. Final SHA, clean status, CI URL, §14.
