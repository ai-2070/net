// The job queue you no longer run (Go).
//
// A producer, two workers, and a durable job log — three in-process mesh nodes
// over loopback UDP plus a local append-only log. Jobs are appended to the log
// (the queue), dispatched to a worker over nRPC, and a worker that fails a job
// is re-issued to its peer. Nothing is re-executed, and the log is the record
// you reconcile from.
//
// Run: go run jobqueue.go
//
// Expected final line: RESULT ok jobs=6 done=6 retried=1 duplicates=0
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

const jobs = 6

// Job 3 is refused by the first worker that sees it, so the retry is observable.
const poison = 3

type job struct {
	ID int `json:"id"`
}

type done struct {
	ID     int    `json:"id"`
	Worker string `json:"worker"`
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
	doneCh := make(chan error, 1)
	go func() {
		_, err := responder.Accept(initiator.NodeID())
		doneCh <- err
	}()
	if err := initiator.Connect(responderAddr, pub, responder.NodeID()); err != nil {
		log.Fatalf("connect: %v", err)
	}
	if err := <-doneCh; err != nil {
		log.Fatalf("accept: %v", err)
	}
}

// workerHandler echoes its own worker so a caller can prove which one ran the
// job. Only ONE worker refuses the poison job — if both did, the retry would
// fail too and nothing would be demonstrated.
func workerHandler(workerHex string, refusePoison bool) func(job) (done, error) {
	return func(j job) (done, error) {
		if refusePoison && j.ID == poison {
			// A typed application refusal, not a transport error. The caller
			// decides to retry; the substrate never does it silently.
			return done{}, fmt.Errorf("worker %s refused job %d", workerHex, j.ID)
		}
		return done{ID: j.ID, Worker: workerHex}, nil
	}
}

func main() {
	producer, addrProducer := build(0x71)
	one, addrOne := build(0x72)
	two, _ := build(0x73)

	handshake(producer, one, addrProducer)
	handshake(producer, two, addrProducer)
	handshake(one, two, addrOne)

	for _, n := range []*mesh.MeshNode{producer, one, two} {
		if err := n.Start(); err != nil {
			log.Fatalf("start: %v", err)
		}
	}
	defer func() {
		for _, n := range []*mesh.MeshNode{producer, one, two} {
			_ = n.Shutdown()
		}
	}()

	// Announce before any call: an nRPC reply channel is bound to the caller's
	// announced identity.
	if err := producer.AnnounceCapabilities(mesh.CapabilitySet{Tags: []string{"dispatcher"}}); err != nil {
		log.Fatalf("announce producer: %v", err)
	}
	if err := one.AnnounceCapabilities(mesh.CapabilitySet{Tags: []string{"worker"}}); err != nil {
		log.Fatalf("announce one: %v", err)
	}
	if err := two.AnnounceCapabilities(mesh.CapabilitySet{Tags: []string{"worker"}}); err != nil {
		log.Fatalf("announce two: %v", err)
	}
	time.Sleep(250 * time.Millisecond)

	oneID := one.NodeID()
	twoID := two.NodeID()
	oneHex := fmt.Sprintf("0x%x", oneID)
	twoHex := fmt.Sprintf("0x%x", twoID)

	// The queue: a local append-only log, one record per submitted job. An
	// empty persistent dir selects the in-memory manager.
	redex := mesh.NewRedex("")
	defer redex.Free()
	queue, err := redex.OpenFile("jobs/queue", &mesh.RedexFileConfig{})
	if err != nil {
		log.Fatalf("open queue: %v", err)
	}
	defer queue.Close()
	results, err := redex.OpenFile("jobs/results", &mesh.RedexFileConfig{})
	if err != nil {
		log.Fatalf("open results: %v", err)
	}
	defer results.Close()

	for id := 1; id <= jobs; id++ {
		seq, err := queue.Append([]byte(fmt.Sprintf("job:%d", id)))
		if err != nil {
			log.Fatalf("append job %d: %v", id, err)
		}
		fmt.Printf("queued job %d at seq %d\n", id, seq)
	}

	rawOne, err := mesh.NewMeshRpc(one)
	if err != nil {
		log.Fatalf("rpc one: %v", err)
	}
	defer rawOne.Close()
	rawTwo, err := mesh.NewMeshRpc(two)
	if err != nil {
		log.Fatalf("rpc two: %v", err)
	}
	defer rawTwo.Close()
	rawClient, err := mesh.NewMeshRpc(producer)
	if err != nil {
		log.Fatalf("rpc client: %v", err)
	}
	defer rawClient.Close()

	typedOne := mesh.NewTypedMeshRpc(rawOne)
	typedTwo := mesh.NewTypedMeshRpc(rawTwo)
	client := mesh.NewTypedMeshRpc(rawClient)

	serveOne, err := mesh.TypedServe[job, done](typedOne, "run", workerHandler(oneHex, true))
	if err != nil {
		log.Fatalf("serve one: %v", err)
	}
	defer serveOne.Close()
	serveTwo, err := mesh.TypedServe[job, done](typedTwo, "run", workerHandler(twoHex, false))
	if err != nil {
		log.Fatalf("serve two: %v", err)
	}
	defer serveTwo.Close()

	targets := []uint64{oneID, twoID}
	retried := 0
	for index := range jobs {
		current := job{ID: index + 1}
		primary := index % len(targets)
		secondary := (index + 1) % len(targets)

		call := func(target uint64) (done, error) {
			ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
			defer cancel()
			return mesh.TypedCall[job, done](ctx, client, target, "run", current)
		}

		result, err := call(targets[primary])
		if err != nil {
			retried++
			fmt.Printf("job %d refused by 0x%x; re-issuing to 0x%x\n", current.ID, targets[primary], targets[secondary])
			result, err = call(targets[secondary])
			if err != nil {
				log.Fatalf("job %d failed on both workers: %v", current.ID, err)
			}
		}
		if _, err := results.Append([]byte(fmt.Sprintf("done:%d:%s", result.ID, result.Worker))); err != nil {
			log.Fatalf("append result %d: %v", current.ID, err)
		}
	}

	// Reconcile from the logs, not from memory. A job id with one result record
	// ran exactly once.
	events, err := results.ReadRange(0, results.Len())
	if err != nil {
		log.Fatalf("read results: %v", err)
	}
	counts := map[string]int{}
	for _, event := range events {
		text := string(event.Payload)
		rest, ok := strings.CutPrefix(text, "done:")
		if !ok {
			continue
		}
		id, _, _ := strings.Cut(rest, ":")
		counts[id]++
	}

	jobsQueued := int(queue.Len())
	jobsDone := len(counts)
	duplicates := 0
	for _, count := range counts {
		if count > 1 {
			duplicates++
		}
	}

	fmt.Printf("queued:     %d\n", jobsQueued)
	fmt.Printf("completed:  %d (one result record each)\n", jobsDone)
	fmt.Printf("re-issued:  %d\n", retried)

	fmt.Printf("RESULT ok jobs=%d done=%d retried=%d duplicates=%d\n", jobsQueued, jobsDone, retried, duplicates)
}
