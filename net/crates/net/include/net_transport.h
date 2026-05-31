/*
 * net_transport.h — C ABI for the Net transport surface:
 *   blob transfer + directory transfer over the fairscheduler stream
 *   transport (Transport SDK plan, T-C).
 *
 * One header, one shared library. These symbols are exported by the
 * same `libnet` cdylib/staticlib as the rest of the C ABI; this header
 * only declares the transport slice. The transfer engine moves
 * content-addressed bytes (and whole directory trees) between peers
 * over reliable, fair-scheduled streams — distinct from RedEX
 * replication (a push primitive) and nRPC (request/reply).
 *
 * # Build
 *   cargo build --release -p net-mesh \
 *     --features net,dataforts,netdb,redex-disk
 *   Artifacts (in target/release): libnet_mesh.so (Linux),
 *   libnet_mesh.dylib (macOS), net_mesh.dll (Windows).
 *
 * # Link
 *   gcc -o transport transport.c -L target/release \
 *     -lnet_mesh -lpthread -ldl -lm
 *
 * # Handle model
 *   Transfer is node-driven, so the fetch/dir functions take a
 *   `net_meshnode_t*` (from net_mesh_new, see net.go.h), and the
 *   store/serve functions take a `net_mesh_blob_adapter_t*` (from
 *   net_mesh_blob_adapter_new). No transport-specific handle type is
 *   introduced. Both handles remain owned by their creators and are
 *   freed by their own `_free` functions; this surface only borrows
 *   them for the duration of a call.
 *
 *   A node MUST install the transfer engine via
 *   net_serve_blob_transfer() before it can serve chunks to peers OR
 *   issue its own fetches. An un-installed node returns
 *   NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED.
 *
 * # Error model
 *   Functions return 0 (NET_TRANSPORT_OK) on success or a negative
 *   NET_ERR_TRANSFER_* / NET_ERR_DIR_* code. The codes occupy a fresh
 *   band (-200..) disjoint from the base (net.h), blob (-110..-120),
 *   and NAT (-130..-137) ranges.
 *
 * # Memory
 *   Byte buffers returned via (out_bytes, out_len) — net_fetch_blob,
 *   net_fetch_blob_discovered, net_store_dir — are owned by the caller
 *   and MUST be freed with net_transport_free_buffer(ptr, len). The
 *   JSON string from net_dir_manifest_read is freed with
 *   net_free_string (see net.go.h). A successful call with no bytes
 *   yields (NULL, 0), which is safe to pass to the free function.
 *
 * # Threading
 *   Do NOT call any transport function from a thread that already
 *   holds a tokio runtime context (the synchronous functions block on
 *   an internal runtime; a runtime-in-runtime aborts). The common
 *   C / Go / Python caller has no Rust runtime, so this is unreachable
 *   for them. Panics crossing the boundary are caught and returned as
 *   NET_ERR_TRANSFER_PANIC.
 */

#ifndef NET_TRANSPORT_H
#define NET_TRANSPORT_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Status / error codes ────────────────────────────────────────── *
 * Kept in sync with the Rust `pub const NET_ERR_*` in
 * src/ffi/transport.rs by tests/transport_error_codes.rs.            */
#define NET_TRANSPORT_OK                       0
#define NET_ERR_TRANSFER_NOT_FOUND          -200  /* holder lacked the content        */
#define NET_ERR_TRANSFER_HASH_MISMATCH      -201  /* bytes did not hash to the address */
#define NET_ERR_TRANSFER_ALL_PEERS_FAILED   -202  /* discovery: no peer served it      */
#define NET_ERR_TRANSFER_CANCELLED          -203  /* fetch cancelled                   */
#define NET_ERR_TRANSFER_NULL_POINTER       -204  /* a required pointer was NULL       */
#define NET_ERR_TRANSFER_SHUTTING_DOWN      -205  /* handle is being freed             */
#define NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED -206 /* call net_serve_blob_transfer 1st */
#define NET_ERR_TRANSFER_BACKEND            -207  /* other substrate transfer failure  */
#define NET_ERR_TRANSFER_PANIC              -208  /* panic caught at the FFI boundary  */
#define NET_ERR_TRANSFER_INVALID_ARGUMENT   -209  /* bad path / oversize length / etc. */
#define NET_ERR_DIR_INVALID_MANIFEST        -210  /* manifest decode / version failure */
#define NET_ERR_DIR_PATH_INVALID            -211  /* manifest entry escaped dest root  */
#define NET_ERR_DIR_IO                      -213  /* filesystem I/O failed on fetch    */

