# R5-G1 raw receipts — Go fabricates the EventTooLarge limit

Retained verbatim, not reconstructed. Every block below is captured
tool output from this working tree; the mutation diffs are `diff -u`
of the repaired file against the mutated copy, produced mechanically
rather than typed.

Repo `C:/Users/chief/orca/workspaces/net/webrtc-transport`, branch
`LZL0/webrtc-transport`, base HEAD `9a0615fbf`. Lane R6Go.

## Environment (cgo actually on)

No `gcc` on the default PATH and `go env CGO_ENABLED` = `0` in this
shell, which is exactly the AGENTS.md silent-skip trap. Every command
below therefore ran with an msys64 mingw gcc prepended to PATH and
`CGO_ENABLED=1` set explicitly:

```
CGO_ENABLED=1
CC=gcc                      (C:\msys64\mingw64\bin\gcc.exe, gcc 15.2.0, MSYS2 Rev13)
CGO_LDFLAGS=-LC:/Users/chief/orca/workspaces/net/webrtc-transport/net/crates/net/target/release
PATH=C:\msys64\mingw64\bin;<...>\net\crates\net\target\release;...   (release dir on PATH so the loader finds net.dll)
RUN_INTEGRATION_TESTS=1     (for the suite runs)
```

Proof the toolchain is live rather than skipped:

```
$ cd go && go env CGO_ENABLED CC
1
gcc
```

and, in the same environment, `CGO_ENABLED=0 go build ./...` still
fails through the intended guard rather than silently succeeding:

```
$ cd go && CGO_ENABLED=0 go build ./...
# github.com/ai-2070/net/go
.\cgo_disabled.go:32:2: undefined: this_package_requires_cgo__set_CGO_ENABLED_1_and_install_a_C_compiler__see_https_ai2070_net_docs_start_install_go
rc=1
grep -c requires_cgo        -> 1
grep -c "undefined: net\."  -> 0
```

## Library build

```
$ cd net/crates/net
$ cargo build --release -p net-ffi --features net-ffi/test-helpers
    Finished `release` profile [optimized] target(s) in 5m 12s
build rc=0
$ ls -la target/release/net.dll target/release/net.dll.lib
-rwxrwxrwx 1 somebody somegroup 9026048 Sep 17 10:54 target/release/net.dll
-rwxrwxrwx 1 somebody somegroup  144444 Sep 17 10:54 target/release/net.dll.lib
```

## Restoration identities (sha256, repaired state)

```
34c9f2e457e194a139b504b5b11e2b8ee49c3ad1e9382983f67af11cd6cdd811  go/mesh.go
a17a4c9964d044eeb8b21e3855e93949eea1324e4daf6955e6bd3c504f735ad2  net/crates/net/src/ffi/mesh.rs
032b2ece0a3721afba673ea1a42f4e0e3e9e9318964a37052d348ebb677f119d  go/event_too_large_attribution_test.go
f589d4ba9091c9092d19fd214e2a3b5cf922d4a8ed5b3561bfa5b9b5c7b5bf68  go/event_too_large_attribution_testhelpers.go
6b4cae1d2bb4445aa95aac8ebf685363862d76804af75f721c8629d1c95f8577  go/net.h
bfba70ab2b0c69ea05d80cc66bfdc627e803f3d027152c0c288ba4567fb44687  net/crates/net/include/net.go.h
e17470be1278590209aba729bcb6cc5be9f7b5838cef1de716ed74ba4b02c691  net/crates/net/bindings/go/net-ffi/Cargo.toml
```

Each inverse below ends with the same hash printed again, which is
the restoration identity — not a claim that the file was restored.

## The two new parity tests

```
go/event_too_large_attribution_test.go
  TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
  TestMixedBatchRefusalIsAttributedToTheRefusedEvent
```

Command used for every RED/GREEN pair below:

```
cd go && go test -tags test_helpers -count=1 \
  -run 'TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit|TestMixedBatchRefusalIsAttributedToTheRefusedEvent' \
  -v ./...
```

---

## INV-1 — Go reconstructs the limit from the single-packet accessor

This is the limit half of her finding in isolation: keep the carried
size, take the limit from `MaxEventSize()` as HEAD did.

