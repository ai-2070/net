# Agent Skills — checked examples: the services you no longer run

> Turn the skills' prose promises into **executed** examples: a bounded set of
> routes that each rebuild a service a developer already operates, on the
> substrate, in one file — in all five bindings, with a single line of stdout as
> the contract. Not a snippet library, not five cloned skills, not a directory
> of illustrative fragments: a manifest of routes and one execution proof per
> binding.
>
> Completes Phase 4 of [`SKILLS_LANGUAGE_ROUTING_PLAN.md`](SKILLS_LANGUAGE_ROUTING_PLAN.md),
> which built the routing and the compile floor and named this as the follow-on.

## Status

**Shipped.** Ten routes across five bindings, on `LZL0/examples`; the last lands
in `1538e75cf`. Waves 1 and 2 are executed in CI (see
`.claude/skills/net-event-bus/examples/README.md` for the per-binding record);
wave 3 was **also executed locally in all five bindings** before the push, and
the local command for each is identical to the command CI runs.

The programme did what Phase 4 of the routing plan said it would, and it also
produced three findings the plan had not anticipated — two of which are the
reason this document exists:

- **One candidate route is not a route.** Gang-claim looks like a mutual-exclusion
  primitive in all five bindings and exposes **no observable exclusion** on the
  flat SDK. Measured, not inferred. Dropped; the measurement is below.
- **One candidate route has no binding-level surface at all.** Custom-subprotocol
  registration is Rust-and-core only. Dropped.
- **One helper the routes depend on is Rust-only.** `call_service_typed_with_retry`
  exists in no other binding, so four of the five ports carry a hand-written
  retry loop. That is the port cost the plan under-estimated.

Two smaller corrections from the routing plan's appendix are also folded in here,
because the examples are what made them visible: the skills' parity claims were
prose until these files compiled, and one of them was already false.

## The rules that scoped it

Three rules did most of the work. They are stated first because every route
below is a consequence.

**A route earns its place only if it rebuilds something the reader already
operates.** "An important operation" is not a scoping rule — it makes the set
subsystems × bindings. *Consul, SQS, S3, LaunchDarkly, Kafka, an ACL sidecar* is.
The route's README line names the product it replaces.

**One canonical file per example, and the docs link only.** The runnable file is
canonical. A hand-copied excerpt diverges as readily as a duplicated file, which
is the thing the rule exists to prevent.

**The manifest states every binding, positively or as a reasoned absence.**
`docs/data/examples.yaml` requires each route to list all five as either a
checked file or an explicit `absent` with a reason, and its coverage report
prints **▶** for executed against **✓** for compiled-only — so a route covering
three languages cannot read as one covering five. A source file in `examples/`
that the manifest does not list is an error, because otherwise it ships to users
with nothing compiling it.

## Wave 1 — the services you no longer run

Four routes, `3a9c5ce4e` (Rust) through `ad2f12b62` (C). These stand up two to
four real mesh nodes over loopback UDP; there is no memory transport and no mock.

| Route | Replaces | Expected line |
|---|---|---|
| `registry.*` | Consul / etcd + a health poller | `RESULT ok providers=3 joined=1 best_moved=1` |
| `jobqueue.*` | Celery / SQS + Redis | `RESULT ok jobs=6 done=6 retried=1 duplicates=0` |
| `objectstore.*` | S3 / MinIO | `RESULT ok dedup=1 readback=1 bytes=64` |
| `liveconfig.*` | LaunchDarkly / Consul KV | `RESULT ok subscribers=2 applied=2 version=2` |

## Wave 2 — logs and credentials

Two routes, `7fdb8fc8a`. Both portable with no absent binding and no caveat.

| Route | Replaces | Expected line |
|---|---|---|
| `eventlog.*` | Kafka + ZooKeeper/raft | `RESULT ok records=8 replayed=8 resumed=3` |
| `tokenchannel.*` | an ACL file + a sidecar auth service | `RESULT ok granted=1 refused=1` |

Wave 2 forced a substrate change, which is the clearest sign the routes were
worth writing: a token's leaf binds to the subscribing peer's `EntityId`, and the
only paths that installed that binding were a signature-verified capability
announcement or verified subnet admission — so a consumer with no services to
publish had to announce capabilities and poll a discovery index **purely to use a
credential issued to it**, surfacing as `Unauthorized`, which points at the
credential rather than at the missing binding. `5c280e37e` makes identity
establishment separate from capability discovery, and `058a31409` removes the
warm-up every binding's example had encoded to work around it.

## Wave 3 — `service-failover`

`1538e75cf`. One route: two providers behind one service name; the caller
addresses the **service**, never a node id; the answering provider dies
mid-flight; the next call lands on the survivor.

| Binding | File | Proof before the push |
|---|---|---|
| Rust | `failover.rs` | executed |
| TypeScript | `failover.ts` | 2 runs |
| Python | `failover.py` | 2 runs + `mypy` clean |
| Go | `failover.go` | 2 runs (`go build` then execute) |
| C | `failover.c` | 3 runs, linked against the real `libnet` cdylib |

Every run printed `RESULT ok providers=2 moved=1 served=2`.

The route is worth its place because it is the one example that demonstrates
*what nRPC buys that a `host:port` does not*: the caller never learned the
provider's identity, so the provider's death is a retry rather than a redial.
It is the same one-line `call_service` in all five bindings.

**The retry helper is Rust-only, and that is the port's real cost.**
`call_service_typed_with_retry` (attempts, backoff, retry predicate) exists in
the Rust SDK and nowhere else, so TypeScript, Python, Go and C each carry a
bounded loop written by hand — six attempts at 250 ms under a 500 ms call
deadline — and each file says so in a comment. The loop is not decoration: the
roster still lists a dead provider until the capability fold converges, so the
first attempt after the kill may be spent on a corpse. A reader in a binding
without the helper is missing the loop, not just the call.

