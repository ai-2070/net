// Live config — the config service you no longer run (Go).
//
// One publisher and two subscribers, three in-process mesh nodes over loopback
// UDP. The publisher registers a channel, both subscribers join by name, and
// every config revision is pushed once and applied by each subscriber locally.
// There is no config server to poll, no cache to invalidate and no reload to
// coordinate.
//
// Run: go run liveconfig.go
//
// Expected final line: RESULT ok subscribers=2 applied=2 version=2
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

const deliver = 5 * time.Second

// Both subscribers are in the roster before the first publish, because
// SubscribeChannel blocks on the publisher's ack.
const subscribers = 2

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

// parse reads `v=<n>;mode=<name>`. Unreadable revisions are ignored.
func parse(payload []byte) (int, string, bool) {
	version := -1
	mode := ""
	for _, field := range strings.Split(string(payload), ";") {
		if rest, ok := strings.CutPrefix(field, "v="); ok {
			if _, err := fmt.Sscanf(rest, "%d", &version); err != nil {
				return 0, "", false
			}
		} else if rest, ok := strings.CutPrefix(field, "mode="); ok {
			mode = rest
		}
	}
	if version < 0 || mode == "" {
		return 0, "", false
	}
	return version, mode, true
}

// checkDelivery fails loudly when a revision did not reach every subscriber.
// The publisher's own report is the evidence, so read it instead of assuming
// the fan-out worked.
func checkDelivery(version int, report *mesh.PublishReport) {
	if report.Attempted != subscribers {
		log.Fatalf("v%d: roster held %d subscribers, expected %d",
			version, report.Attempted, subscribers)
	}
	if report.Delivered != report.Attempted || len(report.Errors) > 0 {
		log.Fatalf("v%d: delivered to %d of %d subscribers, errors: %v",
			version, report.Delivered, report.Attempted, report.Errors)
	}
}

// checkApplied fails loudly unless this subscriber applied both revisions with
// the modes the publisher sent. The final line claims exactly that, so it is
// checked before it is printed: a revision that never arrived is a failure,
// not a quieter success.
func checkApplied(name string, applied map[int]string) {
	for _, revision := range []struct {
		version int
		mode    string
	}{{1, "blue"}, {2, "green"}} {
		if applied[revision.version] != revision.mode {
			log.Fatalf("subscriber %s never applied v%d=%s: %v",
				name, revision.version, revision.mode, applied)
		}
	}
}

// drain polls every shard. A published event lands on the shard derived from
// its stream id, so a consumer drains all of them — Go has no async iterator.
//
// A quiet poll is not the end of the stream: two revisions published
// back-to-back can land one poll apart, so the only reason to stop early is
// holding every revision in expect.
func drain(node *mesh.MeshNode, applied map[int]string, expect []int) int {
	deadline := time.Now().Add(deliver)
	seen := 0
	for {
		for shard := uint16(0); shard < 4; shard++ {
			// A failed receive is not an empty shard. Swallowing it would make
			// "nothing was delivered" and "we never looked" the same answer.
			events, err := node.RecvShard(shard, 64)
			if err != nil {
				log.Fatalf("recv shard %d: %v", shard, err)
			}
			for _, event := range events {
				seen++
				if version, mode, ok := parse(event.Payload); ok {
					applied[version] = mode
				}
			}
		}
		complete := true
		for _, version := range expect {
			if _, ok := applied[version]; !ok {
				complete = false
			}
		}
		if complete || !time.Now().Before(deadline) {
			return seen
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func main() {
	publisher, addrPublisher := build(0xF1)
	one, _ := build(0xF2)
	two, _ := build(0xF3)

	handshake(publisher, one, addrPublisher)
	handshake(publisher, two, addrPublisher)

	for _, n := range []*mesh.MeshNode{publisher, one, two} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{publisher, one, two} {
			_ = n.Shutdown()
		}
	}()

	// The publisher owns the channel config. No broker registers it.
	if err := publisher.RegisterChannel(mesh.ChannelConfig{
		Name:       "config/edge",
		Visibility: "global",
		Reliable:   true,
	}); err != nil {
		log.Fatalf("register channel: %v", err)
	}

	// Subscribers join by name; SubscribeChannel blocks on the publisher's ack,
	// so by the time it returns this node is in the roster.
	if err := one.SubscribeChannel(publisher.NodeID(), "config/edge"); err != nil {
		log.Fatalf("subscribe one: %v", err)
	}
	if err := two.SubscribeChannel(publisher.NodeID(), "config/edge"); err != nil {
		log.Fatalf("subscribe two: %v", err)
	}

	publish := func(version int, mode string) *mesh.PublishReport {
		report, err := publisher.Publish(
			"config/edge",
			[]byte(fmt.Sprintf("v=%d;mode=%s", version, mode)),
			mesh.PublishConfig{Reliability: "reliable"},
		)
		if err != nil {
			log.Fatalf("publish v%d: %v", version, err)
		}
		return report
	}

	first := publish(1, "blue")
	fmt.Printf("published v1 to %d of %d subscribers\n", first.Delivered, first.Attempted)
	checkDelivery(1, first)

	second := publish(2, "green")
	fmt.Printf("published v2 to %d of %d subscribers\n", second.Delivered, second.Attempted)
	checkDelivery(2, second)

	// Both revisions are expected on both subscribers, so drain until each has
	// them rather than until a poll comes back quiet.
	appliedOne := map[int]string{}
	appliedTwo := map[int]string{}
	drain(one, appliedOne, []int{1, 2})
	drain(two, appliedTwo, []int{1, 2})

	fmt.Printf("subscriber one applied:    %v\n", appliedOne)
	fmt.Printf("subscriber two applied:    %v\n", appliedTwo)

	checkApplied("one", appliedOne)
	checkApplied("two", appliedTwo)

	applied := 0
	for _, appliedMap := range []map[int]string{appliedOne, appliedTwo} {
		if _, ok := appliedMap[2]; ok {
			applied++
		}
	}
	fmt.Printf("roster at publish time:    %d\n", second.Attempted)

	fmt.Printf("RESULT ok subscribers=%d applied=%d version=2\n", second.Attempted, applied)
}
