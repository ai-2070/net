# Net v0.36 — "Paranoid"

*Named after Black Sabbath's 1970 title track, whose narrator cannot tell a real threat from an imagined one.*

Four audits and five tracks land, and the shape they share is a substitution:

- **Cross-subsystem (SEC-01 … SEC-07)** — one high, four medium, one low, one candidate. Verdict: **HOLD pending repair and inverse witnesses.**
- **Admin-RPC authorization (AUTH-01 … AUTH-05, plus a witness-needed candidate)** — a frozen follow-on that does not amend the first, carrying the cycle's one **Critical**: any admitted peer could appoint itself a migration orchestrator and extract another daemon's state *and its Ed25519 signing seed*. Verdict: **HOLD.**
- **Linux deployment boundaries (LINUX-01 … LINUX-04)** — three reconfirmations at revised severities and one novel finding, all against an unprivileged local user on a shared host. Verdict: **HOLD.**
- **Go/cgo callbacks (FFI-01, FFI-02)** — two High-availability defects, both process-fatal. Verdict: **HOLD.** The worse bug was not in the audit at all: Go callbacks allocated their response buffers in the application's C runtime and Rust released them in the library's, which is heap corruption on Windows and the root cause of every `STATUS_HEAP_CORRUPTION` abort in the Go suite.
- **The Python wheel** shipped under `panic = "abort"` while every test ran against an unwinding debug build, so the configuration that shipped and the configuration under test disagreed about whether a Rust panic is survivable — and only the survivable one was ever exercised.
- **Go gets the durable issuer surface** the other four bindings got in 0.35, and the four post-0.35 usability residuals all close.

Every one of these audits is still marked **HOLD**. None was re-annotated after remediation, and that is deliberate rather than sloppy: the closure evidence is in code and in named tests, not in a report that says so. Where the shipped repair is narrower than the audit's requested design, this note says which.

The organizing observation follows the last four cycles and pushes them one layer down. v0.32 was *a fast path layered over a correctness path that never moves*; v0.33 was *one shared answer layered over an authority that never moves*; v0.34 was *a boundary layered over an identity that never travels*; v0.35 was *a claim layered over evidence nobody took*. v0.36 is **an authority layered over an admission that never meant one.** Completing the handshake proves possession of the PSK and nothing whatsoever about operator intent, and in six places the code had been reading the first as the second — or authorizing on `caller_origin`, which the sender picks, or on nothing at all. The answer is one primitive and a family of policies that all default closed: if you administer a node remotely, you now have to say who the operators are.

---

## Admission was not authority

`RpcContext::session_peer` is the node id whose AEAD session the frame decrypted under. It is populated at the two cortex RPC ingress sites and cannot be chosen by the sender, and it is the single value every gate in this release authorizes on.

On top of it sit five structurally identical policies, each with a fail-closed default and each with an explicit `AnyAdmittedPeer` variant that names the old behavior honestly instead of leaving it as an unstated default:

```text
MigrationOrchestratorPolicy::LocalOnly    (compute snapshot/migration)
RegistryAdminPolicy::Closed               (aggregator registry)
TransferAdminPolicy::Closed               (blob.transfers)
MeshDbAccessPolicy                        (no default at all)
TaskOwner::Peer(session_peer)             (A2A task ownership)
```

Three properties are worth naming, because they are what makes this a design rather than a scattering of `if` statements.

- **Each gate runs before the expensive or irreversible step.** The migration refusal happens before `start_snapshot`, so before the user's `snapshot()` runs and before any migration state is recorded. The registry gate runs *before decoding the body*, on the grounds that the body is attacker-controlled. The MeshDB gate runs before the query executes or a task is allocated.
- **`MeshDbAccessPolicy` deliberately has no `Default`.** Installing a server is itself the decision: without a policy argument, "install a MeshDB server" means "expose every chain the reader resolves to every admitted peer", and that is not something a default should be able to say quietly. `AllPeersMayReadEveryChain` reproduces the old behavior in one legible line.
- **Denials answer, and answer nothing.** A refusal is a reply rather than a drop, so a misconfiguration looks like a failure instead of a hang — but the refusal carries no information. MeshDB names no chain. A2A returns *unknown*, not *forbidden*: "not yours" and "no such task" must be indistinguishable, or the gate is an existence oracle.

Two of the audits' own suggested future test filters now exist verbatim as passing test names — `migration_take_snapshot_rejects_unapproved_orchestrator` and `unauthorized_peer_cannot_read_protected_chain`.

---

## The Critical one: a self-appointed orchestrator

**AUTH-01.** Compute migration rides native subprotocol `0x0500`. Any admitted peer could send `TakeSnapshot { daemon_origin, target_node: attacker }` and the source daemon would comply: it snapshotted the named daemon and sent the state to the node the *request* named. With identity transport enabled, the source additionally sealed that daemon's private Ed25519 seed to the attacker's X25519 key. Nothing on the path checked who was asking, because the path assumed whoever was asking was the operator.

