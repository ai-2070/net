/*
 * net_test_helpers.h — TEST-ONLY seams of the helper build of libnet.
 *
 * NOT a shipped header. It is staged only into the helper C bundle
 * (`make-c-bundle.py --profile helper`, built with
 * `--features net-ffi/test-helpers`), never into the production bundle,
 * and it lives outside `include/` so the shipped header set is unchanged.
 * A program that includes it runs against the helper library only, and its
 * results are helper-build evidence, not evidence about the shipped
 * library. See docs/internal/plans/C_SDK_CONSUMER_VERIFICATION_PLAN.md, D3.
 *
 * Both functions are gated on the `fixtures` feature of the core crate
 * (src/ffi/blob.rs); the production library does not export them.
 */

#ifndef NET_TEST_HELPERS_H
#define NET_TEST_HELPERS_H

#include <stddef.h>
#include <stdint.h>

#include "net_transport.h"

#ifdef __cplusplus
extern "C" {
#endif

/* Make one data shard of a Reed-Solomon tree blob unavailable: data chunk
 * `data_index` of stripe `stripe_index` in the first erasure leaf, deleted
 * through the adapter's own deletion path. Writes the chunk's 32-byte hash
 * to `out_hash`. Returns 0; NET_ERR_BLOB_INVALID_ARGUMENT for a non-tree
 * ref, a non-erasure tree or an index out of range; NET_ERR_BLOB_BACKEND if
 * the chunk still fetches after deletion. */
int net_mesh_blob_adapter_test_drop_data_chunk(
    const net_mesh_blob_adapter_t* handle,
    const uint8_t* blob_ref_bytes,
    size_t blob_ref_len,
    uint32_t stripe_index,
    uint32_t data_index,
    uint8_t* out_hash);

/* 1 if the chunk with this 32-byte hash fetches from the adapter, 0 if
 * not, negative on error. */
int net_mesh_blob_adapter_test_chunk_present(
    const net_mesh_blob_adapter_t* handle,
    const uint8_t* hash);

#ifdef __cplusplus
}
#endif

#endif /* NET_TEST_HELPERS_H */
