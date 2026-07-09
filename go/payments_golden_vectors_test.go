// Cross-language payments golden vectors (PAYMENTS_IMPLEMENTATION_PLAN.md
// Workstream 1).
//
// Loads `net/crates/net/tests/cross_lang_payments/payment_vectors.json` —
// the fixture the Rust source-of-truth verifier
// (`payments/tests/payments_golden_vectors.rs`) validates — and asserts
// the canonical-encoding regime holds byte-identically from Go:
//
//   - canonical form: one JSON object, all keys sorted bytewise, compact
//     separators, raw UTF-8 (no HTML escaping), integers only
//   - signed payload = canonical form with the top-level `signature` key
//     absent; ed25519 (crypto/ed25519) over those exact bytes
//   - x402 documents ride as base64 of their preserved original bytes —
//     the captured v2 fixtures must survive untouched
//
// CAIP / amount / decimals grammar tables are enforced by the Rust
// verifier (the grammar lives in the Rust core; no payments binding
// exists yet — logic never lives in bindings).

package net

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"testing"
	"unicode/utf8"
)

type paymentsEnvelopeVector struct {
	Name          string  `json:"name"`
	Object        string  `json:"object"`
	Canonical     string  `json:"canonical"`
	SignedPayload *string `json:"signed_payload"`
	SignerHex     *string `json:"signer_hex"`
	SignatureHex  *string `json:"signature_hex"`
}

type paymentsPreservationVector struct {
	Name          string  `json:"name"`
	File          string  `json:"file"`
	Base64        string  `json:"base64"`
	EmbeddedIn    *string `json:"embedded_in"`
	EnvelopeField *string `json:"envelope_field"`
}

type paymentsFailureExpect struct {
	Stage        string `json:"stage"`
	Reason       string `json:"reason"`
	Retryable    bool   `json:"retryable"`
	FundsMoved   string `json:"funds_moved"`
	PriorPayment string `json:"prior_payment"`
	Recovery     struct {
		Class         string `json:"class"`
		Actor         string `json:"actor"`
		SafeToRetry   bool   `json:"safe_to_retry"`
		SafeToRequote bool   `json:"safe_to_requote"`
	} `json:"recovery"`
}

type paymentsFailureCase struct {
	Name            string                 `json:"name"`
	HeaderUTF8      *string                `json:"header_utf8"`
	HeaderBase64    *string                `json:"header_base64"`
	Accepted        bool                   `json:"accepted"`
	Expect          *paymentsFailureExpect `json:"expect"`
	ExpectExtraKeys []string               `json:"expect_extra_keys"`
}

type paymentsFailureVectors struct {
	Tag   string                `json:"tag"`
	Cases []paymentsFailureCase `json:"cases"`
}

type paymentsFixture struct {
	Envelopes               []paymentsEnvelopeVector     `json:"envelopes"`
	X402BytePreservation    []paymentsPreservationVector `json:"x402_byte_preservation"`
	FailureSchematicVectors paymentsFailureVectors       `json:"failure_schematic_vectors"`
}

func paymentsFixtureDir(t *testing.T) string {
	t.Helper()
	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("runtime.Caller failed")
	}
	return filepath.Join(filepath.Dir(thisFile), "..", "net", "crates", "net", "tests", "cross_lang_payments")
}

func loadPaymentsFixture(t *testing.T) paymentsFixture {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(paymentsFixtureDir(t), "payment_vectors.json"))
	if err != nil {
		t.Fatalf("read fixture: %v", err)
	}
	var f paymentsFixture
	if err := json.Unmarshal(raw, &f); err != nil {
		t.Fatalf("parse fixture: %v", err)
	}
	return f
}

// paymentsCanonicalize is the payments canonical writer: sorted keys,
// compact separators, no HTML escaping, integers preserved via
// json.Number, floats rejected (the money path has none).
func paymentsCanonicalize(value interface{}, out *strings.Builder) error {
	switch v := value.(type) {
	case nil:
		out.WriteString("null")
	case bool:
		if v {
			out.WriteString("true")
		} else {
			out.WriteString("false")
		}
	case json.Number:
		s := v.String()
		if strings.ContainsAny(s, ".eE") {
			return fmt.Errorf("non-integer number in envelope: %s", s)
		}
		out.WriteString(s)
	case string:
		escaped, err := paymentsEncodeString(v)
		if err != nil {
			return err
		}
		out.WriteString(escaped)
	case []interface{}:
		out.WriteByte('[')
		for i, item := range v {
			if i > 0 {
				out.WriteByte(',')
			}
			if err := paymentsCanonicalize(item, out); err != nil {
				return err
			}
		}
		out.WriteByte(']')
	case map[string]interface{}:
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		out.WriteByte('{')
		for i, k := range keys {
			if i > 0 {
				out.WriteByte(',')
			}
			escaped, err := paymentsEncodeString(k)
			if err != nil {
				return err
			}
			out.WriteString(escaped)
			out.WriteByte(':')
			if err := paymentsCanonicalize(v[k], out); err != nil {
				return err
			}
		}
		out.WriteByte('}')
	default:
		return fmt.Errorf("unexpected type in envelope: %T", value)
	}
	return nil
}

