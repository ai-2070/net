# S4_BROWSER — Stage 4 browser/leaf lane (`S4Browser.BrowserEvidence`)

## 8.1 S4Browser — the org-scoped streaming witness stage (real-browser evidence vehicle)

Scope (per the lane brief): `net/crates/net/tests/rtc_browser/**` (runner
`org_stream.rs` + `main.rs` wiring + `page/org.{html,js}`) and the three leaf
control-plane files `leaf/src/{control_plane,anchor_control_plane,mock_control_plane}.rs`
(surgical additions). Nothing outside this set was touched.

### The 37-witness roster (ledger order)

Browser→native (browser calls, native serves — the harness ANCHOR is the native
peer, adopted into org A and holding DUAL credentials so granted-mode callers are
real org-B principals on the wire):

1. `org_browser_call_unary_same_org`
2. `org_browser_call_unary_granted`
3. `org_browser_call_streaming_same_org`
4. `org_browser_call_streaming_granted`
5. `org_browser_call_client_stream_same_org`
6. `org_browser_call_client_stream_granted`
7. `org_browser_call_duplex_same_org`
8. `org_browser_call_duplex_granted`

Native→browser (native calls, browser serves):

9. `org_native_call_unary_same_org`
10. `org_native_call_unary_granted`
11. `org_native_call_streaming_same_org`
12. `org_native_call_streaming_granted`
13. `org_native_call_client_stream_same_org`
14. `org_native_call_client_stream_granted`
15. `org_native_call_duplex_same_org`
16. `org_native_call_duplex_granted`

Browser→browser (two ISOLATED browser identities, direct §9 session):

17. `org_browser_pair_unary`
18. `org_browser_pair_streaming`
19. `org_browser_pair_client_stream`
20. `org_browser_pair_duplex`
21. `org_browser_pair_granted`

Attribution/refusal:

22. `org_wrong_peer_frames_refused`
23. `org_old_session_frames_refused`
24. `org_replayed_opening_refused`

Backpressure/half-close:

25. `org_streaming_backpressure_and_window`
26. `org_client_stream_backpressure_half_close`
27. `org_duplex_backpressure_half_close`

Revocation (the control-plane feed raises floors mid-run):

28. `org_midstream_revocation_retires_with_denied`
29. `org_revocation_refuses_new_openings`

Teardown/leader:

30. `org_tab_teardown_retires_without_resume`
31. `org_leader_proxied_call_preserves_follower_attribution` — **THE REQUIRED
    INVERSE WITNESS**: its discriminating tuple pairs each follower's EXACT
    payload with its OWN result and the provider-recorded per-call attribution;
    flipping follower attribution at the proxy production site swaps the pairings
    and reddens exactly this assertion (nothing else in the run reads it).
32. `org_leader_replacement_preserves_attribution`
33. `org_leader_teardown_fails_pending_typed`

Handler level:

34. `org_handler_completion_after_retirement_emits_nothing`

Native parity instruments (engine-independent, ADDITIONAL byte-identity
cross-checks — explicitly NOT a substitute for the browser matrix):

35. `org_parity_codec_round_trip_both_codecs`
36. `org_parity_leaf_proof_verifies_under_core`
37. `org_parity_core_proof_verifies_under_leaf`

Roster wiring: `org_stream::WITNESSES: [&str; 37]` + `PARITY: [&str; 3]`, same
check-roster style as `stage5::WITNESSES`/`stage6::WITNESSES`/`stage7::WITNESSES`,
so Main can pin CI from the run output (`RTCB PASS <name> — <discriminating
detail>` lines).

### The revocation feed (both endpoints)

`ControlPlane` gains `fn take_revocation_bundles(&mut self) -> Vec<Vec<u8>>`
(default "no feed", `leaf/src/control_plane.rs`). The one WS control frame is
`{"type":"org_revocation_bundle","bundle":"<standard base64>"}` — the anchor
protocol's vocabulary plus one frame type, carrying the org root's signed
`OrgRevocationBundle` bytes OPAQUE (the leaf's org module verifies + merges
raise-only; "verified by the leaf, not by the transport").

- `anchor_control_plane.rs`: dual ingress — the frame is recognised on the
  trickle socket AND on a dedicated org-control socket
  (`pub fn open_org_control(url)`), because the Stage 4b listener's trickle
  handler originates exactly one frame and `sdk/` is frozen. The bootstrap URL's
  `#org-control=<ws-url>` override tag points the adapter at the runner's feed
  socket (`/harness/org-control`, consumed inside `bind` — no lane-D connect-opt
  required). `take_revocation_bundles` drains the queue in arrival order.
- `mock_control_plane.rs`: `CarriedKind::Revocation` + `MockMesh::deliver_revocation`
  (the Carried carrier; the Net-packet tripwire applies to feed payloads too) +
  `take_revocation_bundles`; two new tests pin exact-bytes/take-once/ledger and
  the tripwire.
