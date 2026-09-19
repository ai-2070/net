// The sidecar you no longer run: call a service, not a host (Go).
//
// Two providers serve the same service name. A caller addresses the **service**
// — never a node id — and when the provider that answered first goes away, the
// next call lands on the survivor, with a bounded retry absorbing the transient
// while the dead provider is still in the roster.
//
// Rust has a retry helper that does this for you; this binding has none, so the
// loop below is written by hand. That is the only difference from the Rust
// sibling.
//
// Run: go run failover.go
//
// Expected final line: RESULT ok providers=2 moved=1 served=2
package main

import (
	"context"
	"fmt"
	"log"
	"net"
	"strings"
	"time"

	mesh "github.com/ai-2070/net/go"
)

// 64 hex characters = 32 bytes. Every node in a mesh shares it.
var pskHex = strings.Repeat("42", 32)

// The roster still lists a dead provider until the capability fold converges.
const (
	retryAttempts   = 6
	retryInterval   = 250 * time.Millisecond
	callTimeout     = 500 * time.Millisecond
	firstCallTimout = 1 * time.Second
)

type work struct {
	Units uint32 `json:"units"`
}

type answer struct {
	ServedBy string `json:"served_by"`
	Units    uint32 `json:"units"`
}

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
		BindAddr:        addr,
		PskHex:          pskHex,
		IdentitySeedHex: strings.Repeat(fmt.Sprintf("%02x", seed), 32),
		HeartbeatMs:     200,
	})
	if err != nil {
		log.Fatalf("new mesh node: %v", err)
	}
	return node, addr
}

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

func main() {
	caller, addrCaller := build(0xC1)
	providerOne, _ := build(0xC2)
	providerTwo, _ := build(0xC3)

	handshake(caller, providerOne, addrCaller)
	handshake(caller, providerTwo, addrCaller)
	for _, n := range []*mesh.MeshNode{caller, providerOne, providerTwo} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{caller, providerOne, providerTwo} {
			_ = n.Shutdown()
		}
	}()

	oneID := providerOne.NodeID()
	twoID := providerTwo.NodeID()
	oneHex := fmt.Sprintf("0x%x", oneID)
	twoHex := fmt.Sprintf("0x%x", twoID)

	rawOne, err := mesh.NewMeshRpc(providerOne)
	if err != nil {
		log.Fatalf("rpc one: %v", err)
	}
	rawTwo, err := mesh.NewMeshRpc(providerTwo)
	if err != nil {
		log.Fatalf("rpc two: %v", err)
	}
	rawCaller, err := mesh.NewMeshRpc(caller)
	if err != nil {
		log.Fatalf("rpc caller: %v", err)
	}
	defer rawCaller.Close()

	typedOne := mesh.NewTypedMeshRpc(rawOne)
	typedTwo := mesh.NewTypedMeshRpc(rawTwo)
	client := mesh.NewTypedMeshRpc(rawCaller)

	serveOne, err := mesh.TypedServe[work, answer](typedOne, "work", func(w work) (answer, error) {
		return answer{ServedBy: oneHex, Units: w.Units}, nil
	})
	if err != nil {
		log.Fatalf("serve one: %v", err)
	}
	serveTwo, err := mesh.TypedServe[work, answer](typedTwo, "work", func(w work) (answer, error) {
		return answer{ServedBy: twoHex, Units: w.Units}, nil
	})
	if err != nil {
		log.Fatalf("serve two: %v", err)
	}

	// The caller discovers the service by name. It learns who can serve it,
	// never whom to prefer — that is what makes the next part work.
	deadline := time.Now().Add(5 * time.Second)
	var providers []uint64
	for time.Now().Before(deadline) {
		providers, err = client.Raw().FindServiceNodes("work")
		if err == nil && len(providers) == 2 {
			break
		}
		time.Sleep(25 * time.Millisecond)
	}
	fmt.Printf("providers advertising `work`: %d\n", len(providers))

	call := func(units uint32, timeout time.Duration) (answer, error) {
		ctx, cancel := context.WithTimeout(context.Background(), timeout)
		defer cancel()
		return mesh.TypedCallService[work, answer](ctx, client, "work", work{Units: units})
	}

	// Call the service, not a host.
	first, err := call(2, firstCallTimout)
	if err != nil {
		log.Fatalf("first call: %v", err)
	}
	fmt.Printf("first call served by:  %s\n", first.ServedBy)

	// Take that provider out of the mesh entirely — the hard version of a
	// deploy: not a drain, a death. Its RPC handles close first, because a
	// shutting-down handle cannot retire a stream.
	doomedIsOne := first.ServedBy == oneHex
	doomed, rawDoomed, serveDoomed := providerTwo, rawTwo, serveTwo
	survivorServe := serveOne
	if doomedIsOne {
		doomed, rawDoomed, serveDoomed = providerOne, rawOne, serveOne
		survivorServe = serveTwo
	}
	serveDoomed.Close()
	rawDoomed.Close()
	if err := doomed.Shutdown(); err != nil {
		log.Fatalf("shutdown doomed: %v", err)
	}
	fmt.Printf("took %s out\n", first.ServedBy)

	var second answer
	answered := false
	for attempt := 0; attempt < retryAttempts && !answered; attempt++ {
		second, err = call(5, callTimeout)
		if err == nil {
			answered = true
			break
		}
		time.Sleep(retryInterval)
	}
	if !answered {
		log.Fatalf("no provider answered after the death: %v", err)
	}
	fmt.Printf("after the death served by: %s\n", second.ServedBy)

	moved := 0
	if second.ServedBy != first.ServedBy {
		moved = 1
	}
	served := 0
	if first.Units == 2 {
		served++
	}
	if second.Units == 5 {
		served++
	}
	fmt.Printf("RESULT ok providers=%d moved=%d served=%d\n", len(providers), moved, served)

	survivorServe.Close()
}