/* ── Shared opaque handles (defined identically in net.go.h) ──────── *
 * Guarded so a translation unit that also includes net.go.h does not
 * hit a conflicting typedef. Both are owned + freed by their creating
 * surface (net_mesh_* / net_mesh_blob_adapter_*).                     */
#ifndef NET_MESHNODE_T_DEFINED
#define NET_MESHNODE_T_DEFINED
typedef struct net_meshnode_s net_meshnode_t;
#endif
#ifndef NET_MESH_BLOB_ADAPTER_T_DEFINED
#define NET_MESH_BLOB_ADAPTER_T_DEFINED
typedef struct net_mesh_blob_adapter_s net_mesh_blob_adapter_t;
#endif

/*
 * Install the blob-transfer engine on `node` over `adapter`. Required
 * before the node can serve chunks to peers OR issue its own fetches.
 * Idempotent (re-installing replaces the engine). Returns
 * NET_TRANSPORT_OK, or NET_ERR_TRANSFER_NULL_POINTER /
 * NET_ERR_TRANSFER_SHUTTING_DOWN.
 */
int net_serve_blob_transfer(const net_meshnode_t* node,
                            const net_mesh_blob_adapter_t* adapter);

/*
 * Fetch the blob addressed by the 32-byte BLAKE3 `hash` from the known
 * holder `holder_id`. On success writes a freshly-allocated buffer to
 * (*out_bytes, *out_len); free with net_transport_free_buffer. `hash`
 * must point to at least 32 readable bytes.
 */
int net_fetch_blob(const net_meshnode_t* node,
                   uint64_t holder_id,
                   const uint8_t* hash,
                   uint8_t** out_bytes,
                   size_t* out_len);

/*
 * Like net_fetch_blob, but discovers the holder among connected peers.
 * Returns NET_ERR_TRANSFER_ALL_PEERS_FAILED if no connected peer has
 * the content.
 */
int net_fetch_blob_discovered(const net_meshnode_t* node,
                              const uint8_t* hash,
                              uint8_t** out_bytes,
                              size_t* out_len);

/*
 * Store the local directory tree at `root_path` as content-addressed
 * blobs in `adapter`, writing the encoded directory-manifest BlobRef to
 * (*out_manifest_ref, *out_len). That buffer is the opaque token a
 * receiver passes to net_fetch_dir / net_dir_manifest_read; free it
 * with net_transport_free_buffer. `root_path` is a UTF-8, NUL-terminated
 * filesystem path.
 */
int net_store_dir(const net_mesh_blob_adapter_t* adapter,
                  const char* root_path,
                  uint8_t** out_manifest_ref,
                  size_t* out_len);

/*
 * Fetch the directory whose encoded manifest BlobRef is
 * (manifest_ref, manifest_ref_len) from `source_id` and reconstruct it
 * under `dest_path` (created if absent). Writes the number of files
 * written to *out_files and total bytes to *out_bytes; either out-param
 * may be NULL to ignore. Manifest paths are validated to stay within
 * `dest_path`.
 */
int net_fetch_dir(const net_meshnode_t* node,
                  uint64_t source_id,
                  const uint8_t* manifest_ref,
                  size_t manifest_ref_len,
                  const char* dest_path,
                  uint64_t* out_files,
                  uint64_t* out_bytes);

/*
 * Fetch + decode the directory manifest (manifest_ref, manifest_ref_len)
 * from `source_id` WITHOUT reconstructing the tree, writing it as a JSON
 * string to (*out_json, *out_len) for introspection (entry paths, kinds,
 * modes, per-file blob refs). Free the string with net_free_string.
 */
int net_dir_manifest_read(const net_meshnode_t* node,
                          uint64_t source_id,
                          const uint8_t* manifest_ref,
                          size_t manifest_ref_len,
                          char** out_json,
                          size_t* out_len);

/*
 * Free a byte buffer returned by net_fetch_blob / net_fetch_blob_discovered
 * / net_store_dir. NULL or zero-length is a no-op. `len` MUST be the
 * length the producing call wrote to *out_len (the deallocation layout
 * is length-sensitive). Do not call twice on the same pointer.
 */
void net_transport_free_buffer(uint8_t* ptr, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* NET_TRANSPORT_H */
