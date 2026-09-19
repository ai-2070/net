# Stage 7 review, round 5 — head `535a9f05a`

Branch `LZL0/webrtc-transport`, worktree
`C:/Users/chief/orca/workspaces/net/webrtc-transport`.

You held at `715a9ab06` on evidence integrity ("THIS IS NOT A PRODUCTION
HOLD"). Three commits since:

- `b07686a0c` — your F1/F2 plus the owner's F3.
- `7b9e884f1` — criterion 3's two refusal clauses, and the goodbye a
  closing owner never said.
- `535a9f05a` — criterion 4's media clause, and the withdrawal of the
  promotion-under-load claim.

## What to attack

### 1. The owner's farewell (`7b9e884f1`) — the biggest new surface

A host that closes now emits an unsolicited `no {code: owner-lost}` to
every handle it holds, awaited by `close()`, sent only on reply streams
already open. The wire was widened: a `q`-less `no` may now carry
`closed` (expiry) **or** `owner-lost` (goodbye).

Questions worth your probes:

- Can a farewell be delivered to a peer that should not receive it, or
  twice? `farewell()` forgets every handle as it emits.
- Does `close()` still terminate when a reply stream's `send` never
  settles? The farewell is awaited.
- Is the replica's terminal state actually terminal — can anything
  (alive tick, deadline sweep, a late delta) revive it?
- Does anything still read `not-ready` where the owner is gone?

### 2. The two refusal witnesses (`refusals` in `stage7.rs`)

Own contexts, own session, own capability tag, own identities, run
after the measured pair's pages close. Witness 10's control writes
successfully through the same handle BEFORE the replacement.

- Is the successor genuinely a different owner (incarnation) rather
  than the same store re-registered?
- Can the oracle pass with a dead transport? (Witness 9 answers this
  with a later host commit arriving; check I did not leave a hole.)

### 3. The media witness (`535a9f05a`)

Counters wrapped at page load; both pages asserted 0; the player then
CALLS both entry points and re-reads (0 → 1 each). Permission state is
reported, never asserted.

- Can the wrappers be bypassed by the path the leaf actually uses
  (e.g. a `RTCPeerConnection` constructed before the prototype patch,
  or a worker)? If so the zeros are worth less than claimed.

### 4. The withdrawal

`stage7.rs`'s header previously claimed the §9 promotion "does not
survive a page hosting six stores". I ran the reproduction (five
stores and five joins on the measured pair, immediately before the
promotion): it promoted on attempt 1, 57/0. The claim is withdrawn and
no mechanism is asserted in its place. Check the header says exactly
that and no more.

## Evidence I am claiming

Executed locally at `535a9f05a` unless noted:

- Chromium `--stage7`: **58 witnesses, 0 failed**.
- Chromium default: 47/0.
- `npx vitest run`: 675 passed, **exit 0** (the five unhandled
  rejections at `715a9ab06` are gone).
- `node tests/abi_real_package.mjs`: 32/32 against `dist/`.
- `node tests/kyra_review.mjs`: 4/4.
- `npm run size`: PASS.
- Firefox + Stage 7, recorded in CI at `b07686a0c`: 55/0, all eight
  names of that head PASS. Not yet gated: the floor is now 58 and
  Firefox has not run the three witnesses added since.

Inverse receipts are named in the commit messages; re-run any of them.

## Lines

You may hold. You may not edit the tree. Report EXECUTED versus
SOURCE-ESTABLISHED separately, and tell me if any inverse I claim is
non-discriminating.
