// An unrecognized reliability spelling must not become "none".
//
// `Reliability` is a plain string field with no compiler check behind
// it. Every string boundary — Node, PyO3, and the C parser Go reaches
// through — used to map an unknown value to `ReliabilityConfig::None`,
// so a typo like "ful" or "FULL" constructed successfully and silently
// downgraded delivery from acknowledged-and-retransmitted to
// fire-and-forget. Role and backpressure already rejected unknowns.
//
// These are pure Go and run without the native library.

package net

import (
	"strings"
	"testing"
)

func TestValidateReliability_AcceptsDocumentedVocabulary(t *testing.T) {
	for _, mode := range ReliabilityModes {
		cfg := &Config{Net: &NetConfig{Reliability: mode}}
		if err := cfg.validateReliability(); err != nil {
			t.Fatalf("reliability %q must be accepted, got %v", mode, err)
		}
	}
}

func TestValidateReliability_UnsetIsAllowed(t *testing.T) {
	// Empty means "leave the core default in place", not "invalid".
	cfg := &Config{Net: &NetConfig{Reliability: ""}}
	if err := cfg.validateReliability(); err != nil {
		t.Fatalf("unset reliability must be allowed, got %v", err)
	}

	// No Net block at all, and no config at all.
	if err := (&Config{}).validateReliability(); err != nil {
		t.Fatalf("config without Net must be allowed, got %v", err)
	}
	var nilCfg *Config
	if err := nilCfg.validateReliability(); err != nil {
		t.Fatalf("nil config must be allowed, got %v", err)
	}
}

func TestValidateReliability_RejectsEveryNearMiss(t *testing.T) {
	for _, bad := range []string{
		"ful",      // truncation
		"fully",    // extension
		"FULL",     // case — the vocabulary is case-sensitive
		"Full",     //
		"Light",    //
		"NONE",     //
		" full",    // leading whitespace
		"full ",    // trailing whitespace
		"reliable", // plausible synonym that is not the vocabulary
		"best",     // a future mode that does not exist yet
	} {
		cfg := &Config{Net: &NetConfig{Reliability: bad}}
		err := cfg.validateReliability()
		if err == nil {
			t.Fatalf("reliability %q must be rejected, not silently "+
				"downgraded to fire-and-forget", bad)
		}
		// The message has to name the offending value, or the caller
		// has to guess which of several string fields was wrong.
		if !strings.Contains(err.Error(), bad) {
			t.Errorf("error for %q should quote the value, got %v", bad, err)
		}
	}
}

func TestNew_RejectsInvalidReliabilityBeforeCGO(t *testing.T) {
	// Fails at the Go boundary, so the message is specific rather than
	// a generic init failure from the C parser.
	_, err := New(&Config{Net: &NetConfig{
		BindAddr:    "127.0.0.1:0",
		PeerAddr:    "127.0.0.1:1",
		PSK:         strings.Repeat("42", 32),
		Role:        "initiator",
		Reliability: "FULL",
	}})
	if err == nil {
		t.Fatal("New must reject an invalid reliability")
	}
	if !strings.Contains(err.Error(), "invalid reliability") {
		t.Fatalf("want a reliability-specific error, got %v", err)
	}
}
