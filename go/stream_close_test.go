// Close must release core stream state, not just the FFI handle.
//
// `MeshStream.Close` called only `net_mesh_stream_free`, which drops
// the handle and its Arc without touching `MeshNode::close_stream`.
// Core state then survived until node shutdown, so a long-lived Go
// node could not release it eagerly, could not enforce a close/reopen
// epoch, and could not reopen the same stream id under a new
// configuration — the first open's config stayed in force.
//
// These originally opened against node id 1 on a lone mesh, which has
// no peer, so `OpenStream` always failed and both tests always hit
// their `t.Skipf`. They ran green having asserted nothing — the exact
// shape of the gap they were written to close. They now stand up a
// real handshaked pair via `meshHandshakePair` and open against the
// peer's actual node id, so a failure to open is a failure, not a
// skip.
//
// Finding #30 (CODE_REVIEW_2026_09_22_WEBRTC_REPAIR_PASS.md)
// re-scoped the superseded-sentinel test's wrapper leg. MR#116's
// commit claimed it caught "a retry wrapper folding -117 into the
// backpressure loop", but the leg drove a CLOSED stream — whose send
// fails at the Go-side handle guard without reaching C — through
// `SendWithRetry` only, and deferred the -117 claim with "a
// displaced session cannot be built from the exported Go surface".
// That deferral was stale, not a limit: `MeshConfig.IdentitySeedHex`
// reproduces a keypair, and `Connect`/`Accept` install their
// negotiated session with `PriorSession::Any` (`install_direct(…,
// None)` in adapter/net/mesh.rs — unconditional replacement of
// whatever incarnation is installed), so a second same-identity
// incarnation displaces a busy incumbent over the plain UDP
// handshake exactly as
// tests/rtc_repairs.rs::a_displaced_sessions_handle_cannot_address_its_successor
// drives over RTC. The closed-stream leg is now
// TestClosedStreamSendsFailFastThroughBothRetryWrappers — narrowed to
// the property it actually drives, extended to `SendBlocking`, and
// given typed assertions — and
// TestSendWrappersTreatSupersededAsTerminalNotBackpressure drives a
// real -117-classified refusal through `SendWithRetry` AND
// `SendBlocking`. No assertion was deleted: every property the old
// leg named still runs, in renamed form, with more assertions than
// before.

package net

import (
	"errors"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"testing"
)

func TestMeshStream_CloseIsIdempotent(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	stream, err := a.OpenStream(b.NodeID(), 7, StreamConfig{})
	if err != nil {
		t.Fatalf("open stream against the handshaked peer: %v", err)
	}

	stream.Close()
	// A second Close must be a no-op rather than a double free — the
	// handle is nilled under the mutex on the first call.
	stream.Close()
}

func TestMeshStream_ReopenAfterCloseUsesTheNewConfig(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()

	const streamID = 11
	peer := b.NodeID()

	first, err := a.OpenStream(peer, streamID, StreamConfig{
		WindowBytes: WindowBytesOf(16384),
	})
	if err != nil {
		t.Fatalf("open stream against the handshaked peer: %v", err)
	}
	first.Close()

	// Without the core close this reopen inherits the first open's
	// config ("first open wins") rather than the one asked for here.
	second, err := a.OpenStream(peer, streamID, StreamConfig{
		WindowBytes: UnboundedWindow(),
	})
	if err != nil {
		t.Fatalf("reopen after close must succeed: %v", err)
	}
	defer second.Close()
}

