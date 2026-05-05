// Resilience helpers for nRPC calls — Phase B6 of
// `NRPC_BINDINGS_PLAN.md`. Pure-Go wrappers around `MeshRpc.Call` /
// `MeshRpc.CallService` that add retry, hedge, and circuit-breaker
// behavior. Same semantics as the Python wrappers in
// `bindings/python/python/net/mesh_rpc.py` — and the Rust SDK's
// `Mesh::call_with_retry` / `Mesh::call_with_hedge`.
//
// All helpers are thread-safe; concurrent calls through the same
// `*CircuitBreaker` correctly share state.

package net

import (
	"context"
	"errors"
	"fmt"
	"math"
	"math/rand/v2"
	"sync"
	"time"
)

// =====================================================================
// RetryPolicy
// =====================================================================

// RetryPolicy controls how `CallWithRetry` re-attempts on
// retriable failures. The zero value is invalid (no attempts);
// build via `DefaultRetryPolicy()` and tweak.
type RetryPolicy struct {
	// MaxAttempts is the upper bound on call attempts (the initial
	// call counts as 1). MUST be >= 1.
	MaxAttempts int
	// InitialBackoff is the delay before the first retry.
	InitialBackoff time.Duration
	// MaxBackoff caps any single backoff after exponential growth.
	MaxBackoff time.Duration
	// BackoffMultiplier is applied to the previous backoff to
	// produce the next (e.g. 2.0 doubles).
	BackoffMultiplier float64
	// JitterFraction multiplies the computed backoff by a random
	// value in `[1 - JitterFraction, 1 + JitterFraction]` to break
	// up thundering herds. 0.0 disables jitter.
	JitterFraction float64
	// IsRetriable decides whether `err` warrants a retry. Default
	// (`nil`) treats `nrpc:no_route` and `nrpc:transport` as
	// retriable, everything else as terminal.
	IsRetriable func(err error) bool
}

// DefaultRetryPolicy returns a sensible-default policy: 3
// attempts, 50ms initial, 2.0 multiplier, 1s cap, 20% jitter.
func DefaultRetryPolicy() RetryPolicy {
	return RetryPolicy{
		MaxAttempts:       3,
		InitialBackoff:    50 * time.Millisecond,
		MaxBackoff:        time.Second,
		BackoffMultiplier: 2.0,
		JitterFraction:    0.2,
	}
}

// DefaultIsRetriable returns true for `*RpcError` instances whose
// kind is `RpcKindNoRoute` or `RpcKindTransport`. Used when
// `RetryPolicy.IsRetriable` is nil.
func DefaultIsRetriable(err error) bool {
	var re *RpcError
	if errors.As(err, &re) {
		return re.Kind == RpcKindNoRoute || re.Kind == RpcKindTransport
	}
	return false
}

// CallFn is the unary call signature retry / hedge wrappers
// operate on. Bind a closure around your `*MeshRpc` and call site:
//
//	call := func(ctx context.Context) ([]byte, error) {
//	    return rpc.Call(ctx, target, "echo", req)
//	}
//	resp, err := CallWithRetry(ctx, call, DefaultRetryPolicy())
type CallFn func(ctx context.Context) ([]byte, error)

// CallWithRetry invokes `call` up to `policy.MaxAttempts` times,
// sleeping with exponential backoff (clamped + jittered) between
// attempts. Stops early on a non-retriable error or context
// cancellation.
func CallWithRetry(ctx context.Context, call CallFn, policy RetryPolicy) ([]byte, error) {
	if policy.MaxAttempts < 1 {
		return nil, fmt.Errorf("RetryPolicy.MaxAttempts must be >= 1, got %d", policy.MaxAttempts)
	}
	isRetriable := policy.IsRetriable
	if isRetriable == nil {
		isRetriable = DefaultIsRetriable
	}
	var lastErr error
	backoff := policy.InitialBackoff
	for attempt := 1; attempt <= policy.MaxAttempts; attempt++ {
		resp, err := call(ctx)
		if err == nil {
			return resp, nil
		}
		lastErr = err
		if attempt == policy.MaxAttempts || !isRetriable(err) {
			return nil, lastErr
		}
		sleep := jitter(backoff, policy.JitterFraction)
		select {
		case <-time.After(sleep):
		case <-ctx.Done():
			return nil, ctx.Err()
		}
		backoff = nextBackoff(backoff, policy.BackoffMultiplier, policy.MaxBackoff)
	}
	return nil, lastErr
}

