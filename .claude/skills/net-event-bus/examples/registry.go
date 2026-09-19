// The service registry you no longer run (Go).
//
// Four in-process mesh nodes over loopback UDP: two providers announce the same
// capability, a caller discovers them and ranks them locally, a third provider
// appears, and the caller's next lookup ranks it first — with no registry
// process, no health-check poller, no config reload and no announcement to any
// address.
//
// Run: go run registry.go
//
// Expected final line: RESULT ok providers=3 joined=1 best_moved=1
package main

import (
	"fmt"
	"log"
	"net"
	"strings"
	"time"

	mesh "github.com/ai-2070/net/go"
)

// 64 hex characters = 32 bytes. Every node in a mesh shares it.
var pskHex = strings.Repeat("42", 32)

const converge = 5 * time.Second

// reserveAddr picks a free loopback port. The Go binding does not expose
// `localAddr`, so a node cannot bind to `:0` and be asked where it landed —
// the address has to exist before the node does.
func reserveAddr() string {
	conn, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		log.Fatalf("reserve udp port: %v", err)
	}
	addr := conn.LocalAddr().String()
	_ = conn.Close()
	return addr
}

func build(seed byte) (*mesh.MeshNode, string) {
	addr := reserveAddr()
	node, err := mesh.NewMeshNode(mesh.MeshConfig{
		BindAddr: addr,
		PskHex:   pskHex,
		// A distinct identity per node. The same seed on two nodes gives them
		// the same node id, and calls to "the second one" then land on whichever
		// peer entry won.
		IdentitySeedHex: strings.Repeat(fmt.Sprintf("%02x", seed), 32),
		HeartbeatMs:     200,
	})
	if err != nil {
		log.Fatalf("new mesh node: %v", err)
	}
	return node, addr
}

// handshake pairs one connect with one accept. Accept must land before Start;
// both calls block until the Noise handshake completes.
func handshake(responder, initiator *mesh.MeshNode, responderAddr string) {
	pub, err := responder.PublicKey()
	if err != nil {
		log.Fatalf("public key: %v", err)
	}
	done := make(chan error, 1)
	go func() {
		_, err := responder.Accept(initiator.NodeID())
		done <- err
	}()
	if err := initiator.Connect(responderAddr, pub, responder.NodeID()); err != nil {
		log.Fatalf("connect: %v", err)
	}
	if err := <-done; err != nil {
		log.Fatalf("accept: %v", err)
	}
}

func until(what string, pred func() bool) {
	deadline := time.Now().Add(converge)
	for time.Now().Before(deadline) {
		if pred() {
			return
		}
		time.Sleep(25 * time.Millisecond)
	}
	log.Fatalf("timed out waiting for %s", what)
}

func tagFilter() mesh.CapabilityFilter {
	return mesh.CapabilityFilter{RequireTags: []string{"api"}}
}

func main() {
	a, addrA := build(0xA1) // provider, 16 GB
	b, addrB := build(0xB2) // provider, 64 GB
	c, addrC := build(0xC3) // provider that appears later, 256 GB
	caller, addrCaller := build(0xD4)

	_ = addrC

	// Every accept() completes before any start().
	handshake(b, a, addrB)           // A <-> B
	handshake(a, caller, addrA)      // A <-> caller
	handshake(caller, b, addrCaller) // B <-> caller
	handshake(caller, c, addrCaller) // C <-> caller

	for _, n := range []*mesh.MeshNode{a, b, c, caller} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{a, b, c, caller} {
			_ = n.Shutdown()
		}
	}()

	// The two original providers announce. That is the entire registration.
	if err := a.AnnounceCapabilities(mesh.CapabilitySet{
		Hardware: &mesh.HardwareCaps{MemoryGB: 16},
		Tags:     []string{"api"},
	}); err != nil {
		log.Fatalf("announce a: %v", err)
	}
	if err := b.AnnounceCapabilities(mesh.CapabilitySet{
		Hardware: &mesh.HardwareCaps{MemoryGB: 64},
		Tags:     []string{"api"},
	}); err != nil {
		log.Fatalf("announce b: %v", err)
	}

	count := func(n *mesh.MeshNode) int {
		ids, err := n.FindNodes(tagFilter())
		if err != nil {
			log.Fatalf("find nodes: %v", err)
		}
		return len(ids)
	}

	until("both providers to appear", func() bool { return count(caller) == 2 })
	providers := count(caller)

	// Ranking is local and free: prefer more memory, so the bigger machine wins
	// without any scheduler deciding it centrally. `ok` disambiguates "no match"
	// from node id 0, which is a real id.
	req := mesh.CapabilityRequirement{Filter: tagFilter(), PreferMoreMemory: 1.0}
	first, ok, err := caller.FindBestNode(req)
	if err != nil || !ok {
		log.Fatalf("no provider matched: %v", err)
	}

	// A new provider appears: it announces once and is immediately addressable.
	if err := c.AnnounceCapabilities(mesh.CapabilitySet{
		Hardware: &mesh.HardwareCaps{MemoryGB: 256},
		Tags:     []string{"api"},
	}); err != nil {
		log.Fatalf("announce c: %v", err)
	}
	until("the new provider to appear", func() bool { return count(caller) == 3 })
	after := count(caller)

	best, ok, err := caller.FindBestNode(req)
	if err != nil || !ok {
		log.Fatalf("no provider matched after the join: %v", err)
	}

	moved := 0
	if first != best {
		moved = 1
	}
	fmt.Printf("providers found at first lookup: %d\n", providers)
	fmt.Printf("ranked best:                     0x%x\n", first)
	fmt.Printf("providers after the join:        %d\n", after)
	fmt.Printf("ranked best:                     0x%x\n", best)

	fmt.Printf("RESULT ok providers=%d joined=1 best_moved=%d\n", after, moved)
}