// paymentsEncodeString emits a JSON string without HTML escaping (matches
// serde_json / JSON.stringify / json.dumps(ensure_ascii=False)).
func paymentsEncodeString(s string) (string, error) {
	var buf strings.Builder
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(s); err != nil {
		return "", err
	}
	return strings.TrimSuffix(buf.String(), "\n"), nil
}

func paymentsCanonicalString(t *testing.T, doc string) string {
	t.Helper()
	dec := json.NewDecoder(strings.NewReader(doc))
	dec.UseNumber()
	var parsed interface{}
	if err := dec.Decode(&parsed); err != nil {
		t.Fatalf("parse canonical doc: %v", err)
	}
	var out strings.Builder
	if err := paymentsCanonicalize(parsed, &out); err != nil {
		t.Fatalf("canonicalize: %v", err)
	}
	return out.String()
}

func TestPaymentsCanonicalEmissionIsAFixedPoint(t *testing.T) {
	f := loadPaymentsFixture(t)
	if len(f.Envelopes) == 0 {
		t.Fatal("fixture has no envelope vectors")
	}
	for _, env := range f.Envelopes {
		env := env
		t.Run(env.Name, func(t *testing.T) {
			if got := paymentsCanonicalString(t, env.Canonical); got != env.Canonical {
				t.Fatalf("canonical emission drifted:\n got: %s\nwant: %s", got, env.Canonical)
			}
		})
	}
}

func TestPaymentsSignaturesVerify(t *testing.T) {
	f := loadPaymentsFixture(t)
	for _, env := range f.Envelopes {
		env := env
		if env.SignatureHex == nil {
			continue
		}
		t.Run(env.Name, func(t *testing.T) {
			dec := json.NewDecoder(strings.NewReader(env.Canonical))
			dec.UseNumber()
			var parsed map[string]interface{}
			if err := dec.Decode(&parsed); err != nil {
				t.Fatalf("parse: %v", err)
			}
			delete(parsed, "signature")
			var out strings.Builder
			if err := paymentsCanonicalize(parsed, &out); err != nil {
				t.Fatalf("canonicalize: %v", err)
			}
			payload := out.String()
			if env.SignedPayload == nil || payload != *env.SignedPayload {
				t.Fatalf("signed payload derivation drifted:\n got: %s", payload)
			}

			pub, err := hex.DecodeString(*env.SignerHex)
			if err != nil || len(pub) != ed25519.PublicKeySize {
				t.Fatalf("bad signer key: %v", err)
			}
			sig, err := hex.DecodeString(*env.SignatureHex)
			if err != nil {
				t.Fatalf("bad signature hex: %v", err)
			}
			if !ed25519.Verify(ed25519.PublicKey(pub), []byte(payload), sig) {
				t.Fatal("signature does not verify over the derived payload")
			}
			if ed25519.Verify(ed25519.PublicKey(pub), []byte(payload+" "), sig) {
				t.Fatal("tampered payload must not verify")
			}
		})
	}
}

func TestPaymentsX402FixturesSurviveUntouched(t *testing.T) {
	f := loadPaymentsFixture(t)
	dir := paymentsFixtureDir(t)
	if len(f.X402BytePreservation) == 0 {
		t.Fatal("fixture has no preservation vectors")
	}
	for _, p := range f.X402BytePreservation {
		p := p
		t.Run(p.Name, func(t *testing.T) {
			fileBytes, err := os.ReadFile(filepath.Join(dir, filepath.FromSlash(p.File)))
			if err != nil {
				t.Fatalf("read %s: %v", p.File, err)
			}
			decoded, err := base64.StdEncoding.DecodeString(p.Base64)
			if err != nil {
				t.Fatalf("decode base64: %v", err)
			}
			if string(decoded) != string(fileBytes) {
				t.Fatal("fixture file and vector base64 disagree")
			}
			if base64.StdEncoding.EncodeToString(fileBytes) != p.Base64 {
				t.Fatal("base64 re-encoding is not byte-exact")
			}

			if p.EmbeddedIn != nil && p.EnvelopeField != nil {
				var envDoc string
				for _, env := range f.Envelopes {
					if env.Name == *p.EmbeddedIn {
						envDoc = env.Canonical
					}
				}
				if envDoc == "" {
					t.Fatalf("envelope %s not found", *p.EmbeddedIn)
				}
				var parsed map[string]interface{}
				if err := json.Unmarshal([]byte(envDoc), &parsed); err != nil {
					t.Fatalf("parse envelope: %v", err)
				}
				field, ok := parsed[*p.EnvelopeField].(string)
				if !ok || field != p.Base64 {
					t.Fatalf("envelope %s.%s does not embed the fixture bytes", *p.EmbeddedIn, *p.EnvelopeField)
				}
			}
		})
	}
}

func TestPaymentsCanonicalWriterRejectsFloats(t *testing.T) {
	var out strings.Builder
	err := paymentsCanonicalize(map[string]interface{}{"price": json.Number("1.5")}, &out)
	if err == nil {
		t.Fatal("floats must be rejected by the canonical writer")
	}
}

