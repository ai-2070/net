// Command org-matrix-peer is the Go driver of the cross-process,
// cross-language org-call matrix (run_matrix.py). It plays either role of one
// cross-org Granted call — `provider` or `caller` — for either shape — `unary`
// or `server-streaming` — as its own OS process over a real mesh. The contract
// (command line, stdout protocol lines, payloads, ordering, timeouts) is
// net/crates/net/tests/cross_lang_org/matrix/PROTOCOL.md; this file implements
// it and nothing else. stdout carries ONLY protocol lines; diagnostics go to
// stderr.
//
// Build (from go/, once the net cdylib exists in
// net/crates/net/target/release — see go/README.md):
//
//	go build ./cmd/org-matrix-peer
//
// Run (the orchestrator does this; by hand for debugging):
//
//	org-matrix-peer caller   --manifest <dir> --shape <unary|server-streaming>
//	org-matrix-peer provider --manifest <dir> --shape <unary|server-streaming> --caller-node-id <u64>
//
// The libnet shared library must be loadable at run time (net.dll on PATH on
// Windows, LD_LIBRARY_PATH / DYLD_LIBRARY_PATH elsewhere).
package main

import (
	"bufio"
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	stdnet "net"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"sync"
	"time"

	net "github.com/ai-2070/net/go"
)

const (
	shapeUnary     = "unary"
	shapeStreaming = "server-streaming"

	heartbeatMs = 200
	// Match the Python NetMesh default (4): a shard-count mismatch strands
	// capability announcements on unpooled shards (see
	// go/org_streaming_opening_vectors_test.go, the mixed pair).
	numShards = 4

	discoveryRetryEvery = 500 * time.Millisecond
	discoveryRetryFor   = 60 * time.Second
	callTimeout         = 60 * time.Second // per attempt; also the stream's hard deadline
	acceptTimeout       = 30 * time.Second
	handlerWatchdog     = 120 * time.Second
	drainedTimeout      = 120 * time.Second
	providerLineTimeout = 120 * time.Second
	reannounceEvery     = time.Second
)

// The payloads (PROTOCOL.md "Payloads"): compact UTF-8 JSON, compared after
// parsing, never byte-for-byte.
var (
	requestBody    = []byte(`{"n":7}`)
	unaryResponse  = `{"n":8}`
	streamingItems = []string{`{"i":0}`, `{"i":1}`, `{"i":2}`}
)

// ---- manifest (sdk/src/org/fixtures.rs CrossOrgScenarioManifest subset) ----

type providerRole struct {
	SeedHex         string `json:"seed_hex"`
	OrgIDHex        string `json:"org_id_hex"`
	AuthorityDir    string `json:"authority_dir"`
	GrantPath       string `json:"grant_path"`
	GrantSecretPath string `json:"grant_secret_path"`
}

type callerRole struct {
	SeedHex         string `json:"seed_hex"`
	OrgIDHex        string `json:"org_id_hex"`
	AuthorityDir    string `json:"authority_dir"`
	MembershipPath  string `json:"membership_path"`
	DispatcherPath  string `json:"dispatcher_path"`
	GrantPath       string `json:"grant_path"`
	GrantSecretPath string `json:"grant_secret_path"`
}

type manifest struct {
	PskHex         string       `json:"psk_hex"`
	GrantedService string       `json:"granted_service"`
	Provider       providerRole `json:"provider"`
	Caller         callerRole   `json:"caller"`

	dir string
}

// path resolves a manifest-relative path.
func (m *manifest) path(rel string) string { return filepath.Join(m.dir, rel) }

func loadManifest(dir string) (*manifest, error) {
	raw, err := os.ReadFile(filepath.Join(dir, "manifest.json"))
	if err != nil {
		return nil, fmt.Errorf("read manifest: %w", err)
	}
	var m manifest
	if err := json.Unmarshal(raw, &m); err != nil {
		return nil, fmt.Errorf("parse manifest: %w", err)
	}
	m.dir = dir
	return &m, nil
}

// ---- small helpers ----

// protocol prints one protocol line on stdout (os.Stdout is unbuffered, so
// each line is flushed as it is written).
func protocol(format string, args ...any) {
	fmt.Fprintf(os.Stdout, format+"\n", args...)
}