Applied diff (`diff -u go/mesh.go <mutated copy>`):

```
--- go/mesh.go	2026-09-17 10:58:04.547154300 +0200
+++ go/mesh.go (INV-1)	2026-09-17 10:58:01.971363400 +0200
@@ -184,7 +184,7 @@
 	if code != netErrMeshEventTooLarge {
 		return meshErrorFromCode(code)
 	}
-	return &EventTooLargeError{Size: int(size), Limit: int(limit)}
+	return &EventTooLargeError{Size: int(size), Limit: MaxEventSize()}
 }
 
 // EventTooLargeError is the typed form of ErrEventTooLarge returned
```

RED (exit 1):

```
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
    event_too_large_attribution_test.go:81: EventTooLargeError.Limit = 8104, want 64832 (the fragmentation ceiling that applied); MaxEventSize() is 8104 and reporting it here is the defect — it tells a caller to split an event the transport would have carried
--- FAIL: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
    event_too_large_attribution_test.go:153: EventTooLargeError.Limit = 8104, want 64832
--- FAIL: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
FAIL
FAIL	github.com/ai-2070/net/go	0.538s
FAIL
rc=1
```

Restored, identity and GREEN (exit 0):

```
34c9f2e457e194a139b504b5b11e2b8ee49c3ad1e9382983f67af11cd6cdd811  go/mesh.go
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
--- PASS: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
--- PASS: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
PASS
ok  	github.com/ai-2070/net/go	0.536s
rc=0
```

---

## INV-2 — HEAD's fabricating `sendErrorFromCode` restored verbatim

The pre-repair function body reinstated exactly as `9a0615fbf` has it,
with the four call sites adjusted only so it compiles. This is the
inverse that reproduces **both** of Kyra's reported numbers.

Applied diff:

```
--- go/mesh.go	2026-09-17 10:57:32.186232000 +0200
+++ go/mesh.go (INV-2)	2026-09-17 10:57:27.011956600 +0200
@@ -180,11 +180,19 @@
 // recoverable from this side of the boundary: the limit is a
 // property of the resolved peer and the offending element is the
 // core's choice, so the numbers have to travel with the refusal.
-func sendErrorFromCode(code C.int, size, limit C.size_t) error {
+func sendErrorFromCode(code C.int, payloads [][]byte) error {
 	if code != netErrMeshEventTooLarge {
 		return meshErrorFromCode(code)
 	}
-	return &EventTooLargeError{Size: int(size), Limit: int(limit)}
+	limit := MaxEventSize()
+	size := 0
+	for _, p := range payloads {
+		if len(p) > limit {
+			size = len(p)
+			break
+		}
+	}
+	return &EventTooLargeError{Size: size, Limit: limit}
 }
 
 // EventTooLargeError is the typed form of ErrEventTooLarge returned
@@ -1055,7 +1063,7 @@
 	var refusedSize, refusedLimit C.size_t
 	code := C.net_mesh_send(s.handle, ptrs, lens, count, n.handle,
 		&refusedSize, &refusedLimit)
-	return sendErrorFromCode(code, refusedSize, refusedLimit)
+	return sendErrorFromCode(code, payloads)
 }
 
 // SendWithRetry absorbs ErrBackpressure with exponential backoff up
@@ -1078,7 +1086,7 @@
 	var refusedSize, refusedLimit C.size_t
 	code := C.net_mesh_send_with_retry(s.handle, ptrs, lens, count,
 		C.uint32_t(maxRetries), n.handle, &refusedSize, &refusedLimit)
-	return sendErrorFromCode(code, refusedSize, refusedLimit)
+	return sendErrorFromCode(code, payloads)
 }
 
 // SendBlocking retries ErrBackpressure up to ~13 min worst case.
@@ -1101,7 +1109,7 @@
 	var refusedSize, refusedLimit C.size_t
 	code := C.net_mesh_send_blocking(s.handle, ptrs, lens, count, n.handle,
 		&refusedSize, &refusedLimit)
-	return sendErrorFromCode(code, refusedSize, refusedLimit)
+	return sendErrorFromCode(code, payloads)
 }
 
 // StreamStats returns a snapshot. `nil` if the stream isn't open.
--- go/event_too_large_attribution_testhelpers.go	2026-09-17 10:57:32.197401200 +0200
+++ go/event_too_large_attribution_testhelpers.go (INV-2)	2026-09-17 10:57:27.031740600 +0200
@@ -81,5 +81,5 @@
 		C.size_t(attributedIndex), C.size_t(limit),
 		&refusedSize, &refusedLimit,
 	)
-	return sendErrorFromCode(code, refusedSize, refusedLimit)
+	return sendErrorFromCode(code, payloads)
 }
```

