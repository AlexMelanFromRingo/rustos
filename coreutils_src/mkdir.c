/* mkdir(1) — create directories.  POSIX flag `-p` (don't error if it
 * already exists; create parent components — we honour the first half;
 * recursive parent creation is left to a follow-up since we'd need to
 * walk the path component-by-component).
 */
#include "libc.h"

int main(int argc, char **argv) {
    int p_flag = 0;
    int idx = 1;
    while (idx < argc && argv[idx][0] == '-') {
        if (strcmp(argv[idx], "-p") == 0) { p_flag = 1; idx++; }
        else { fprintf(stderr, "mkdir: bad flag '%s'\n", argv[idx]); return 1; }
    }
    if (idx == argc) {
        fprintf(stderr, "mkdir: missing operand\n");
        return 1;
    }
    int rc = 0;
    for (int i = idx; i < argc; i++) {
        if (mkdir(argv[i], 0755) != 0) {
            if (p_flag && errno == EEXIST) continue;
            perror(argv[i]);
            rc = 1;
        }
    }
    return rc;
}
