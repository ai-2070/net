/*
 * smoke.c — the smallest consumer of the C SDK bundle (C0).
 *
 * Proves only that a program built against the bundle's headers, by name,
 * links against the bundle's library, loads the staged library and no other,
 * and can call into it. Every consumer program starts with the same
 * loaded-library identity check.
 *
 * Built and run by .github/scripts/run-c-consumers.py; not meant to be
 * compiled by hand against the source tree.
 */

#include <stdio.h>
#include <string.h>

#include "net.h"

#include "loaded_module.h"

static int checks = 0;

#define CHECK(name, cond)                                   \
    do {                                                    \
        if (!(cond)) {                                      \
            printf("FAIL %s\n", name);                      \
            return 1;                                       \
        }                                                   \
        printf("ok %s\n", name);                            \
        checks++;                                           \
    } while (0)

int main(void) {
    static const clm_fn_t used[] = {
        CLM_FN(net_version),
    };
    const char* version;

    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }

    version = net_version();
    CHECK("net_version returns a string", version != NULL);
    CHECK("net_version is non-empty", strlen(version) > 0);
    printf("net_version: %s\n", version);

    printf("NET-CHECKS: %d\n", checks);
    return 0;
}
