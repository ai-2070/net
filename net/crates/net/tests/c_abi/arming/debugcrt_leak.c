/*
 * Arming negative for the MSVC /MDd checker lane (C SDK plan, C5): this
 * program leaks a block of its OWN heap. That is the whole of the lane's
 * domain: the debug CRT tracks the consumer's allocations, not the release
 * net.dll's (another heap). Run only by run-c-consumers.py --debug-crt
 * --arming, which requires it to be caught.
 *
 * NET-LANE: debug-crt
 * NET-EXPECT: Detected memory leaks!
 * NET-EXPECT: 777 bytes long
 */

#include <stdlib.h>
#include <string.h>

#include "net.h"

#include "loaded_module.h"

int main(void) {
    static const clm_fn_t used[] = {CLM_FN(net_version)};
    char* lost;
    if (clm_check_loaded_module(used, sizeof used / sizeof used[0]) != 0) {
        return 2;
    }
    lost = (char*)malloc(777);
    if (lost == NULL || net_version() == NULL) {
        return 1;
    }
    memset(lost, 0x2A, 777);
    lost = NULL; /* the defect: never freed */
    return 0;
}
