# Deck — Features Overview
*Operator cyberdeck for the mesh*

Deck is the real‑time terminal UI into Net’s distributed substrate.  
It exposes everything MeshOS, MeshDB, RedEX, and Dataforts are doing — live, streaming, low‑latency — the way a cyberpunk cluster console should.

Below are the core features, grouped the way an operator thinks.

---

1. Cluster Topology Map
Live mesh view:

- all nodes  
- health status  
- RTT between nodes  
- avoid‑list indicators  
- maintenance flags  
- draining nodes  
- rejoin / recovery states  
- replica density & heat overlays  

This is your “map of the jungle.”

---

2. Replica & Placement Inspector
Direct view into Dataforts + MeshOS behavior:

- desired vs actual replica counts  
- active migrations  
- blob pulls  
- greedy pulls  
- eviction candidates  
- artifact scoring (5‑axis)  
- drift indicators  
- placement stability score  

If Dataforts is the brain, Deck shows every electrical impulse.

---

3. Daemon Supervision Panel
Live supervision of every node‑resident daemon:

- health (Healthy / Degraded / Unhealthy)  
- saturation (0.0 → 1.0)  
- recent restarts  
- crash‑loop indicators  
- log tail (live)  
- controls:
  - restart daemon  
  - drain daemon  
  - send MeshOS control events  

This is the “per-process cockpit.”

---

4. Maintenance Node Control
Full maintenance-mode operator surface:

- enter maintenance  
- track drain progress  
- track replica evacuation  
- stalled migrations  
- deadline countdown  
- exit maintenance  
- stuck → DrainFailed warnings  
- recovery window progress  

Every state transition from MeshOS is rendered cleanly.

---

5. Behavior Timeline (MeshOS Snapshot)
Backed by MeshDB fold (MeshOsSnapshot):

- in-flight actions  
- pending actions  
- recent failures  
- drift  
- locality map  
- per-daemon snapshots  
- placement stability  
- node maintenance state  

The “story” of the cluster in one fold.

---

6. Blob & Artifact Explorer
Real-time object navigation:

- replica locations  
- chain metadata  
- blob movement history  
- heat level  
- access frequency charts  
- anti‑entropy cycles  
- artifact ancestry (fork-of walks)  
- shard inspection  

This effectively replaces every S3/minio tool you’ve ever used.

---

7. Admin Surface (Signed Ops)
Admin-chain powered actions:

- drain node  
- cordon / uncordon  
- enter / exit maintenance  
- drop replicas  
- invalidate placement  
- restart all daemons  
- clear avoid lists  
- view admin-event ledger  

All actions sign with the operator identity and propagate via RedEX.

---

8. MeshDB Console
Fully interactive:

- run MeshDB queries  
- inspect folds  
- trace chain cursors  
- debug planner output  
- resume queries  
- federated queries across nodes  
- streaming result mode  

Deck becomes the built-in query editor for your mesh.

---

9. Log Matrix (RED/HEAT/INFO Streams)
High-speed, scrollable grid:

- node → daemon → log lines  
- filter by level / daemon / node  
- jump to recent crash  
- follow mode  
- hyperlink to behavior snapshot events  

Logs stream directly via RedEX chain subscriptions.

---

10. Operator Identity & Audit Trail
Operator-signed actions:

- identity loaded from maintenance node  
- key rotation workflow  
- per-action signatures  
- RedEX-committed audit record  
- timeline of operator events  

A real audit system — not a cloud imitation.

---

11. Node Inventory
Inventory per node:

- CPU / mem / disk  
- saturation trend  
- capability set  
- fork-of ancestry  
- software versions  
- essential daemons vs optional  

---

12. Multi‑Cluster Switcher
If you run multiple meshes:

- switch contexts  
- persistent bookmarks
- SSH-style “known meshes”  
- optional pinning per tab  

This lets Deck act like tmux for clusters.

---

13. ICE (Operator Safeguards)
Deck integrates directly with MeshOS backpressure:

- warn when cluster is overwhelmed  
- highlight dangerous actions  
- show replication cooldown windows  
- show migration throttle  
- prevent unsafe drain under high load  

The console refuses to let you break yourself.

---

In summary
Deck gives operators:

- real-time truth  
- cluster visibility  
- signed control  
- MeshDB access  
- maintenance workflows  
- daemon supervision  
- placement insight  
- anti-entropy awareness  
- behavior timelines  
- log streaming  

It turns a distributed OS into a mesh you can actually see and command.

Operator ICE is the high‑authority intervention surface inside Deck —  
the layer an SRE, ops lead, or cluster owner uses when the jungle is on fire and the mesh needs decisive action.

ICE exposes:

1. Hard overrides (signed)
- Force‑drain  
- Force‑evict replica  
- Force‑restart daemon  
- Force‑cutover  
- Kill stuck migrations  
- Flush avoid-lists  
- Freeze/unfreeze replica movement  

All ICE operations require:
- operator key  
- signature  
- confirmation  
- and appear on the admin chain for full auditability.

---

2. Cluster Freeze / Thaw
The “break-glass” switch.

- Freeze → suspend non-essential actions  
- Thaw → resume behavior loop  
- Visual warning banners  
- TTL to prevent accidental permanent freezes  

Used in:
- partition debugging  
- bad code rollout  
- live incident triage  

---

3. Safety Envelope Visualization
ICE shows:

- backpressure levels  
- drain-rate throttle status  
- crash-loop gating  
- stabilization windows  
- event storm warnings  
- replica churn heat  
- node distress signals  

When an op uses ICE, they see exactly what their action will touch.

---

4. High-fidelity replay (read-only)
Time-travel snapshots for:

- chain commits  
- MeshOS actions  
- capability drift  
- locality shifts  
- daemon restarts  
- admin events  

An operator can scroll backward and see the story of how the cluster entered a bad state.

---

5. “Blast Radius” pre-execution check
Before an ICE action executes, Deck simulates:

- which replicas move  
- which daemons restart  
- which nodes become hot  
- expected drain delay  
- placement impacts  
- stability consequences  

Then it prints:

 “This action affects 4 nodes, 12 replicas, and 2 daemons. Continue?”

---

6. Lockout / Escalation
Optional ICE safety:

- Only maintenance nodes can authorize ICE  
- Multi-operator signing (2‑of‑N)  
- Lockout timer after dangerous actions  

---

Operator ICE is the “cyberpunk SRE panel”.

Not a dashboard.  
Not a UI.  
Not a toy.

It’s the:
- break-glass console  
- high-authority override  
- deep cluster surgery kit  
- “I need control now” interface  
- equivalent of root on the entire mesh  

ICE is what turns Deck from an observability tool into a true cyberdeck.