The default is now `MigrationOrchestratorPolicy::LocalOnly`, and the hook is deliberately not an `Option` — its `Default` *is* the fail-closed variant, so there is no state in which the field is absent and the check is skipped. A source names its orchestrator with `DaemonRuntime::set_migration_orchestrator_policy(...)`, which returns an error rather than silently no-oping if called after `start()`. An empty allowlist refuses everyone remote, and that is pinned by its own test. A refused request gets a `MigrationFailed` reply naming "not an authorized orchestrator", so a misconfigured fleet reports a clear failure instead of a timeout.

The audit asked for more than shipped: an issuer-signed migration entitlement binding subject, origin, target, phase, validity and generation, and it warned that "a static operator allowlist may be an interim deployment control but is not a complete transferable migration-authority design." What shipped is the allowlist. That is the same missing entitlement primitive v0.35 deferred, now with a second caller waiting on it.

---

## The rest of the authorization gates

- **The aggregator registry had mesh admission as its only operator boundary (SEC-01 / AUTH-04).** `RegistryAdminPolicy::Closed` is the default; both handlers authorize before decoding; the C surface gained `NET_REGISTRY_ERR_UNAUTHORIZED = 11` with an operator-facing message. `install_registry_service*` take the policy as a *required* argument rather than a defaulted one. Daemon config accepts `operators = ["0x…"]` or `operators_any_admitted_peer = true` and refuses both at once — `OperatorPolicyAmbiguous`, "these contradict — remove one" — rather than guessing which one an operator meant.
- **The registry had no capacity ceiling.** `replica_count` was a `u8` validated only as non-zero, so one request could ask for 255, and nothing bounded group count at all. `DEFAULT_MAX_GROUPS = 64` and `DEFAULT_MAX_REPLICA_COUNT = 16` are now enforced with refusals that name the config key to raise.
- **The registry's status output carried private key material.** `group_seed` becomes `group_seed_fingerprint`: eight bytes of a domain-separated BLAKE3, sixteen hex characters, with tests pinning that a fingerprint which is merely a *truncation* of the seed fails and that the domain separator matters. The release notes here should be as careful as the code was: this **was not an exploitable disclosure** — replica keys authorize nothing on any path the audit could find — and the change is hygiene. A status API should not carry private key material regardless of what it currently authorizes.
- **MeshDB remote execution dropped its authorization context entirely (AUTH-02).** `ChainReader` had no caller, session or authorization parameter, so the per-chain ACL the plan documented was, in the remediation's own words, "not merely unenforced — it was unimplementable." The authenticated peer is now checked against *every* chain the plan touches, read off the plan tree by a new `ExecutionPlan::chain_origins()` whose match is deliberately exhaustive with no wildcard arm — a wildcard would be an authorization bypass no test would notice.
- **A2A tasks are bound to their authenticated submitter.** `status` and `cancel` keyed on the caller-supplied task id, and the registry stored no submitter at all, so any in-root peer that learned an id could read the full task brief — prompt and context references — or cancel someone else's work. Entries are now keyed `(owner, task_id)`, so two peers may use the same id without interfering and no peer can squat obvious ids to deny another. Submission stays open to every in-root peer, deliberately.
- **`blob.transfers` was world-readable to the mesh (SEC-03 / AUTH-05).** Any admitted peer could enumerate in-flight stream ids, holder identities, expected content hashes and byte counts, and `Cancel` an arbitrary transfer — failing the owner's fetch. `TransferAdminPolicy::Closed` is the default; `serve_blob_transfer_rpc` keeps its signature and now serves nothing remotely, which is a silent runtime break rather than a compile break, and is called out below.
- **Overflow pushes trusted a caller-supplied sender id (AUTH-03).** Peer A could set `sender_node_id = B` and inherit B's overflow opt-in, scope tags and audit attribution. Reading capabilities from the fold rather than from the request body stopped a sender forging its *capabilities* and did nothing about a sender forging *who it is* — the fold entry being looked up was chosen by the attacker. Capability synthesis now keys on the authenticated peer.

---

## Two denial-of-service repairs

- **Pingwave admission permitted unbounded persistent topology growth (SEC-02, the one High in the cross-subsystem set).** `max_nodes` was declared and never read; neither the dedup cache nor the edge map had a bound at all. All three are enforced now — 10,000 nodes, 40,000 seen pingwaves, 40,000 edges — with a new `RejectedCapacity` admission verdict and stale-edge sweeping driven from the heartbeat tick. The interesting part is the *reserve*: a flat cap let a flood suppress the very topology it competed with, and with a ten-second dedup timeout against a sixty-second sweep that meant real peers idling past their node timeout and being evicted. So a quarter of the dedup cache is reserved for origins the graph already knows, and the arithmetic is pinned by tests. A flood now fails to add topology instead of destroying it.
- **Unauthenticated forwarding spawned a task per datagram (SEC-07).** The audit rated this a *candidate* and said explicitly not to promote it without a witness demonstrating real accumulation. The witness was never taken; the spawn was removed anyway, on both the legacy relay and the pingwave rebroadcast paths, replaced with a non-blocking send that drops and logs when the egress socket is not ready. Verbatim `send_to` semantics are not lost, because UDP was always allowed to drop this. Dropping the spawn also dropped the four clones it required.