## Two routes that earned a `no`

This is the part of the plan that paid for itself. Both candidates looked obvious
and both were wrong; recording *why* is cheaper than the next person re-deriving
it.

### Gang-claim exposes no observable exclusion on the flat SDK

`claim_island` / `reserve_island` / `match_islands` / `find_islands` are typed in
all five bindings, so "two nodes contend for one mutually-exclusive resource"
looked like the natural third route. It does not hold. Measured directly, on one
mesh, with the released bindings:

- two nodes each `claim_island` the **same** island, and **both** get `Some`;
- `reserve_island` on an island a peer already holds also returns `Some`;
- a node that already holds an island still appears in its own `match_islands`
  candidate list.

The exclusion lives one layer down, in the quorum promotion to the `Active`
state — core-only and unreachable from the bindings. So a `lock` example would
have taught a mutex that is **not one**, and a reader would have shipped mutual
exclusion they never had. The `claim`-convergence crate's own docs describe the
property correctly; the flat SDK is where it does not compose. **Dropped rather
than documented**, because a documented negative is easy to read past and this
one fails in the unsafe direction.

### Custom subprotocols are not a binding-level surface

The subprotocol registry has no typed exposure in any of the five SDKs. A route
there would be Rust-and-core only, which this grid does not run and this
programme does not ship. **Dropped.**

## Binding gaps found while porting

Recorded in the examples README with the reproduction; listed here with the
commit, because they are the measurable return on writing examples that must
compile in five languages:

| Gap | Commit |
|---|---|
| `object-store` could not be written in C or Go — neither ABI could **mint** a blob address | `21be25dde`, `59882eafc` |
| `net_rpc.h` did not parse on its own (`RpcResponseSinkHandleC` used before its forward typedef) | `a122e9961` |
| Python nRPC unreachable from the wheel (six sync streaming classes unregistered, so `TypedMeshRpc` raised `MeshRpc unavailable`) | `704c42ca1` |
| Python blob bindings untested, not absent — the tested artifact and the shipped one differed | `ef67a7334` |
| TypeScript `MeshNode` wrapped the publish verbs and none of the receive verbs | `808a7658f` |

Four of the five are the same class of defect: **a surface that exists, is
documented, and is not reachable from the binding that documents it.** No
compile floor would have found them, because the *skill* compiled — the code in
it did not.

## What CI proves here, precisely

Stated at the precision the implementation supports, since the routes' whole
value is that this is higher than the snippets':

- Every example is **compiled or type-checked** on each pull request. C is
  `gcc -fsyntax-only` against the public headers; Go is `go vet` against the real
  module (not `go build`, which would need a release cdylib); Rust builds against
  the workspace SDK; Python is `mypy`. TypeScript is type-checked in the job that
  produced the napi declaration it needs.
- Every example is also **executed**, with its stdout matched against the
  contract in `docs/data/examples.yaml` and bounded by a timeout. Each runs where
  its artifacts already exist: Rust and TypeScript in `skills.yml`, Python in
  `ci.yml`'s `python-tests`, Go and C in `ci.yml`'s `go-tests`.
- Driving execution from the manifest is what makes the coverage report
  trustworthy. Before it, `check-skill-examples.sh` hardcoded five `hello.*`
  paths, so a route covering three languages would have looked exactly like one
  covering five.

Two honest qualifications:

- **The manifest's `run:` contract is a stdout match, not a semantic assertion.**
  It proves the file ran to completion and reported the outcome it claims; it
  does not prove the outcome is meaningful. `service-failover` counts both calls'
  returned payloads rather than asserting `served=2` outright precisely because
  the weaker version would pass on a call that returned nothing.
- **Local execution and CI execution are the same command, not the same
  artifact.** On Windows, linking C requires a MinGW import library
  (`gendef` + `dlltool`) because the emitted one is MSVC-flavoured; CI is Linux
  and does not. The wave-3 C proof was local and against a locally built
  `libnet`; CI re-proves it on its own artifact.

## Explicitly not doing

- **An example per subsystem per binding.** That is subsystems × bindings, and it
  is how a checked-example programme becomes a maintenance burden that gets
  deleted. Routes are added where a binding's shape is surprising or the
  behaviour is commercially load-bearing — not by default.
- **Executing snippets quoted inside the skills.** The canonical file runs; the
  docs link to it. A quoted excerpt is a second copy that CI cannot keep honest
  as the thing it quotes.
- **A lock/island-contention route**, per the measurement above, until the
  quorum `Active` path is reachable from the bindings.
- **A subprotocol route**, until the registry is a binding-level surface.
- **Claiming a binding supports an operation because its symbol exists.** That is
  the mistake that produced four of the five gaps above.

## Appendix — reproducing wave 3

```
Rust        cargo run --example failover            (Rust SDK examples crate)
TypeScript  npx tsx failover.ts                     (after importing '@net-mesh/sdk')
Python      PYTHONPATH=…/sdk-py/src python3 failover.py
Go          go run failover.go                      (libnet cdylib built, on the loader path)
C           gcc failover.c -I include -o failover net.dll -lpthread -lm && ./failover
```

Each prints four lines and ends on the contract:

```
providers advertising `work`: 2
first call served by:  0x9cb65b0249637c9b
took 0x9cb65b0249637c9b out
after the death served by: 0xbcc998bd08d4ed82
RESULT ok providers=2 moved=1 served=2
```

The `first …` / `after …` pair is the property, and it is the reason the route
asserts on `moved` rather than only on the final line: a run where the second
call went back to the dead provider would still print a `RESULT` line, and the
contract would read as green.