func diag(format string, args ...any) {
	fmt.Fprintf(os.Stderr, "org-matrix-peer: "+format+"\n", args...)
}

// oneLine flattens a detail so a RESULT/FACTS line stays a single line.
func oneLine(s string) string {
	return strings.Join(strings.Fields(s), " ")
}

// resultFail prints `RESULT fail <detail>` and returns the exit code.
func resultFail(code int, format string, args ...any) int {
	detail := oneLine(fmt.Sprintf(format, args...))
	protocol("RESULT fail %s", detail)
	diag("RESULT fail %s", detail)
	return code
}

// reserveLoopbackAddr picks a free 127.0.0.1 UDP port. The Go binding exposes
// no LocalAddr(), so the node binds to a reserved address that this process
// remembers (the go/ test-suite idiom, reserveLocalUDPPort) — the equivalent
// of PROTOCOL.md's "bound to 127.0.0.1:0".
func reserveLoopbackAddr() (string, error) {
	conn, err := stdnet.ListenPacket("udp", "127.0.0.1:0")
	if err != nil {
		return "", err
	}
	addr := conn.LocalAddr().String()
	if err := conn.Close(); err != nil {
		return "", err
	}
	return addr, nil
}

// newNode builds a mesh node from a role's seed and returns it with the
// address it is bound to.
func newNode(m *manifest, seedHex string) (*net.MeshNode, string, error) {
	addr, err := reserveLoopbackAddr()
	if err != nil {
		return nil, "", fmt.Errorf("reserve bind addr: %w", err)
	}
	node, err := net.NewMeshNode(net.MeshConfig{
		BindAddr:        addr,
		PskHex:          m.PskHex,
		IdentitySeedHex: seedHex,
		HeartbeatMs:     heartbeatMs,
		NumShards:       numShards,
	})
	if err != nil {
		return nil, "", err
	}
	diag("node %d bound to %s", node.NodeID(), addr)
	return node, addr, nil
}

// stdinLines delivers stdin one trimmed line at a time; the channel closes on
// EOF or a read error.
func stdinLines() <-chan string {
	ch := make(chan string, 4)
	go func() {
		defer close(ch)
		r := bufio.NewReader(os.Stdin)
		for {
			line, err := r.ReadString('\n')
			if s := strings.TrimSpace(line); s != "" {
				ch <- s
			}
			if err != nil {
				if !errors.Is(err, io.EOF) {
					diag("stdin: %v", err)
				}
				return
			}
		}
	}()
	return ch
}

// readLine waits for the next stdin line, or reports EOF / timeout.
func readLine(lines <-chan string, timeout time.Duration) (string, error) {
	select {
	case line, ok := <-lines:
		if !ok {
			return "", errors.New("stdin closed")
		}
		return line, nil
	case <-time.After(timeout):
		return "", fmt.Errorf("no stdin line within %v", timeout)
	}
}

// jsonEqual compares two JSON documents after parsing them.
func jsonEqual(got []byte, want string) bool {
	var g, w any
	if err := json.Unmarshal(got, &g); err != nil {
		return false
	}
	if err := json.Unmarshal([]byte(want), &w); err != nil {
		return false
	}
	return reflect.DeepEqual(g, w)
}

// ---- command line ----

type options struct {
	role         string
	manifestDir  string
	shape        string
	callerNodeID uint64
}

