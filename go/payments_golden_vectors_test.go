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

type paymentsFixture struct {
	Envelopes            []paymentsEnvelopeVector     `json:"envelopes"`
	X402BytePreservation []paymentsPreservationVector `json:"x402_byte_preservation"`
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
