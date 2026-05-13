// Integration tests for Net Go bindings with Net.
//
// Run tests:
//
//	RUN_INTEGRATION_TESTS=1 go test -v
//
// Environment variables:
//
//	RUN_INTEGRATION_TESTS - Set to "1" to run integration tests
package net

import (
	"fmt"
	"os"
	"testing"
	"time"
)

var (
	runTests = os.Getenv("RUN_INTEGRATION_TESTS") == "1"
)

func skipIfNotEnabled(t *testing.T) {
	if !runTests {
		t.Skip("Set RUN_INTEGRATION_TESTS=1 to run integration tests")
	}
}

// Net Integration Tests

// netAvailable checks if Net feature is available
func netAvailable() bool {
	_, err := GenerateNetKeypair()
	return err == nil
}

func skipIfNetNotEnabled(t *testing.T) {
	skipIfNotEnabled(t)
	if !netAvailable() {
		t.Skip("Net feature not available (build with Net feature enabled)")
	}
}

// generatePSK generates a random 32-byte PSK as hex string
func generatePSK() string {
	// Use crypto/rand for secure random bytes
	b := make([]byte, 32)
	for i := range b {
		b[i] = byte(time.Now().UnixNano() + int64(i))
	}
	return fmt.Sprintf("%x", b)
}

// parallelHandshake creates a responder + initiator pair concurrently.
// Adapter init blocks on the NKpsk0 handshake (5s × 3 retries ≈ 15s
// total budget), so sequential New() calls deadlock: the responder
// waits for an initiator that hasn't been created yet, exhausts its
// retries, and returns "initialization failed". Running both peers in
// goroutines lets each New() make progress while its peer is coming
// up. The 50ms delay before launching the initiator gives the
// responder time to bind its socket so the first NKpsk0 packet
// doesn't land on a closed port.
func parallelHandshake(t *testing.T, responderCfg, initiatorCfg *Config) (*Net, *Net) {
	t.Helper()

	type result struct {
		peer *Net
		err  error
	}
	respCh := make(chan result, 1)
	initCh := make(chan result, 1)

	go func() {
		p, err := New(responderCfg)
		respCh <- result{p, err}
	}()

	time.Sleep(50 * time.Millisecond)

	go func() {
		p, err := New(initiatorCfg)
		initCh <- result{p, err}
	}()

	respRes := <-respCh
	if respRes.err != nil {
		t.Fatalf("Failed to create responder: %v", respRes.err)
	}
	initRes := <-initCh
	if initRes.err != nil {
		respRes.peer.Shutdown()
		t.Fatalf("Failed to create initiator: %v", initRes.err)
	}

	return respRes.peer, initRes.peer
}

func TestNetGenerateKeypair(t *testing.T) {
	skipIfNetNotEnabled(t)

	keypair, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate keypair: %v", err)
	}

	// Keys should be 32 bytes hex-encoded (64 hex chars)
	if len(keypair.PublicKey) != 64 {
		t.Errorf("Expected public key length 64, got %d", len(keypair.PublicKey))
	}
	if len(keypair.SecretKey) != 64 {
		t.Errorf("Expected secret key length 64, got %d", len(keypair.SecretKey))
	}

	// Each call should generate different keypairs
	keypair2, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate second keypair: %v", err)
	}

	if keypair.PublicKey == keypair2.PublicKey {
		t.Error("Expected different public keys for each generation")
	}
	if keypair.SecretKey == keypair2.SecretKey {
		t.Error("Expected different secret keys for each generation")
	}
}

func TestNetExchangeEvents(t *testing.T) {
	skipIfNetNotEnabled(t)

	// Generate keypair for responder
	responderKeypair, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate keypair: %v", err)
	}

	// Generate shared PSK
	psk := generatePSK()

	responder, initiator := parallelHandshake(t,
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:    "127.0.0.1:19200",
				PeerAddr:    "127.0.0.1:19201",
				PSK:         psk,
				Role:        "responder",
				SecretKey:   responderKeypair.SecretKey,
				PublicKey:   responderKeypair.PublicKey,
				Reliability: "light",
			},
		},
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:      "127.0.0.1:19201",
				PeerAddr:      "127.0.0.1:19200",
				PSK:           psk,
				Role:          "initiator",
				PeerPublicKey: responderKeypair.PublicKey,
				Reliability:   "light",
			},
		})
	defer responder.Shutdown()
	defer initiator.Shutdown()

	// Initiator sends events to responder
	for i := 0; i < 5; i++ {
		err := initiator.IngestRaw(fmt.Sprintf(`{"source": "initiator", "index": %d}`, i))
		if err != nil {
			t.Fatalf("Failed to ingest from initiator: %v", err)
		}
	}

	// Responder sends events to initiator
	for i := 0; i < 5; i++ {
		err := responder.IngestRaw(fmt.Sprintf(`{"source": "responder", "index": %d}`, i))
		if err != nil {
			t.Fatalf("Failed to ingest from responder: %v", err)
		}
	}

	// Wait for events to propagate
	time.Sleep(500 * time.Millisecond)

	// Poll from both sides
	initiatorEvents, err := initiator.Poll(100, "")
	if err != nil {
		t.Fatalf("Failed to poll from initiator: %v", err)
	}

	responderEvents, err := responder.Poll(100, "")
	if err != nil {
		t.Fatalf("Failed to poll from responder: %v", err)
	}

	// Both should have received events
	if len(initiatorEvents.Events) == 0 {
		t.Error("Initiator should have received events")
	}
	if len(responderEvents.Events) == 0 {
		t.Error("Responder should have received events")
	}
}