---

## The deployment boundary

One new leaf module, `secret_file`, becomes the choke point for every reader of secret-bearing text in the tree, and it is the most upgrade-hostile change in the release.

- **It validates the opened descriptor, not the path** — an fstat rather than a stat, which closes the check-then-swap window — and it checks the file *type* first, so an already-open FIFO is refused rather than read. Three conditions: regular file, owned by this process's effective uid, and `mode & 0o077 == 0`. Ownership is the substantive addition, and the module doc says why in one line: `0o600` proves nothing about *whose* `0o600` it is.
- **The fan-in is everything that holds a secret.** The aggregator daemon's config (which carries `psk_hex`), the CLI profile, the single gated reader behind identity, org and subnet seed files, and the SDK's device enrollment. The CLI's `--identity` attach path — also used by `wrap` and `mcp serve` — had *no* permission check at all before this, and is now gated with the waiver hard-coded off, because none of its three callers has such a flag to thread.
- **The escape hatch arrived second, and that is the story.** The remediation shipped with a waiver that existed only as an in-process Rust API: the aggregator daemon got `--insecure-permissions`, and the CLI got nothing, so every pre-existing `0644` profile — the umask default, on a file the CLI never writes itself — became an unoverridable hard failure on upgrade. The follow-up adds a global `--insecure-config-permissions` (and `NET_MESH_INSECURE_CONFIG_PERMISSIONS`), deliberately *not* sharing the per-command flag's name: reusing it is a duplicate-argument panic, and sharing it would be wrong on the merits anyway. The waiver skips ownership and mode; it never skips the regular-file check.
- **Secret-bearing TOML diagnostics reproduced their own source lines (LINUX-03, raised from Low to Medium).** `toml::de::Error`'s `Display` embeds the offending line, so a malformed `psk_hex` landed in journald by way of a boot-failure log. Three loaders now drop the parser error and report the path with a category. Line and column are the stated cost.
- **Hostile directory manifests could request setuid (LINUX-01).** `DirEntry::File.mode` comes from the manifest *publisher* and reached `Permissions::from_mode` verbatim, so a manifest asking for `0o4755`, fetched by a privileged service, produced a root-owned setuid binary in a directory another local user can execute — local privilege escalation delivered by a download. Special bits are stripped unconditionally, written as `& !0o7000` rather than `& 0o777` so the rule is legible where it is enforced, and pinned by an exhaustive property test over every mode value.
- **`fetch_dir` exposed files at umask width before its final chmod (LINUX-04, novel).** The reconstruction tree inherited the umask beside a traversable destination, and a file destined for `0o600` was world-readable *for the length of its transfer*, because the handle stays open across every chunk. The temp tree is created `0o700` and regular files are opened carrying their final sanitized mode, so they never exist at umask width; a trailing chmod is retained only to widen what the umask narrowed.

Windows is unchanged and still a gap: `std::fs` exposes no usable NTFS ACL view, so the gate warns and proceeds. That is recorded as a real gap rather than a platform where the problem does not exist — and it is exactly why this lands as a surprise on the first Linux host. The same asymmetry produced four separate test-fixture commits this cycle: `std::fs::write` leaves the umask default, which the gate rejects on Unix and merely warns about on Windows, so the same defect signature appeared in the aggregator, the CLI config tests, a CLI integration test and an SDK enrollment fixture.

The operator-facing page for all of this ships in the crate as `docs/SECURITY_DEFAULTS_0.35.md` — titled for the release it was drafted against, shipping here — with nine numbered defaults, a source-compatibility table, an operator-identity workflow, and a refusal-diagnosis table.

---

## The cgo boundary