- Runner side (`org_stream.rs`): `OrgControlFeed` (WS endpoint registry) +
  `raise_floor()` — the signed bundle is applied at the provider's installed
  `OrgRevocationStore` AND pushed over the control plane to the affected leaf.

Boundary discipline kept: all 8 `tests/control_plane_boundary.rs` scans pass with
the additions (`cargo test --features mock-control-plane --test control_plane
_boundary` → 8 passed), including `the_anchor_implementations_public_surface_
leaks_nothing` (`open_org_control` takes an opaque `String`, leaks nothing) and
`the_bindgen_surface_does_no_transport_of_its_own`.

### Provisioning (the runner is the org operator)

- Org roots A/B (`OrgKeypair::generate`); the ANCHOR adopted into org A
  (`NodeAuthority::adopt` + `install_node_authority` +
  `set_owner_cert_emission(true)`, revocation store retained from
  `authority.revocation`) — the `sdk/src/org/tests_live.rs::fast_mesh` blueprint.
- The anchor holds DUAL credentials (org-A + org-B membership/dispatcher pairs
  for its wire entity) so granted-mode native callers are real org-B principals
  (`authenticated_caller` = the AEAD-verified wire entity = the proof's caller).
- Custodial page identities generated natively (`EntityKeypair::generate` +
  `try_secret_bytes`), passed as `entitySecretHex`/`noiseSecretHex`, so every
  entity id is known before the page loads (proofs name providers/callers by
  entity).
- Credential sets minted with CORE issue fns (`OrgMembershipCert::try_issue`,
  `OrgDispatcherGrant::try_issue`, `OrgCapabilityGrant::try_issue` with
  `GrantRights::INVOKE.union(DISCOVER)` and `GrantTargetScope::ExactNode`) and
  handed to pages as the `OrgCallCredentials` wire bytes (hex at the step
  boundary). Native callers use `CallOptions::org_proof_intent` (the frozen mint
  glue signs).

### Hand-built openings (refusal witnesses)

`mint_opening` reproduces `attach_signed_admission` byte-for-byte in public:
`OrgCallProof::sign_for_call` / `OrgStreamCallProof::sign_for_stream_call` (core,
the exact fns the glue calls) over `org_admission_gate::org_request_digest` of the
finalized request, exactly one `net-org-admission` header, then
`EventMeta ‖ RpcRouteV1 ‖ RpcRequestPayload` (the
`publish_rpc_request_unsubscribed` contract). `session_binding` comes from the
public `MeshNode::peer_session_binding`. The parity instruments (35/36/37) are
the byte-identity receipts that let these stand as "a captured opening".
Delivery: `MeshNode::open_stream` + `send_with_retry` on the request channel's
carrier (stage5's native stream API).

### Files landed (F16 size+sha256 recorded per write; final states)

| file | bytes | sha256 |
|---|---|---|
| `leaf/src/control_plane.rs` | 14419 | `1945ae78d84b69cb8f212109e3592a3f1ebc73714dc1151a77d4ab983c38bb5d` |
| `leaf/src/mock_control_plane.rs` | 30747 | `ec768efe2edc6848993b80b191ba7bb607ed01c58b6a539330a10e5f885161fe` |
| `leaf/src/anchor_control_plane.rs` | 28789 | `840cb5c63e0f8c9be3128449dc62dcafb73ba2b7aa0b7a936a0ca409d903bd4f` |
| `runner/src/org_stream.rs` | 154106 | `05e1e1b00f7dda0f6c51ded88b3383dbf1c49dba4a158bc77b51bf0cab7d86d7` |
| `runner/src/main.rs` | 171739 | `f2ad00b9130a6b6377d68738b70b49c1c8c937fd9fede99aceff5a6385139aea` |
| `page/org.html` | 232 | `d312ae2706a43a6dd84b403c1d1497dc238ebc9730d7b44f8be9d83ef378cb08` |
| `page/org.js` | 26753 | `ccc73ecdf8b72d748ee0a77ea4236c5e266c93347a6b0d310571805aa0f327ba` |
| `runner/Cargo.toml` | 2573 | `31cb4da69038f43e5ee010fa2f396da0e724407e2412e0be1348628f2d201f7a` |

### Scoped proof (pre-run)

- `cargo build --manifest-path net/crates/net/leaf/Cargo.toml` — green (native).
- `cargo check --target wasm32-unknown-unknown` (leaf) — green (wasm32,
  including `anchor_control_plane.rs`); green AFTER lane D's wasm.rs landed.
- `cargo test --features mock-control-plane` filtered `control_plane` — 12 lib
  tests passed (10 pre-existing + 2 new feed tests);
  `--test control_plane_boundary` — **8 passed** (all boundary scans hold).
- `cargo build` (runner) — green.

### Run results

(PENDING — see the per-engine sections below.)

<!-- RESULTS-CHROMIUM -->
<!-- RESULTS-FIREFOX -->
