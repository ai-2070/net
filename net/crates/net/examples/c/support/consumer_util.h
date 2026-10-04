/*
 * consumer_util.h — what the C consumer programs share: named checks, the
 * few portable OS services they need (a free loopback port, a thread, a
 * sleep, files), and bringing up two in-process mesh nodes.
 *
 * Part of the C consumer runner, not a Net API (hence the `cu_` prefix).
 * Builds unchanged with GCC, MSVC and MinGW; on Windows the runner links
 * ws2_32.
 *
 * Checks print `ok <name>` on success. The first failure prints
 * `FAIL <name>: <detail>` and returns 1 from the function using the macro,
 * so a program stops at its first broken assumption. `cu_finish` prints
 * `NET-CHECKS: <n>`, which the runner requires and which CI holds to a
 * floor per program.
 */

#ifndef CU_CONSUMER_UTIL_H
#define CU_CONSUMER_UTIL_H

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#include "net.go.h"

extern int cu_checks;

#define CU_CHECK(name, cond)                                                  \
    do {                                                                      \
        if (!(cond)) {                                                        \
            printf("FAIL %s: `%s` is false (%s:%d)\n", name, #cond, __FILE__, \
                   __LINE__);                                                 \
            fflush(stdout);                                                   \
            return 1;                                                         \
        }                                                                     \
        printf("ok %s\n", name);                                              \
        cu_checks++;                                                          \
    } while (0)

#define CU_CHECK_RC(name, got, want)                                         \
    do {                                                                     \
        long long cu_got_ = (long long)(got), cu_want_ = (long long)(want);  \
        if (cu_got_ != cu_want_) {                                           \
            printf("FAIL %s: got %lld, want %lld (%s:%d)\n", name, cu_got_,  \
                   cu_want_, __FILE__, __LINE__);                            \
            fflush(stdout);                                                  \
            return 1;                                                        \
        }                                                                    \
        printf("ok %s\n", name);                                             \
        cu_checks++;                                                         \
    } while (0)

/* Print `NET-CHECKS: <n>` and return 0. */
int cu_finish(void);

/* ---- OS services ---- */

/* Once per process, before any socket use (WSAStartup on Windows). */
int cu_net_init(void);

/* A free loopback UDP port as "127.0.0.1:<port>". 0 on success. */
int cu_reserve_port(char* out, size_t out_len);

typedef struct cu_thread cu_thread;
/* Run fn(arg) on a new thread; NULL on failure. */
cu_thread* cu_thread_start(void (*fn)(void*), void* arg);
void cu_thread_join(cu_thread* t);

void cu_sleep_ms(unsigned ms);

/* This process's id, for unique scratch names. */
unsigned long cu_pid(void);

/* mkdir; 0 if created or already there. */
int cu_mkdir(const char* path);
/* Write `len` bytes to `path`. 0 on success. */
int cu_write_file(const char* path, const void* data, size_t len);
/* The whole file, malloc'd (free it); NULL if unreadable. */
unsigned char* cu_read_file(const char* path, size_t* out_len);
/* 1 if `path` exists. */
int cu_exists(const char* path);

/* ---- Mesh ---- */

/* The PSK every consumer node shares (64 hex characters). */
extern const char* CU_PSK_HEX;

/* Build a node on a freshly reserved loopback port, with an identity seed
 * of 32 copies of `seed_byte`. Retries a few times, since the reserved
 * port can be taken between reservation and bind. Writes the address to
 * `addr`. Uses net_mesh_new. 0 on success. */
int cu_mesh_build(unsigned char seed_byte, net_meshnode_t** out, char* addr, size_t addr_len);

/* Handshake `initiator` to `responder` (listening at `responder_addr`):
 * accept on a thread, connect from this one. Uses net_mesh_public_key_hex,
 * net_mesh_node_id, net_mesh_accept, net_mesh_connect and net_free_string.
 * 0 when both sides succeed. */
int cu_mesh_handshake(net_meshnode_t* responder, net_meshnode_t* initiator,
                      const char* responder_addr);

/* The Net functions this file calls, for a program's loaded-module list. */
#define CU_MESH_FNS                                                          \
    CLM_FN(net_mesh_new), CLM_FN(net_mesh_public_key_hex),                   \
        CLM_FN(net_mesh_node_id), CLM_FN(net_mesh_accept),                   \
        CLM_FN(net_mesh_connect), CLM_FN(net_free_string)

#endif /* CU_CONSUMER_UTIL_H */