- **FFI-01: a panicking Go callback killed the process.** Ten of the seventeen exported trampolines ran user code with no containment, and a Go panic crossing back into Rust is not recoverable there. Each one now installs a recover guard as its *first* statement — covering handle lookup and payload conversion, not just the user call — converts the panic into that callback's own documented failure value, and bumps a process-wide `CallbackPanicCount()`. The panic report goes to stderr with a stack, deliberately not through a logging hook, because the guard runs on a Rust-owned thread mid-unwind. The property is enforced structurally rather than per-trampoline: a test walks the package's AST and fails any exported function that invokes user code without that guard, with a vacuity floor and an explicitly sized exemption table for the trampolines that carry no user code.
- **The allocator bug the audit did not find.** Go callbacks allocated response buffers with `C.malloc`/`C.CString` — in the *application's* C runtime — and Rust released them with `libc::free`, in the library's. Invisible on glibc; on Windows each module carries its own CRT heap, and Application Verifier named it exactly: `StopCode 0x6 — Corrupted heap pointer or using wrong heap`, on a 26-byte block that was precisely one callback's JSON response, freed by `ucrtbase!free_base` under `net!net_org_serve`. That is the root cause of the `STATUS_HEAP_CORRUPTION` (`0xC0000374`) aborts in the subnet, snapshot and migration tests. The invariant now is *the allocator that creates a callback-owned buffer releases it*: Go registers one deallocator — a real C symbol, so its address is takeable — through `net_{org,rpc,compute}_set_callback_free`, Rust routes every release through it, and the contract is declared in four C headers with the ordering rule. Registration of any dispatcher **fails on every platform** without it, not just on Windows, because Rust no longer calls `libc::free` on these buffers anywhere — so on Linux a missing deallocator is no longer an invisible-but-correct free, it leaks one buffer per callback forever. A CI checker bans `libc::free` call sites in the Go FFI crates outright.
- **FFI-02: the MeshOS `cgo.Handle` was deleted while a callback was already admitted.** Registry removal deliberately lets in-flight host `Arc` clones continue, so "I asked for teardown" and "no callback can still arrive" are different instants with no signal between them that Go can observe — which is why an interim Go-side guard closed the common interleavings and not the last one. The lifetime moved to Rust: registration takes a destructor, the bridge invokes it exactly once from its `Drop`, and that drop cannot run while a callback holds the bridge. The destructor arrives through a distinct `..._v2` entry point rather than a seventh vtable field, because the vtable crosses as a pointer and growing it would make the library read past the end of a struct an older consumer allocated — a real out-of-bounds read, not a theoretical one. A version mismatch is therefore a link error.
- **`goBytesChecked` replaces the last unchecked length conversion.** `C.int` is 32-bit signed even on 64-bit hosts, so a length with bit 31 set flips negative and cgo panics with "negative length" — *before* any recover guard runs — while a length at or past 4 GiB modulo 2³² yields a short copy that desynchronizes framing. The audit de-rated this as local-only and said not to describe it as remotely exploitable; it was fixed for parity.

---

## The Python wheel unwinds

The published wheel was built on `[profile.release]`, which sets `panic = "abort"`, while CI tested a `maturin develop` debug build that unwinds. 884 tests passed against the configuration that does not ship.

- **A dedicated `[profile.python-release]` inherits release and sets `panic = "unwind"`.** `[profile.release]` is deliberately unchanged: the CLI, the daemon and the Node/Go/C artifacts own their processes; a pyo3 extension does not. A companion `[profile.python-diagnostic]` adds full debug info for investigation, because a stripped release backtrace resolves to the nearest exported symbol and prints the same name thirty times — "that was tried."
- **`GuardedRuntime` makes runtime drop context-aware.** CPython can run a garbage collection while the GIL is held inside a pyo3 callback executing on a tokio worker; if the collected cycle holds the last handle to a runtime, `Runtime::drop` runs on that runtime's own worker and tokio panics with *Cannot drop a runtime in a context where blocking is not allowed*. Outside a runtime the guard keeps the ordinary blocking drop; inside one it detaches. Doing that unconditionally would turn every ordinary shutdown into a detach and leak threads.
- **The structural half is where the guard goes.** An earlier sweep wrapped runtimes at their struct *fields*, and two sites built a bare runtime and wrapped it several fallible steps later — one of them with twenty early exits in that window, every one dropping an unguarded runtime. They are wrapped at construction now, and a recursive in-crate test flags any runtime constructor without a guard within three lines, with an explicit one-entry exemption table and a vacuity floor, because pinning the two known-bad sites would only have pinned those two.
- **The witness proves the artifact, not the intent.** A compile-gated `_panic_strategy_probe` export, off by default and banned from the published feature set, is driven from a *child* process by an oracle that refuses to render a verdict until the child prints an armed marker — a child that died from a missing DLL or a loader error would exit non-zero and look exactly like abort. `--expect abort` requires genuinely abnormal termination (a signal on POSIX, an NTSTATUS-range status on Windows), because `exit 1` is what a Python traceback produces and must never read as evidence of abort. The previous version of this witness accepted a non-zero exit, and its own `if:` condition named triggers the workflow it lived in does not declare, so two thirds of the condition was dead. It is a standalone weekly workflow now, and a separate CI job installs an ordinary wheel into a clean environment and runs the whole suite against *that*.
- **The causal story was withdrawn rather than repaired.** An earlier note linked the runtime-drop panic to a Windows `0xC0000409` fast-fail. The panic was observed on a `--release` wheel and the process *survived*, which an effective abort profile cannot do, so both halves — whether the shipped extension really aborted, and whether that panic caused that termination — are now stated as unestablished. Both were real; the chain between them was assumed. The profile change is justified on policy, not on that chain.

---

## Go gets the durable issuer

The C entry points had existed for months with no wrapper, which made Go the one binding whose issuer could not survive a rotation across a restart. `IdentityStateSize`, `IssuerGeneration`, `AtGeneration`, `ToState` and `IdentityFromState` close the gap that v0.35's identity work left open — that release could name Node, Python and C, and not Go.

