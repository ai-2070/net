//go:build !cgo

// Package net — build guard for cgo-disabled builds.
//
// Every useful file in this package imports "C": the Go binding is a thin
// wrapper over `libnet`, so `New`, `Config`, `Mesh`, and the rest live in
// cgo files. With `CGO_ENABLED=0` the toolchain silently drops all of them
// and compiles the package down to the handful of plain-Go type
// declarations that remain. The package still *exists*, so the build gets
// far enough to type-check the caller against an API that is now almost
// empty, and reports:
//
//	./main.go:11:21: undefined: net.New
//	./main.go:11:30: undefined: net.Config
//
// pointed at the user's own file. That is the wrong diagnosis and it
// arrives at the wrong place. `CGO_ENABLED=0` is the default on a Windows
// Go install with no C compiler on PATH, so the first thing a new user
// sees is the documented quickstart apparently calling functions that do
// not exist — and nothing anywhere names cgo.
//
// This file makes the package itself fail instead. Referencing an
// undeclared identifier is what produces a readable message: the compiler
// prints the name verbatim, attributes it to this file rather than to the
// caller, and stops before the caller's package is type-checked at all —
// so the misleading `undefined: net.New` never appears.
package net

func init() {
	//nolint:staticcheck // The identifier is deliberately undeclared; its
	// name is the error message. See the package comment above.
	this_package_requires_cgo__set_CGO_ENABLED_1_and_install_a_C_compiler__see_https_ai2070_net_docs_start_install_go()
}
