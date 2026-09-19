# Raw inverse receipts — rows P1, P2, X1, X3 (leaf)

Each log holds, in order: the **bounded source diff** of the inverse
mutation, the **exact command**, its **exit code**, the mutated run's
output, the restoration diff (`EXIT=0` ⇒ byte-identical to the
repaired source), and the **restored run's output** with its exit
code. Nothing is described; everything is the captured stream.

| Log | Row | Inverse mutation | Mutated exit | Restored exit |
|---|---|---|---|---|
| `P1a-expire-surrenders-ownership.log` | P1 | `Reassembler::expire` stops reporting the reaped group's stream (`-note_abandoned(abandoned, key.0, p);`) | 101 / 101 | 0 / 0 |
| `P1b-expiry-precedes-acknowledgement.log` | P1 | the deadline stops being evaluated before the arrival's admission/ack decision (`-self.sweep_reassemblies(now);` at the top of `on_datagram`) | 101 | 0 |
| `P1c-malformed-cleanup-surrenders-ownership.log` | P1 | malformed-group cleanup drops acknowledged bytes with only a counter (`self.abandon(key)` → `self.groups.remove(&key)`, both sites) | 101 | 0 |
| `P2-close-retains-receive-cursor.log` | P2 | `close_stream` deletes the receive cursor again instead of retaining it (`rx_closed.insert` → `rx_streams.remove`) | 101 / 101 | 0 / 0 |
| `X1-reset-retires-stream-fragments.log` | X1 | a RESET stops retiring the reset stream's fragment groups (`-reassembler.retire_stream(..)`) | 101 | 0 |
| `X3-group-binds-plane-and-mode.log` | X3 | the group-wide check stops binding subprotocol and reliability (two clauses removed) | 101 | 0 |

The diffs above were taken against the repaired `frame.rs` and
`node.rs` as committed in `fix(net): stage 5 repairs P1/P2 and
X1/X2/X3/X4`. Full copies of those files were deliberately NOT kept
here: the commit is the attributable base, and a second copy of two
4 000-line sources in the evidence directory would go stale the first
time either file changed. `git show <commit>:<path>` reproduces the
exact base each diff applies to.

Baseline at the end of the round, same working tree:
`cargo test` in `net/crates/net/leaf` = **220 native tests, 0
failures**; `cargo fmt -p net-mesh-leaf --check` = 0; `cargo clippy
--all-targets` and `cargo clippy --target wasm32-unknown-unknown
--lib` = no warnings; `wasm_leaf` 16/16 and `wasm_anchorless` 2/2 in
headless Chromium.
