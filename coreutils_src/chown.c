/* chown(1) — change file owner / group.
 *
 *   chown UID FILE...        only UID
 *   chown UID:GID FILE...    UID and GID
 *   chown :GID FILE...       only GID (UID stays via 0xFFFFFFFF marker)
 *
 * UIDs/GIDs are numeric — no name lookup (we don't have /etc/passwd-
 * driven name resolution in the C side).
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "Usage: chown UID[:GID] FILE...\n");
        return 1;
    }
    unsigned uid = 0xFFFFFFFFu, gid = 0xFFFFFFFFu;
    char *colon = strchr(argv[1], ':');
    if (colon) {
        *colon = 0;
        if (argv[1][0]) uid = (unsigned)atol(argv[1]);
        if (colon[1])   gid = (unsigned)atol(colon + 1);
    } else {
        uid = (unsigned)atol(argv[1]);
    }
    int rc = 0;
    for (int i = 2; i < argc; i++) {
        if (chown(argv[i], uid, gid) != 0) {
            perror(argv[i]);
            rc = 1;
        }
    }
    return rc;
}