RED (exit 1) — `limit 8104` for the tagged case and `Size 9000` for
the `[9000, 64833]` batch, which are the two values her §4 reports:

```
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
    event_too_large_attribution_test.go:81: EventTooLargeError.Limit = 8104, want 64832 (the fragmentation ceiling that applied); MaxEventSize() is 8104 and reporting it here is the defect — it tells a caller to split an event the transport would have carried
--- FAIL: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
    event_too_large_attribution_test.go:144: EventTooLargeError.Size = 9000, want 64833 — the element the core refused, not the first one above the 8104-byte single-packet limit (9000 bytes), which this peer carries fragmented
--- FAIL: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
FAIL
FAIL	github.com/ai-2070/net/go	0.595s
FAIL
rc=1
```

Restored, identities and GREEN (exit 0):

```
34c9f2e457e194a139b504b5b11e2b8ee49c3ad1e9382983f67af11cd6cdd811  go/mesh.go
f589d4ba9091c9092d19fd214e2a3b5cf922d4a8ed5b3561bfa5b9b5c7b5bf68  go/event_too_large_attribution_testhelpers.go
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
--- PASS: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
--- PASS: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
PASS
ok  	github.com/ai-2070/net/go	0.543s
rc=0
```

---

## INV-3 — the C boundary stops carrying the pair

The Rust half. `stream_err_to_code_attributed` keeps its signature and
its callers but writes nothing, i.e. the ABI goes back to discarding
`{size, limit}`. Required a full cdylib rebuild in both directions.

Applied diff:

```
--- net/crates/net/src/ffi/mesh.rs	2026-09-17 11:03:50.070478000 +0200
+++ src/ffi/mesh.rs (INV-3)	2026-09-17 10:58:20.207253700 +0200
@@ -407,14 +407,7 @@
     out_size: *mut usize,
     out_limit: *mut usize,
 ) -> c_int {
-    if let StreamError::EventTooLarge { size, limit } = err {
-        if !out_size.is_null() {
-            unsafe { *out_size = *size };
-        }
-        if !out_limit.is_null() {
-            unsafe { *out_limit = *limit };
-        }
-    }
+    let _ = (out_size, out_limit);
     stream_err_to_code(err)
 }
```

Rebuild:

```
$ cd net/crates/net && cargo build --release -p net-ffi --features net-ffi/test-helpers
    Finished `release` profile [optimized] target(s) in 5m 15s
build rc=0
```

RED (exit 1). The third failure is the pre-existing LIVE test on a
real handshaken pair, which proves the out-params are load-bearing
through `net_mesh_send` itself and not only through the fixtures seam:

```
$ cd go && go test -tags test_helpers -count=1 -run 'TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit|TestMixedBatchRefusalIsAttributedToTheRefusedEvent|TestSendRefusesOversizePayloadWithTypedError' -v ./...
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
    event_too_large_attribution_test.go:78: EventTooLargeError.Size = 0, want 64833
--- FAIL: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
    event_too_large_attribution_test.go:144: EventTooLargeError.Size = 0, want 64833 — the element the core refused, not the first one above the 8104-byte single-packet limit (9000 bytes), which this peer carries fragmented
--- FAIL: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
=== RUN   TestSendRefusesOversizePayloadWithTypedError
    event_too_large_test.go:150: EventTooLargeError.Size = 0, want 8105 (the oversize element, not the 2-byte one in front of it)
--- FAIL: TestSendRefusesOversizePayloadWithTypedError (0.02s)
FAIL
FAIL	github.com/ai-2070/net/go	0.541s
FAIL
rc=1
```

Restored, identity and rebuild:

