package net

import "testing"

// SDK audit 2026-10-11: cancellation parsed as Unknown and the server
// status was only text, unlike Node and Python.
func TestParseRpcErrorCancelledAndStatus(t *testing.T) {
	if got := parseRpcError("cancelled: call cancelled by caller"); got.Kind != RpcKindCancelled {
		t.Fatalf("cancelled parsed as %q", got.Kind)
	}
	got := parseRpcError("server_error: status=0x0009 message=admission denied")
	if got.Kind != RpcKindServerError || got.Status == nil || *got.Status != 0x0009 {
		t.Fatalf("server_error status not parsed: %+v", got)
	}
	if got.Message != "status=0x0009 message=admission denied" {
		t.Fatalf("message changed: %q", got.Message)
	}
	if s := parseRpcError("timeout: elapsed_ms=5").Status; s != nil {
		t.Fatalf("a non-server error carries no status, got %d", *s)
	}
	if s := parseRpcError("server_error: message=no status").Status; s != nil {
		t.Fatalf("a missing status must stay nil, got %d", *s)
	}
}
