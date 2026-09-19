// The object store you no longer run (Go).
//
// Two in-process mesh nodes over loopback UDP and a content-addressed blob
// store: a producer stores bytes and mints an address, a second node fetches
// them by that address, and storing the same bytes again produces the same
// content hash. There is no bucket to create, no region to pick and no
// replication factor to configure — the address *is* the data.
//
// Run: go run objectstore.go
//
// Expected final line: RESULT ok dedup=1 readback=1 bytes=64
package main

import (
	"fmt"
	"log"
	"net"
	"strings"

	mesh "github.com/ai-2070/net/go"
)

// 64 hex characters = 32 bytes. Every node in a mesh shares it.
var pskHex = strings.Repeat("42", 32)

// A fixed-size payload, so the byte count in the result line is deterministic.
var payload = strings.Repeat("\x5a", 64)

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
	holder, addrHolder := build(0x91)
	reader, _ := build(0x92)

	handshake(holder, reader, addrHolder)

	for _, n := range []*mesh.MeshNode{holder, reader} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{holder, reader} {
			_ = n.Shutdown()
		}
	}()

	// A content-addressed store per node, over its own in-memory log.
	holderRedex := mesh.NewRedex("")
	defer holderRedex.Free()
	holderStore, err := mesh.NewMeshBlobAdapter(holderRedex, "objects", nil)
	if err != nil {
		log.Fatalf("holder adapter: %v", err)
	}
	defer holderStore.Close()

	readerRedex := mesh.NewRedex("")
	defer readerRedex.Free()
	readerStore, err := mesh.NewMeshBlobAdapter(readerRedex, "incoming", nil)
	if err != nil {
		log.Fatalf("reader adapter: %v", err)
	}
	defer readerStore.Close()

	// Install the transfer engine on BOTH nodes before any serve or fetch —
	// a fetch needs it just as much as a serve does.
	if err := holder.ServeBlobTransfer(holderStore); err != nil {
		log.Fatalf("serve (holder): %v", err)
	}
	if err := reader.ServeBlobTransfer(readerStore); err != nil {
		log.Fatalf("serve (reader): %v", err)
	}

	// Mint the address. `Store` needs an already-encoded ref, so `Publish` is
	// the producer half: BLAKE3 the bytes, persist, hand back the encoded ref.
	encoded, err := holderStore.Publish("mesh:orders/2026-09/payload", []byte(payload))
	if err != nil {
		log.Fatalf("publish: %v", err)
	}
	hash, err := mesh.BlobRefHash(encoded)
	if err != nil {
		log.Fatalf("ref hash: %v", err)
	}
	fmt.Printf("stored %d bytes\n", len(payload))
	fmt.Printf("minted address: %x\n", hash)

	// Read your own write, across the mesh. The reader does not have to know
	// which node the bytes settled on — the hash resolves through the mesh.
	readback, err := reader.FetchBlob(holder.NodeID(), hash[:])
	if err != nil {
		log.Fatalf("fetch: %v", err)
	}
	same := string(readback) == payload
	readbackOK := 0
	if same {
		readbackOK = 1
	}

	// Content addressing: the same bytes hash to the same address, whatever
	// URI they were published under. The URI is part of the *ref*, not of the
	// content hash, so the hash is the thing that must agree.
	again, err := holderStore.Publish("mesh:another/name/entirely", []byte(payload))
	if err != nil {
		log.Fatalf("republish: %v", err)
	}
	againHash, err := mesh.BlobRefHash(again)
	if err != nil {
		log.Fatalf("ref hash: %v", err)
	}
	dedup := 0
	if againHash == hash {
		dedup = 1
	}

	fmt.Printf("read back from the mesh:  %d bytes\n", len(readback))
	fmt.Printf("same bytes:               %v\n", same)
	fmt.Printf("republishing elsewhere:   same content hash = %v\n", dedup == 1)

	fmt.Printf("RESULT ok dedup=%d readback=%d bytes=%d\n", dedup, readbackOK, len(readback))
}
