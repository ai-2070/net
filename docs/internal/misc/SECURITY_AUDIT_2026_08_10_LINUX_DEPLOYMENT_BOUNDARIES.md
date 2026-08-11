# Linux and Deployment-Boundary Security Audit — 2026-08-10

## Status

**Verdict: HOLD.** The review reconfirmed dangerous privileged extraction, secret-file permission, and secret-diagnostic findings and retained one novel shared-host extraction race.

```text
Audited commit: 43b66dbc740381cf97e6cc1e19fa52fb7bf9c99a
Branch: security-4
Upstream: origin/security-4
Divergence during audit: 0 ahead / 0 behind
Repository: C:\Users\chief\Documents\git\net
Audit host: Windows; Linux witnesses designed but not claimed as executed
```

This is a frozen follow-on to `SECURITY_AUDIT_2026_08_10_CROSS_SUBSYSTEM.md`. Severity is refined here according to the explicit privileged/shared-host deployment prerequisites.

## LINUX-01 — Privileged directory extraction preserves setuid/setgid regular-file modes

**Severity:** High under privileged extraction  
**CWE:** CWE-732, Incorrect Permission Assignment for Critical Resource  
**Confidence:** High from production code  
**Status:** Reconfirmed SEC-04; Linux witness required

### Violated invariant

Remote directory metadata must not create locally privileged executables. Umask is not a defense against an explicit final chmod.

### Production path

Unix source mode is recorded without masking special bits:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:428-432
```

Remote regular-file mode is accepted and passed to reconstruction:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:576-590
```

Both single- and multi-chunk paths apply the mode:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:610-622
net/crates/net/src/adapter/net/dataforts/dir.rs:831-865
```

`apply_mode` passes the remote value directly to `Permissions::from_mode`:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:1027-1033
```

### Prerequisites and impact

A privileged Unix process fetches a manifest controlled by or sourced from a less-trusted peer. A remote regular file requesting `04755` or `02755` can become a root-owned setuid/setgid executable. A second local user may then execute it for privilege escalation.