The Go-specific decisions are the interesting ones. `AtGeneration` mints a *new* `Identity`, because the C ABI hands out owning pointers, and leaves the receiver untouched. The state buffer is sized from the library rather than from the header constant, on the principle that when they disagree the library is right and the header is stale. And the collapsed `NET_ERR_IDENTITY` is re-narrated into a message that says rotation only moves forward and re-applying the current generation is fine, while still wrapping the sentinel so `errors.Is` works. The 37-byte cross-binding state layout — version byte, seed, little-endian generation — is now pinned from Go for the first time, probed with an asymmetric constant so a byte-order flip cannot pass by coincidence.

---

## The residuals close

All four post-0.35 usability residuals are shut, each paired with a guard whose trigger matches the event that can invalidate it.

- **U-1 (Major): a `serveTool` node could not be shut down at all.** The shared `tool.metadata.fetch` handle was never released, so the documented teardown path failed with *cannot shutdown: outstanding references exist*. The registry handle is now closed, nulled and unmapped when the last tool goes — on registration rollback and in a `finally` on close, so an inner failure cannot skip it. The re-serve witness had to be re-pointed: closing the handle without nulling it makes the installer take its early return, the tool registers, and *no peer can fetch its metadata* — invisible to shutdown accounting and to every fake. A second node now actually fetches.
- **The Node suite had been testing compiled shims.** `build:ts` emits a `.js` beside seven `.ts` sources and Vite's default resolution order preferred it, so extensionless imports loaded the artifact rather than the source under repair. The control is stated plainly: swapping the unfixed source back in left the suite green.
- **U-2 (Minor):** the TypeScript quickstart now says that `localAddr()` is how a `:0` bind becomes connectable.
- **U-3 (Moderate):** the C overview stopped claiming five shared libraries. The one-library checker was broadened after it missed "six libraries", "six cdylibs", "both libraries" and two named per-surface cdylibs, and a new header-count checker derives the number from the tracked headers themselves — with a second rule for near-complete enumerations, because a page listing all but two headers is an index and must list them all. Two skill pages said "ten headers"; the missing row was `net_subnet.h`.
- **U-4 (Moderate): install pages named the previous release for an entire cycle.** The old checker derived "latest" from release-note filenames, and v0.35.0 shipped tagged and published with no release note in the tree, so the pages certified 0.34 while every registry served 0.35. The truth is now the newest stable unified `vX.Y.Z` tag reachable from the deployed commit — and it needed a *new workflow*, because a tag changes no file and therefore matches no path filter. Until this cycle, the unified release tag was a tag no CI job had ever looked at.
- **Two Windows-only web defects came out with it.** A hand-rolled basename returned -1 against a path built with `join()`, so `_shared.md` was never found and every adaptive documentation page silently decomposed into five ordinary sibling pages — the exact five-manuals navigation the mechanism exists to prevent, with the build dying downstream on an unrelated assertion. Linux CI never saw either. A counting witness now runs from static-params generation, so it cannot be skipped, and refuses to pass vacuously when it finds nothing.

The CLI change beside them matters more than its size: **`--identity` now also fixes the attached mesh's `node_id`.** Before, it set only the signing identity and the mesh came up anonymous with a fresh id every run, which made every node-id allowlist in this release unsatisfiable from the CLI — the secure configuration existed but could not be reached by the tool operators actually use. `net-mesh identity show` gained `node_id_hex`, which is the value that goes in an allowlist.

---

## The flakes, and what they turned out to be

The v0.35 cycle closed with a red coverage run and three named failures. Two of them are now resolved and one is not, which is recorded as one and not the other.

- **The handshake timeout was a budget, not a bug.** A three-attempt, three-second handshake in a three-node star test could not complete under coverage instrumentation; it now uses the same four-by-four budget the direct-connect path already used, rather than inventing a third number. A previous attempt had raised it to nine seconds, and nine seconds still was not enough.
- **A corrective-send witness was racing `serve_rpc`'s own re-announce**, at one run in a hundred both locally and at an unmodified head. Fixing the race exposed a second defect *in the test*: its final assertion watched a tag the re-announce also carries, so it could pass with no corrective send at all — proven by an always-refusing probe under which the old assertion still passed. It now waits for the spawned task and witnesses a tag only the probed announce introduces. Three hundred consecutive runs, clean.
- **An adopt/floor-raise race turned out to have a third interleaving**, in which adoption publishes membership under the revocation-state lock and a floor raise lands in the window before the startup loader re-verifies — leaving a refused adoption with membership already on disk. This is deliberately *not* fixed in production code: the loader cannot distinguish the certificate it just wrote from one a previous successful adoption left, so rolling back would destroy a valid authority on any transient failure, and the error return is already fail-closed. The test now pins the invariant that actually holds across all three interleavings — the node can never *start* with the floored certificate.
- **Doctests had never run for the FFI and binding crates.** The matrix ran `--lib` only, so two malformed doc blocks in the Python binding — one where `status='pending'` parsed as a character literal — had been sitting unbuilt. The doc target now runs alongside the lib target for every matrix entry, and the sweep came back clean across ten crates.
- **Three MeshOS destructor tests shared two static counters** and reset-then-asserted, so one failed about one run in three on an unmodified tree. Each test's own context pointer is now its counter, collapsing "was it called" and "with the right context" into a single observation.
- **Hard-coded test ports produced `EADDRINUSE` on a port no test in the repository claims.** The counters kept the suite from colliding with itself and did nothing about anything else on the machine; both fixtures now let the OS pick, and the two consumers that dial read the bound address back.
- The remaining coverage failure reproduced once, passed in isolation, and is claimed as **neither** a regression nor pre-existing evidence. It is open, and recorded as open.

