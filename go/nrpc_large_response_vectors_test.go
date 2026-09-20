// SPDX-License-Identifier: MIT OR Apache-2.0
// Independent byte-layout check, not native-mesh interoperability.
package net

import (
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"os"
	"testing"
)

func TestLargeResponseWireVector(t *testing.T) {
	data, err := os.ReadFile("../net/crates/net/tests/cross_lang_nrpc/golden_vectors_large_response.json")
	if err != nil {
		t.Fatal(err)
	}
	var fixture struct {
		Flag         int    `json:"request_flag"`
		Header       string `json:"header_name"`
		Chunk        int    `json:"chunk_bytes"`
		Maximum      int    `json:"max_response_bytes"`
		MaxFragments int    `json:"max_fragments"`
		Length       int    `json:"encoded_response_bytes"`
		Response     struct {
			Status uint16 `json:"status"`
			Byte   byte   `json:"body_byte"`
			Length int    `json:"body_length"`
		} `json:"response"`
		Fragments []struct {
			Index  int    `json:"index"`
			Header string `json:"header_value_hex"`
			Length int    `json:"body_bytes"`
		} `json:"fragments"`
	}
	if err := json.Unmarshal(data, &fixture); err != nil {
		t.Fatal(err)
	}
	if fixture.Flag != 64 || fixture.Header != "nrpc-response-fragment-v1" || fixture.Maximum/fixture.Chunk != fixture.MaxFragments {
		t.Fatal("negotiation or bound mismatch")
	}
	encoded := make([]byte, 7+fixture.Response.Length)
	binary.LittleEndian.PutUint16(encoded, fixture.Response.Status)
	binary.LittleEndian.PutUint32(encoded[3:], uint32(fixture.Response.Length))
	for i := 7; i < len(encoded); i++ {
		encoded[i] = fixture.Response.Byte
	}
	if len(encoded) != fixture.Length || len(fixture.Fragments) != (len(encoded)+fixture.Chunk-1)/fixture.Chunk {
		t.Fatal("encoded length mismatch")
	}
	for _, piece := range fixture.Fragments {
		header := make([]byte, 6)
		binary.LittleEndian.PutUint32(header, uint32(len(encoded)))
		binary.LittleEndian.PutUint16(header[4:], uint16(piece.Index))
		if hex.EncodeToString(header) != piece.Header || min(fixture.Chunk, len(encoded)-piece.Index*fixture.Chunk) != piece.Length {
			t.Fatalf("fragment %d mismatch", piece.Index)
		}
	}
}