Directory special modes are not currently preserved: directory modes are recorded but ignored during creation:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:291-295
net/crates/net/src/adapter/net/dataforts/dir.rs:528-559
```

That distinction does not close regular-file exploitation.

### Required Linux witness

1. Build a manifest with regular files requesting `04755` and `02755`, plus directories with equivalent modes.
2. Fetch as root into a root-owned destination.
3. Assert that regular files retain `S_ISUID`/`S_ISGID` and directories do not.
4. After repair, assert `(st_mode & 07000) == 0` for every extracted regular file.

Proposed focused test:

```text
cargo nextest run -p net --features dataforts -E 'test(privileged_fetch_dir_rejects_or_strips_special_file_modes)'
```

### Minimal repair boundary

Mask regular-file modes to a safe allowlist such as `mode & 0o0777`; normally apply stricter group/other-write policy. Special-bit restoration, if ever required for trusted archival workflows, must be explicit and unavailable to ordinary remote fetch. Apply sanitized directory modes after reconstruction, deepest-first.

## LINUX-02 — Secret-bearing configuration and enrollment files are accepted with unsafe permissions or ownership

**Severity:** High for PSK exposure; Medium for isolated seed-loader cases  
**CWE:** CWE-732  
**Confidence:** High from source  
**Status:** Reconfirmed SEC-05; Linux multi-UID witnesses required

### Production paths

Aggregator configuration contains `psk_hex`:

```text
net/crates/net/aggregator-daemon/src/lib.rs:96-105
```

It is read and accepted regardless of the permission-check result:

```text
net/crates/net/aggregator-daemon/src/lib.rs:284-310
```

Group/world accessibility produces only a warning:

```text
net/crates/net/aggregator-daemon/src/lib.rs:830-868
```

The general CLI profile stores a PSK and loads without a mode or ownership check:

```text
net/crates/net/cli/src/config.rs:61-67
net/crates/net/cli/src/config.rs:97-117
```

`DeviceEnrollment::load` reads a persisted private device seed without checking Unix permissions or owner:

```text
net/crates/net/sdk/src/enrollment.rs:1162-1195
```

Identity, organization, and subnet command loaders demonstrate stricter mode behavior but still need common owner/type semantics:

```text
net/crates/net/cli/src/commands/identity.rs:539-559
net/crates/net/cli/src/commands/org.rs:840-855
net/crates/net/cli/src/commands/subnet.rs:1108-1129
```

### Impact

A permissive PSK file lets another local user acquire mesh membership. An exposed enrollment file compromises the device signing identity. This amplifies remote administrative vulnerabilities once the local attacker joins the mesh.

### Required Linux witnesses

- `chmod 0644 aggregator.toml`: daemon currently warns and continues.
- `chmod 0644 config.toml`: CLI currently consumes `psk_hex` silently.
- `chmod 0644 enrollment.json`: `DeviceEnrollment::load` currently succeeds.
- Repeat with a `0600` file owned by a different UID under a privileged reader to prove that mode-only checks do not establish trusted ownership.

### Minimal repair boundary

Implement a shared secret-file opener that validates the opened object—not only the path—for:

- regular-file type;
- expected owner;
- `mode & 0077 == 0`;
- no unsafe path replacement between validation and read.

Use it for aggregator configuration, PSK-bearing CLI configuration, enrollment state, identity keys, organization keys, and subnet keys. Fail closed by default, with a distinctly named compatibility override.

## LINUX-03 — Secret-bearing TOML diagnostics can reproduce source excerpts

**Severity:** Medium  
**CWE:** CWE-532, Sensitive Information in Log Files  
**Confidence:** High for path; rendered-output witness required  
**Status:** Reconfirmed SEC-06 with revised severity

### Production paths

Aggregator parsing retains a full `toml::de::Error` and the binary logs it:

```text
net/crates/net/aggregator-daemon/src/lib.rs:164-174
net/crates/net/aggregator-daemon/src/lib.rs:301
net/crates/net/aggregator-daemon/src/main.rs:13-17
```

CLI profile parsing embeds the TOML error:

```text
net/crates/net/cli/src/config.rs:107-111
net/crates/net/cli/src/config.rs:128-142
```

Identity parsing interpolates the parser error after reading the secret-bearing file:

```text
net/crates/net/cli/src/commands/identity.rs:393-411
```

Organization and subnet key loaders already sanitize parse errors and provide the preferred pattern:

```text
net/crates/net/cli/src/commands/org.rs:858-871
net/crates/net/cli/src/commands/subnet.rs:1108-1138
```

### Impact

Malformed `psk_hex` or `seed_hex` lines can be reproduced in journald, CI logs, shell captures, or centralized telemetry whose readers are broader than the secret file's readers.

### Required witness

Use a dummy sentinel in malformed secret input and prove current output includes it:

```sh
cfg="$(mktemp)"
chmod 600 "$cfg"
printf '%s\n' \
  'listen = "127.0.0.1:0"' \
  'psk_hex = "PSK_SENTINEL_0123456789" trailing' >"$cfg"
net-aggregator-daemon --config "$cfg" 2>&1 | tee /tmp/net-sec06-aggregator.log
grep -F 'PSK_SENTINEL_0123456789' /tmp/net-sec06-aggregator.log
```

Repeat for the general CLI profile and identity `seed_hex`. Repaired output must omit the sentinel while preserving path and safe location/category information.

### Minimal repair boundary

Never format `toml::de::Error` from a secret-bearing file. Return a stable sanitized category plus path and safe line/column information. Scrub the source buffer after parsing.

## LINUX-04 — `fetch_dir` exposes files under umask-derived modes before final chmod

**Severity:** Medium; High for privileged/shared-host secret extraction  
**CWE:** CWE-276, Incorrect Default Permissions  
**Confidence:** High from code; local-race witness required  
**Status:** Novel

### Violated invariant

A file intended to be private at completion must never be exposed under a broader temporary mode during reconstruction.

### Production path

The sibling temporary root is created with plain `create_dir`, inheriting process umask:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:702-735
```