```
a17a4c9964d044eeb8b21e3855e93949eea1324e4daf6955e6bd3c504f735ad2  net/crates/net/src/ffi/mesh.rs
    Finished `release` profile [optimized] target(s) in 5m 12s
build rc=0
```

GREEN after restoration is the full-suite run below.

---

## Green state at the repaired head

```
$ cd go && go test -count=1 ./...
ok  	github.com/ai-2070/net/go	90.918s
UNTAGGED rc=0

$ cd go && go test -tags test_helpers -count=1 ./...
ok  	github.com/ai-2070/net/go	95.648s
TAGGED rc=0

$ cd go && go test -count=1 -tags abi_stability -run 'TestABIStability' -v ./...
--- PASS: TestABIStabilityDaemonErrorPrefix
--- PASS: TestABIStabilityDuplicateKindErrorFormat
--- PASS: TestABIStabilityTraversalSentinels
--- PASS: TestABIStabilityChannelAuthSentinel
--- PASS: TestABIStabilityMigrationErrorKinds
--- PASS: TestABIStabilityParseMigrationErrorRoundTrip (+11 subtests)
--- PASS: TestABIStabilityU64FFIRoundTrip
PASS
ok  	github.com/ai-2070/net/go	0.507s
ABI rc=0

$ cd go && go vet ./...
VET rc=0

$ cd go/example && go build .
rc=0

$ gofmt -l go/   -> my four touched/added Go files are absent from the list
                    (the seven files listed are pre-existing deviations
                     in other lanes' files and were not reformatted)
$ rustfmt --edition 2021 --check net/crates/net/src/ffi/mesh.rs        rc=0
$ rustfmt --edition 2021 --check net/crates/net/bindings/node/src/lib.rs  rc=0
```

Header / doc guards that cover the two headers this change edits:

```
$ python .github/scripts/check-one-library-docs.py    rc=0
$ python .github/scripts/check-header-count.py        rc=0
$ python .github/scripts/check-rpc-abi-parity.py      rc=0
  ok    compute: go\net.h matches net\crates\net\bindings\go\compute-ffi\src\lib.rs
$ python .github/scripts/check-callback-buffer-ownership.py  rc=0
```

## Export baseline — ONE added symbol, not regenerated here

```
$ python .github/scripts/check-ffi-exports.py --artifact net/crates/net/target/release/net.dll
  baseline generated-at: 870be93c4c7db5f0aa2c30e30e91e07b6aa1d3b3
  baseline count: 569
✗ net.dll: 1 export(s) NOT in the baseline (added):
    + net_mesh_test_send_refusal_attribution

  A C-ABI export changed. If that is intended, regenerate the baseline
  with `--update` in the same commit and say why; if not, the change
  broke every libnet consumer at once.
rc=1
```

Added: `net_mesh_test_send_refusal_attribution` — one symbol, nothing
removed. Reason: it is the fixtures-gated attribution seam the two new
parity tests drive. Not regenerated by this lane, per instruction.

The three signature changes (`net_mesh_send`,
`net_mesh_send_with_retry`, `net_mesh_send_blocking` each gain
`size_t* out_size, size_t* out_limit`) do **not** appear in this
report because the export checker compares NAMES; they are a C source
and calling-convention break with no symbol-set delta, recorded
separately in the lane report.

## Final state, artifact rebuilt from the final source

After the last (doc-comment-only) edits the cdylib was rebuilt so the
artifact matches the committed source exactly, and the five
event-size tests were re-run against it:

```
$ cd net/crates/net && cargo build --release -p net-ffi --features net-ffi/test-helpers
    Finished `release` profile [optimized] target(s) in 5m 15s
build rc=0

$ cd go && go test -tags test_helpers -count=1 -run 'TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit|TestMixedBatchRefusalIsAttributedToTheRefusedEvent|TestSendRefusesOversizePayloadWithTypedError|TestEventTooLargeHeaderParity|TestMaxEventSizeCrossesTheABI' -v ./...
=== RUN   TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit
--- PASS: TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit (0.00s)
=== RUN   TestMixedBatchRefusalIsAttributedToTheRefusedEvent
--- PASS: TestMixedBatchRefusalIsAttributedToTheRefusedEvent (0.00s)
=== RUN   TestEventTooLargeHeaderParity
--- PASS: TestEventTooLargeHeaderParity (0.00s)
=== RUN   TestMaxEventSizeCrossesTheABI
--- PASS: TestMaxEventSizeCrossesTheABI (0.00s)
=== RUN   TestSendRefusesOversizePayloadWithTypedError
--- PASS: TestSendRefusesOversizePayloadWithTypedError (0.02s)
PASS
ok  	github.com/ai-2070/net/go	0.565s
rc=0
```