// jitter applies a `[1 - frac, 1 + frac]` multiplier to `d`. Clamps
// to a non-negative duration. Uses `math/rand/v2` (goroutine-safe,
// not deprecated) — jitter is not security-relevant.
func jitter(d time.Duration, frac float64) time.Duration {
	if frac <= 0 {
		return d
	}
	span := 2 * frac
	mult := 1 - frac + rand.Float64()*span
	if mult < 0 {
		mult = 0
	}
	return time.Duration(float64(d) * mult)
}

// nextBackoff returns `min(prev * mult, maxCap)`, clamping in case
// of overflow. `maxCap == 0` disables the cap.
//
// Parameter named `maxCap` rather than `cap` to avoid shadowing the
// `cap` builtin — readers expecting the builtin would misread.
func nextBackoff(prev time.Duration, mult float64, maxCap time.Duration) time.Duration {
	if mult <= 0 {
		return prev
	}
	next := time.Duration(float64(prev) * mult)
	if next < prev {
		// overflow protection
		next = math.MaxInt64
	}
	if maxCap > 0 && next > maxCap {
		return maxCap
	}
	return next
}

// =====================================================================
// HedgePolicy
// =====================================================================

// HedgePolicy controls how `CallWithHedge` races parallel
// attempts. Hedging amortizes p99 latency by firing a backup
// request after a delay; the first response wins, the rest are
// canceled.
type HedgePolicy struct {
	// MaxParallel is the total number of in-flight attempts the
	// hedge will eventually fan out to (initial + N hedges). MUST
	// be >= 1. A value of 1 disables hedging.
	MaxParallel int
	// HedgeDelay is the wait between starting one attempt and
	// firing the next. Subsequent hedges fire at the same cadence.
	HedgeDelay time.Duration
	// CancelLosers, if true, cancels the per-attempt context of
	// any in-flight attempts as soon as one returns successfully.
	// Defaults to true; set false to let losers run to completion
	// (useful when the server-side handler is idempotent + cheap).
	CancelLosers bool
}

// DefaultHedgePolicy returns a sensible-default policy: 2
// parallel, 50ms hedge delay, cancel losers.
func DefaultHedgePolicy() HedgePolicy {
	return HedgePolicy{
		MaxParallel:  2,
		HedgeDelay:   50 * time.Millisecond,
		CancelLosers: true,
	}
}

// CallWithHedge fans out hedge requests on a delay until one
// succeeds, all attempts fail, or `ctx` cancels. Returns the
// first successful response. If every attempt errors, returns the
// last attempt's error (matches the Python wrapper's contract).
func CallWithHedge(ctx context.Context, call CallFn, policy HedgePolicy) ([]byte, error) {
	if policy.MaxParallel < 1 {
		return nil, fmt.Errorf("HedgePolicy.MaxParallel must be >= 1, got %d", policy.MaxParallel)
	}
	if policy.HedgeDelay <= 0 && policy.MaxParallel > 1 {
		// `HedgeDelay == 0` collapses the wait between hedges into
		// a `time.Timer(0)` that fires synchronously, busy-firing
		// the next hedge until `MaxParallel` is hit. Reject
		// upfront — the user wanted "fire all in parallel" or
		// they wanted a cadence; either way `0` is a misuse, and
		// the Python wrapper validates the same way.
		return nil, fmt.Errorf(
			"HedgePolicy.HedgeDelay must be > 0 when MaxParallel > 1, got %s",
			policy.HedgeDelay,
		)
	}
	type result struct {
		resp []byte
		err  error
	}
	rootCtx, cancelRoot := context.WithCancel(ctx)
	defer cancelRoot()
	results := make(chan result, policy.MaxParallel)

	var wg sync.WaitGroup
	fire := func() {
		wg.Add(1)
		attemptCtx, cancelAttempt := context.WithCancel(rootCtx)
		go func() {
			defer wg.Done()
			defer cancelAttempt()
			resp, err := call(attemptCtx)
			select {
			case results <- result{resp: resp, err: err}:
			case <-rootCtx.Done():
			}
		}()
	}
	// Initial attempt.
	fire()
	inFlight := 1

	var lastErr error
	completed := 0
	hedgeTimer := time.NewTimer(policy.HedgeDelay)
	defer hedgeTimer.Stop()

	for {
		var hedgeCh <-chan time.Time
		if inFlight < policy.MaxParallel {
			hedgeCh = hedgeTimer.C
		}
		select {
		case r := <-results:
			completed++
			if r.err == nil {
				if policy.CancelLosers {
					cancelRoot()
				}
				// Drain remaining attempts in the background so
				// goroutines exit cleanly.
				go func() { wg.Wait() }()
				return r.resp, nil
			}
			lastErr = r.err
			if completed == inFlight && inFlight >= policy.MaxParallel {
				return nil, lastErr
			}
		case <-hedgeCh:
			fire()
			inFlight++
			if inFlight < policy.MaxParallel {
				hedgeTimer.Reset(policy.HedgeDelay)
			}
		case <-ctx.Done():
			cancelRoot()
			go func() { wg.Wait() }()
			return nil, ctx.Err()
		}
	}
}

