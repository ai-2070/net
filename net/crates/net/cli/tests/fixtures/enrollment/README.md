# Enrollment journey: operator, devices, relations and leave

This is the operator/joiner/provider walkthrough for managed nodes and join
links. The runnable harness is `../../enrollment_workflow.rs`. From a source
checkout (the pinned Rust toolchain, Python 3 and cargo-nextest), run it from
`net/crates/net/`:

```sh
cargo nextest run -p net-cli --test enrollment_workflow --no-tests=fail --retries 0
```

It starts three participants as separate `net-mesh` processes on one machine:
an operator and two devices, B and C. Each has an isolated state directory
and config, and router port mapping is disabled. Loopback proves the
multi-process, multi-node behaviour. It does **not** prove separate
computers, NAT traversal or a public relay.

The commands below are the ones the harness runs, in order. Every command
that talks to a running node takes `--state-dir <DIR>`, the directory that
node's `up` was started with.

## Terminal ownership

- **Operator machine:** holds the offline roots (a subnet authority key and a
  channel root identity) and runs `net-mesh up --enroll`. The roots are used
  by offline commands only and never reach the node.
- **Each device:** runs `net-mesh join` once and then `net-mesh up`. A device
  can instead run a provider or consumer directly as its enrolled self
  (`wrap --joined`, `mcp serve --joined`) while its `up` is stopped.

## 1. Offline authority (operator)

```sh
# Subnet: a root key, a delegated issuer key, and the root-signed issuer grant.
net-mesh subnet keygen --out subnet-root.toml
net-mesh subnet keygen --out subnet-issuer.toml
net-mesh subnet issue-issuer --root-key subnet-root.toml --authority <ROOT_HEX> \
  --issuer <ISSUER_HEX> --scope 3 --max-rights attach --out issuer.grant

# Channel: the root is an operator identity; the grant names the enrollment
# issuer that `up --enroll` reports as `enrollment.issuer`.
net-mesh identity generate --out channel-root.toml
net-mesh channel issue-grant --root-identity channel-root.toml \
  --issuer <ENROLLMENT_ISSUER_HEX> --channel fleet.telemetry --out channel.grant
```

## 2. The operator node

```sh
net-mesh up --enroll \
  --subnet-issuer-grant issuer.grant --subnet-issuer-key subnet-issuer.toml \
  --channel-grant channel.grant
net-mesh channel serve fleet.telemetry --token-root <CHANNEL_ROOT_HEX>
```

- `up` generates and keeps the mesh PSK on first start. `--psk-from
  file:<path>` or `stdin` supplies one instead.
- The ready row reports the enrollment endpoint, the issuer, the subnet this
  node verifies and the channels it can mint.
- `channel serve` gates the channel so only chains anchored at that root may
  subscribe. It is persisted and re-applied on every `up`.

## 3. Join links (operator → device)

```sh
net-mesh invite create --subnet 3.7 --channel fleet.telemetry --channel-rights subscribe
net-mesh invite inspect <TOKEN>          # offline; redeems nothing
net-mesh join <TOKEN> --yes              # on the device
net-mesh up                              # on the device
```

- **One link per device, several relations.** The link carries mesh
  membership, a subnet attachment (a delegated credential for this device
  only) and a channel credential (a chain `root → issuer → device`).
- **Channel rights** are `publish`, `subscribe` or `publish,subscribe`.
  Subscribe sends the device to the operator node as publisher, so the
  channel must be served there first. `ADMIN`, wildcard and delegation are
  never issued.
- **Approval.** Links are preauthorized by default. With
  `--require-approval` a link issues only after `invite approve`.
- **Org membership** (`--org <ORG>`) is always approval-gated
  (`org approve --org-key`).
- **Tokens are bearer authorization** unless bound with `--for <ENTITY>`.
  Treat them as secrets.