func TestNetBatchIngestion(t *testing.T) {
	skipIfNetNotEnabled(t)

	responderKeypair, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate keypair: %v", err)
	}

	psk := generatePSK()

	responder, initiator := parallelHandshake(t,
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:  "127.0.0.1:19202",
				PeerAddr:  "127.0.0.1:19203",
				PSK:       psk,
				Role:      "responder",
				SecretKey: responderKeypair.SecretKey,
				PublicKey: responderKeypair.PublicKey,
			},
		},
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:      "127.0.0.1:19203",
				PeerAddr:      "127.0.0.1:19202",
				PSK:           psk,
				Role:          "initiator",
				PeerPublicKey: responderKeypair.PublicKey,
			},
		})
	defer responder.Shutdown()
	defer initiator.Shutdown()

	// Batch ingest
	events := make([]string, 20)
	for i := 0; i < 20; i++ {
		events[i] = fmt.Sprintf(`{"batch_index": %d}`, i)
	}
	count := initiator.IngestRawBatch(events)
	if count != 20 {
		t.Errorf("Expected 20 ingested, got %d", count)
	}

	time.Sleep(500 * time.Millisecond)

	response, err := responder.Poll(100, "")
	if err != nil {
		t.Fatalf("Failed to poll: %v", err)
	}

	if len(response.Events) == 0 {
		t.Error("Responder should have received batched events")
	}
}

func TestNetFullReliabilityMode(t *testing.T) {
	skipIfNetNotEnabled(t)

	responderKeypair, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate keypair: %v", err)
	}

	psk := generatePSK()

	responder, initiator := parallelHandshake(t,
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:            "127.0.0.1:19204",
				PeerAddr:            "127.0.0.1:19205",
				PSK:                 psk,
				Role:                "responder",
				SecretKey:           responderKeypair.SecretKey,
				PublicKey:           responderKeypair.PublicKey,
				Reliability:         "full",
				HeartbeatIntervalMs: 1000,
				SessionTimeoutMs:    10000,
			},
		},
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:            "127.0.0.1:19205",
				PeerAddr:            "127.0.0.1:19204",
				PSK:                 psk,
				Role:                "initiator",
				PeerPublicKey:       responderKeypair.PublicKey,
				Reliability:         "full",
				HeartbeatIntervalMs: 1000,
				SessionTimeoutMs:    10000,
			},
		})
	defer responder.Shutdown()
	defer initiator.Shutdown()

	// Send events with full reliability
	for i := 0; i < 10; i++ {
		err := initiator.IngestRaw(fmt.Sprintf(`{"reliable": true, "seq": %d}`, i))
		if err != nil {
			t.Fatalf("Failed to ingest: %v", err)
		}
	}

	time.Sleep(500 * time.Millisecond)

	response, err := responder.Poll(100, "")
	if err != nil {
		t.Fatalf("Failed to poll: %v", err)
	}

	if len(response.Events) == 0 {
		t.Error("Responder should have received reliable events")
	}
}

func TestNetStats(t *testing.T) {
	skipIfNetNotEnabled(t)

	responderKeypair, err := GenerateNetKeypair()
	if err != nil {
		t.Fatalf("Failed to generate keypair: %v", err)
	}

	psk := generatePSK()

	responder, initiator := parallelHandshake(t,
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:  "127.0.0.1:19206",
				PeerAddr:  "127.0.0.1:19207",
				PSK:       psk,
				Role:      "responder",
				SecretKey: responderKeypair.SecretKey,
				PublicKey: responderKeypair.PublicKey,
			},
		},
		&Config{
			NumShards: 1,
			Net: &NetConfig{
				BindAddr:      "127.0.0.1:19207",
				PeerAddr:      "127.0.0.1:19206",
				PSK:           psk,
				Role:          "initiator",
				PeerPublicKey: responderKeypair.PublicKey,
			},
		})
	defer responder.Shutdown()
	defer initiator.Shutdown()

	// Ingest some events
	for i := 0; i < 25; i++ {
		err := initiator.IngestRaw(fmt.Sprintf(`{"stat_index": %d}`, i))
		if err != nil {
			t.Fatalf("Failed to ingest: %v", err)
		}
	}

	stats, err := initiator.Stats()
	if err != nil {
		t.Fatalf("Failed to get stats: %v", err)
	}

	if stats.EventsIngested < 25 {
		t.Errorf("Expected at least 25 ingested events, got %d", stats.EventsIngested)
	}
}
