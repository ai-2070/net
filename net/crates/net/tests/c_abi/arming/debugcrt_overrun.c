/*
 * Arming negative for the MSVC /MDd checker lane (C SDK plan, C5): a write
 * past the end of one of this program's own allocations, which the debug
 * CRT's guard bytes catch when the block is freed. Run only by
 * run-c-consumers.py --debug-crt --arming, which requires it to be caught.
 *
 * NET-LANE: debug-crt
 * NET-EXPECT: HEAP CORRUPTION DETECTED
 */

#include <stdlib.h>
#include <string.h>

#include "net.h"

#include "loaded_module.h"

int main(void) {
    static const clm_fn_t used[] = {CLM_FN(net_version)};
    volatile size_t len = 64;
    char* block;
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    block = (char*)malloc(len);
    if (block == NULL || net_version() == NULL) {
        return 1;
    }
    memset(block, 0x11, len + 4); /* the defect: four bytes past the end */
    free(block);
    return 1;
}
