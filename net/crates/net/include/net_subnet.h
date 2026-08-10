/*
 * net_subnet.h — C SDK header for the subnet AUTHORITY surface
 * (SUBNET_AUTH_SDK_PLAN.md S4c / §6.4).
 *
 * These symbols sit beside net_org.h's, in the org-ffi rlib that is
 * linked into `libnet`. They are declared in a separate header only
 * because they are a separate concept. Hosting them there reuses the org
 * handler dispatcher, the net_org_caller_t projection, and the
 * Arc<MeshNode> ownership contract: the base FFI inside the `net` crate
 * cannot depend on `net-mesh-sdk`, while the subnet SDK
 * (`net_sdk::subnet`) does, and the org surface already carries that
 * dependency.
 *
 * Like every other header here, this one resolves out of one `libnet`:
 *
 *   cargo build --release -p net-ffi
 *   gcc -o app app.c -L target/release -lnet -lpthread -ldl -lm
 *
 * # The subnet surface, in one paragraph
 *
 * Topology is not authority. This header authorizes protected transport:
 * a GATEWAY installs credential sets, declares boundaries, and applies
 * signed control facts; a PROVIDER serves an organization-protected nRPC
 * service against one exact exported crossing; a CALLER invokes it with
 * organization authority only (net_org_call_exported, in net_org.h) —
 * never joining the provider's subnet. Every signed artifact is minted by
 * `net-mesh subnet …` and crosses as opaque canonical wire bytes; nothing
 * here signs, and no signing key crosses this boundary.
 *
 * # Error model
 *
 * `int` returns share the org surface's namespace. A subnet
 * provisioning / configuration / serve failure returns
 * NET_ORG_ERR_SUBNET (-13) and writes the stable `subnet:<kind>` wire
 * string to `out_err` (free with net_org_free_cstring). It is a LOCAL,
 * startup-shaped failure — never a call domain. Remote exported-call
 * refusals surface through the org call domains (net_org.h).
 *
 * # Trust anchors
 *
 * Declaring which authorities a node trusts, its security attachment path,
 * and its control channel is CONFIG-TIME state, supplied in the JSON
 * `net_mesh_new` already takes:
 *
 *   {
 *     "bind_addr": "127.0.0.1:0",
 *     "psk_hex": "…",
 *     "subnet_authorities": [
 *       { "authority_hex": "…",
 *         "root_hexes": ["…"],
 *         "maximum_grant_lifetime_secs": 604800 }
 *     ],
 *     "subnet_attachment": [3],
 *     "subnet_control_channel": "subnet.control"
 *   }
 *
 * Rust converts and validates these through the same frozen DTOs every
 * other SDK uses, before the node exists: a duplicate authority, an empty
 * or duplicated root set, a zero lifetime, a path deeper than four levels,
 * or a malformed hex id all return NET_ERR_MESH_INIT. An absent or empty
 * authority list means every protected subnet assertion fails closed.
 *
 * A standalone C program can therefore stand up a subnet GATEWAY on its
 * own; it does not need a node handed to it from Rust, Node, or Python.
 * (Until review-10 P1-7 this was impossible — the conversion lived in
 * net-mesh-sdk, which base libnet's constructor cannot depend on.)
 *
 * # Handle & ownership model
 *
 * Identical to net_org.h: every mesh_arc comes from net_mesh_arc_clone
 * and is CONSUMED (mint a fresh clone per call; do NOT free it — the node
 * lives on via the Go MeshNode). Serve returns a NetOrgServeHandle (from
 * net_org.h), freed with net_org_serve_handle_free. Wholesale-replace
 * semantics: install/declare replace the whole set, so pass every
 * currently-held artifact, not a delta.
 *
 * CONSUMED means consumed on EVERY path, including a refusal caused by
 * some other malformed argument. The single exception is a NULL mesh_arc
 * itself, which names nothing to consume. Do not reclaim the clone after
 * an error return.
 *
 * # Array counts
 *
 * Every count below (set_count, boundary_count) is the exact number of
 * elements behind its pointer. Counts whose total byte length would
 * exceed the platform's maximum slice length are refused deterministically
 * without the arrays being read, so a (size_t)-1 typo is an error return
 * rather than undefined behavior — but a count that merely EXCEEDS the
 * real array length is still a caller bug this ABI cannot detect.
 */

#ifndef NET_SUBNET_H
#define NET_SUBNET_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

/* NetOrgServeHandle, net_org_caller_t, net_compute_mesh_arc_t, the
 * NET_ORG_ERR_* codes, net_org_free_cstring, net_org_reserve_handler_id,
 * net_org_set_handler_dispatcher, and net_org_call_exported all live in
 * net_org.h — this header is a companion, not a replacement. */
#include "net_org.h"