- **What the device's `up` reports:** `joined.subnet.admitted` (the
  verifier's verdict on this session) and `joined.channel.subscribed` (the
  publisher's ACK of the full chain). Both are re-established by the node
  itself after every reconnect.

A device already on the mesh adds a relation with a standalone link:
`subnet invite` then `subnet join`, or `org invite` then `org join`.

## 4. Publish through the device's own gate

```sh
net-mesh channel serve fleet.telemetry --token-root <CHANNEL_ROOT_HEX>   # on the device
net-mesh channel publish fleet.telemetry --data "t=21.5"
net-mesh channel status
```

- **Readiness.** A publish credential is *ready* only while the device's own
  channel config trusts the root. No root is installed implicitly.
- **The gate's verdict:**
  - `gate: passed` means this node's production gate accepted the publish;
  - `gate: open` means the channel is ungated here, which is no credential
    evidence;
  - a denial carries the gate's reason.
- **Delivery counts** are this node's sends, not subscriber receipts.

## 5. Selective removal (operator)

```sh
net-mesh subnet remove --root-key subnet-root.toml --authority <ROOT_HEX> \
  --scope 3.7 --topology-epoch 0 --revision 1 --subject <B_ENTITY_HEX> \
  --minimum-generation 2 --verifier self
net-mesh subnet members 3.7
```

- `subnet remove` signs a subject floor with the offline root and hands it
  to each named verifier. It reports each verifier's own signed attestation,
  and `complete` is true only when every named verifier persisted it.
- B's next session is refused admission with its old credentials. C, in the
  same subnet, is untouched.
- `org remove` does the same for an org member.

## 6. A tool call between enrolled devices

```sh
# B, with its `up` stopped: provide a tool as the enrolled device.
net-mesh --output ndjson wrap journey --joined <B_STATE> --allow <C_ORIGIN_HASH> -- python3 server.py
# C, with its `up` stopped: consume as the enrolled device, attached to B.
net-mesh mcp serve --joined <C_STATE> --node-addr <B_BIND> --node-pubkey <B_KEY> --node-id <B_NODE_ID>
net-mesh mcp pin approve <B_NODE_ID>/journey_echo
```

- `--joined` loads the device identity, the mesh PSK and the enrolled
  contact from the join. A running `up` owns the join, so stop it first.
- The provider stays owner-only and admits C only through `--allow`.
- C's consent pin is separate and cannot override the provider.
- Two devices attached only through the operator cannot yet invoke each
  other. The consumer names the provider as its peer.

## 7. Leave, restart and cleanup

```sh
net-mesh channel leave            # the channel relation only
net-mesh subnet leave 3.7         # one subnet relation only
net-mesh org leave                # the org membership only
net-mesh leave                    # the whole mesh: stops the node, erases the delivered credentials
net-mesh down                     # stop a node (the operator's, here)
```

What leaving does:
- **Every leave is local and durable.** It is recorded before anything
  else, keeps the device identity, and survives restart. A left relation is
  never presented, renewed or re-subscribed again.
- **Live side effects:**
  - `channel leave` is an acknowledged unsubscribe plus removal of exactly
    the installed publish credential (`publish_stop: confirmed` or
    `unconfirmed`);
  - `subnet leave` asks the verifier over the session to drop the admission
    (`withdrawal: confirmed` or `unconfirmed`);
  - a whole `leave` of a subscribed device withdraws the subscription before
    it stops.
- **Leaving is not revocation.** Credentials stay valid until they expire
  or the operator removes the device (`subnet remove`, `org remove`).
- **Rejoining:** `join --rejoin` after a whole leave; a fresh link after a
  relation leave.

## Evidence limits

- Every subprocess wait and protocol request has a ceiling. A discovery or
  readiness timeout means availability was not established.
- An invocation timeout does not prove the handler had no effect. Inspect
  the provider record instead of retrying.
- This harness is one machine on loopback. It is not evidence of off-host
  operation, NAT traversal, a deployed relay, or behaviour under packet
  loss.