Small files use `std::fs::write` before `apply_mode`:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:1021-1033
```

Multi-chunk files use `File::create`, stay open throughout transfer, and receive the requested mode only at the end:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:831-865
```

Remote directory modes, including `0700`, are not applied during reconstruction:

```text
net/crates/net/src/adapter/net/dataforts/dir.rs:528-559
```

### Prerequisites and impact

On a shared host, the destination parent is traversable by another local user and the process uses a normal umask such as `022`. The temporary tree is typically `0755` and files are initially `0644`. Another user can read content intended to finish as `0600`; a throttled multi-chunk transfer enlarges the exposure window. Intended private directory modes are also lost.

### Required witness

Under umask `022`, fetch a throttled multi-chunk file whose manifest mode is `0600`. A second UID watches the sibling `.fetch_*` path and attempts to read during transfer. Current behavior should permit the read; repaired behavior must deny it from creation onward.

### Minimal repair boundary

Create the temporary root as `0700` in the creation syscall. Create files with sanitized final permissions from the beginning using `OpenOptionsExt::mode`, `create_new`, and descriptor-relative operations. Apply sanitized directory modes after children complete.

## Deployment-dependent boundaries, not retained as product vulnerabilities

- `FileSystemAdapter` explicitly requires its root to be writable only by the substrate UID and documents remaining canonicalize/open/rename TOCTOU windows:

  ```text
  net/crates/net/src/adapter/net/dataforts/blob/fs.rs:59-92
  net/crates/net/src/adapter/net/dataforts/blob/fs.rs:175-188
  ```

  A shared-writable CAS root violates that contract; hardened shared roots would require descriptor-relative or `openat2(RESOLVE_BENEATH)` confinement.

- `fetch_dir` temp/backup allocation and existence/rename sequencing assume a trusted parent:

  ```text
  net/crates/net/src/adapter/net/dataforts/dir.rs:702-793
  ```

  A hostile writer of the parent can replace entries. This is deployment-dependent rather than an attack when the documented ownership model holds.

- Mode-only key checks do not validate UID or regular-file type. This matters when privileged automation accepts paths under writable mounts, but not for a normal user-owned protected configuration tree.

- No Unix-domain-socket listener was found. Relevant control surfaces are mesh RPC services over UDP/Noise.

- No repository systemd unit, Dockerfile, Kubernetes manifest, Helm chart, or deployment template was found. The production guide does not prescribe service users, `UMask=0077`, `ProtectSystem`, secret-mount modes, writable-state ownership, `runAsNonRoot`, or read-only-root settings:

  ```text
  web/src/content/docs/guides/production-deployment.md:19-28
  web/src/content/docs/guides/production-deployment.md:111-123
  ```

  This is a deployment-documentation gap, not a standalone vulnerability.

## Ruled out

- Directory transfer represents only regular files, directories, and symlinks; device nodes, sockets, and FIFOs are skipped.
- Hardlinks are not recreated; they become independent regular files.
- Manifest paths reject absolute paths, prefixes, `..`, and non-normal components.
- Symlink targets reject absolute/escaping paths and composed-symlink traversal.
- Identity, organization-root, subnet-root, and audience-secret creation generally uses creation-time `0600`, staged publication, and cleanup.
- Organization and subnet TOML parse diagnostics are sanitized.
- Hostile-root attacks are out of model because root can read process memory, replace binaries, alter mounts, and bypass file controls.

## Dependency-advisory limitation

`cargo-audit` and `cargo-deny` were not installed, and no repository-native advisory workflow was found. No unpinned online advisory claim was used. No dependency vulnerability is asserted by this report.

## Acceptance order

1. LINUX-01: special-bit stripping and privileged extraction witness.
2. LINUX-04: private-from-creation temporary tree/file semantics.
3. LINUX-02: shared fail-closed secret-file opener and multi-UID witnesses.
4. LINUX-03: secret diagnostic redaction witnesses.
5. Add deployment guidance only after product defaults and ownership contracts are exact.

Each repair must include the inverse witness, positive controls, nonzero focused tests, exact-head Git/CI evidence, and no broad unrelated hardening changes.