---

## What's deferred (honestly)

- **All four audits are still marked HOLD.** They were frozen as evidence packets and not re-annotated; the closure evidence is code and named tests. Anyone reading the reports alone will find no dispositions.
- **The overflow size field is still caller-controlled.** AUTH-03's identity half is fixed; its size half — resolve or verify the authoritative object size before reserving disk headroom — is not, and it carries no stated disposition either way. The only defence is a pre-existing floor whose own comment concedes the number cannot be trusted. Relatedly, the `sender_node_id` field's documentation claims the receiver refuses a mismatch; the handler warns. The code is the safe behavior and the doc oversells it.
- **Windows secret-file permissions are not enforced.** The gate warns. Closing it needs an ACL query the standard library does not expose. Restrict the DACL out of band.
- **The deployment guidance the audit asked for is not written.** No systemd unit, no `UMask=0077`, no `ProtectSystem`, no non-root guidance, no secret-mount modes — the audit sequenced it after exact defaults and ownership contracts, and those landed first.
- **Migration authority is an allowlist, not an entitlement.** See above; this is the second consumer waiting on the issuer-signed `(subject, axis, value, validity)` primitive v0.35 deferred.
- **`session_peer` authenticates the deliverer, not an end-to-end origin.** Under a relay that terminates and reissues nRPC, the relay becomes the authenticated peer and an allowlist authorizes it plus everything it forwards — so an A2A task submitted through one is owned by the relay, not by the agent behind it. That is the documented limit of public nRPC attribution, and closing it means a protected service or an application-level signature, not another allowlist.
- **Third-party A2A observation is deferred by design.** A task id is a name, not a bearer capability; letting a third party watch someone else's task needs an explicit delegated capability rather than a leaked id.
- **Real cross-node replica identities are deferred.** The seed fingerprint is hygiene. A genuine replica identity needs a randomly generated seed persisted with explicit ownership and rotation semantics — not the current derivation, and not a public deployment salt over it, which would only be another secret seed wearing a different name.
- **The Go callback audit's acceptance gate was not literally met.** It asked for child-process panic witnesses and a deterministic two-barrier teardown witness. What shipped is an AST property test over every trampoline, source-shape pins, and drop-based unit tests. The v1 MeshOS registration path is also retained, so a consumer that stays on it keeps the FFI-02 exposure by construction.
- **The residual Python runtime-drop panic is open.** After every binding-created runtime was guarded, one panic was still seen; it has not reproduced since, which is not evidence it is gone. The open question is *ownership* — a runtime the binding never constructed cannot be fixed by guarding the binding's constructors, and the blocking HTTP client behind the payments transport is named as a suspect, not a finding. What shipped is instrumentation: a hunt harness that scans output rather than exit codes (the panic is on a background thread, so the suite still exits 0), refuses to report a clean sweep as fixed, and a GC-stress plugin that raises collection density by roughly two orders of magnitude.
- **The improvement roadmap is proposed, not authorized.** Nine workstreams — a released-artifact conformance pack, a cross-SDK strictness contract, canonical error semantics, security-journey parity — with a governing principle worth quoting: improve SDKs as complete, independently verifiable developer journeys, not as collections of similarly named methods.
- **Organization load balancing did not move.** No OLB slice landed this cycle; the routing-plane witness gate is unchanged.

---

## Breaking changes

This is the release where fail-closed defaults arrive, and **each one can stop a working deployment on upgrade.** They are grouped by who feels them.

**Operators**

