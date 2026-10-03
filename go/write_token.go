// Package net — CortEX read-your-writes tokens.
//
// A write returns its sequence number; the adapter knows its own origin.
// Together they name the write, and WaitForToken blocks until the
// adapter's fold has applied it, so an immediate List sees it.
//
//	seq, err := tasks.Create(id, "title", now)
//	...
//	if err := tasks.WaitForToken(tasks.Token(seq), time.Second); err != nil { ... }
//
// The existing CRUD signatures are unchanged; Token is the additive way to
// get a token from them.

package net

/*
#include "net.h"
*/
import "C"

import (
	"context"
	"errors"
	"fmt"
	"time"
)

// WriteToken names one write: the origin that made it and its sequence
// number.
type WriteToken struct {
	OriginHash uint64
	Seq        uint64
}

var (
	// ErrTokenTimeout - the write was not applied before the deadline (or,
	// with a zero timeout, not yet). It also matches ErrStreamTimeout.
	ErrTokenTimeout = fmt.Errorf("%w: write token not applied in time", ErrStreamTimeout)
	// ErrWrongOrigin - the token's origin is not this adapter's. The check
	// is on the origin, not on which adapter issued the token: another
	// adapter opened with the same origin accepts it.
	ErrWrongOrigin = errors.New("write token is from a different origin")
	// ErrWaitQueueFull - too many waiters on this adapter's channel.
	ErrWaitQueueFull = errors.New("write token wait queue is full")
	// ErrFoldStopped - the adapter's fold stopped (for example, it was
	// closed) before applying the write.
	ErrFoldStopped = errors.New("adapter fold stopped")
)

// tokenWaitSlice bounds one native wait inside WaitForTokenContext, which is
// therefore how quickly a cancellation is noticed.
const tokenWaitSlice = 50 * time.Millisecond

func tokenErrorFromCode(code C.int) error {
	switch code {
	case 0:
		return nil
	case 1:
		return ErrTokenTimeout
	case -104:
		return ErrWrongOrigin
	case -105:
		return ErrWaitQueueFull
	case -106:
		return ErrFoldStopped
	default:
		return cortexErrorFromCode(code)
	}
}

// tokenTimeoutMs converts a WaitForToken timeout. Zero or negative polls
// once (0 on the wire). A positive timeout under a millisecond rounds up to
// one, so it still waits rather than silently becoming a poll.
func tokenTimeoutMs(d time.Duration) C.uint32_t {
	if d <= 0 {
		return 0
	}
	if d < time.Millisecond {
		return 1
	}
	return durationToMillisU32(d)
}

// waitForTokenContext is the shared context loop: check ctx before every
// native wait (so a cancelled context never reports success, even for a
// token that is already applied), then wait at most one slice.
func waitForTokenContext(ctx context.Context, wait func(C.uint32_t) error) error {
	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		slice := tokenWaitSlice
		if dl, ok := ctx.Deadline(); ok {
			if left := time.Until(dl); left < slice {
				slice = left
			}
		}
		err := wait(tokenTimeoutMs(slice))
		if !errors.Is(err, ErrTokenTimeout) {
			return err
		}
	}
}

// OriginHash is the origin this adapter stamps on its writes.
func (t *TasksAdapter) OriginHash() uint64 { return t.origin }

// Token names the write that returned seq on this adapter.
func (t *TasksAdapter) Token(seq uint64) WriteToken {
	return WriteToken{OriginHash: t.origin, Seq: seq}
}

// WaitForToken blocks until the fold has applied tok, or timeout passes.
// A zero timeout checks once without waiting — unlike WaitForSeq, where
// zero waits indefinitely. For an open-ended wait use WaitForTokenContext.
func (t *TasksAdapter) WaitForToken(tok WriteToken, timeout time.Duration) error {
	return t.waitForToken(tok, tokenTimeoutMs(timeout))
}

// WaitForTokenContext waits for tok until it is applied or ctx ends.
// A context that is already done returns its error without checking tok.
func (t *TasksAdapter) WaitForTokenContext(ctx context.Context, tok WriteToken) error {
	return waitForTokenContext(ctx, func(ms C.uint32_t) error { return t.waitForToken(tok, ms) })
}

func (t *TasksAdapter) waitForToken(tok WriteToken, ms C.uint32_t) error {
	t.mu.RLock()
	defer t.mu.RUnlock()
	if t.handle == nil {
		return ErrShuttingDown
	}
	return tokenErrorFromCode(C.net_tasks_wait_for_token(
		t.handle, C.uint64_t(tok.OriginHash), C.uint64_t(tok.Seq), ms))
}

// OriginHash is the origin this adapter stamps on its writes.
func (m *MemoriesAdapter) OriginHash() uint64 { return m.origin }

// Token names the write that returned seq on this adapter.
func (m *MemoriesAdapter) Token(seq uint64) WriteToken {
	return WriteToken{OriginHash: m.origin, Seq: seq}
}

// WaitForToken blocks until the fold has applied tok, or timeout passes.
// A zero timeout checks once without waiting — unlike WaitForSeq, where
// zero waits indefinitely. For an open-ended wait use WaitForTokenContext.
func (m *MemoriesAdapter) WaitForToken(tok WriteToken, timeout time.Duration) error {
	return m.waitForToken(tok, tokenTimeoutMs(timeout))
}

// WaitForTokenContext waits for tok until it is applied or ctx ends.
// A context that is already done returns its error without checking tok.
func (m *MemoriesAdapter) WaitForTokenContext(ctx context.Context, tok WriteToken) error {
	return waitForTokenContext(ctx, func(ms C.uint32_t) error { return m.waitForToken(tok, ms) })
}

func (m *MemoriesAdapter) waitForToken(tok WriteToken, ms C.uint32_t) error {
	m.mu.RLock()
	defer m.mu.RUnlock()
	if m.handle == nil {
		return ErrShuttingDown
	}
	return tokenErrorFromCode(C.net_memories_wait_for_token(
		m.handle, C.uint64_t(tok.OriginHash), C.uint64_t(tok.Seq), ms))
}
