# Release notes — subnet auth branch: operator-facing behavior changes

Two changes on this branch alter what an already-deployed
configuration *does* without any config file changing. Both are
deliberate; both need to be known before upgrade, not discovered from
traffic.

## 1. `Visibility::Exported` goes from inert to active

**Before this branch** the `Exported` visibility arm was
unconditionally closed: a channel configured `Exported` reached
nobody, and the export table populated through `net gateway export`
was consulted only by `SubnetGateway::should_forward`, which has no
production callers. Export rules were, in practice, write-only state.

**After this branch** the arm is enforced on both the subscribe gate
and the publish fan-out: an `Exported` channel propagates to a peer
iff the peer's derived subnet falls under one of the channel's
declared export targets.

**What that means on upgrade.** Any deployment that both

- configured one or more channels as `Exported`, **and**
- populated export rules through `net gateway export`

begins propagating traffic that previously went nowhere — no config
change, no operator action. Two compounding semantics to check:

- **Subtree containment.** A target covers its whole subtree: a rule
  declared at a fleet reaches every vehicle under it. A rule written
  when it did nothing has a blast radius nobody had reason to check.
- **Global target.** A target of `SubnetId::GLOBAL` (`0.0.0.0`) in an
  existing rule now matches every destination.

**Before upgrading**, audit `net gateway exports` on every gateway
node and delete or narrow rules you do not want live. The rest of the
mode is fail-closed: a peer whose subnet cannot be derived is denied,
and a channel with no export rule exports nothing.

**Observability.** Installing (or re-applying) an export rule for a
currently-`Exported` channel now emits one `info` line naming the
channel and its targets, so the moment a rule becomes live policy is
visible in the log rather than deduced from traffic.

## 2. Gateway nodes stop relaying untagged legacy routed packets

A node that holds subnet gateway credentials no longer relays legacy
(non-route-hop) routed packets at all: a protected route may never
downgrade to the public path, so the relay branch refuses untagged
traffic outright rather than forwarding it around the authority
checks.

**What that means on upgrade:** a node that acquires gateway
credentials silently stops being a general-purpose relay for legacy
routed traffic. If a deployment used such a node as a plain relay,
that traffic stops when credentials are installed. Route legacy
traffic through a node without gateway credentials, or move the flows
onto the protected route-hop path.