// paymentsHasSchematicShape checks presence + JSON type of every required
// FailureSchematic field (its non-optional fields; quote_id / tool_id / extra
// keys stay optional) — the structural half of from_header_bytes (a full typed
// serde deserialize). A tag-only or mistyped object does not deserialize, so it
// is not accepted.
func paymentsHasSchematicShape(obj map[string]interface{}) bool {
	for _, k := range []string{"object", "code", "stage", "reason", "message", "funds_moved", "prior_payment"} {
		if _, ok := obj[k].(string); !ok {
			return false
		}
	}
	for _, k := range []string{"retryable", "handler_executed"} {
		if _, ok := obj[k].(bool); !ok {
			return false
		}
	}
	rec, ok := obj["recovery"].(map[string]interface{})
	if !ok {
		return false
	}
	for _, k := range []string{"class", "actor"} {
		if _, ok := rec[k].(string); !ok {
			return false
		}
	}
	for _, k := range []string{"safe_to_retry", "safe_to_requote"} {
		if _, ok := rec[k].(bool); !ok {
			return false
		}
	}
	return true
}

// paymentsTolerantParse mirrors FailureSchematic::from_header_bytes: decode the
// header bytes as strict UTF-8 JSON (Go's encoding/json already rejects
// Infinity/NaN) and accept iff the value deserializes to the full schematic
// shape AND carries the tag — else nil (fall back to the human error).
func paymentsTolerantParse(raw []byte, tag string) map[string]interface{} {
	if !utf8.Valid(raw) {
		return nil
	}
	var v interface{}
	if err := json.Unmarshal(raw, &v); err != nil {
		return nil
	}
	obj, ok := v.(map[string]interface{})
	if !ok {
		return nil
	}
	if obj["object"] != tag {
		return nil
	}
	if !paymentsHasSchematicShape(obj) {
		return nil
	}
	return obj
}

func TestPaymentsFailureSchematicTolerance(t *testing.T) {
	f := loadPaymentsFixture(t)
	block := f.FailureSchematicVectors
	if len(block.Cases) == 0 {
		t.Fatal("fixture has no failure-schematic cases")
	}
	for _, c := range block.Cases {
		c := c
		t.Run(c.Name, func(t *testing.T) {
			var raw []byte
			switch {
			case c.HeaderUTF8 != nil:
				raw = []byte(*c.HeaderUTF8)
			case c.HeaderBase64 != nil:
				decoded, err := base64.StdEncoding.DecodeString(*c.HeaderBase64)
				if err != nil {
					t.Fatalf("decode base64: %v", err)
				}
				raw = decoded
			default:
				t.Fatal("case has neither header_utf8 nor header_base64")
			}

			parsed := paymentsTolerantParse(raw, block.Tag)
			if (parsed != nil) != c.Accepted {
				t.Fatalf("tolerance verdict drifted: got accepted=%v want %v", parsed != nil, c.Accepted)
			}
			if parsed == nil {
				return
			}
			if parsed["object"] != block.Tag {
				t.Fatal("accepted schematic does not carry the tag")
			}
			if c.Expect != nil {
				if parsed["stage"] != c.Expect.Stage {
					t.Fatalf("stage: got %v want %v", parsed["stage"], c.Expect.Stage)
				}
				if parsed["reason"] != c.Expect.Reason {
					t.Fatalf("reason: got %v want %v", parsed["reason"], c.Expect.Reason)
				}
				if parsed["retryable"] != c.Expect.Retryable {
					t.Fatalf("retryable: got %v want %v", parsed["retryable"], c.Expect.Retryable)
				}
				if parsed["funds_moved"] != c.Expect.FundsMoved {
					t.Fatalf("funds_moved: got %v want %v", parsed["funds_moved"], c.Expect.FundsMoved)
				}
				if parsed["prior_payment"] != c.Expect.PriorPayment {
					t.Fatalf("prior_payment: got %v want %v", parsed["prior_payment"], c.Expect.PriorPayment)
				}
				rec, ok := parsed["recovery"].(map[string]interface{})
				if !ok {
					t.Fatal("recovery is not an object")
				}
				if rec["class"] != c.Expect.Recovery.Class {
					t.Fatalf("recovery.class: got %v want %v", rec["class"], c.Expect.Recovery.Class)
				}
				if rec["actor"] != c.Expect.Recovery.Actor {
					t.Fatalf("recovery.actor: got %v want %v", rec["actor"], c.Expect.Recovery.Actor)
				}
				if rec["safe_to_retry"] != c.Expect.Recovery.SafeToRetry {
					t.Fatalf("recovery.safe_to_retry: got %v", rec["safe_to_retry"])
				}
				if rec["safe_to_requote"] != c.Expect.Recovery.SafeToRequote {
					t.Fatalf("recovery.safe_to_requote: got %v", rec["safe_to_requote"])
				}
			}
			for _, k := range c.ExpectExtraKeys {
				if _, ok := parsed[k]; !ok {
					t.Fatalf("extra key %q not preserved", k)
				}
			}
		})
	}
}