- **Secret-bearing files must be regular files, owned by you, and inaccessible to group and other.** A `~/.config/net-mesh/config.toml` at the umask default `0644` now fails every command with a typed refusal naming the path, the mode, and `chmod 600` — exit code 3. The aggregator daemon refuses to boot on the same condition. Waivers, one per surface and deliberately unshared: `--insecure-config-permissions` (or `NET_MESH_INSECURE_CONFIG_PERMISSIONS`) for the CLI profile, `--insecure-permissions` for the daemon config and for the per-command seed inputs, `ConfigFile::load_with(path, true)` for an embedded profile, `DeviceEnrollment::load_allowing_insecure` for enrollment. **Windows deployments are unaffected**, which is why this arrives as a surprise on the first Linux host.
- **Cross-node compute migration stops working until the source names its orchestrator.** The refusal is a `MigrationFailed` reply, so this appears as a clear failure rather than a timeout.
- **The aggregator registry serves no remote administration until you configure operators**, and `blob.transfers` serves nothing remotely. Setting both `operators` and `operators_any_admitted_peer` is an error, not a preference.
- **`replica_count` above 16 and a 65th hosted group are refused**, naming the config key to raise.
- **`net-mesh --identity` now also fixes the attached mesh's node id.** This is what makes the allowlists above reachable from the CLI — and it means two concurrent invocations sharing one identity now share a node id, so the second attach displaces the first. Unattended automation needs its own identity. Anonymous attach remains the default precisely so that an operator who has not asked for a stable identity does not acquire that failure mode by upgrading.
- **`group_seed` is now `group_seed_fingerprint` everywhere it was published**: the Rust summary struct, the C FFI JSON (`group_seed_hex` 64 characters → `group_seed_fingerprint_hex` 16), the Node and Python surfaces, and `net-mesh aggregator ls/status/spawn`.
- **Fetched directories are owner-only.** `0o700` travels with the inode through the final rename where the destination previously inherited the umask. Widen it explicitly if another local user needs it.
- **setuid, setgid and sticky bits are stripped from remote file manifests unconditionally.** There is no configuration. If you were relying on a fetched directory to carry setuid bits, that never worked safely.
- **TOML parse diagnostics lost their line and column.** Scripts grepping the daemon or CLI parse-error text now see the path and a category only.

**Rust API consumers**

- **`RpcContext` gained `session_peer` and is now `#[non_exhaustive]`.** The attribute is a break in its own right, taken *because* adding the field was already one — nothing outside the crate constructs an `RpcContext`, so it costs nothing now and makes the next field additive. Test harnesses and mocks that built the struct literally must stop; handlers receive it.
- **`MeshDbServer::new` requires a `MeshDbAccessPolicy`.** `AllPeersMayReadEveryChain` reproduces the previous behavior.
- **`install_registry_service*` require a trailing `RegistryAdminPolicy`.**
- **`serve_blob_transfer_rpc` keeps its signature and changes its behavior** — a silent runtime break. Use `serve_blob_transfer_rpc_with_policy` to name operators.
- **`TaskRegistry::{submit, status, record, cancel, forget}` take a leading `TaskOwner`**, `submit` returns a `Result` (reusing an id for a *different* brief is a rejection rather than a silent hand-back, while an identical re-submit stays idempotent for the retransmit case), and `list` returns owner-attributed pairs — use `list_for(owner)`. The wire format is unchanged, so an older peer still interoperates.

**C and Go consumers**

- **Dispatcher registration fails without a callback deallocator, on every platform.** `net_{rpc,org,compute}_set_*_dispatcher` now refuse unless `net_*_set_callback_free` was called first. A Go wrapper or hand-written C consumer built before this release, loaded against a v0.36 library, refuses at startup instead of corrupting a heap on Windows or leaking one buffer per callback elsewhere. The Go wrapper turns that refusal into a panic naming the version mismatch.
- **Rust never calls `libc::free` on callback-owned buffers.** Supply a `free` matching the allocator your handlers use; returning static or stack buffers is undefined behavior.
- **MeshOS registration moved to a `..._v2` entry point** carrying the user-context destructor. A wrapper built against the new header and an older library gets an unresolved-symbol link error by design; the v1 symbol and the six-pointer vtable ABI are unchanged.
- **`net_org_check_abi_version` requires exact equality**, matching the correction nRPC took in v0.35. Its "additive bumps only" premise was never enforced, and this crate disproved it in 0.35 when a dispatcher setter changed return type.
- **A panicking Go callback no longer kills the process.** It returns that callback's documented failure value and increments `CallbackPanicCount()`; the work that panicked is lost.

**Language packages**

- **Published Python wheels unwind instead of aborting.** An internal Rust panic surfaces as pyo3's `PanicException` and is catchable, and a panicking background task becomes a join error instead of an abort. Anyone building the wheel by hand must switch `--release` to `--profile python-release`; the release-feature guard fails the build otherwise. Non-Python artifacts keep `abort`. This is not blanket panic-to-exception: a detached task nobody joins still fails silently.
- **Dropping the last handle to a binding runtime from inside an async context detaches** rather than panicking; worker threads are not waited on in that path. Outside a runtime the blocking wait is unchanged.
- **`net-mesh-sdk` requires `net-mesh>=0.36.0,<0.37.0`**, and `@net-mesh/sdk` requires `@net-mesh/core >=0.36.0`. Mixed core and wrapper versions remain unsupported.
- **Go's identity surface is purely additive** — `IdentityStateSize`, `IssuerGeneration`, `AtGeneration`, `ToState`, `IdentityFromState` — and `net-mesh identity show` gains `node_id_hex`, which breaks only scripts asserting an exact JSON key set.

---

## How to upgrade

