# Subnet authorization — SDK plan (SSDK)

Bring the subnet authorization surface — authority roots, attachment,
credential provisioning, gateway rights, control facts, and the exported
provider boundary — to the Rust SDK (`net_sdk`) and the TypeScript, Python,
Go, and C bindings. Companion to
[`SUBNET_AUTH_PLAN.md`](SUBNET_AUTH_PLAN.md), which specifies the substrate
this wraps (shipped, PR #750), and to
[`ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md`](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md)
/ [`PAYMENTS_LANGUAGE_SDKS_PLAN.md`](PAYMENTS_LANGUAGE_SDKS_PLAN.md), the
multi-language workstream template this follows. The Rust-facade anatomy
copies [`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md).

**The sentence:** every language gets the same three operator roles —
configure trust, provision credentials, serve an exported boundary — over
the same six wire artifacts and the same `subnet:` error vocabulary; no
language gets a way to mint authority, and no language gets a type in which
topology placement and transport authority can be confused.

## Status

**v0.1 (2026-08-04). Draft — nothing implemented.** Written on branch
`subnet-sdk` at `c9ced5336`, from a same-day five-surface ground-truth
survey (Rust core, Rust SDK, Node+sdk-ts, Python+sdk-py, Go+C+CLI).

Substrate state, stated precisely:

- S0–S5 plus the §6 BMW two-vehicle end-to-end evidence merged to master
  via PR #750 (`subnet-auth-e2e`, merge `88ad03ccd`).
- [`CODE_REVIEW_2026_08_04_SUBNET_AUTH.md`](../misc/CODE_REVIEW_2026_08_04_SUBNET_AUTH.md)
  was opened at review head `94ef4e092`; twelve follow-up commits on the
  branch closed findings before merge (`18f7c3946` is the last), but the
  review doc's header still reads **OPEN, not signed off**. SSDK treats the
  substrate as shipped and does not re-litigate it; if a finding in that
  document is later reopened against a seam SSDK wraps, the affected
  workstream pauses rather than papering over it.
- `SUBNET_AUTH_PLAN.md` §9 ("Files touched") lists **zero SDK or binding
  files**, and §11 (non-goals) never mentions the SDKs. SDK exposure was
  never in the substrate plan's scope. This plan is that scope.

## Substrate ground truth — what exists to wrap (surveyed 2026-08-04)

Everything below is from the merged tree. Paths relative to
`net/crates/net/` unless noted.

**The credential/verification family is complete and exported** from
`src/adapter/net/subnet/` (`mod.rs:24-40`): `SubnetRights` (`auth.rs:72`,
`try_from_bits` rejecting unknown bits and the empty mask), `SubnetRef`
(`auth.rs:120`, `contains` at `:137`), `SubnetGrant` (`auth.rs:261`,
`WIRE_SIZE = 198`), `SubnetIssuerGrant` (`auth.rs:487`),
`SubnetRevocationFloor` (`auth.rs:678`), `SubnetFloorRegistry`
(`auth.rs:821`), `SubnetAuthorityConfig` (`auth.rs:956`),
`SubnetCredentialSet` (`auth.rs:977`, `Direct | OneHop`, `to_bytes` /
`from_bytes` / `credential_set_hash`), `SubnetAuthPresentation`
(`auth.rs:1213`), `VerifiedSubnetContext` (`auth.rs:1379`),
`VerifiedGatewayContext(Set)` (`auth.rs:1462/:1560`), `SubnetBoundarySet`
(`auth.rs:1623`), `SubnetExportBinding` (`auth.rs:1700`), `SubnetAuthError`
(`auth.rs:149`, `Display` renders stable snake_case reason codes at
`:215`), and the S5 control-fact family in `control.rs`
(`SubnetControlFact`, `SubnetControlStore`, `SubnetControlOutcome`,
`SubnetFactKind`, three descriptive facts + the floor).

**Node config and provisioning seams exist** (`src/adapter/net/mesh.rs`):

| Seam | Where |
|---|---|
| `MeshNodeConfig.subnet_authorities: Vec<SubnetAuthorityConfig>` | `:1893` — trust anchors; empty fails closed |
| `MeshNodeConfig.subnet_attachment: Option<SubnetId>` | `:1901` — the node's *security* attachment, distinct from topology `subnet`; **no `with_` builder — set by direct field write** (`tests/subnet_auth_e2e.rs:193` is the canonical pattern) |
| `MeshNodeConfig.subnet_control_channel: Option<ChannelName>` | `:1913` — a fact *arrival path*; confers no authority |
| `with_subnet_authority(..)` (repeatable) | `:2584` |
| `with_subnet_control_channel(..)` | `:2592` |
| `MeshNode::install_subnet_gateway_credentials(&[SubnetCredentialSet])` | `:11802` — compiles + publishes atomically; **wholesale replace** |
| `MeshNode::declare_subnet_boundaries(SubnetBoundarySet)` | `:11919` — mandatory for protected forwarding |
| `MeshNode::apply_subnet_control_fact(&[u8])` | `:11735` — "the ONE entry every arrival path shares" (floors + three descriptive kinds, from wire bytes) |
| `MeshNode::apply_subnet_floor(&SubnetRevocationFloor)` | `:11688` — typed floor entry the control-fact door subsumes |
| `MeshNode::advance_subnet_topology_epoch()` / `subnet_topology_epoch()` | `:11564` / `:11553` |
| `MeshNode::serve_rpc_subnet_exported(service, handler, OrgAdmission, SubnetExportBinding, provider_policy)` | `mesh_rpc.rs:3272` — the D7 four-plane composition; re-verifies `verify_subnet_export` per call (`:1134`) |
| Observability: `protected_relay_stats()`, `subnet_boundaries()`, `subnet_gateway_contexts()` | `:11973`, `:11993`, `:12002` — the latter two documented as *not* a coherent pair |

**Issuance is pure and offline.** Every `try_issue`
(`SubnetGrant::try_issue` `auth.rs:306`, `SubnetIssuerGrant::try_issue`
`:527`, `SubnetRevocationFloor::try_issue` `:707`, plus the control-fact
constructors) takes only an `&EntityKeypair` — no node, no async. Issuance
needs no mesh, which is exactly why it does not need a binding (§Non-goals).

**Nothing issues credentials outside test fixtures today.** The `net
subnet` CLI is inspection only (`show|ls|tree`,
`cli/src/commands/subnet.rs:26-34`); `net gateway export` validates flags
and then errors ("preview", `cli/src/commands/gateway.rs:41-45`); `net org`
has issuance verbs, `net subnet` has none. Until Workstream T lands, the
only sources of a `SubnetGrant` are the Rust test fixtures.

**One deliberately unresolved trace.** The verifier-side admission APIs are
public (`issue_subnet_challenge` `mesh.rs:11585`, `admit_subnet_session`
`:11602`, `withdraw_subnet_admission` `:12038`), but this survey did not
pin the production **subject-side** flow — where a connecting node loads
its own `SubnetCredentialSet` and answers a challenge on the wire. R0
traces the live owner before any binding signature freezes. Per the
substrate plan's own rule, implementation must not guess or create a
duplicate state owner.

## Ground truth per language (as surveyed 2026-08-04)

| Surface | Subnet auth today | Subnet topology today | House style (load-bearing receipts) |
|---|---|---|---|
| **Rust core** | ✅ complete (above) | `SubnetId`/`SubnetPolicy`, `peer_subnets`, `Visibility` | — |
| **Rust SDK** (`sdk/`, `net-mesh-sdk`, lib `net_sdk`) | **None.** Zero subnet-auth types reachable; `sdk/src/subnets.rs:62-69` re-exports exactly six topology types + `Visibility` | `MeshBuilder::subnet` / `subnet_policy` (`sdk/src/mesh.rs:189/:202`) | Org facade anatomy is the template: re-export spine (`org/types.rs`), validated collection (`OrgCredentials`), domain error separate from `SdkError`, `#[doc(hidden)]` node-based seam + `Mesh` one-liner ("one pipeline, two doors", `org/client.rs:211-219`), provisioning module with its own plain error (`org/provision.rs:16-19`), serve verb, fixtures + a `super::`-importing re-export compile guard (`org.rs:108-163`). Subnet surfaces are bundled under the `net` feature by declared policy (`sdk/Cargo.toml:143-151`) |
| **TS / Node** (`bindings/node` → `@net-mesh/core`; `sdk-ts` → `@net-mesh/sdk`) | **None** | `MeshOptions.subnet`/`subnetPolicy` (`src/lib.rs:1159/:1163`), conversion helpers only in `src/subnets.rs` (no exported classes); sdk-ts `MeshNodeConfig.subnet`/`subnetPolicy` forwarded through the **explicit field map** (`sdk-ts/src/mesh.ts:218/:224/:313-324` — not spread; every new field is an edit here) | napi 3, generated `index.d.ts` (never committed), hand-written sibling TS (`errors.ts` native-free with `classifyError`; org classes at `errors.ts:199-317`), `#[napi] async fn` → Promise, stable message prefixes single-sourced in Rust, explicit `close()`, org verbs exposed from `@net-mesh/core/org` — **no sdk-ts wrapper** |
| **Python** (`bindings/python` → wheel `net-mesh`, imports `net`; `sdk-py` → `net_sdk`) | **None** | `subnet=`/`subnet_policy=` kwargs (`src/lib.rs:1220-1221`), helpers in `src/subnets.rs` ("mirror of bindings/node", `:1-2`); **`sdk-py` has no subnets module and `MeshNode.__init__` drops the subnet kwargs** (`sdk-py/src/net_sdk/mesh.py:96-111`) — pre-existing gap, closed by P | PyO3 + maturin, sync-with-GIL-release default, `create_exception!` per domain carrying the wire string, `close()` + `__enter__`/`__exit__`, zero `__del__`, hand-maintained `_net.pyi` with two drift guards; **`org` is not in the Python default features** (`Cargo.toml:33-52`) while it is in Node's — flag placement is decided per binding, not assumed |
| **Go** (`go/`, module `github.com/ai-2070/net/go`) | **None** (`gateway` has zero hits in `go/*.go`) | `go/subnets.go` is 34 lines of pure config types crossing as JSON inside `MeshConfig` (`mesh.go:125/:129`); single-mesh test discipline stated in `go/subnets_test.go:1-6` | cgo over hand-written headers (no cbindgen, house rule); base surface in `go/net.h` guarded by `header_parity_test.go`; separate cdylibs declare prototypes inline in the owning Go file; config structs + `X`/`XWithOptions`, zero functional options; sentinels + typed `Kind` errors; `Close()` + `SetFinalizer`; Variant-A trampoline for callbacks; **`bindings/go/net/` reference tree is dead — do not mirror into it** (org G7 precedent) |
| **C** (`include/` — the header directory *is* the SDK) | **None** (no `net_subnet_*` symbol anywhere) | subnet appears only as JSON fields inside `net_mesh_new` config | Ten headers over six libraries (`include/README.md:20-31`); per-cdylib ABI stamp + own negative error namespace from −1 (`net_org.h` precedent — do **not** splice into `net_error_t`); consumed typed mesh-arc; double-pointer frees; consumer-mallocs/Rust-frees buffers; `ffi_guard!` + plain `Box::from_raw` in Go FFI crates (HandleGuard is core-only) |
| **CLI** | Inspection only; **no issuance** | `net subnet show\|ls\|tree`, `net gateway stats\|exports\|export(preview)` | `net org (keygen\|issue-cert\|grant-…)` is the issuance template |

## Non-goals

- **Changing subnet authorization semantics.** The substrate's decisions —
  fail-closed verification, exact attachments, wholesale-replace install,
  bounded transition checks — are fixed. Binding code is marshaling.
  **Review rule (inherited verbatim):** an SSDK PR touching `bindings/` or
  `go/` may contain marshaling only; anything resembling an authority
  decision cites the Rust function it defers to. For a transport-admission
  surface this rule is doubly binding.
- **Issuance or administration in any binding.** `try_issue` requires the
  authority root (or delegated issuer) keypair, and root keys never cross a
  language boundary. Credentials are minted by `net subnet …` CLI verbs
  (Workstream T) and arrive as files/bytes. A language SDK that could sign
  a grant would be a second issuance path — the same thing the org plan
  refused, with a stronger key behind it.
- **Exposing the verifier internals.** `issue_subnet_challenge`,
  `admit_subnet_session`, `withdraw_subnet_admission`, the challenge/context
  stores, `verify_credential_set`, and the gateway compile functions are
  substrate wiring. No binding drives admission by hand.
- **Authoring control facts in bindings.** Facts are signed artifacts;
  authoring is issuance. Bindings only *apply* received fact bytes.
- **Topology administration.** `advance_subnet_topology_epoch` is an
  operator action with substrate-wide invalidation consequences; it stays
  Rust/CLI-side in v1.
- **A per-packet or per-session policy surface, negative ACLs, recursive
  issuers, cross-authority federation, `ADMIN`** — inherited whole from
  `SUBNET_AUTH_PLAN.md` §11.
- **Retiring or renaming the topology surface.** `subnet`/`subnet_policy`
  config, `SubnetPolicy` rules, and visibility strings stay exactly as
  shipped; SSDK adds the authority plane beside them.
- **wasm/browser TS.** napi is Node-only — the standing category line.

## What ships

**Per language: six concepts, three config knobs, three provisioning verbs,
one serve verb.**

Concepts (names per language convention, semantics identical):

1. `SubnetRights` — `{attach, route, export}`; unknown bits/names refuse.
2. `SubnetRef` — authority-qualified target (32-byte authority EntityId +
   compact path). **Visibly distinct from the topology subnet id in every
   language** (§D2).
3. `SubnetCredentialSet` — direct or one-hop credential bytes; opaque,
   fixed-size, decoded and verified only in Rust.
4. `SubnetAuthorityConfig` — `{authority, roots[], maximumGrantLifetimeSecs}`.
5. `SubnetBoundarySet` — `{authority, topologyEpoch, boundaries[]}`.
6. `SubnetExportBinding` — `{authority, path, topologyEpoch}` for the
   exported provider boundary.

Config (mesh construction):

- `subnetAuthorities: SubnetAuthorityConfig[]`
- `subnetAttachment: <topology path>` (defaults to the topology `subnet`)
- `subnetControlChannel: <channel name>`

Provisioning (node handle; startup-shaped, plain errors — §D5):

- `installSubnetGatewayCredentials(credentialSets: bytes[])` — wholesale
  replace, documented as such everywhere.
- `declareSubnetBoundaries(boundarySet)`
- `applySubnetControlFact(factBytes) -> outcome` — the one door for floors
  and descriptive facts alike.

Serve:

- `serveSubnetExported(service, orgAccess, exportBinding, handler)` — the
  D7 composition verb over `serve_rpc_subnet_exported`; handler receives
  the org caller exactly as `serveOrg` does.

**Plus, cross-cutting:** a single-sourced `subnet:` error vocabulary with a
golden-vector fixture (§D5, X1), a deterministic BMW-scenario generator +
manifest for live cells (§X2), and drift guards per binding (§X3).

**What this plan does NOT ship (deferred, §Deferred):** binding-side
issuance, observability counters in bindings, manual admission APIs, an
async Python pair, floor-refresh daemons, and any `@net-mesh/sdk` /
`net_sdk` (Python) ergonomic wrapper beyond config plumbing.

## Doctrine

- **Topology is not authority — now with types.** The substrate holds this
  line with two Rust types; every binding must hold it with two visibly
  different names (§D2). A reviewer seeing a topology value passed where an
  authority-qualified value is required should not need context to reject it.
- **No logic in bindings.** Every verification already happened in Rust.
- **No new secret crosses the boundary — because none needs to.** Subnet
  grants, issuer grants, floors, and facts are public signed artifacts and
  cross as canonical wire bytes. The only secrets in the system are the
  node's own identity (already crossing as `identity_seed` under existing
  rules) and issuer/root keys (which never cross; issuance is CLI-only).
  This is *stronger* than org, which had to invent a path-not-bytes rule
  for its audience secret. SSDK must not weaken it by adding one.
- **Wire vocabulary is single-sourced.** The `subnet:` kinds are generated
  from one Rust function and pinned by one fixture consumed by five suites.
- **Fail-closed, without counterfeiting.** A binding that cannot classify
  an error reports `unknown` — never a success, never a canonical kind it
  could not establish (org §D5a, inherited).
- **Provisioning is the surface.** The org plan discovered late that verbs
  without provisioning are inert ("the classes exist" is not "the surface
  works"). SSDK ships the installs *first-class from day one* — they are
  three of its seven verbs, not an afterthought.

## SDK design decisions

### D0 — Three roles, one surface

The substrate serves five roles; the SDK exposes three and deliberately
hides two:

| Role | SDK exposure |
|---|---|
| Attaching subject (holds `ATTACH`, joins protected sessions) | config (`subnetAuthorities` + `subnetAttachment`) + R0-traced credential loading |
| Gateway (self-held `ROUTE`/`EXPORT`, declared boundaries) | the three provisioning verbs |
| Exported provider | `serveSubnetExported` |
| Verifier (challenge/admit/withdraw) | **hidden** — substrate wiring |
| Issuer (authority root) | **hidden** — CLI only (Workstream T) |

### D1 — The Rust facade comes first, and it is mostly a spine

Unlike org, nothing here is generic and nothing needs a byte-dual: the core
API is already byte- and struct-shaped. R is therefore thinner than org's R
— a re-export spine plus seams — but it is still the gate, because every
binding reaches the same functions through it and the error vocabulary must
be frozen before any binding parses it.

The spine grows inside `sdk/src/subnets.rs` (not a new module): the module
already carries the topology six + `Visibility` and its doc already frames
it as the one-stop security-surface import. It gains a clearly sectioned
auth block:

```rust
// sdk/src/subnets.rs — auth section (spine, no invention)
pub use net::adapter::net::subnet::{
    SubnetAuthError, SubnetAuthorityConfig, SubnetBoundarySet,
    SubnetControlFact, SubnetControlOutcome, SubnetCredentialSet,
    SubnetDescriptor, SubnetExportBinding, SubnetFactKind, SubnetGrant,
    SubnetIssuerGrant, SubnetRef, SubnetRevocationFloor, SubnetRights,
    TopologySubnetId,
};
```

Builder + seams (the org "one pipeline, two doors" shape):

```rust
impl MeshBuilder {
    pub fn subnet_authority(mut self, config: SubnetAuthorityConfig) -> Self;   // repeatable
    pub fn subnet_attachment(mut self, path: TopologySubnetId) -> Self;         // direct field
    pub fn subnet_control_channel(mut self, channel: ChannelName) -> Self;      //   write in build()
}
// node-based seams, #[doc(hidden)], + impl Mesh one-liners
pub fn install_subnet_gateway_credentials_node(node: &Arc<MeshNode>, sets: &[SubnetCredentialSet])
    -> Result<(), SubnetProvisionError>;
pub fn declare_subnet_boundaries_node(node: &Arc<MeshNode>, set: SubnetBoundarySet)
    -> Result<(), SubnetProvisionError>;
pub fn apply_subnet_control_fact_node(node: &Arc<MeshNode>, fact: &[u8])
    -> Result<SubnetControlOutcome, SubnetProvisionError>;
```

`subnet_attachment` forwards through a direct `MeshNodeConfig` field write —
the `configured_identity` precedent at `sdk/src/mesh.rs:303`; do not add a
core `with_` method just for symmetry.

`SubnetProvisionError` follows `OrgProvisionError` exactly (plain error,
not folded into call-path domains, rationale quoted from
`org/provision.rs:16-19`).

**Feature placement:** spine + config + provisioning behind the existing
`net` flag (the declared bundling policy for subnet surfaces,
`sdk/Cargo.toml:143-151`); `serve_subnet_exported` behind `net + cortex`
beside the org verbs. **No new feature flag.**

**R0 (survey, blocks signature freeze):** trace the production subject-side
attach flow (how a connecting node's `SubnetCredentialSet` reaches the
verifier's challenge). Outcome A — a config/API owner exists: the SDK
forwards it. Outcome B — the e2e suite drives admission manually and no
wire owner exists yet: the SDK v1 ships config + gateway + serve only, and
the subject-side loading surface is named substrate follow-up work, not
invented here. Either way the finding is recorded in this document before
N/P/C/G start.

### D2 — Two subnet identities stay two types, in every language

The substrate's founding correction was that a topology coordinate had been
wired as authority (`SUBNET_AUTH_PLAN.md` §1.1, §1.6). The binding-level
regression of that bug is a shared type or a shared parameter shape. Locked
naming:

| Language | Topology (existing, unchanged) | Authority-qualified (new) |
|---|---|---|
| Rust SDK | `SubnetId` / `TopologySubnetId` alias | `SubnetRef { authority, path }` |
| TS | `SubnetId { levels }` (`sdk-ts/src/subnets.ts:39`) | `SubnetRef { authority: Buffer, levels: number[] }` |
| Python | `list[int]` levels | `SubnetRef` dict `{authority: bytes, levels: list[int]}` |
| Go | `[]uint32` in `MeshConfig.Subnet` | `SubnetRef { AuthorityHex string; Levels []uint32 }` |
| C | JSON config field | `net_subnet_ref_t { uint8_t authority[32]; uint32_t levels[4]; uint8_t depth; }` |

Every language carries a refusal test: the topology shape is not accepted
where a `SubnetRef` is required (compile-level where the type system
allows, runtime refusal where it does not).

### D3 — The credential boundary: bytes in, nothing back

Public signed artifacts cross as canonical wire bytes (`Buffer` / `bytes`
/ `[]byte` / `(ptr,len)`), decoded and verified only in Rust, exactly as
org membership certs do. Fixed `WIRE_SIZE` constants plus
trailing-byte-rejecting decoders make length bugs loud. No binding re-encodes
a signed artifact as JSON; no binding returns decoded credential fields
except through explicitly chosen inspection surfaces (deferred — v1 has
none; a grant is provisioned, not browsed).

There is **no path-based loader in the bindings** for subnet credentials
— unlike the org audience secret, there is nothing here that must be kept
out of GC'd memory. Applications read the CLI-minted files themselves and
hand bytes over. (T's file format is raw wire bytes, one artifact per
file, so this is `fs.readFile` — not an envelope parser.)

### D4 — Provisioning verbs: exact semantics, stated everywhere

- `installSubnetGatewayCredentials` is **wholesale replace** — the
  substrate compiles and atomically publishes the whole set
  (`mesh.rs:11802`); an "add one" call shape would misrepresent it. Every
  language's doc states: pass the complete current set; refresh recomputes;
  stale rights never accumulate.
- `declareSubnetBoundaries` is mandatory for protected forwarding and also
  wholesale.
- `applySubnetControlFact` is the **one door** for floors and descriptive
  facts (the substrate calls it "the ONE entry every arrival path
  shares"). Bindings do not expose `apply_subnet_floor` separately — a
  typed second door would invite divergent behavior between arrival paths,
  which is precisely what the one-door design exists to prevent. The
  outcome enum crosses as a string (`applied`, `stale`, `ignored`, … — the
  exact vocabulary frozen in R from `SubnetControlOutcome`).
- All three surface **plain provisioning errors** carrying the `subnet:`
  kind string — not call-path error domains (org §D9 rule, inherited).

### D5 — Error vocabulary: one source, one fixture, five consumers

New R item: `SubnetAuthError::to_wire()` (or an SDK-side single-source
function if core should stay presentation-free) emitting
`subnet:<kind>[ : k=v …]` for every variant, with the kind strings being
exactly the snake_case reason codes the substrate already renders
(`auth.rs:215`) and `SUBNET_AUTH_PLAN.md` D9 enumerates (`missing_grant`,
`scope_not_ancestor`, `wrong_topology_epoch`, `presentation_replayed`, …).

- Fixture: `tests/cross_lang_subnet/error_vectors.json`, generated by a
  deterministic example (`gen_subnet_error_fixtures`), consumed by Rust,
  Node (native-free, via `errors.ts`), Python (importorskip, pure-Python
  parser), Go (golden-vector test), and a C/FFI-crate test. **Lands before
  any binding work** (X1 precedent).
- Node: `ERR_SUBNET_PREFIX` + `SubnetError` classes in the native-free
  `errors.ts`, `classifySubnetError`, a branch in `classifyError`.
- Python: `create_exception!` family carrying the wire string in the
  message, plus a pure-Python `parse_subnet_error`.
- Go: `SubnetError{Kind, Message}` + sentinels + `subnetErrorFromCode`.
- C: negative code **and** `out_err` string, both.
- The fifth class is `unknown` and impersonates nothing (org §D5a,
  inherited unchanged).
- **Remote denials stay coarse.** A caller refused at an exported boundary
  sees the existing org/nRPC admission vocabulary; the detailed `subnet:`
  reasons are local/audit-side only. Enriching a remote refusal would
  build a credential oracle (substrate D9 keeps reasons internal; the org
  plan's E2.2 rule carries over).

### D6 — The serve verb composes; it does not collapse

`serveSubnetExported(service, orgAccess, exportBinding, handler)` marshals
directly onto `serve_rpc_subnet_exported`. The handler signature, TSFN /
`spawn_blocking` bridges, dispatcher-trampoline, JSON typed layer, and
disposal contracts are **copied from the org serve implementations, not
reinvented** — same files, same shapes, same two-stage timeouts.

What the docs must say in every language: registration checks the binding
at serve time and *every call re-verifies the export* against the current
gateway context set — so revoking the gateway's `EXPORT` credential or
raising a floor darkens the provider without re-registration, and neither
org admission nor the export binding substitutes for the other (removing
either independently denies; witnessed in the substrate's
`subnet_org_boundary.rs`, asserted per-binding only as marshaling).

### D7 — The C ABI, and Go over it

New crate `bindings/go/subnet-ffi` → `libnet_subnet`, new hand-written
`include/net_subnet.h`, own ABI stamp `0x0001`, own error namespace from
−1 — the org-ffi precedent applied verbatim, including its eight recorded
corrections (plain `Box::from_raw` + `ffi_guard!`, no HandleGuard; a
standalone header, never spliced into `net_error_t`; consumed mesh-arc via
`net_mesh_arc_clone`; double-pointer frees; response buffers
consumer-malloc'd/Rust-freed; the Cargo feature list copied from
org-ffi/compute-ffi **verbatim** against the cfg-unification hazard).

```c
#define NET_SUBNET_ABI_VERSION 0x0001
#define NET_SUBNET_RIGHT_ATTACH 0x01
#define NET_SUBNET_RIGHT_ROUTE  0x02
#define NET_SUBNET_RIGHT_EXPORT 0x04
/* mirrored by a Rust<->header numeric test inside subnet-ffi, the
 * net_transport.h precedent — rights are a u8, never JSON. */

typedef struct { uint8_t authority[32]; uint32_t levels[4]; uint8_t depth; } net_subnet_ref_t;

int net_subnet_install_gateway_credentials(net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* const* set_ptrs, const size_t* set_lens, size_t set_count,
    char** out_err);
int net_subnet_declare_boundaries(net_compute_mesh_arc_t* mesh_arc,
    const char* boundary_set_json, char** out_err);
int net_subnet_apply_control_fact(net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* fact_ptr, size_t fact_len,
    char** out_outcome, char** out_err);
int net_subnet_serve_exported(net_compute_mesh_arc_t* mesh_arc,
    const char* service_ptr, size_t service_len,
    int org_access, const net_subnet_ref_t* export_ref, uint32_t topology_epoch,
    uint64_t handler_id,
    NetSubnetServeHandle** out_handle, char** out_err);
/* + set_handler_dispatcher / reserve_handler_id / serve_handle_close/_free,
 *   net_subnet_free_cstring, net_subnet_abi_version / check_abi_version —
 *   all org-ffi shapes. */
```

Marshaling split: wire artifacts as `(ptr,len)`, the boundary set as JSON
(config-shaped, matching the house JSON-for-structured rule), rights as
`uint8_t`, `SubnetRef` as the typed POD. Mesh **config** additions
(`subnet_authorities`, `subnet_attachment`, `subnet_control_channel`)
ride the existing `net_mesh_new` JSON config in base `libnet`
(`src/ffi/mesh.rs` `MeshNewConfig`) — new JSON fields, **zero new base-C
symbols**, so `net.go.h`/`go/net.h` need only comment updates and
`header_parity_test.go` stays quiet.

Go: new `go/subnet_auth.go` — **not** grown into `go/subnets.go`, so the
topology/authority split is visible in the file layout itself — with the
inline cgo prelude mirroring `net_subnet.h`, `MeshConfig` gaining
`SubnetAuthorities []SubnetAuthorityConfig` / `SubnetAttachment []uint32`
/ `SubnetControlChannel string` (json tags), free functions
`InstallSubnetGatewayCredentials` / `DeclareSubnetBoundaries` /
`ApplySubnetControlFact` / `ServeSubnetExported[Req,Resp]`, and the
Variant-A trampoline with a unique `goNetSubnet*` export prefix.

### D8 — Rights representation per language

Canonical vocabulary `{attach, route, export}`, frozen in R, pinned by the
X1 fixture (which includes an unknown-name row and an unknown-bit row):

| Language | Shape | Precedent |
|---|---|---|
| Rust | `SubnetRights` bitflags | core |
| TS | `('attach'\|'route'\|'export')[]` | `TokenScope` crosses as string array |
| Python | `list[str]` | same |
| Go | `SubnetRights uint8` + `SubnetRightAttach/Route/Export` consts | numeric, close to the wire |
| C | `uint8_t` + `NET_SUBNET_RIGHT_*` defines | numeric mirror test |

Unknown names/bits refuse in every language — `try_from_bits` semantics
are not softened at any boundary.

### D9 — Config plumbing: every constructor is a separate code path

The org work's most-repeated lesson (`configured_identity` omitted three
separate times): a node-level config field must be wired into **each**
language's constructor and witnessed there. The exact edit list per field
(`subnetAuthorities`, `subnetAttachment`, `subnetControlChannel`):

| Surface | Files |
|---|---|
| Rust SDK | `sdk/src/mesh.rs` builder + `build()` translation |
| Node | `MeshOptions` (`bindings/node/src/lib.rs:1136-1226`) + consumption in `create` (`:1515-1522` region) |
| sdk-ts | `MeshNodeConfig` (`sdk-ts/src/mesh.ts:190-233`) **and the explicit field map** (`:313-324`) |
| Python | kwargs + signature (`bindings/python/src/lib.rs:1210-1273`) + consumption (`:1302-1309` region) + `_net.pyi` |
| sdk-py | `MeshNode.__init__` (`sdk-py/src/net_sdk/mesh.py:96-111`) — **which also finally forwards the existing topology `subnet`/`subnet_policy` kwargs it drops today**; that closure is in scope for P |
| Go | `MeshConfig` (`go/mesh.go:104-232`) json tags |
| C | `MeshNewConfig` (`src/ffi/mesh.rs:413` region) + header doc comment |

Each constructor lands with a witness: a mesh built with authorities
configured actually fails closed / succeeds exactly as the same config
does through the Rust builder.

## The parity matrix (the contract)

A language column is "done" when every row is ✅. All cells are planned
today — **nothing in this matrix exists yet, in any language, including
Rust SDK**. Cross-reference from `net_sdk::subnets`' module doc when R
lands, and add rows to `docs/data/capabilities/` (+ regenerate
`coverage.md`) as each binding ships — `not exposed` is the honest cell
value until then.

| Capability | Rust SDK | TS | Python | Go | C |
|---|---|---|---|---|---|
| Auth types spine reachable (compile-guard test) | — | n/a | n/a | n/a | n/a |
| Two subnet identities are distinct types/shapes (refusal witnessed) | — | — | — | — | — |
| Config: `subnetAuthorities` (roots; empty fails closed, witnessed) | — | — | — | — | — |
| Config: `subnetAttachment` | — | — | — | — | — |
| Config: `subnetControlChannel` | — | — | — | — | — |
| Credential sets cross as wire bytes (no JSON re-encode) | — | — | — | — | — |
| `installSubnetGatewayCredentials` — wholesale replace documented + witnessed | — | — | — | — | — |
| `declareSubnetBoundaries` | — | — | — | — | — |
| `applySubnetControlFact` — one door; outcome vocabulary | — | — | — | — | — |
| `serveSubnetExported` — org admission + export binding compose | — | — | — | — | — |
| Rights vocabulary `{attach,route,export}`; unknown refuses | — | — | — | — | — |
| `subnet:` error vocabulary + golden fixture | — | — | — | — | — |
| `unknown` fallback that impersonates no kind | — | — | — | — | — |
| Provisioning errors are plain, not call domains | — | — | — | — | — |
| Live cell from the scenario manifest (§X2) | — | — | — | — | — |
| Drift guard (stub / header / ABI stamp) | n/a | — | — | — | — |
| sdk-py forwards subnet kwargs (gap closure) | n/a | n/a | — | n/a | n/a |

## Workstreams

### Workstream R — Rust SDK facade (blocks everything)

- **R0 — the subject-side trace** (§D1). Recorded in this doc before
  binding signatures freeze.
- **R1 — the spine.** Auth section in `sdk/src/subnets.rs`; root
  re-exports for the load-bearing five (`SubnetRights`, `SubnetRef`,
  `SubnetAuthorityConfig`, `SubnetCredentialSet`, `SubnetExportBinding`);
  a `subnet_auth_sdk_reexport` compile-guard test in the `org.rs:108-163`
  shape (imports via `super::`, then actually issues each credential
  through the facade using fixture keys).
- **R2 — builder + seams.** §D1's three builder methods, three node-based
  provisioning seams + `Mesh` one-liners, `SubnetProvisionError`.
- **R3 — the `subnet:` vocabulary.** One function, every kind frozen here
  and nowhere else; outcome-string vocabulary for `SubnetControlOutcome`.
- **R4 — serve.** `Mesh::serve_subnet_exported` (+ node-based seam for
  bindings) over `serve_rpc_subnet_exported`, reusing the org serve
  bridge shapes.
- **R5 — fixtures + scenario generator.** `write_subnet_scenario` behind
  the `fixtures` feature + `examples/gen_subnet_scenario.rs`: mints the
  §6 BMW two-vehicle world (authority roots, vehicle/gateway/camera
  grants, an exported world-model boundary, a partner org) to a directory
  with seeded identities and a `manifest.json` contract — the org X2
  generator pattern, reusing the substrate e2e fixtures where possible.
- **R6 — docs.** Module doc states the role split (§D0), the
  wholesale-replace and one-door contracts (§D4), and quotes the
  topology-vs-authority doctrine.

**Acceptance:** a Rust integration test builds two nodes from the
generated manifest through **SDK surfaces only** (builder config +
provisioning verbs + `serve_subnet_exported`), runs one protected-boundary
call, and proves a denial when the gateway credential set is replaced
without `EXPORT` — i.e. exactly what a binding will do.

### Workstream T — CLI issuance verbs (blocks live cells everywhere)

`net subnet` grows issuance beside its inspection verbs, following `net
org`'s shapes and secret discipline (0600 files, `ScrubbedBytes`-style
handling for root seeds, no key material in logs):

- `net subnet keygen` — authority root keypair to disk.
- `net subnet issue-grant` — leaf `SubnetGrant` (direct or via issuer).
- `net subnet issue-issuer` — one-hop `SubnetIssuerGrant`.
- `net subnet issue-floor` — `SubnetRevocationFloor`.
- `net subnet issue-fact` — descriptor / gateway advertisement / export
  policy.
- `net subnet inspect <file>` — decode + verify a credential file
  (public data only).

File format: raw canonical wire bytes, one artifact per file. Scope `0`
renders as the explicit authority-root scope, never as empty/unscoped
(substrate §8 risk, honored at the tool).

T is Rust work reviewed under CLI conventions; it does not gate R but
gates every language's live cell (X2) and any real deployment.

### Workstream N — Node

- **N1** config fields in `MeshOptions` + `create` (+ witness).
- **N2** `installSubnetGatewayCredentials` / `declareSubnetBoundaries` /
  `applySubnetControlFact` on `NetMesh` (sync, node-seam calls).
- **N3** `serveSubnetExported` — TSFN bridge copied from `src/org.rs`
  serve; `SubnetServeHandle` with `close()`; documented teardown order.
- **N4** `errors.ts` subnet classes + `classifySubnetError` (native-free)
  + `classifyError` branch; fixture test with no cdylib.
- **N5** sdk-ts: `MeshNodeConfig` fields + the explicit map; `SubnetRef`
  type in `sdk-ts/src/subnets.ts` beside the topology types with the
  distinct-shape refusal test.
- **N6** tests: refusal paths through the real napi boundary (malformed
  set bytes, unknown right, topology-shape-where-ref-required), plus the
  live cell (§X2) from the manifest.

### Workstream P — Python

- **P1** constructor kwargs + `_net.pyi` (+ witness).
- **P2** the three provisioning functions + `serve_subnet_exported`
  (pyfunction, `spawn_blocking` bridge per `org_serve.rs`), handle with
  `close()`/`__enter__`/`__exit__`.
- **P3** exception family + pure-Python `net/subnet_auth.py` parser
  mirroring `org.py`; `__init__.py` conditional import block; stub drift
  entries.
- **P4** sdk-py: forward the topology subnet kwargs it drops today
  **and** the new auth kwargs through `net_sdk.MeshNode`.
- **P5** tests: fixture vectors (no wheel), binding refusals, live cell.
- Async pair: deferred exactly as org P6 (entry: an `async def` consumer).

### Workstream C — the header IS the SDK

- **C1–C4** per §D7: `bindings/go/subnet-ffi` crate (feature list copied
  verbatim), `include/net_subnet.h`, dispatcher + reserved id, ABI stamp,
  numeric-mirror tests (rights defines, error codes), `include/README.md`
  table row.
- **C5** mesh-config JSON fields in `src/ffi/mesh.rs` (base libnet — no
  new symbols; comment updates mirrored to `net.go.h`/`go/net.h`).
- **Acceptance:** a C example provisions a gateway from CLI-minted files,
  serves one exported capability, and valgrind reports no leak across
  create/serve/free.

### Workstream G — Go over the C ABI

- **G0** config fields in `MeshConfig` + witness (the constructor-gap
  lesson, pre-empted this time).
- **G1** workspace + CI registration (`members`, the one-shot cargo build,
  ffi-clippy/ffi-tests matrices, rustdoc job).
- **G2** `go/subnet_auth.go`: types (§D2/§D8), the three installs,
  `ServeSubnetExported[Req,Resp]`, `SubnetError` + sentinels, trampoline.
- **G3** tests: config acceptance + refusal + error mapping (single-mesh
  discipline per `go/subnets_test.go:1-6`; multi-node semantics stay in
  the Rust suite), golden vectors, live cell on CI (`RUN_INTEGRATION_TESTS=1`).

### Workstream X — cross-cutting conformance

- **X1 — error fixture.** `tests/cross_lang_subnet/error_vectors.json`
  (kinds, outcome strings, rights vocabulary, unknown rows) generated
  from R3's single source. **Lands before N/P/C/G.**
- **X2 — the live matrix.** Phase 1: R5's generator + manifest + the Rust
  from-disk cell (in R's acceptance). Phase 2 (CI-only, owed after two
  languages ship): per-language manifest-driven harnesses — provider in X,
  caller in Y, across the exported boundary; minimum Rust↔Go, Rust↔Node,
  Rust↔Python, and one non-Rust pair.
- **X3 — drift guards.** Node `abi_stability.test.ts` prefixes; Python
  `test_stub_drift` / `test_pyi_stub_coverage` entries; Go ABI pin in
  `init()` + numeric mirrors; the C header row in `include/README.md`;
  `docs/data/capabilities/` rows + `capability_records.py --check`.

## Rollout order

1. **R** (with R0 recorded) — nothing compiles against the surface until
   it lands.
2. **X1** — the vocabulary fixture, so every binding is written against
   one frozen contract.
3. **T** — issuance verbs; unblocks live cells and real consumers.
4. **The named language workstream(s), in demand order.** Precedent
   applies on both sides: the org plan gated languages on named consumers,
   and then the gate was lifted by direction because org auth is the
   load-bearing auth surface. Subnet auth is the transport-admission
   surface of the same library, so the same ruling is plausible — but this
   plan does not presume it. Absent direction, N and P go first (both
   binding crates already depend on `net-mesh-sdk`; both are the proven
   cheap template pair), then **C + G as one inseparable reviewed unit**.
5. **X2 phase 2** once two languages exist; **X3** rides each binding.

## Test strategy

- **Unit (per binding):** marshaling only — bytes length/shape refusals,
  rights parsing, `SubnetRef` vs topology-shape refusal, error
  classification.
- **Golden fixtures:** X1, five consumers, deterministic regeneration.
- **Integration (per binding):** provisioning + serve against a real
  in-process mesh in each language's own harness (vitest serialized suite,
  pytest `mesh_pair`, Go `meshHandshakePair`), refusal-first.
- **Cross-language:** X2 from the manifest.
- **Security-shaped, every language:**
  - empty `subnetAuthorities` + a protected assertion fails closed;
  - a topology value cannot stand in for a `SubnetRef`;
  - install-without-`EXPORT` darkens the exported provider (coarse remote
    denial), and re-install restores it — the wholesale-replace witness;
  - an unclassifiable error surfaces as `unknown`, never a canonical kind;
  - no binding API exists that signs anything (absence test, per the
    org "no secret-bytes constructor" precedent).
- **Substrate semantics are NOT re-tested in bindings** — the removal
  matrices, hierarchy truth tables, and forwarding witnesses live in the
  Rust suites; binding tests assert marshaling and the documented
  contracts only.

## Locked decisions

1. **Two subnet identity types never merge at any boundary** — distinct
   names and shapes in every language, with refusal witnesses (§D2).
2. **All subnet credentials and facts cross as canonical wire bytes**; no
   binding re-encodes or partially decodes a signed artifact.
3. **No issuance in any binding; root/issuer keys never cross.**
   Issuance is `net subnet …` (T) and the Rust `fixtures` surface only.
4. **One control-fact door** (`applySubnetControlFact`); no per-kind or
   typed-floor setters in bindings.
5. **Gateway install and boundary declaration are wholesale-replace**,
   documented and witnessed in every language.
6. **The `subnet:` vocabulary is generated from one Rust function** and
   pinned by one fixture consumed by five suites; kinds are the
   substrate's snake_case reason codes, not a new taxonomy.
7. **`unknown` is a fifth class and impersonates nothing** (org §D5a).
8. **Remote export denials stay coarse**; detailed subnet reasons are
   local/audit-side. No credential oracle.
9. **Provisioning errors are plain errors**, not call-path domains.
10. **subnet-ffi gets its own header, ABI stamp (`0x0001`), and error
    namespace from −1**; consumed mesh-arc; double-pointer frees; feature
    list copied verbatim from the sibling FFI crates.
11. **Bindings contain marshaling only**; the review rule is enforceable
    at PR time, and for this surface an "authority decision in a binding"
    is a rejection, not a nit.
12. **Rights vocabulary `{attach, route, export}` is frozen**; unknown
    names/bits refuse everywhere; no language invents a fourth right.

## Risks

| Risk | Containment |
|---|---|
| The two subnet identities get conflated at a binding boundary — the exact bug class the substrate plan exists to correct | Locked decision #1; distinct names/shapes per §D2; per-language refusal tests; review rule treats a shared shape as a design error, not style |
| No issuance tooling exists, so bindings ship "complete" but nothing real can mint a credential (the org §D9 lesson, one layer down) | T is a first-class workstream gating every live cell; the parity matrix carries a live-cell row per language so a column cannot read done without it |
| The subject-side attach flow has no traced production owner | R0 blocks signature freeze; outcome recorded in this doc; v1 narrows rather than guesses (§D1) |
| Wholesale-replace install misread as additive → silently dropped rights | Contract stated in every language's doc + the install/darken/restore witness per binding |
| A fourth constructor omits a config field (the `configured_identity` failure mode, three prior instances) | §D9 lists every file per field; each constructor lands with a fail-closed witness; G0 exists specifically |
| subnet-ffi feature list diverges from the standalone build → cfg-gated field-offset UB | Copy the sibling crates' list verbatim + their warning comment; `-p net-subnet-ffi` in the single CI cargo invocation |
| The substrate review doc's OPEN header creates ambiguity about what SSDK may rely on | §Status states the position; any reopened finding on a wrapped seam pauses the affected workstream |
| Error-kind drift across five languages | X1 fixture + X3 guards; a rename fails five suites |
| Serve-bridge deadlocks (JS main thread / GIL) | Reuse the org bridges verbatim — two-stage TSFN timeout, `spawn_blocking` + `Python::attach`, bounded Go trampoline; nothing is invented here |
| Scope `0` rendered as "empty/unscoped" in a tool or binding doc | T renders it as the explicit authority-root scope (substrate §8); binding docs quote the same line |

## Effort

~3,300 LoC. R ~450 (spine + seams + vocabulary + generator, incl. tests);
T ~600 (six verbs + file handling + tests); N ~550 (napi module + errors.ts
+ sdk-ts types + tests); P ~600 (module pair + stubs + sdk-py closure +
tests); C+G ~850 as one unit (crate + header + `subnet_auth.go` + tests);
X ~250 (fixture generator + five consumers + manifest harness glue).

R ~3 days (R0 may add a day if the trace is murky). T ~3 days. N and P
~4 days each. C+G ~1.5 weeks as one unit. X1 ~1 day; X2 phase 2 ~2 days
once two languages exist; X3 rides each binding.

## Activation gate

- **R** gates on nothing beyond review — additive work over a shipped
  substrate.
- **X1** gates on R3. **T** gates on R1 (types) only.
- **N, P** gate on R + X1. **C, G** gate on R + X1 and land as one
  reviewed unit.
- Live cells (per language) and X2 gate on T.
- Each language beyond the template pair gates on a **named consumer or
  explicit direction** — the same rule the org plan carried, including the
  recorded possibility that direction lifts it for a load-bearing auth
  surface.

## Deferred

Each with entry criteria, per house style.

- **Binding-side issuance** — entry: an operator tool that cannot shell
  out to the CLI *and* a root-key-handling story that keeps the key out of
  GC'd memory. Until then, never.
- **Subject-side credential loading in bindings** — entry: R0 outcome A
  (a traced production owner), or the substrate follow-up landing if R0
  finds outcome B.
- **Observability surface** (`protectedRelayStats`, gateway context /
  boundary inspection) in bindings — entry: a consumer that needs
  counters cross-language; Rust SDK may re-export earlier at zero cost.
- **Credential inspection APIs** (decode a grant's fields in a binding) —
  entry: a tooling consumer; today `net subnet inspect` covers it.
- **AsyncSubnet Python pair** — entry: an `async def` consumer (org P6
  precedent).
- **Topology-epoch administration in bindings** — entry: a remote-ops
  consumer plus a substrate story for authenticated remote topology
  mutation (currently a substrate non-goal).
- **A `@net-mesh/sdk` / `net_sdk`(py) ergonomic wrapper beyond config** —
  entry: the surface acquiring policy worth centralizing, which by design
  it has not (org precedent: verbs live in core).

## See also

- [`SUBNET_AUTH_PLAN.md`](SUBNET_AUTH_PLAN.md) — the substrate (shipped,
  PR #750).
- [`CODE_REVIEW_2026_08_04_SUBNET_AUTH.md`](../misc/CODE_REVIEW_2026_08_04_SUBNET_AUTH.md)
  — substrate review; findings closed on-branch, header still OPEN.
- [`ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md`](ORG_CAPABILITY_LANGUAGE_SDKS_PLAN.md)
  — the workstream template, including the eight ground-truth corrections
  and five N/P lessons this plan inherits.
- [`ORG_CAPABILITY_SDK_PLAN.md`](ORG_CAPABILITY_SDK_PLAN.md) — the Rust
  facade anatomy R copies.
- [`PAYMENTS_LANGUAGE_SDKS_PLAN.md`](PAYMENTS_LANGUAGE_SDKS_PLAN.md) — the
  original multi-language template (fixtures-precede-bindings).
- [`SDK_GO_PARITY_PLAN.md`](SDK_GO_PARITY_PLAN.md) — the Go security
  surface (Stages G-1..G-5) this extends.
- `docs/data/capabilities/` + `.claude/skills/net-event-bus/bindings/coverage.md`
  — the canonical parity records SSDK must add rows to.