// =====================================================================
// CircuitBreaker
// =====================================================================

// BreakerState is the breaker's current operating state.
type BreakerState int

const (
	BreakerClosed BreakerState = iota
	BreakerOpen
	BreakerHalfOpen
)

func (s BreakerState) String() string {
	switch s {
	case BreakerClosed:
		return "closed"
	case BreakerOpen:
		return "open"
	case BreakerHalfOpen:
		return "half-open"
	default:
		return fmt.Sprintf("BreakerState(%d)", int(s))
	}
}

// ErrBreakerOpen is returned by `CircuitBreaker.Call` when the
// breaker is open and refuses to admit a call.
var ErrBreakerOpen = errors.New("nrpc:breaker_open: circuit breaker rejected call")

// CircuitBreaker tracks consecutive failures and trips open after
// a threshold. Open breakers reject calls outright until a
// reset-after timeout elapses; the next admit attempts a
// half-open probe whose result re-decides the state.
//
// Matches the Python wrapper's semantics in
// `bindings/python/python/net/mesh_rpc.py::CircuitBreaker`.
type CircuitBreaker struct {
	// FailureThreshold is the consecutive-failure count that trips
	// the breaker open. MUST be >= 1.
	FailureThreshold int
	// ResetAfter is how long to wait between trip-open and
	// allowing a half-open probe.
	ResetAfter time.Duration
	// IsFailure decides whether `err` counts as a failure for
	// breaker purposes. Defaults to "any non-nil error".
	IsFailure func(err error) bool

	mu        sync.Mutex
	state     BreakerState
	failures  int
	openedAt  time.Time
}

// NewCircuitBreaker constructs a breaker. `failureThreshold` MUST
// be >= 1. Pass nil `isFailure` for the default (any error
// counts).
func NewCircuitBreaker(
	failureThreshold int,
	resetAfter time.Duration,
	isFailure func(err error) bool,
) *CircuitBreaker {
	if failureThreshold < 1 {
		failureThreshold = 1
	}
	return &CircuitBreaker{
		FailureThreshold: failureThreshold,
		ResetAfter:       resetAfter,
		IsFailure:        isFailure,
	}
}

// State returns the breaker's current state. Note: the underlying
// state is mutated lazily on `Call` — observers may see a stale
// `Open` value until the next `Call` triggers the half-open
// transition.
func (b *CircuitBreaker) State() BreakerState {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.state
}

// Call admits the call iff the breaker isn't open (or has aged
// past `ResetAfter` for a half-open probe). On success, resets
// the failure count + closes the breaker. On failure, increments
// the count + may trip open.
func (b *CircuitBreaker) Call(ctx context.Context, call CallFn) ([]byte, error) {
	if !b.tryAdmit() {
		return nil, ErrBreakerOpen
	}
	resp, err := call(ctx)
	b.recordResult(err)
	return resp, err
}

// tryAdmit decides whether to let a call through. Mutates state
// for the open->half-open transition.
func (b *CircuitBreaker) tryAdmit() bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	switch b.state {
	case BreakerClosed:
		return true
	case BreakerHalfOpen:
		// Already probing — let the in-flight probe run; reject
		// concurrent attempts so the probe result is a single
		// signal.
		return false
	case BreakerOpen:
		if time.Since(b.openedAt) >= b.ResetAfter {
			b.state = BreakerHalfOpen
			return true
		}
		return false
	}
	return true
}

// recordResult updates breaker state based on the call outcome.
func (b *CircuitBreaker) recordResult(err error) {
	failed := false
	if err != nil {
		if b.IsFailure != nil {
			failed = b.IsFailure(err)
		} else {
			failed = true
		}
	}
	b.mu.Lock()
	defer b.mu.Unlock()
	if failed {
		b.failures++
		if b.state == BreakerHalfOpen || b.failures >= b.FailureThreshold {
			b.state = BreakerOpen
			b.openedAt = time.Now()
		}
		return
	}
	// Success — close the breaker and reset the count.
	b.state = BreakerClosed
	b.failures = 0
}
