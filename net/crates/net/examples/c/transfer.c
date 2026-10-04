/*
 * transfer.c — blob and directory transfer between two nodes, from C
 * (C SDK plan, C2).
 *
 * A holder node publishes content into its blob adapter; a reader node
 * fetches it by address (from the known holder, then by discovery) and
 * fetches a whole directory tree, compared byte for byte. Then the refusals
 * the transfer surface documents, each checked for its code:
 *
 *   - fetching before net_serve_blob_transfer installed the engine;
 *   - an address nobody holds, from the holder and by discovery;
 *   - a directory manifest whose entry escapes the destination root, built
 *     here as the postcard bytes of a DirManifest (version 1, one Dir entry
 *     "../escape") and published as an ordinary blob — the fetch must refuse
 *     it and must not create anything outside the destination;
 *   - a manifest ref naming content the source does not have;
 *   - two "empty" manifests that are not the same thing: a zero-length ref
 *     (refused), and a valid manifest of an empty directory (accepted, no
 *     entries).
 *
 * Codes and output contracts are net_transport.h's unless a check cites
 * src/ffi/transport.rs. Built and run by .github/scripts/run-c-consumers.py.
 */

#include <stdlib.h>
#include <string.h>

#include "net.go.h"
#include "net_cortex.h"
#include "net_transport.h"

#include "consumer_util.h"
#include "loaded_module.h"

#define BIG_LEN 5000

static int same_file(const char* path, const unsigned char* want, size_t want_len) {
    size_t len = 0;
    unsigned char* got = cu_read_file(path, &len);
    int same = got != NULL && len == want_len && (len == 0 || memcmp(got, want, len) == 0);
    free(got);
    return same;
}

