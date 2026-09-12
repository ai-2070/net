# natsim — real-NAT scenario harness

Stage 4 of `docs/internal/plans/NAT_TRAVERSAL_V2_PLAN.md`: the only place the
NAT-traversal layer is validated against **actual NAT behavior**
(Linux netfilter masquerade) instead of loopback, where every packet
trivially arrives.

## Topology

```
              nsim_wan  ("the internet", 10.99.0.0/24 on br0)
        .10 = R (relay/coordinator)    .11 = X (aux classify target)
        .12 = B when it plays the public peer (relay_upgrade,
              rtc_anchor_direct — there it is the RTC client)
            |                       |
        nsim_gwa (.2)           nsim_gwb (.3)      ← NAT gateways
      static snat + input drop      masquerade
        | masquerade fully-random      ...
            |                       |
        nsim_a (192.168.101.2)  nsim_b (192.168.102.2)
```

- **cone** = static `snat to <public>:<port>` for the joiner's own port,
  plus an INPUT drop for unsolicited inbound on that port. That gives
  endpoint-independent mapping (one public port for all destinations)
  with address-restricted filtering — the realistic punch-needing NAT.
  The classifier reads it as `Cone`.

  > **`masquerade persistent` is not a cone NAT.** The earlier version of
  > this harness used it and claimed endpoint-independent mapping here.
  > `persistent` pins the source *address*, not the port; port
  > preservation is only netfilter's best-effort heuristic, and it yields
  > under tuple collision. In a simultaneous punch the peer's keep-alive
  > can reach the gateway before the local outbound leaves; with no
  > mapping yet it lands in INPUT, where conntrack records it and claims
  > the very tuple the outbound then needs — so the gateway allocated a
  > fresh public port while the node had already advertised the old one
  > through the rendezvous. The "cone" NAT degraded to symmetric under
  > exactly the condition `cone_cone_punch` exists to exercise, and that
  > test failed for as long as it did. The INPUT drop fixes it by
  > refusing the packet *before* the conntrack confirm hook, so no entry
  > is inserted and the tuple stays free.
  >
  > A static DNAT would also break the race, and was rejected: it makes
  > the gateway full-cone, so the peer is reachable unsolicited and
  > `cone_cone_punch` would pass without any hole being punched.
- **symmetric** = `masquerade fully-random`: fresh public port per
  connection tuple. The classifier reads it as `Symmetric` because R
  and X (two *distinct* public IPs) observe different mappings.

A cone gateway can also pin a **second** UDP port for the same
joiner (`setup.sh --rtc-port-a <port>`), with the same `snat to
<public>:<port>` + INPUT drop pair. That is what makes a NAT'd
**RTC anchor** possible: its WebRTC socket is a separate socket from
the mesh socket, so without the pin its public mapping is whatever
`masquerade` happens to pick — unknowable in advance and therefore
impossible to advertise. With the pin, `10.99.0.2:<port>` is a
stable mapping the anchor can publish as `rtc_addr`.

R and X are two IPs on the same bridge precisely so classification
has two distinct destinations to compare — the cone/symmetric
distinction is real, not forced by a test hook.

## Pieces

| file | role |
|---|---|
| `setup.sh` / `teardown.sh` | provision / destroy the namespaces, veths, masquerade rules |
| `run_scenario.sh <name>` | orchestrate one scenario: setup → launch helpers → collect verdict → teardown |
| `../../examples/natsim_node.rs` | the helper node (roles: `keygen`, `capabilities`, `public`, `joiner`) |
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
| `rtc_anchor_direct` | cone (+ RTC port 7101 pinned) | — (B public, the client) | the NAT'd **anchor** announces `rtc_addr = 10.99.0.2:7101` (its mapped address, not its `192.168.101.2:7101` bind), and the outside client's relay-signalled session ends up on a DataChannel (`transport: "rtc"`, `stats.rtc.ice_direct ≥ 1`). Needs a helper built with `webrtc`; the scenario refuses before provisioning if it isn't. Note B, not A, writes the verdict here — the client is the side that drives the upgrade |

Deferred (documented, not yet wired): the parent-decision-11 IPv6
pair — dual-stack both-open → direct, and a NAT64/464XLAT topology
(needs tayga/jool in the runner image). Add as scenarios 7–8 when a
consumer needs them; the harness shape (per-side gateway namespaces)
already accommodates both.

### What `rtc_anchor_direct` does and does not prove

It proves two things. **The announcement**: `anchor_rtc_addr` in the
verdict is read back from the anchor's own emitted
`CapabilityAnnouncement`, so the assertion pins what went on the
wire, not what the flag said. **The reachability**: a DataChannel
installs across the real masquerade, so the client is reaching the
anchor's RTC socket through the pinned mapping.

It does **not** prove that the advertised candidate is the pair ICE
selected. The anchor's own connectivity checks leave through the
same mapping, so a client told nothing would discover
`10.99.0.2:7101` as a peer-reflexive candidate anyway (measured on
loopback: with a deliberately wrong `--rtc-public`, ICE still
connects). Same address, different provenance — the announcement
half is what pins the provenance.

## Running locally (Linux, root)

```bash
cargo build --example natsim_node --features net,nat-traversal,webrtc
cargo test --test natsim --features net,nat-traversal,webrtc -- --ignored --test-threads=1
# or a single scenario, directly:
sudo tests/natsim/run_scenario.sh cone_cone_punch /tmp/natsim-state
sudo tests/natsim/run_scenario.sh rtc_anchor_direct /tmp/natsim-rtc
```

`webrtc` is only needed for `rtc_anchor_direct`; without it that
scenario refuses (`natsim: ... needs a helper built with the webrtc
feature`) before provisioning anything, and its wrapper test does
not exist.

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

For `rtc_anchor_direct` specifically: the verdict's `stats.rtc`
block says which half failed. `signal_delivered == 0` on the anchor
means the `0x0D02` offer never arrived (a relay/routing problem, not
an ICE one); `ice_attempted > 0` with `ice_relayed == ice_attempted`
means ICE ran and never connected — check `nsim_gwa_nat.log` for
whether the gateway really mapped the RTC socket to `sport=7101`,
because an unpinned mapping is the one failure the anchor cannot
detect itself.
