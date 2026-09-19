// Scoped credentials, not an open channel (Go).
//
// Two nodes. The publisher owns a channel whose subscriber ACL is rooted at
// its own entity id; a subscriber holding a token minted for *its* entity id,
// for *this* channel, with the subscribe scope is admitted. The same
// subscriber asking without the token is refused.
//
// This is the shape a broker makes you build out of ACL files and a separate
// auth service: there is one identity, one token, one place the decision is
// made, and the credential is presented per subscribe rather than cached by a
// connection.
//
// Run: go run tokenchannel.go
//
// Expected final line: RESULT ok granted=1 refused=1
package main

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"log"
	"net"
	"strings"

	mesh "github.com/ai-2070/net/go"
)

// 64 hex characters = 32 bytes. Every node in a mesh shares it.
var pskHex = strings.Repeat("42", 32)

// The token's lifetime. Long enough for the example, short enough that a
// minted credential is never a standing secret.
const tokenTTLSeconds = 300

func reserveAddr() string {
	conn, err := net.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		log.Fatalf("reserve udp port: %v", err)
	}
	addr := conn.LocalAddr().String()
	_ = conn.Close()
	return addr
}

func seedBytes(seed byte) []byte {
	return bytes.Repeat([]byte{seed}, 32)
}

func build(seed byte) (*mesh.MeshNode, *mesh.Identity, string) {
	identity, err := mesh.IdentityFromSeed(seedBytes(seed))
	if err != nil {
		log.Fatalf("identity: %v", err)
	}
	addr := reserveAddr()
	node, err := mesh.NewMeshNode(mesh.MeshConfig{
		BindAddr:        addr,
		PskHex:          pskHex,
		IdentitySeedHex: hex.EncodeToString(seedBytes(seed)),
		HeartbeatMs:     200,
	})
	if err != nil {
		log.Fatalf("new mesh node: %v", err)
	}
	return node, identity, addr
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
	const channel = "config/gated"

	publisher, publisherIdentity, publisherAddr := build(0xE1)
	defer publisherIdentity.Close()
	subscriber, subscriberIdentity, _ := build(0xE2)
	defer subscriberIdentity.Close()

	handshake(publisher, subscriber, publisherAddr)

	for _, n := range []*mesh.MeshNode{publisher, subscriber} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{publisher, subscriber} {
			_ = n.Shutdown()
		}
	}()

	// Nothing else to set up. A token's leaf binds to the subscribing peer's
	// EntityId, and the runtime establishes that binding as part of the
	// token-bearing subscribe itself — a bounded, session-bound identity
	// proof over the encrypted session. The subscriber advertises no
	// capabilities and queries no discovery index; a consumer should not
	// have to publish services to use a credential issued to it.

	// The channel's subscriber ACL is rooted at the publisher's own entity id.
	// Setting TokenRoots is what turns token enforcement on, so this is one
	// declaration rather than a flag plus a trust anchor that can disagree.
	publisherEntityID, err := publisherIdentity.EntityID()
	if err != nil {
		log.Fatalf("publisher entity id: %v", err)
	}
	if err := publisher.RegisterChannel(mesh.ChannelConfig{
		Name:         channel,
		Visibility:   "global",
		RequireToken: true,
		TokenRoots:   []string{hex.EncodeToString(publisherEntityID)},
	}); err != nil {
		log.Fatalf("register channel: %v", err)
	}
	fmt.Printf("channel gated on a token rooted at 0x%x\n", publisherIdentity.OriginHash())

	// A credential scoped three ways: to this subscriber's entity id, to this
	// channel, and to the subscribe action alone. It cannot publish, and it is
	// useless to any other node.
	subscriberEntityID, err := subscriberIdentity.EntityID()
	if err != nil {
		log.Fatalf("subscriber entity id: %v", err)
	}
	token, err := publisherIdentity.IssueToken(mesh.IssueTokenRequest{
		Subject:    subscriberEntityID,
		Scope:      []string{"subscribe"},
		Channel:    channel,
		TTLSeconds: tokenTTLSeconds,
	})
	if err != nil {
		log.Fatalf("issue token: %v", err)
	}
	fmt.Println("issued a subscribe-only token to the subscriber")

	// Without it: refused. The publisher answers and says no, which is a
	// different outcome from "the publisher never answered".
	refused := subscriber.SubscribeChannel(publisher.NodeID(), channel) != nil
	fmt.Printf("bare subscribe refused:            %v\n", refused)

	// With it: admitted. The credential is presented on the subscribe request
	// itself, not negotiated once per connection.
	granted := subscriber.SubscribeChannelWithToken(publisher.NodeID(), channel, token) == nil
	fmt.Printf("token-carrying subscribe admitted: %v\n", granted)

	grantedFlag, refusedFlag := 0, 0
	if granted {
		grantedFlag = 1
	}
	if refused {
		refusedFlag = 1
	}
	fmt.Printf("RESULT ok granted=%d refused=%d\n", grantedFlag, refusedFlag)
}