#ifdef __cplusplus
extern "C" {
#endif

/* NET_ORG_ERR_SUBNET (-13) — the subnet provisioning / configuration /
 * serve-registration failure code — is defined in net_org.h (it is an
 * org-namespace code returned by these subnet functions, and lives in
 * the same numeric-mirror contract as the other NET_ORG_ERR_* codes). It
 * carries the stable `subnet:<kind>` wire string on out_err and is NOT a
 * call domain. */

/* ======================================================================
 * Access modes (who may call a subnet-exported service).
 *
 * These name the values used in the `subnet_exports` JSON handed to
 * net_mesh_new ("sameOrg" / "granted"). No function in this header takes
 * an access mode: choosing who may call an export is CONFIGURATION, not a
 * per-serve argument (review-10 P1-6).
 * ==================================================================== */

#define NET_SUBNET_ACCESS_SAME_ORG 0  /* SubnetExportAccess::SameOrg  */
#define NET_SUBNET_ACCESS_GRANTED  1  /* SubnetExportAccess::Granted  */

/* ======================================================================
 * net_subnet_path_t — a compact hierarchy path.
 *
 * Layout is part of the ABI (a Rust offset/size test pins it): 5 bytes,
 * depth + 4 levels. `depth` is 0..=4; levels[depth..] MUST be zero (the
 * canonical form — a non-zero inactive tail is refused, never silently
 * truncated). depth == 0 is the authority-root (global) path.
 *
 * Used ONLY by net_subnet_declare_boundaries, an operator verb. There is
 * deliberately no authority-qualified-crossing type here: ordinary C
 * application code names a configured export and constructs no authority
 * objects (review-10 P1-6). The former net_subnet_ref_t is gone.
 * ==================================================================== */

typedef struct {
    uint8_t depth;      /* active level count, 0..=4               */
    uint8_t levels[4];  /* path labels; inactive tail must be zero */
} net_subnet_path_t;

/* ======================================================================
 * Gateway provisioning (SSDK §3.4) — WHOLESALE REPLACE.
 * ==================================================================== */

/* Install this node's own gateway credential sets. Every artifact decodes
 * BEFORE anything installs, so one malformed set refuses the whole batch
 * with no node-state mutation. `set_ptrs`/`set_lens` are parallel arrays of
 * length `set_count` (may be 0). `mesh_arc` is CONSUMED. On failure returns
 * NET_ORG_ERR_SUBNET with the `subnet:<kind>` wire on `out_err`. */
int net_subnet_install_gateway_credentials(
    net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* const* set_ptrs, const size_t* set_lens, size_t set_count,
    char** out_err);

/* Declare this node's protected boundary inventory — also wholesale.
 * `authority` is a 32-byte entity id; `boundaries` is `boundary_count`
 * net_subnet_path_t values (may be 0). `mesh_arc` is CONSUMED. A
 * non-canonical path or bad pointer returns NET_ORG_ERR_SUBNET. */
int net_subnet_declare_boundaries(
    net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* authority, uint32_t topology_epoch,
    const net_subnet_path_t* boundaries, size_t boundary_count,
    char** out_err);

/* Apply one signed control fact from its outer wire frame — the ONE door
 * (floors and descriptive facts alike). `mesh_arc` is CONSUMED. On success
 * writes the outcome kind to `*out_kind` (a malloc'd C string; free with
 * net_org_free_cstring) and `*out_applied`; `applied == false` is an
 * authenticated stale/idempotent outcome, NOT a failure. On failure
 * returns NET_ORG_ERR_SUBNET. */
int net_subnet_apply_control_fact(
    net_compute_mesh_arc_t* mesh_arc,
    const uint8_t* fact_ptr, size_t fact_len,
    char** out_kind, bool* out_applied, char** out_err);

/* ======================================================================
 * The exported provider verb (SSDK §3.5).
 * ==================================================================== */

/* Serve a subnet-exported, organization-protected service against a NAMED
 * export. The handler is the SAME dispatcher net_org_serve uses
 * (net_org_set_handler_dispatcher + a net_org_reserve_handler_id), and it
 * receives the verified net_org_caller_t.
 *
 * `export_name` is a provider-local label from the `subnet_exports` array
 * given to net_mesh_new. Rust resolves it against the checked map the node
 * holds — so this call constructs NO authority objects: no crossing, no
 * topology epoch, no access mode. The name is never announced and never
 * accepted from a caller. An unknown name fails HERE, before any
 * registration or announcement, with subnet:unknown_export_name on out_err.
 *
 * `mesh_arc` is CONSUMED; `handler_id` MUST already be reserved and stored
 * in the language registry. Announcement visibility is always public — the
 * external caller never joins this node's subnet. Requires an installed
 * node authority. Returns NET_ORG_ERR_ALREADY_SERVING /
 * NET_ORG_ERR_NO_DISPATCHER / NET_ORG_ERR_SUBNET on failure. */
int net_subnet_serve_exported(
    net_compute_mesh_arc_t* mesh_arc,
    const char* service_ptr, size_t service_len,
    const char* export_name_ptr, size_t export_name_len,
    uint64_t handler_id,
    NetOrgServeHandle** out_handle, char** out_err);

/* The caller verb — net_org_call_exported — lives on the OrgClient handle
 * in net_org.h: a subnet-exported call is an organization call to a
 * publicly discoverable service. See net_org.h. */

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* NET_SUBNET_H */
