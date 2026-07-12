# natsim — real-NAT scenario harness

Stage 4 of `docs/plans/NAT_TRAVERSAL_V2_PLAN.md`: the only place the
NAT-traversal layer is validated against **actual NAT behavior**
(Linux netfilter masquerade) instead of loopback, where every packet
trivially arrives.

## Topology

```
              nsim_wan  ("the internet", 10.99.0.0/24 on br0)
        .10 = R (relay/coordinator)    .11 = X (aux classify target)
        .12 = B when it plays the public peer (relay_upgrade)
            |                       |
        nsim_gwa (.2)           nsim_gwb (.3)      ← NAT gateways
      masquerade [persistent|       masquerade
        fully-random]                  ...
            |                       |
        nsim_a (192.168.101.2)  nsim_b (192.168.102.2)
```

- **cone** = plain `masquerade persistent`: endpoint-independent
  mapping (one public port for all destinations) with conntrack
  (address+port-restricted) filtering — the realistic punch-needing
  NAT. The classifier reads it as `Cone`.
- **symmetric** = `masquerade fully-random`: fresh public port per
  connection tuple. The classifier reads it as `Symmetric` because R
  and X (two *distinct* public IPs) observe different mappings.

R and X are two IPs on the same bridge precisely so classification
has two distinct destinations to compare — the cone/symmetric
distinction is real, not forced by a test hook.

## Pieces

| file | role |
|---|---|
| `setup.sh` / `teardown.sh` | provision / destroy the namespaces, veths, masquerade rules |
| `run_scenario.sh <name>` | orchestrate one scenario: setup → launch helpers → collect verdict → teardown |
| `../../examples/natsim_node.rs` | the helper node (roles: `keygen`, `public`, `joiner`) |
| `../natsim.rs` | `#[ignore]`d Rust tests wrapping the scripts; assert outcome + `traversal_stats` deltas |
| `.github/workflows/natsim.yml` | CI job: traversal-touching PRs + nightly + manual |

Helpers coordinate through a shared state directory (namespaces
share the filesystem): identity files, accept-turn markers (the
`accept(node_id)` contract needs exactly one dialer in flight per
public node), readiness markers, and the initiator's
`a_outcome.json` verdict.

## Scenario matrix

| scenario | NAT A | NAT B | expectation |
|---|---|---|---|
| `cone_cone_punch` | cone | cone | punch lands; session on B's public mapping; `attempted == succeeded == 1` |
| `symmetric_cone_punch` | symmetric | cone | exactly one attempt, `punch_timeouts == 1`, relay fallback (parent decision 8) |
| `symmetric_symmetric_skip` | symmetric | symmetric | matrix skip: zero attempts, relay fallback |
| `dropped_keepalives` | cone | cone (+ direct-UDP drop on both gateways) | attempt times out, falls back within deadline |
| `relay_upgrade` | cone | — (B public) | relay-routed session migrates off the relay (`upgrades_succeeded ≥ 1`); the NAT'd joiner is forced to be the lower node id (C1 initiator) via `keygen` ordering |

Deferred (documented, not yet wired): the parent-decision-11 IPv6
pair — dual-stack both-open → direct, and a NAT64/464XLAT topology
(needs tayga/jool in the runner image). Add as scenarios 6–7 when a
consumer needs them; the harness shape (per-side gateway namespaces)
already accommodates both.

## Running locally (Linux, root)

```bash
cargo build --example natsim_node --features net,nat-traversal
cargo test --test natsim --features net,nat-traversal -- --ignored --test-threads=1
# or a single scenario, directly:
sudo tests/natsim/run_scenario.sh cone_cone_punch /tmp/natsim-state
```

`--test-threads=1` is mandatory: scenarios share namespace names and
the `10.99.0.0/24` range. Everything the harness creates is
namespaced (`nsim_*`) — `teardown.sh` removes it all and is safe to
run at any time.

## Debugging a failed scenario

`run_scenario.sh` keeps helper logs in the state dir
(`<state>/{r,x,a,b}.log`) and dumps their tails on timeout. The
usual suspects, in order: the helper binary wasn't rebuilt after a
mesh change; a classifier read `Unknown` because one public didn't
come up (check `x.log`); conntrack surprises from a previous run
(`teardown.sh`, then retry — namespace deletion drops all state).