func parseArgs(args []string) (options, error) {
	var o options
	if len(args) == 0 {
		return o, errors.New("missing role (provider|caller)")
	}
	o.role = args[0]
	fs := flag.NewFlagSet("org-matrix-peer "+o.role, flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	fs.StringVar(&o.manifestDir, "manifest", "", "the gen_org_scenario output directory")
	fs.StringVar(&o.shape, "shape", "", "unary | server-streaming")
	var callerID string
	switch o.role {
	case "provider":
		fs.StringVar(&callerID, "caller-node-id", "", "the caller's u64 node id")
	case "caller":
	default:
		return o, fmt.Errorf("unknown role %q (want provider|caller)", o.role)
	}
	if err := fs.Parse(args[1:]); err != nil {
		return o, err
	}
	if fs.NArg() != 0 {
		return o, fmt.Errorf("unexpected arguments: %v", fs.Args())
	}
	if o.manifestDir == "" {
		return o, errors.New("--manifest is required")
	}
	if o.shape != shapeUnary && o.shape != shapeStreaming {
		return o, fmt.Errorf("--shape must be %s or %s, got %q", shapeUnary, shapeStreaming, o.shape)
	}
	if o.role == "provider" {
		if callerID == "" {
			return o, errors.New("--caller-node-id is required for provider")
		}
		id, err := strconv.ParseUint(callerID, 10, 64)
		if err != nil {
			return o, fmt.Errorf("--caller-node-id: %w", err)
		}
		o.callerNodeID = id
	}
	return o, nil
}

func main() {
	os.Exit(run(os.Args[1:]))
}

func run(args []string) int {
	o, err := parseArgs(args)
	if err != nil {
		return resultFail(1, "usage: %v", err)
	}
	m, err := loadManifest(o.manifestDir)
	if err != nil {
		return resultFail(1, "%v", err)
	}
	if o.role == "provider" {
		return runProvider(o, m)
	}
	return runCaller(o, m)
}

// =========================================================================
// Caller (PROTOCOL.md "Caller").
// =========================================================================

func runCaller(o options, m *manifest) int {
	// 1. Build the node; do NOT start it.
	node, _, err := newNode(m, m.Caller.SeedHex)
	if err != nil {
		return resultFail(1, "caller node: %v", err)
	}
	defer func() { _ = node.Shutdown() }()

	// 2. ID line.
	protocol("ID %d", node.NodeID())

	// 3. PROVIDER <addr> <public_key_hex> <node_id>.
	line, err := readLine(stdinLines(), providerLineTimeout)
	if err != nil {
		return resultFail(1, "waiting for PROVIDER line: %v", err)
	}
	fields := strings.Fields(line)
	if len(fields) != 4 || fields[0] != "PROVIDER" {
		return resultFail(1, "bad PROVIDER line: %q", line)
	}
	providerAddr, providerPub := fields[1], fields[2]
	providerID, err := strconv.ParseUint(fields[3], 10, 64)
	if err != nil {
		return resultFail(1, "bad provider node id %q: %v", fields[3], err)
	}

	// 4. connect (never accept), start, install the authority.
	if err := node.Connect(providerAddr, providerPub, providerID); err != nil {
		return resultFail(1, "connect: %v", err)
	}
	if err := node.Start(); err != nil {
		return resultFail(1, "start: %v", err)
	}
	if err := net.InstallOrgAuthority(node, m.path(m.Caller.AuthorityDir)); err != nil {
		return resultFail(1, "install caller authority: %v", err)
	}

	// 5. Bind credentials.
	client, err := bindCallerClient(node, m)
	if err != nil {
		return resultFail(1, "%v", err)
	}
	defer client.Close() // runs before the node's Shutdown (LIFO)

	// 6–7. Call and check.
	var detail string
	switch o.shape {
	case shapeUnary:
		detail = callUnary(node, client, m.GrantedService)
	default:
		detail = callStreaming(node, client, m.GrantedService)
	}

	// 8. RESULT, then shut down (deferred: client.Close, then node.Shutdown).
	if detail != "" {
		return resultFail(1, "%s", detail)
	}
	protocol("RESULT ok")
	return 0
}

func bindCallerClient(node *net.MeshNode, m *manifest) (*net.OrgClient, error) {
	membership, err := os.ReadFile(m.path(m.Caller.MembershipPath))
	if err != nil {
		return nil, fmt.Errorf("read membership: %w", err)
	}
	dispatcher, err := os.ReadFile(m.path(m.Caller.DispatcherPath))
	if err != nil {
		return nil, fmt.Errorf("read dispatcher: %w", err)
	}
	cfg := net.OrgCredentialsConfig{Membership: membership, Dispatcher: dispatcher}
	if m.Caller.GrantPath != "" {
		grant, err := os.ReadFile(m.path(m.Caller.GrantPath))
		if err != nil {
			return nil, fmt.Errorf("read caller grant: %w", err)
		}
		cfg.Grants = [][]byte{grant}
	}
	if m.Caller.GrantSecretPath != "" {
		// By PATH: the raw audience key never enters this process.
		cfg.AudienceSecretPaths = []string{m.path(m.Caller.GrantSecretPath)}
	}
	creds, err := net.NewOrgCredentials(cfg)
	if err != nil {
		return nil, fmt.Errorf("caller credentials: %w", err)
	}
	client, err := net.NewOrgClient(node, creds)
	if err != nil {
		return nil, fmt.Errorf("bind org client: %w", err)
	}
	return client, nil
}

// retryDiscovery runs attempt until it stops failing with the LOCAL
// discovery-domain refusal (nothing was sent, no proof minted), every 500 ms
// for up to 60 s. Any other outcome — success, an admission denial, a
// transport error — is returned at once: it is a result, never retried.
func retryDiscovery(node *net.MeshNode, attempt func() error) error {
	deadline := time.Now().Add(discoveryRetryFor)
	tries := 0
	for {
		// Force scoped-catalog emission on this side each pass (the mixed
		// pair's `_Pair.announce` discipline); harmless for a pure caller.
		if err := node.AnnounceCapabilities(net.CapabilitySet{}); err != nil {
			diag("caller announce: %v", err)
		}
		tries++
		err := attempt()
		if err == nil || !errors.Is(err, net.ErrOrgDiscovery) {
			if err == nil {
				diag("call admitted after %d attempt(s)", tries)
			}
			return err
		}
		if time.Now().After(deadline) {
			return fmt.Errorf("discovery did not converge within %v after %d attempts (last: %w)", discoveryRetryFor, tries, err)
		}
		time.Sleep(discoveryRetryEvery)
	}
}

// callUnary returns "" on success, else a failure detail.
func callUnary(node *net.MeshNode, client *net.OrgClient, service string) string {
	var resp []byte
	err := retryDiscovery(node, func() error {
		ctx, cancel := context.WithTimeout(context.Background(), callTimeout)
		defer cancel()
		var err error
		resp, err = client.CallBytes(ctx, service, requestBody)
		return err
	})
	if err != nil {
		return describeCallError("call", err)
	}
	if !jsonEqual(resp, unaryResponse) {
		return fmt.Sprintf("unary response %q, want %s", resp, unaryResponse)
	}
	return ""
}

// callStreaming returns "" on success, else a failure detail.
func callStreaming(node *net.MeshNode, client *net.OrgClient, service string) string {
	var stream *net.RpcStream
	var streamCancel context.CancelFunc
	err := retryDiscovery(node, func() error {
		// The ctx stays armed for the stream's whole life (its deadline is
		// the stream's hard deadline, and cancelling it tears the stream
		// down), so it is cancelled only after the drain — or on a failed
		// opening.
		ctx, cancel := context.WithTimeout(context.Background(), callTimeout)
		s, err := client.CallStreaming(ctx, service, requestBody)
		if err != nil {
			cancel()
			return err
		}
		stream, streamCancel = s, cancel
		return nil
	})
	if err != nil {
		return describeCallError("open stream", err)
	}
	defer streamCancel()
	defer stream.Close() // before the ctx cancel (LIFO)

	var items [][]byte
	for {
		chunk, err := stream.Recv()
		if errors.Is(err, net.ErrStreamDone) {
			break
		}
		if err != nil {
			return describeCallError(fmt.Sprintf("recv after %d item(s)", len(items)), err)
		}
		items = append(items, chunk)
		if len(items) > len(streamingItems) {
			return fmt.Sprintf("more than %d items (got %q)", len(streamingItems), chunk)
		}
	}
	if len(items) != len(streamingItems) {
		return fmt.Sprintf("got %d item(s) before the clean end, want %d", len(items), len(streamingItems))
	}
	for i, want := range streamingItems {
		if !jsonEqual(items[i], want) {
			return fmt.Sprintf("item %d = %q, want %s", i, items[i], want)
		}
	}
	return ""
}

func describeCallError(what string, err error) string {
	var oe *net.OrgError
	if errors.As(err, &oe) {
		return fmt.Sprintf("%s: domain=%s kind=%s: %v", what, oe.Domain, oe.Kind, err)
	}
	return fmt.Sprintf("%s: %v", what, err)
}

// =========================================================================
// Provider (PROTOCOL.md "Provider").
// =========================================================================

// handlerState records what the handler observed. fired closes once, when
// the first handler invocation has finished.
type handlerState struct {
	mu      sync.Mutex
	calls   int
	facts   net.OrgCaller
	reqErr  string // non-empty when a request failed the n == 7 check
	sinkErr string
	once    sync.Once
	fired   chan struct{}
}

func (h *handlerState) enter(caller net.OrgCaller, req []byte) error {
	h.mu.Lock()
	defer h.mu.Unlock()
	h.calls++
	if h.calls == 1 {
		h.facts = caller
	}
	if err := checkRequest(req); err != nil {
		if h.reqErr == "" {
			h.reqErr = err.Error()
		}
		return err
	}
	return nil
}

func (h *handlerState) done() { h.once.Do(func() { close(h.fired) }) }

func checkRequest(req []byte) error {
	var body map[string]any
	if err := json.Unmarshal(req, &body); err != nil {
		return fmt.Errorf("request is not JSON (%q): %v", req, err)
	}
	n, ok := body["n"].(float64)
	if !ok || n != 7 {
		return fmt.Errorf("request %q: want n == 7", req)
	}
	return nil
}

func (h *handlerState) recordSinkErr(err error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.sinkErr == "" {
		h.sinkErr = err.Error()
	}
}

// snapshot returns the recorded state under the lock.
func (h *handlerState) snapshot() (calls int, facts net.OrgCaller, reqErr, sinkErr string) {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.calls, h.facts, h.reqErr, h.sinkErr
}

// checkFacts verifies the provider-verified caller projection the handler was
// given: the acting org is the caller's, the provider org is ours, and the
// call is cross-org. Returns "" when the facts hold.
func checkFacts(m *manifest, facts net.OrgCaller) string {
	var bad []string
	if got, want := hex.EncodeToString(facts.ActingOrg[:]), strings.ToLower(m.Caller.OrgIDHex); got != want {
		bad = append(bad, fmt.Sprintf("acting_org=%s want %s", got, want))
	}
	if got, want := hex.EncodeToString(facts.ProviderOrg[:]), strings.ToLower(m.Provider.OrgIDHex); got != want {
		bad = append(bad, fmt.Sprintf("provider_org=%s want %s", got, want))
	}
	if facts.IsSameOrg() {
		bad = append(bad, "is_same_org=true want false")
	}
	return strings.Join(bad, "; ")
}

func runProvider(o options, m *manifest) int {
	// 1. Build the node.
	//
	// Shutdown is never deferred here: it takes the node's write lock, and an
	// Accept still in flight holds the read lock, so a Shutdown on the
	// accept-timeout path would hang the exit. Every other path shuts down
	// explicitly once the accept is known to have returned.
	node, addr, err := newNode(m, m.Provider.SeedHex)
	if err != nil {
		return resultFail(1, "provider node: %v", err)
	}
	failAfterShutdown := func(format string, args ...any) int {
		_ = node.Shutdown()
		return resultFail(1, format, args...)
	}

	// 2. Install the authority.
	if err := net.InstallOrgAuthority(node, m.path(m.Provider.AuthorityDir)); err != nil {
		return failAfterShutdown("install provider authority: %v", err)
	}
	pub, err := node.PublicKey()
	if err != nil {
		return failAfterShutdown("public key: %v", err)
	}
	pub = strings.ToLower(pub)
	// Read the provider grant now so a missing file fails before READY.
	providerGrant, err := os.ReadFile(m.path(m.Provider.GrantPath))
	if err != nil {
		return failAfterShutdown("read provider grant: %v", err)
	}
	lines := stdinLines() // DRAINED arrives here later

	// 3. accept(<caller-node-id>) in the background.
	accepted := make(chan error, 1)
	go func() {
		_, err := node.Accept(o.callerNodeID)
		accepted <- err
	}()

	// 4. READY <addr> <public_key_hex> <node_id>.
	protocol("READY %s %s %d", addr, pub, node.NodeID())

	// 5. Wait for the accept, then start (Start is refused while an Accept
	// is in flight, so it strictly follows).
	select {
	case err := <-accepted:
		if err != nil {
			return failAfterShutdown("accept: %v", err)
		}
	case <-time.After(acceptTimeout):
		return resultFail(1, "accept did not complete within %v", acceptTimeout)
	}
	diag("handshake with caller %d done", o.callerNodeID)
	if err := node.Start(); err != nil {
		return failAfterShutdown("start: %v", err)
	}

	// 6. Serve as Granted, THEN install the provider grant audience. The
	// handle stays bound until step 9: its Close deregisters the service and
	// retires live protected streams.
	state := &handlerState{fired: make(chan struct{})}
	var handle *net.OrgServeHandle
	switch o.shape {
	case shapeUnary:
		handle, err = net.ServeOrgBytes(node, m.GrantedService, net.OrgAccessGranted,
			func(caller net.OrgCaller, req []byte) ([]byte, error) {
				defer state.done()
				if err := state.enter(caller, req); err != nil {
					return nil, err
				}
				return []byte(unaryResponse), nil
			})
	default:
		handle, err = net.ServeOrgStreamingBytes(node, m.GrantedService, net.OrgAccessGranted,
			func(caller net.OrgCaller, req []byte, sink *net.ResponseSinkSend) error {
				defer state.done()
				if err := state.enter(caller, req); err != nil {
					return err
				}
				for _, item := range streamingItems {
					if err := sink.Send([]byte(item)); err != nil {
						state.recordSinkErr(err)
						return err
					}
				}
				return nil // the clean end
			})
	}
	if err != nil {
		return failAfterShutdown("serve %s: %v", m.GrantedService, err)
	}
	if err := net.InstallProviderGrantAudience(node, providerGrant, m.path(m.Provider.GrantSecretPath)); err != nil {
		handle.Close()
		return failAfterShutdown("install provider grant audience: %v", err)
	}
	diag("serving %s (%s), announcing", m.GrantedService, o.shape)

	// 7. Re-announce about once a second until the handler has run.
	stopAnnounce := make(chan struct{})
	var announcer sync.WaitGroup
	announcer.Add(1)
	go func() {
		defer announcer.Done()
		ticker := time.NewTicker(reannounceEvery)
		defer ticker.Stop()
		for {
			if err := node.AnnounceCapabilities(net.CapabilitySet{}); err != nil {
				diag("provider announce: %v", err)
			}
			select {
			case <-state.fired:
				return
			case <-stopAnnounce:
				return
			case <-ticker.C:
			}
		}
	}()

	select {
	case <-state.fired:
		close(stopAnnounce)
		announcer.Wait()
	case <-time.After(handlerWatchdog):
		close(stopAnnounce)
		announcer.Wait()
		handle.Close()
		_ = node.Shutdown()
		return resultFail(2, "callback-loss")
	}

	// 8. FACTS.
	_, facts, _, _ := state.snapshot()
	factsDetail := checkFacts(m, facts)
	if factsDetail == "" {
		protocol("FACTS ok")
	} else {
		protocol("FACTS fail %s", oneLine(factsDetail))
		diag("FACTS fail %s", factsDetail)
	}

	// 9. Wait for DRAINED; only then drop the serve handle and shut down.
	line, drainErr := readLine(lines, drainedTimeout)
	handle.Close()
	_ = node.Shutdown()

	// 10. RESULT.
	if drainErr != nil {
		return resultFail(1, "no DRAINED: %v", drainErr)
	}
	if line != "DRAINED" {
		return resultFail(1, "unexpected stdin line %q, want DRAINED", line)
	}
	calls, _, reqErr, sinkErr := state.snapshot()
	switch {
	case calls != 1:
		return resultFail(1, "calls=%d want exactly 1", calls)
	case factsDetail != "":
		return resultFail(1, "facts: %s", factsDetail)
	case reqErr != "":
		return resultFail(1, "bad request: %s", reqErr)
	case sinkErr != "":
		return resultFail(1, "stream send: %s", sinkErr)
	}
	protocol("RESULT ok calls=1")
	return 0
}