static int run(void) {
    net_meshnode_t *holder = NULL, *reader = NULL;
    char holder_addr[32], reader_addr[32];
    net_redex_t *redex_h, *redex_r;
    net_mesh_blob_adapter_t *store_h, *store_r;
    uint64_t holder_id;
    unsigned char payload[4096], big[BIG_LEN], hash[32], ghost[32];
    uint8_t *ref = NULL, *out = NULL, *manifest = NULL, *empty_manifest = NULL, *foreign = NULL;
    size_t ref_len = 0, out_len = 0, manifest_len = 0, empty_manifest_len = 0, foreign_len = 0;
    char *json = NULL;
    size_t json_len = 0;
    uint64_t files = 99, bytes = 99;
    char root[64], path[160];
    size_t i;

    for (i = 0; i < sizeof payload; i++) {
        payload[i] = (unsigned char)(i * 7 + 3);
    }
    for (i = 0; i < sizeof big; i++) {
        big[i] = (unsigned char)(i % 251);
    }
    memset(ghost, 0xEE, sizeof ghost);
    snprintf(root, sizeof root, "run-%lu", cu_pid());
    CU_CHECK("scratch directory", cu_mkdir(root) == 0);

    /* ---- two nodes, two blob stores ---- */

    CU_CHECK_RC("bring-up: holder", cu_mesh_build(0xC1, &holder, holder_addr, sizeof holder_addr), 0);
    CU_CHECK_RC("bring-up: reader", cu_mesh_build(0xC2, &reader, reader_addr, sizeof reader_addr), 0);
    CU_CHECK_RC("bring-up: handshake", cu_mesh_handshake(holder, reader, holder_addr), 0);
    CU_CHECK_RC("bring-up: start holder", net_mesh_start(holder), 0);
    CU_CHECK_RC("bring-up: start reader", net_mesh_start(reader), 0);
    holder_id = net_mesh_node_id(holder);
    redex_h = net_redex_new(NULL);
    redex_r = net_redex_new(NULL);
    store_h = net_mesh_blob_adapter_new(redex_h, "objects", 0, NULL);
    store_r = net_mesh_blob_adapter_new(redex_r, "objects", 0, NULL);
    CU_CHECK("bring-up: two in-memory blob adapters",
             redex_h && redex_r && store_h && store_r);

    /* ---- before the engine is installed ---- */

    CU_CHECK_RC("net_fetch_blob before net_serve_blob_transfer is ENGINE_NOT_INSTALLED",
                net_fetch_blob(reader, holder_id, ghost, &out, &out_len),
                NET_ERR_TRANSFER_ENGINE_NOT_INSTALLED);
    CU_CHECK_RC("net_serve_blob_transfer: holder", net_serve_blob_transfer(holder, store_h), NET_TRANSPORT_OK);
    CU_CHECK_RC("net_serve_blob_transfer: reader", net_serve_blob_transfer(reader, store_r), NET_TRANSPORT_OK);
    CU_CHECK_RC("net_serve_blob_transfer: NULL node is NULL_POINTER",
                net_serve_blob_transfer(NULL, store_h), NET_ERR_TRANSFER_NULL_POINTER);

    /* ---- one blob, two ways ---- */

    CU_CHECK_RC("publish 4 KiB on the holder",
                net_mesh_blob_adapter_publish(store_h, (const uint8_t*)"mesh:c2/payload", 15, payload,
                                              sizeof payload, &ref, &ref_len),
                0);
    CU_CHECK_RC("net_blob_ref_hash", net_blob_ref_hash(ref, ref_len, hash), 0);
    CU_CHECK_RC("net_fetch_blob from the holder", net_fetch_blob(reader, holder_id, hash, &out, &out_len),
                NET_TRANSPORT_OK);
    CU_CHECK("net_fetch_blob: the same bytes",
             out != NULL && out_len == sizeof payload && memcmp(out, payload, out_len) == 0);
    net_transport_free_buffer(out, out_len);
    out = NULL;
    out_len = 0;
    CU_CHECK_RC("net_fetch_blob_discovered", net_fetch_blob_discovered(reader, hash, &out, &out_len),
                NET_TRANSPORT_OK);
    CU_CHECK("net_fetch_blob_discovered: the same bytes",
             out != NULL && out_len == sizeof payload && memcmp(out, payload, out_len) == 0);
    net_transport_free_buffer(out, out_len);
    out = NULL;
    out_len = 0;

    /* ---- an address nobody holds ---- */

    CU_CHECK_RC("net_fetch_blob: an unknown address is NOT_FOUND",
                net_fetch_blob(reader, holder_id, ghost, &out, &out_len), NET_ERR_TRANSFER_NOT_FOUND);
    CU_CHECK_RC("net_fetch_blob_discovered: an unknown address is ALL_PEERS_FAILED",
                net_fetch_blob_discovered(reader, ghost, &out, &out_len),
                NET_ERR_TRANSFER_ALL_PEERS_FAILED);
    CU_CHECK_RC("net_fetch_blob: a NULL hash is NULL_POINTER",
                net_fetch_blob(reader, holder_id, NULL, &out, &out_len), NET_ERR_TRANSFER_NULL_POINTER);
    /* net_transport.h: "NULL or zero-length is a no-op". */
    net_transport_free_buffer(NULL, 0);
    CU_SURVIVED("net_transport_free_buffer: NULL is a no-op");

    /* ---- a directory tree, store -> inspect -> fetch ---- */

    snprintf(path, sizeof path, "%s/src", root);
    CU_CHECK("tree: src/", cu_mkdir(path) == 0);
    snprintf(path, sizeof path, "%s/src/sub", root);
    CU_CHECK("tree: src/sub/", cu_mkdir(path) == 0);
    snprintf(path, sizeof path, "%s/src/empty", root);
    CU_CHECK("tree: src/empty/", cu_mkdir(path) == 0);
    snprintf(path, sizeof path, "%s/src/a.txt", root);
    CU_CHECK("tree: src/a.txt", cu_write_file(path, "hello", 5) == 0);
    snprintf(path, sizeof path, "%s/src/sub/b.bin", root);
    CU_CHECK("tree: src/sub/b.bin", cu_write_file(path, big, sizeof big) == 0);

    snprintf(path, sizeof path, "%s/src", root);
    CU_CHECK_RC("net_store_dir", net_store_dir(store_h, path, &manifest, &manifest_len), NET_TRANSPORT_OK);
    CU_CHECK("net_store_dir: a manifest ref", manifest != NULL && manifest_len > 0);
    CU_CHECK_RC("net_dir_manifest_read",
                net_dir_manifest_read(reader, holder_id, manifest, manifest_len, &json, &json_len),
                NET_TRANSPORT_OK);
    CU_CHECK("net_dir_manifest_read: names a.txt, sub/b.bin and empty",
             json != NULL && strstr(json, "\"a.txt\"") && strstr(json, "\"sub/b.bin\"") &&
                 strstr(json, "\"empty\""));
    net_free_string(json);
    json = NULL;

    snprintf(path, sizeof path, "%s/dst", root);
    CU_CHECK_RC("net_fetch_dir",
                net_fetch_dir(reader, holder_id, manifest, manifest_len, path, &files, &bytes),
                NET_TRANSPORT_OK);
    CU_CHECK_RC("net_fetch_dir: two files written", (long long)files, 2);
    CU_CHECK_RC("net_fetch_dir: their total size", (long long)bytes, 5 + BIG_LEN);
    snprintf(path, sizeof path, "%s/dst/a.txt", root);
    CU_CHECK("net_fetch_dir: a.txt byte for byte", same_file(path, (const unsigned char*)"hello", 5));
    snprintf(path, sizeof path, "%s/dst/sub/b.bin", root);
    CU_CHECK("net_fetch_dir: sub/b.bin byte for byte", same_file(path, big, sizeof big));
    snprintf(path, sizeof path, "%s/dst/empty", root);
    CU_CHECK("net_fetch_dir: the empty directory survives", cu_exists(path));

    /* ---- a manifest entry that escapes the destination ---- */
    {
        /* postcard(DirManifest{version: 1, entries: [DirEntry{path: "../escape",
         * kind: Dir{mode: 0}}]}): u8 version, varint entry count, varint path
         * length + bytes, varint variant index (File 0, Dir 1, Symlink 2),
         * varint mode. src/adapter/net/dataforts/dir.rs. */
        static const unsigned char evil[] = {1,   1,   9,   '.', '.', '/', 'e', 's',
                                             'c', 'a', 'p', 'e', 1,   0};
        uint8_t* evil_ref = NULL;
        size_t evil_ref_len = 0;
        CU_CHECK_RC("publish a hand-built manifest with a \"../escape\" entry",
                    net_mesh_blob_adapter_publish(store_h, (const uint8_t*)"mesh:c2/evil", 12, evil,
                                                  sizeof evil, &evil_ref, &evil_ref_len),
                    0);
        snprintf(path, sizeof path, "%s/evil-dst", root);
        files = bytes = 99;
        CU_CHECK_RC("net_fetch_dir: an escaping entry is DIR_PATH_INVALID",
                    net_fetch_dir(reader, holder_id, evil_ref, evil_ref_len, path, &files, &bytes),
                    NET_ERR_DIR_PATH_INVALID);
        CU_CHECK("net_fetch_dir: counters read 0 after a refusal", files == 0 && bytes == 0);
        snprintf(path, sizeof path, "%s/escape", root);
        CU_CHECK("net_fetch_dir: nothing was created outside the destination", !cu_exists(path));
        net_blob_free_buffer(evil_ref, evil_ref_len);
    }

    /* ---- a manifest the source does not have ---- */

    snprintf(path, sizeof path, "%s/sub-only", root);
    CU_CHECK("tree: a tree only the reader stores", cu_mkdir(path) == 0);
    snprintf(path, sizeof path, "%s/sub-only/only.txt", root);
    CU_CHECK("tree: sub-only/only.txt", cu_write_file(path, "only here", 9) == 0);
    snprintf(path, sizeof path, "%s/sub-only", root);
    CU_CHECK_RC("net_store_dir on the reader", net_store_dir(store_r, path, &foreign, &foreign_len),
                NET_TRANSPORT_OK);
    json = (char*)"untouched";
    json_len = 7;
    CU_CHECK_RC("net_dir_manifest_read: a manifest the holder lacks is NOT_FOUND",
                net_dir_manifest_read(reader, holder_id, foreign, foreign_len, &json, &json_len),
                NET_ERR_TRANSFER_NOT_FOUND);
    CU_CHECK("net_dir_manifest_read: (NULL, 0) after a refusal", json == NULL && json_len == 0);
    snprintf(path, sizeof path, "%s/foreign-dst", root);
    CU_CHECK_RC("net_fetch_dir: a manifest the holder lacks is NOT_FOUND",
                net_fetch_dir(reader, holder_id, foreign, foreign_len, path, &files, &bytes),
                NET_ERR_TRANSFER_NOT_FOUND);

    /* ---- two empty manifests ---- */

    snprintf(path, sizeof path, "%s/zero-dst", root);
    /* src/ffi/transport.rs read_blob_ref: an empty buffer decodes to no ref. */
    CU_CHECK_RC("net_fetch_dir: a zero-length manifest ref is INVALID_ARGUMENT",
                net_fetch_dir(reader, holder_id, manifest, 0, path, &files, &bytes),
                NET_ERR_TRANSFER_INVALID_ARGUMENT);
    snprintf(path, sizeof path, "%s/empty-src", root);
    CU_CHECK("tree: an empty directory", cu_mkdir(path) == 0);
    CU_CHECK_RC("net_store_dir: an empty directory",
                net_store_dir(store_h, path, &empty_manifest, &empty_manifest_len), NET_TRANSPORT_OK);
    CU_CHECK_RC("net_dir_manifest_read: the empty directory's manifest",
                net_dir_manifest_read(reader, holder_id, empty_manifest, empty_manifest_len, &json,
                                      &json_len),
                NET_TRANSPORT_OK);
    CU_CHECK("net_dir_manifest_read: it has no entries",
             json != NULL && strstr(json, "\"entries\":[]") != NULL);
    net_free_string(json);
    json = NULL;
    snprintf(path, sizeof path, "%s/empty-dst", root);
    CU_CHECK_RC("net_fetch_dir: the empty directory's manifest",
                net_fetch_dir(reader, holder_id, empty_manifest, empty_manifest_len, path, &files,
                              &bytes),
                NET_TRANSPORT_OK);
    CU_CHECK("net_fetch_dir: no files, no bytes", files == 0 && bytes == 0);

    /* ---- teardown ---- */

    net_transport_free_buffer(manifest, manifest_len);
    net_transport_free_buffer(empty_manifest, empty_manifest_len);
    net_transport_free_buffer(foreign, foreign_len);
    net_blob_free_buffer(ref, ref_len);
    net_mesh_blob_adapter_free(store_h);
    net_mesh_blob_adapter_free(store_r);
    net_redex_free(redex_h);
    net_redex_free(redex_r);
    CU_CHECK_RC("net_mesh_shutdown: holder", net_mesh_shutdown(holder), 0);
    CU_CHECK_RC("net_mesh_shutdown: reader", net_mesh_shutdown(reader), 0);
    net_mesh_free(holder);
    net_mesh_free(reader);
    return 0;
}

int main(void) {
    static const clm_fn_t used[] = {
        CU_MESH_FNS,
        CLM_FN(net_mesh_start),
        CLM_FN(net_mesh_shutdown),
        CLM_FN(net_mesh_free),
        CLM_FN(net_redex_new),
        CLM_FN(net_redex_free),
        CLM_FN(net_mesh_blob_adapter_new),
        CLM_FN(net_mesh_blob_adapter_free),
        CLM_FN(net_mesh_blob_adapter_publish),
        CLM_FN(net_blob_ref_hash),
        CLM_FN(net_blob_free_buffer),
        CLM_FN(net_serve_blob_transfer),
        CLM_FN(net_fetch_blob),
        CLM_FN(net_fetch_blob_discovered),
        CLM_FN(net_store_dir),
        CLM_FN(net_fetch_dir),
        CLM_FN(net_dir_manifest_read),
        CLM_FN(net_transport_free_buffer),
    };
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    if (cu_net_init() != 0) {
        printf("FAIL socket layer init\n");
        return 1;
    }
    if (run() != 0) {
        return 1;
    }
    return cu_finish();
}