The two full-suite runs quoted above (`90.918s` / `95.648s`, both exit
0) were executed against the immediately preceding build of the same
object code; the only source delta since then is rustdoc prose in
`src/ffi/mesh.rs` and the two sibling-binding doc comments.

## Pre-existing failure NOT caused by this lane

A rustdoc run over the core crate with the cdylib's feature set plus
`fixtures` and `RUSTDOCFLAGS=-D warnings` fails on a link this lane
did not touch and did not add:

```
$ RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p net-mesh \
    --features "net,netdb,redex-disk,tool,meshos,meshdb,dataforts,nat-traversal,fixtures"
error: unresolved link to `DeckClient::rtc_anchors`
   --> src\adapter\net\behavior\deck.rs:353:26
    |
353 | /// One RTC anchor, as [`DeckClient::rtc_anchors`] reports it.
    |                          ^^^^^^^^^^^^^^^^^^^^^^^ the struct `DeckClient` has no field or associated item named `rtc_anchors`
doc rc=101
```

`src/adapter/net/behavior/deck.rs` is unmodified in this working tree
(absent from `git status --porcelain`), so this is either committed on
the branch or feature-combination-specific. It is reported, not fixed,
and it is the ONLY rustdoc diagnostic emitted — which is also the
evidence that the intra-doc links added by this lane resolve.

## Confirmation: cgo was genuinely enabled for the inverse runs

Asked for explicitly. Every RED and GREEN above was produced by the
same `bash` environment block, verified from inside it:

```
$ cd go && go env CGO_ENABLED CC && "$(go env CC)" --version | head -1
1
gcc
gcc.exe (Rev13, Built by MSYS2 project) 15.2.0
```

The exact command that produced the INV-1 / INV-2 RED blocks, run in
that environment:

```
cd go && go test -tags test_helpers -count=1 \
  -run 'TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit|TestMixedBatchRefusalIsAttributedToTheRefusedEvent' \
  -v ./...
```

Negative control — the same filter with cgo off does not silently
skip to a meaningless verdict, it fails to build the test binary at
all, so a RED from a cgo-less run is not a thing this package can
produce:

```
$ cd go && CGO_ENABLED=0 go test -tags test_helpers -count=1 \
    -run 'TestTaggedRefusalReportsTheAppliedCeilingNotTheSinglePacketLimit|TestMixedBatchRefusalIsAttributedToTheRefusedEvent' ./...
# github.com/ai-2070/net/go [github.com/ai-2070/net/go.test]
.\meshos_test.go:46:2: undefined: MeshOsDefaultDaemon
.\benchmark_test.go:52:38: undefined: Net
.\capabilities_test.go:16:36: undefined: MeshNode
.\capability_aggregation_e2e_test.go:37:35: undefined: MeshNode
.\compute_test.go:20:34: undefined: MeshNode
.\compute_test.go:39:37: undefined: Identity
...
rc=1
```

Three further intrinsic proofs that those RED numbers came from the
linked cdylib and not from skipped files:

1. `8104` in the RED text is the value of `MaxEventSize()`, which is
   a live `C.net_mesh_max_event_size()` call into `net.dll`. With the
   cgo files dropped, the symbol does not exist.
2. Both tests call `net_mesh_test_send_refusal_attribution` through
   `go/event_too_large_attribution_testhelpers.go`, a cgo file gated
   on `test_helpers`. Its extern reference resolves only against a
   cdylib built with `net-ffi/test-helpers`; a mismatch is a link
   failure, not a skip.
3. INV-3's RED required a 5-minute Rust rebuild to appear and a
   second rebuild to clear. A Go run that had skipped the cgo files
   could not have changed verdict on a Rust-only edit.
