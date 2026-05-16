/* rm(1) — remove files (and directories with -d / -r).  POSIX subset.
 *
 *   rm FILE...      unlink each FILE
 *   rm -f FILE...   never prompt; suppress missing-file errors
 *   rm -d DIR...    rmdir empty directories
 *   rm -r DIR...    recursive removal — out of scope, we just call
 *                   rmdir (succeeds if empty) and report otherwise
 */
#include "libc.h"

int main(int argc, char **argv) {
    int force = 0, dir_flag = 0;
    int idx = 1;
    while (idx < argc && argv[idx][0] == '-' && argv[idx][1]) {
        const char *p = argv[idx] + 1;
        for (; *p; p++) {
            switch (*p) {
                case 'f': force = 1; break;
                case 'd': case 'r': case 'R': dir_flag = 1; break;
                default:
                    fprintf(stderr, "rm: unknown flag '%c'\n", *p);
                    return 1;
            }
        }
        idx++;
    }
    if (idx == argc) {
        if (force) return 0;
        fprintf(stderr, "rm: missing operand\n");
        return 1;
    }
    int rc = 0;
    for (int i = idx; i < argc; i++) {
        int r = unlink(argv[i]);
        if (r != 0 && dir_flag) {
            /* Maybe it's a directory — try rmdir. */
            errno = 0;
            r = rmdir(argv[i]);
        }
        if (r != 0) {
            if (force && errno == ENOENT) continue;
            perror(argv[i]);
            rc = 1;
        }
    }
    return rc;
}