1. **Fix your file modes first.** `chmod 600` the CLI profile, the aggregator config, and every identity, org, subnet and enrollment seed file, and make sure they are owned by the user running the process. If you cannot do that today, pass `--insecure-config-permissions` (CLI) or `--insecure-permissions` (daemon) *deliberately* rather than discovering the refusal in production. On Linux this is the change most likely to stop a working deployment; on Windows the gate only warns, so a mixed fleet will fail asymmetrically.
2. **Decide who your operators are, and give them stable node ids.** Run `net-mesh identity show` to read `node_id_hex`, put those ids in `operators`, `TransferAdminPolicy`, `RegistryAdminPolicy` and `MigrationOrchestratorPolicy`, and attach with `--identity` — without it the CLI is anonymous and every allowlist refuses it. Remember that two concurrent invocations under one identity now displace each other.
3. **If you administer nodes remotely and skip step 2, remote administration is simply off.** That is the intent: registry administration, transfer inspection and cross-node migration all refuse by default, and each refusal is an answer rather than a hang.
4. **Rebuild Go wrappers and `libnet` together.** The callback deallocator is required on every platform and the MeshOS registration symbol changed; a mismatched pair now fails at startup or at link time instead of corrupting memory. If you maintain a hand-written C consumer, register a `free` matching your handlers' allocator before installing any dispatcher.
5. **Reinstall the Python wheel and re-read your panic handling.** Panics crossing a pyo3 boundary are now catchable exceptions rather than process death; if you build the wheel yourself, use `--profile python-release`. If you had test fixtures on fixed ports, they now bind `:0` and report the address back.
6. **Rust callers: add the policy arguments and stop constructing `RpcContext`.** `MeshDbServer::new` and `install_registry_service*` will not compile without a policy, `serve_blob_transfer_rpc` will compile and serve nothing, and A2A task calls need a `TaskOwner`.
7. **Update anything that parsed `group_seed`, an aggregator JSON key set, or a TOML parse error's line number.**
8. **Everyone else** gets the audits' fixes, Go's durable issuer surface, and no behavior change to existing paths.

---

## Dependency updates

The crate version bumps `0.35.0 → 0.36.0` across all eighteen workspace members and both npm launcher manifests. The cycle spans 92 non-merge commits over 180 files (+15.4k/−1.6k). **The Rust toolchain pin does not move** (1.97.1), no workspace member was added or removed, and no first-party crypto moved.

The bump itself shipped a defect worth recording, because it is the second consecutive cycle in which this one line was what the release got wrong: rewriting every `0.35.0` in the Python SDK's manifest also rewrote the *upper* bound, producing `net-mesh>=0.36.0,<0.36.0` — the empty set, and an uninstallable package. It was caught and fixed inside the cycle, the comment beside it is now version-agnostic, and the rule is written down: a release bump must move **both** bounds. In v0.35 the same line failed the other way, by having no upper bound at all.

- **Rust (lockfile only):** the `futures` family to 0.3.34, `clap_mangen` 0.3.3, `eyre` 0.6.14, `pest` and its derive/generator/meta crates to 2.9.0, `rustls-webpki` 0.103.14, `http-body-util` 0.1.5, `num-integer` 0.1.47.
- **Cargo profiles:** new `[profile.python-release]` (release plus `panic = "unwind"`, the profile the wheel now ships on) and `[profile.python-diagnostic]` (the same panic path with full symbols, deliberately unpublished).
- **Docs / web:** `better-auth` 1.6.27 and `zustand` 5.0.15 in the manifest; `posthog-js` to 1.417.0, `posthog-node` to 5.49.0, `@typescript-eslint` 8.67.0, `terser` 5.50.0, `kysely` 0.29.5 and the Sentry core refresh in the lock.
- **CI:** five new scripts and two new workflows. `check-header-count.py` (a documented header count that disagrees with the tracked headers), `check-callback-buffer-ownership.py` (a `libc::free` call site reappearing in a Go FFI crate), `check-panic-strategy.py` (a wheel with the wrong panic strategy, and a witness that prints `ok` having proved nothing), plus `hunt-runtime-drop-panic.py` and a GC-stress pytest plugin as instruments for the open residual. `install-version.yml` runs on the unified release tag, a daily cron and dispatch; `panic-probe-witness.yml` moves the panic witness out of a workflow whose triggers it named and which does not declare them. Existing checkers grew accordingly — the ABI parity gate went table-driven across three surfaces and found `net_org_check_abi_version`'s `>=` on the way, the one-library matcher was broadened per line, the install-version checker moved from filenames to tags, and `ci.yml` gained a shipped-wheel acceptance job, the C-surface guards, the callback-ownership guard, doctests for every FFI matrix entry, and a live tool-lifecycle witness.
- **Skills:** both skill corpora record `net-version: 0.36.0`.

---

Released 2026-08-13.

## License

Dual-licensed under [MIT](../../LICENSE-MIT) **OR** [Apache-2.0](../../LICENSE-APACHE), at your option.
