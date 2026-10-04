/*
 * mesh_opaque.h — two connected, started mesh nodes for a program that
 * includes net.h.
 *
 * net.h and net.go.h share the NET_SDK_H include guard, so one translation
 * unit sees only one of them; net_mesh_new and the handshake are declared
 * in net.go.h alone. The documented answer is to split the program across
 * translation units. This header is that split: it declares no Net type,
 * and its implementation (mesh_opaque.c) is the unit that includes
 * net.go.h. The mesh handles cross as `void*`, the type net.h's
 * aggregator clients take.
 *
 * Part of the C consumer runner, not a Net API.
 */

#ifndef CU_MESH_OPAQUE_H
#define CU_MESH_OPAQUE_H

#include <stddef.h>
#include <stdint.h>

#include "loaded_module.h"

/* Build two nodes (identity seeds of 32 copies of `seed_a` / `seed_b`),
 * handshake b to a, and start both. 0 on success. */
int cu_opaque_pair(unsigned char seed_a, unsigned char seed_b, void** out_a, void** out_b);

/* The node id of a handle from cu_opaque_pair. */
uint64_t cu_opaque_node_id(void* node);

/* net_mesh_shutdown then net_mesh_free. 0 if the shutdown succeeded. */
int cu_opaque_teardown(void* node);

/* The Net functions this file calls, for the caller's loaded-module list
 * (the caller's own unit cannot take their addresses: net.go.h declares
 * them). Writes up to `cap` entries to `out`; returns how many there are. */
size_t cu_opaque_fns(clm_fn_t* out, size_t cap);

#endif /* CU_MESH_OPAQUE_H */
