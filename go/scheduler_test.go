package net

import (
	"testing"
)

// Task-lifecycle (WorkflowAdapter) + Tier-2 trigger engine round-trips.
//
// These exercise the *consuming* trigger calls (OnTick / OnTaskChange),
// which a naive "size with a NULL probe, then fill" two-pass would break:
// the probe pass fires + disarms the triggers and discards the actions, so
// the caller would always see an empty slice. The wrappers size the buffer
// from ArmedCount and make a single call instead — these tests pin that the
// fired actions actually come back.

func openWorkflow(t *testing.T) (*Redex, *WorkflowAdapter) {
	t.Helper()
	r := NewRedex("") // in-memory
	wf, err := OpenWorkflow(r, testOrigin, false)
	if err != nil {
		r.Free()
		t.Fatalf("OpenWorkflow: %v", err)
	}
	return r, wf
}

// AtTick triggers must fire on the OnTick that reaches their deadline and
// stay armed before it — the regression guard for the consuming-two-pass
// bug, which dropped every fired action.
func TestTriggerOnTickFiresDueActions(t *testing.T) {
	r, wf := openWorkflow(t)
	defer r.Free()
	defer wf.Free()

	eng, err := NewTriggerEngine(wf)
	if err != nil {
		t.Fatalf("NewTriggerEngine: %v", err)
	}
	defer eng.free()

	// Arm AtTick(5) -> Submit(2).
	if err := eng.ArmAtTick(5, TriggerAction{Kind: "submit", ID: 2}); err != nil {
		t.Fatalf("ArmAtTick: %v", err)
	}
	if n, err := eng.ArmedCount(); err != nil || n != 1 {
		t.Fatalf("ArmedCount before fire = %d, %v; want 1, nil", n, err)
	}

	// Clock below the deadline: nothing fires, trigger stays armed.
	got, err := eng.OnTick(4)
	if err != nil {
		t.Fatalf("OnTick(4): %v", err)
	}
	if len(got) != 0 {
		t.Fatalf("OnTick(4) fired %v; want none", got)
	}
	if n, _ := eng.ArmedCount(); n != 1 {
		t.Fatalf("ArmedCount after OnTick(4) = %d; want 1", n)
	}

	// Clock reaches the deadline: the action fires and the trigger disarms.
	got, err = eng.OnTick(5)
	if err != nil {
		t.Fatalf("OnTick(5): %v", err)
	}
	if len(got) != 1 || got[0].Kind != "submit" || got[0].ID != 2 {
		t.Fatalf("OnTick(5) = %v; want [{submit 2}]", got)
	}
	if n, _ := eng.ArmedCount(); n != 0 {
		t.Fatalf("ArmedCount after fire = %d; want 0", n)
	}
}

// AfterTask(A) -> Submit(B) must fire on the OnTaskChange after A reaches
// Done, returning the action for the caller to apply.
func TestTriggerOnTaskChangeFiresAfterDone(t *testing.T) {
	r, wf := openWorkflow(t)
	defer r.Free()
	defer wf.Free()

	const a, b = uint64(1), uint64(2)

	eng, err := NewTriggerEngine(wf)
	if err != nil {
		t.Fatalf("NewTriggerEngine: %v", err)
	}
	defer eng.free()

	if err := eng.ArmAfterTask(a, TriggerAction{Kind: "submit", ID: b}); err != nil {
		t.Fatalf("ArmAfterTask: %v", err)
	}

	// Drive A to Done.
	if _, err := wf.Submit(a); err != nil {
		t.Fatalf("Submit(A): %v", err)
	}
	if _, err := wf.Start(a); err != nil {
		t.Fatalf("Start(A): %v", err)
	}
	seq, err := wf.Complete(a)
	if err != nil {
		t.Fatalf("Complete(A): %v", err)
	}
	if err := wf.WaitForSeq(seq, 5000); err != nil {
		t.Fatalf("WaitForSeq: %v", err)
	}

	// A changed -> evaluate its triggers. AfterTask(A) is now satisfied.
	got, err := eng.OnTaskChange(a, 0)
	if err != nil {
		t.Fatalf("OnTaskChange(A): %v", err)
	}
	if len(got) != 1 || got[0].Kind != "submit" || got[0].ID != b {
		t.Fatalf("OnTaskChange(A) = %v; want [{submit 2}]", got)
	}
	if n, _ := eng.ArmedCount(); n != 0 {
		t.Fatalf("ArmedCount after fire = %d; want 0", n)
	}
}
