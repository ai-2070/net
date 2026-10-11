// Code review 2026-10-11: MeshNode.Shutdown runs net_mesh_shutdown under
// the READ lock and nils the handle only afterwards, so a constructor
// running beside the native shutdown saw a live handle and returned a
// client bound to a stopped node. Shutdown now marks the node shutting
// down — waiting out any constructor in progress — before the native
// call, and every child constructor refuses once that mark is set.

package net

import "testing"

func TestMeshNode_ConstructorsRefuseOnceShutdownHasBegun(t *testing.T) {
	node := newLocalMesh(t)
	defer node.Shutdown()

	// Shutdown's first phase, frozen: marked, native shutdown not yet run,
	// handle still live — the window the constructors used to slip into.
	node.ctorMu.Lock()
	node.shuttingDown = true
	node.ctorMu.Unlock()
	node.mu.RLock()
	live := node.handle != nil
	node.mu.RUnlock()
	if !live {
		t.Fatal("test setup: the handle must still be live")
	}

	if c, err := NewRegistryClient(node); err == nil {
		c.Close()
		t.Error("NewRegistryClient bound a client to a node that is shutting down")
	}
	if c, err := NewFoldQueryClient(node); err == nil {
		c.Close()
		t.Error("NewFoldQueryClient bound a client to a node that is shutting down")
	}
	if r, err := NewMeshRpc(node); err == nil {
		r.Close()
		t.Error("NewMeshRpc bound a client to a node that is shutting down")
	}
	if rt, err := NewDaemonRuntime(node); err == nil {
		rt.Close()
		t.Error("NewDaemonRuntime bound a runtime to a node that is shutting down")
	}
	if err := node.Shutdown(); err != nil {
		t.Fatalf("Shutdown after the mark: %v", err)
	}
}
