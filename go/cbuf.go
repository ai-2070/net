package net

// Checked copies out of C-owned buffers.
//
// `C.GoBytes(unsafe.Pointer, C.int)` takes its length as a C int. A
// `size_t` length above 2^31-1 converted with `C.int(n)` wraps negative on
// every 64-bit target, and checking against `math.MaxInt` first does not
// help: Go's int is wider than C's there. These helpers keep the length in
// Go's int the whole way, so the only size limit is the one `checkedLen`
// states.
//
// Plain Go on purpose (no `import "C"`): call sites convert their
// `C.size_t` with `uint64(n)`, and the boundary tests can exercise these
// directly — `_test.go` files cannot use cgo.

import (
	"errors"
	"fmt"
	"math"
	"unsafe"
)

// errCBuf is the root of every checked-copy failure. Call sites wrap it in
// their own surface's sentinel (ErrBlob, ErrTransfer, …).
var errCBuf = errors.New("c buffer")

// checkedLen converts a C length to a Go int, or fails if it does not fit.
// Pure, so the boundary is testable without allocating anything.
func checkedLen(n uint64) (int, error) {
	if n > uint64(math.MaxInt) {
		return 0, fmt.Errorf("%w: length %d exceeds the maximum Go slice length", errCBuf, n)
	}
	return int(n), nil
}

// copyCBuf copies n bytes out of a C buffer into Go memory. It never frees
// p — the caller still frees it with the allocator that produced it.
//
// (nil, 0) and (p, 0) yield an empty, non-nil slice. (nil, n>0) is an error:
// the length claims bytes the pointer cannot hold.
func copyCBuf(p unsafe.Pointer, n uint64) ([]byte, error) {
	l, err := checkedLen(n)
	if err != nil {
		return nil, err
	}
	if l == 0 {
		return []byte{}, nil
	}
	if p == nil {
		return nil, fmt.Errorf("%w: null pointer with length %d", errCBuf, l)
	}
	out := make([]byte, l)
	copy(out, unsafe.Slice((*byte)(p), l))
	return out, nil
}