// -117 must surface as the public [ErrSessionSuperseded] sentinel,
// and it must be the code the C header declares.
//
// Pre-fix `meshErrorFromCode` had no -117 arm, so every send, retry
// and blocking wrapper returned a freshly allocated
// `mesh unknown error (code -117)`: unmatchable by `errors.Is`, and
// indistinguishable from a genuinely unknown future code. The C
// constant existing in both headers did not close Go parity.
func TestSessionSupersededSentinelMatchesTheHeaderCode(t *testing.T) {
	code, ok := headerConstant(t, "NET_ERR_MESH_SESSION_SUPERSEDED")
	if !ok {
		t.Fatal("NET_ERR_MESH_SESSION_SUPERSEDED absent from go/net.h")
	}
	if code != -117 {
		t.Fatalf("NET_ERR_MESH_SESSION_SUPERSEDED = %d, want -117", code)
	}

	err := meshErrorFromCode(-117)
	if !errors.Is(err, ErrSessionSuperseded) {
		t.Fatalf("meshErrorFromCode(-117) = %v, want ErrSessionSuperseded", err)
	}
	// Distinct from the neighbouring stream errors: "the peer is gone
	// or this id is closed" and "the peer is here but this handle
	// belongs to a session it replaced" are different recoveries.
	if errors.Is(err, ErrNotConnected) || errors.Is(err, ErrBackpressure) {
		t.Fatal("ErrSessionSuperseded must not alias ErrNotConnected/ErrBackpressure")
	}
	// The message is the same stable prefix the N-API surface emits
	// (`ERR_SESSION_SUPERSEDED_PREFIX`), so a polyglot operator sees
	// one vocabulary.
	if got, want := ErrSessionSuperseded.Error(), "stream session superseded"; got != want {
		t.Fatalf("ErrSessionSuperseded = %q, want %q", got, want)
	}
}

// A closed stream's sends must fail fast with the typed
// [ErrShuttingDown] refusal, through BOTH retry wrappers, and must
// not be absorbed the way [ErrBackpressure] is.
//
// What this drives is exactly the closed-stream property — and only
// that. `MeshStream.CloseErr` nils the handle under the mutex, so
// these sends fail at the Go-side handle guard before any C call: no
// -117 classification happens here and none can. MR#116's repair
// probed this leg through `SendWithRetry` alone and let its commit
// claim a -117 fold this shape cannot see (finding #30); the -117
// claim lives in
// [TestSendWrappersTreatSupersededAsTerminalNotBackpressure], which
// drives a genuinely displaced handle through both wrappers. The
// "absorb exactly one sentinel" half is unchanged from MR#116 — the
// wrappers absorb ErrBackpressure and nothing else — but is now
// asserted with a concrete typed refusal instead of only "not
// backpressure".
func TestClosedStreamSendsFailFastThroughBothRetryWrappers(t *testing.T) {
	a, b, cleanup := meshHandshakePair(t)
	defer cleanup()
	peer := b.NodeID()
	stream, err := a.OpenStream(peer, 0x1170, StreamConfig{Reliability: "reliable"})
	if err != nil {
		t.Fatalf("open stream: %v", err)
	}
	stream.Close()
	for _, tc := range []struct {
		name string
		send func() error
	}{
		{"SendWithRetry", func() error {
			return stream.SendWithRetry([][]byte{[]byte("x")}, 3)
		}},
		{"SendBlocking", func() error {
			return stream.SendBlocking([][]byte{[]byte("x")})
		}},
	} {
		got := tc.send()
		if got == nil {
			t.Fatalf("a closed stream's %s must fail", tc.name)
		}
		if !errors.Is(got, ErrShuttingDown) {
			t.Fatalf("%s on a closed stream = %v, want the typed ErrShuttingDown refusal",
				tc.name, got)
		}
		if errors.Is(got, ErrBackpressure) {
			t.Fatalf("the retry wrappers absorb only ErrBackpressure; a closed stream's %v must not ride that absorption", got)
		}
	}
}

