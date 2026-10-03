//go:build test_helpers

package net

// The Reed-Solomon repair witness (plan S6, as corrected by review R3).
//
// A stripe only gets parity once k full chunks have arrived; a short
// trailing stripe is stored Replicated. At the default 4 MiB chunk size and
// k = 4, the smallest blob with parity to repair from is 16 MiB, and the
// four chunks must differ or content addressing collapses them.

import (
	"bytes"
	"testing"
)

func TestBlobRepairRestoresADroppedDataShard(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-repair", nil)
	defer r.Free()
	defer a.Close()

	data := distinctChunks(4, 4*mib) // exactly 16 MiB: one full stripe
	if len(data) != 16*mib {
		t.Fatalf("fixture is %d bytes, want 16 MiB", len(data))
	}
	ref, err := a.StoreTree(data, EncodingReedSolomon(4, 2))
	if err != nil {
		t.Fatalf("StoreTree RS(4,2): %v", err)
	}
	info, err := DescribeBlobRef(ref)
	if err != nil {
		t.Fatalf("DescribeBlobRef: %v", err)
	}
	if info.Encoding == nil || !info.Encoding.ReedSolomon || info.Encoding.K != 4 || info.Encoding.M != 2 {
		t.Fatalf("stored with encoding %+v, want RS(4,2)", info.Encoding)
	}

	// Make a real data shard unavailable, and prove it before repairing.
	hash, err := testDropDataChunk(a, ref, 0, 1)
	if err != nil {
		t.Fatalf("drop data chunk: %v", err)
	}
	if present, err := testChunkPresent(a, hash); err != nil || present {
		t.Fatalf("dropped shard still present (%v, %v)", present, err)
	}

	report, err := a.RepairBlob(ref)
	if err != nil {
		t.Fatalf("RepairBlob: %v", err)
	}
	if report.ChunksRestored != 1 || report.StripesRepaired != 1 || report.StripesUnrecoverable != 0 {
		t.Fatalf("repair report = %+v, want 1 chunk restored in 1 stripe", report)
	}
	if present, err := testChunkPresent(a, hash); err != nil || !present {
		t.Fatalf("repaired shard not present (%v, %v)", present, err)
	}
	got, err := a.FetchRange(ref, 0, info.Size)
	if err != nil {
		t.Fatalf("FetchRange after repair: %v", err)
	}
	if !bytes.Equal(got, data) {
		t.Fatal("blob differs from the stored bytes after repair")
	}

	again, err := a.RepairBlob(ref)
	if err != nil {
		t.Fatalf("second RepairBlob: %v", err)
	}
	if again.ChunksRestored != 0 || again.StripesAlreadyHealthy != 1 {
		t.Fatalf("second repair = %+v, want nothing restored and the stripe healthy", again)
	}
}

// More than m shards lost: the stripe cannot be rebuilt, and that is a
// counter in the report, not an error.
func TestBlobRepairCountsAnUnrecoverableStripe(t *testing.T) {
	r, a := newBlobAdapter(t, "", "go-repair-lost", nil)
	defer r.Free()
	defer a.Close()

	data := distinctChunks(4, 4*mib)
	ref, err := a.StoreTree(data, EncodingReedSolomon(4, 2))
	if err != nil {
		t.Fatalf("StoreTree: %v", err)
	}
	for i := uint32(0); i < 3; i++ { // m = 2, so three is one too many
		if _, err := testDropDataChunk(a, ref, 0, i); err != nil {
			t.Fatalf("drop data chunk %d: %v", i, err)
		}
	}
	report, err := a.RepairBlob(ref)
	if err != nil {
		t.Fatalf("RepairBlob over the tolerance returned an error (%v); it should report, not fail", err)
	}
	if report.StripesUnrecoverable != 1 || report.ChunksRestored != 0 {
		t.Fatalf("repair report = %+v, want 1 unrecoverable stripe and nothing restored", report)
	}
}
