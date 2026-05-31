/*
 * Net C SDK — Transport Example (blob + directory transfer)
 *
 * Walks the transport surface: install the transfer engine on a node,
 * store a local directory tree as content-addressed blobs (holder
 * side), then — from a peer that has connected to the holder — read the
 * directory manifest and reconstruct the whole tree (fetcher side).
 *
 * This file focuses on the TRANSPORT calls (net_serve_blob_transfer,
 * net_store_dir, net_fetch_dir, net_dir_manifest_read, net_fetch_blob).
 * The node bring-up + peer connection (net_mesh_new, net_mesh_start,
 * net_mesh_connect / net_mesh_accept) and the blob adapter + redex
 * construction (net_redex_*, net_mesh_blob_adapter_new) follow the
 * patterns in examples/mesh_demo.rs and the net.go.h declarations; they
 * are shown here in outline so the transport flow is self-contained.
 *
 * Build:
 *   cargo build --release -p net-mesh \
 *     --features net,dataforts,netdb,redex-disk
 *   gcc -o transport transport.c -L ../target/release -lnet_mesh \
 *       -lpthread -ldl -lm
 *
 * Run:
 *   LD_LIBRARY_PATH=../target/release ./transport     (Linux)
 *   DYLD_LIBRARY_PATH=../target/release ./transport   (macOS)
 */

#include "../include/net_transport.h"
#include "../include/net.go.h" /* net_meshnode_t, net_mesh_blob_adapter_t, net_redex_*, net_free_string */

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* Holder side: install the engine, store a directory, hand back the
 * encoded manifest BlobRef (caller frees via net_transport_free_buffer). */
static int holder_publish_dir(net_meshnode_t* node,
                              net_mesh_blob_adapter_t* adapter,
                              const char* root_path,
                              uint8_t** out_ref,
                              size_t* out_ref_len) {
    /* The engine must be installed before the node can serve (or fetch). */
    int rc = net_serve_blob_transfer(node, adapter);
    if (rc != NET_TRANSPORT_OK) {
        fprintf(stderr, "serve_blob_transfer failed: %d\n", rc);
        return rc;
    }

    rc = net_store_dir(adapter, root_path, out_ref, out_ref_len);
    if (rc != NET_TRANSPORT_OK) {
        fprintf(stderr, "store_dir(%s) failed: %d\n", root_path, rc);
        return rc;
    }
    printf("stored %s -> manifest ref is %zu bytes\n", root_path, *out_ref_len);
    return NET_TRANSPORT_OK;
}

/* Fetcher side: `source_id` is a connected holder peer (its node id from
 * net_mesh_node_id, learned out of band). Inspect the manifest, then
 * reconstruct the tree under `dest_path`. */
static int fetcher_pull_dir(net_meshnode_t* node,
                            net_mesh_blob_adapter_t* adapter,
                            uint64_t source_id,
                            const uint8_t* manifest_ref,
                            size_t manifest_ref_len,
                            const char* dest_path) {
    /* The fetching node ALSO needs the engine installed (it registers
     * the pending transfer locally). */
    int rc = net_serve_blob_transfer(node, adapter);
    if (rc != NET_TRANSPORT_OK) {
        fprintf(stderr, "serve_blob_transfer (fetcher) failed: %d\n", rc);
        return rc;
    }

    /* Optional: introspect before reconstructing. */
    char* manifest_json = NULL;
    size_t json_len = 0;
    rc = net_dir_manifest_read(node, source_id, manifest_ref, manifest_ref_len,
                               &manifest_json, &json_len);
    if (rc == NET_TRANSPORT_OK) {
        printf("manifest (%zu bytes JSON): %s\n", json_len, manifest_json);
        net_free_string(manifest_json); /* JSON is a CString, not a byte buffer */
    } else {
        fprintf(stderr, "manifest_read failed: %d\n", rc);
        /* non-fatal for the demo: fall through to the reconstruct */
    }

    uint64_t files = 0, bytes = 0;
    rc = net_fetch_dir(node, source_id, manifest_ref, manifest_ref_len,
                       dest_path, &files, &bytes);
    if (rc != NET_TRANSPORT_OK) {
        fprintf(stderr, "fetch_dir failed: %d\n", rc);
        return rc;
    }
    printf("reconstructed %" PRIu64 " files (%" PRIu64 " bytes) under %s\n",
           files, bytes, dest_path);
    return NET_TRANSPORT_OK;
}

int main(void) {
    /*
     * Bring-up outline (see net.go.h + mesh_demo.rs for the full forms):
     *
     *   net_meshnode_t* holder; net_mesh_new(holder_cfg_json, &holder);
     *   net_meshnode_t* fetcher; net_mesh_new(fetcher_cfg_json, &fetcher);
     *   net_mesh_start(holder); net_mesh_start(fetcher);
     *   // connect fetcher -> holder (net_mesh_connect / accept handshake)
     *   uint64_t holder_id = net_mesh_node_id(holder);
     *
     *   net_redex_t* redex_h = net_redex_new(...);  // per-node store
     *   net_mesh_blob_adapter_t* adapter_h =
     *       net_mesh_blob_adapter_new(redex_h, "blobs", 0, "");
     *   // ...same for the fetcher node...
     *
     * The two halves below are the transport surface this example
     * demonstrates; wire them onto the handles above.
     */
    fprintf(stderr,
            "transport.c is an API walkthrough; wire holder_publish_dir() "
            "and fetcher_pull_dir() onto live nodes per the bring-up "
            "outline in main(). See net.go.h + mesh_demo.rs.\n");

    /* Suppress unused-function warnings in the walkthrough build. */
    (void)holder_publish_dir;
    (void)fetcher_pull_dir;
    return 0;
}