// A superseded (-117) refusal must surface from BOTH retry wrappers
// as the terminal [ErrSessionSuperseded] — not folded into the
// backpressure loop those wrappers exist to absorb.
//
// This is the defect MR#116's commit names ("a retry wrapper folding
// -117 into the backpressure loop stayed green") and its guard could
// not see: no -117 error ever reached a wrapper there. The
// displacement is built here from the exported Go surface — the
// `MeshConfig.IdentitySeedHex` + plain-UDP `Connect`/`Accept` recipe
// the file header documents — mirroring
// tests/rtc_repairs.rs::a_displaced_sessions_handle_cannot_address_its_successor
// (one identity, two incarnations; the successor displaces the busy
// incumbent) without its RTC transport and epoch-collision
// machinery.
//
// Discrimination: `MeshNode::send_with_retry` (adapter/net/mesh.rs)
// absorbs exactly `StreamError::Backpressure` and propagates every
// other variant from its first attempt; `send_blocking` is that same
// loop with a larger budget. A wrapper that folded -117 into the
// backpressure loop would either misclassify it (failing the typed
// `ErrSessionSuperseded` assertions below) or retry it as pressure —
// and `SendBlocking` would then sit in its ~13-minute (4096 × 200 ms)
// budget instead of returning promptly. A green run therefore
// observes both a typed terminal AND a prompt return through each
// wrapper.
//
// Positive control: after the refusals, the successor session's own
// handle sends through the same two wrappers — so the refusals are
// about the displaced handle, not a broken send path. Pre-displacement
// the stale handle itself also sends successfully, so a later refusal
// cannot be blamed on a handle that never worked.
func TestSendWrappersTreatSupersededAsTerminalNotBackpressure(t *testing.T) {
	// One identity, two incarnations: b holds the incumbent session;
	// b2 is the same node id arriving from its own socket. Both
	// live simultaneously, like TestOrgSeededMeshesStableEphemeralNot.
	seed := hexSeed(0xB2)
	aAddr, bAddr := allocPortPair(t)
	a, err := NewMeshNode(MeshConfig{BindAddr: aAddr, PskHex: meshPsk})
	if err != nil {
		t.Fatalf("new mesh a: %v", err)
	}
	b, err := NewMeshNode(MeshConfig{BindAddr: bAddr, PskHex: meshPsk, IdentitySeedHex: seed})
	if err != nil {
		a.Shutdown()
		t.Fatalf("new mesh b: %v", err)
	}
	b2Addr := reserveLocalUDPPort(t)
	b2, err := NewMeshNode(MeshConfig{
		BindAddr:        b2Addr,
		PskHex:          meshPsk,
		IdentitySeedHex: seed,
	})
	if err != nil {
		a.Shutdown()
		b.Shutdown()
		t.Fatalf("new mesh b2: %v", err)
	}
	cleanup := func() {
		a.Shutdown()
		b.Shutdown()
		b2.Shutdown()
	}
	defer cleanup()

	bPub, err := b.PublicKey()
	if err != nil {
		t.Fatalf("public key: %v", err)
	}
	// The successor's OWN transport key. `MeshNode::new` generates
	// the Noise static per process (`StaticKeypair::generate()` in
	// adapter/net/mesh.rs) — `IdentitySeedHex` pins the ed25519
	// entity/node id, not the handshake key — so b2 shares b's node
	// id but not its x25519 static, exactly like the Rust
	// displacement's `node_with_identity` second incarnation. The
	// second handshake must address the key the successor process
	// actually holds (and it does NOT advertise b's: identity here
	// is the node id, which is what the session fencing keys on).
	b2Pub, err := b2.PublicKey()
	if err != nil {
		t.Fatalf("b2 public key: %v", err)
	}
	aID, bID := a.NodeID(), b.NodeID()
	if got := b2.NodeID(); got != bID {
		t.Fatalf("b2 node id = %d, want the incumbent's %d — b2 must be the same identity, or nothing is displaced", got, bID)
	}

	// The incumbent session, handshaked like meshHandshakePair.
	acceptDone := make(chan error, 1)
	go func() {
		_, err := b.Accept(aID)
		acceptDone <- err
	}()
	connErr := a.Connect(bAddr, bPub, bID)
	accErr := <-acceptDone
	if connErr != nil || accErr != nil {
		t.Fatalf("incumbent handshake: connect=%v accept=%v", connErr, accErr)
	}
	if err := a.Start(); err != nil {
		t.Fatalf("start a: %v", err)
	}
	if err := b.Start(); err != nil {
		t.Fatalf("start b: %v", err)
	}

	// The handle that will be displaced — opened AND used on the
	// incumbent, so a refusal later cannot be blamed on a handle
	// that never worked. Deliberately not closed: a closed handle is
	// spent at the Go guard and can never reach the C wrappers.
	stale, err := a.OpenStream(bID, 0x0776, StreamConfig{Reliability: "reliable"})
	if err != nil {
		t.Fatalf("open stream on the incumbent: %v", err)
	}
	defer stale.Close()
	if err := stale.SendWithRetry([][]byte{[]byte("incumbent")}, 3); err != nil {
		t.Fatalf("the handle must send on the session it was opened on: %v", err)
	}

	// The displacement: a second handshake with the same identity,
	// from b2's own socket. `Connect`/`Accept` install with
	// `PriorSession::Any` (`install_direct(…, None)` — unconditional
	// replacement), so this replaces the incumbent on both sides and
	// strands `stale`. b2 accepts pre-Start, exactly like the
	// incumbent did.
	acceptDone = make(chan error, 1)
	go func() {
		_, err := b2.Accept(aID)
		acceptDone <- err
	}()
	connErr = a.Connect(b2Addr, b2Pub, bID)
	accErr = <-acceptDone
	if connErr != nil || accErr != nil {
		t.Fatalf("second same-identity handshake: connect=%v accept=%v", connErr, accErr)
	}
	if err := b2.Start(); err != nil {
		t.Fatalf("start b2: %v", err)
	}

	// The refusals, typed and prompt, through BOTH wrappers. If the
	// incumbent was not actually displaced these sends succeed and
	// the "must fail" leg names that premise break; if a wrapper
	// folds -117 into its backpressure loop these legs fail on the
	// typed assertion (misclassified) or never return (retried as
	// pressure — SendBlocking's budget is ~13 min).
	for _, tc := range []struct {
		name string
		send func() error
	}{
		{"SendWithRetry", func() error {
			return stale.SendWithRetry([][]byte{[]byte("stale")}, 3)
		}},
		{"SendBlocking", func() error {
			return stale.SendBlocking([][]byte{[]byte("stale")})
		}},
	} {
		got := tc.send()
		if got == nil {
			t.Fatalf("%s on a displaced handle must fail — the incumbent session was not replaced", tc.name)
		}
		if !errors.Is(got, ErrSessionSuperseded) {
			t.Fatalf("%s on a displaced handle = %v, want the terminal ErrSessionSuperseded (not retried, not folded)", tc.name, got)
		}
		if errors.Is(got, ErrBackpressure) {
			t.Fatalf("%s folded a superseded refusal into backpressure: %v", tc.name, got)
		}
	}

	// Restored positive: the successor's own handle sends through the
	// same wrappers — the refusals above are about the displaced
	// handle, not the send path.
	fresh, err := a.OpenStream(bID, 0x0776, StreamConfig{Reliability: "fire_and_forget"})
	if err != nil {
		t.Fatalf("reopen the same id on the successor session: %v", err)
	}
	defer fresh.Close()
	if err := fresh.SendWithRetry([][]byte{[]byte("fresh")}, 3); err != nil {
		t.Fatalf("the successor's handle must send through the retry wrapper: %v", err)
	}
	if err := fresh.SendBlocking([][]byte{[]byte("fresh")}); err != nil {
		t.Fatalf("the successor's handle must send through the blocking wrapper: %v", err)
	}
}

// headerConstant reads one NET_* enum constant out of `go/net.h` —
// the header cgo actually compiles against — so the Go sentinel and
// the C ABI cannot drift apart silently.
func headerConstant(t *testing.T, name string) (int, bool) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(".", "net.h"))
	if err != nil {
		t.Fatalf("read net.h: %v", err)
	}
	re := regexp.MustCompile(`\b` + regexp.QuoteMeta(name) + `\s*=\s*(-?\d+)`)
	m := re.FindSubmatch(raw)
	if m == nil {
		return 0, false
	}
	value, err := strconv.Atoi(string(m[1]))
	if err != nil {
		t.Fatalf("parse %s: %v", name, err)
	}
	return value, true
}
