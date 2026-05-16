/* mv(1) — rename / move a file.  POSIX `mv SRC DST`.
 *
 * Pure rename(2).  Cross-filesystem moves (where rename returns EXDEV)
 * would fall back to cp+rm, but our kernel doesn't yet distinguish
 * filesystems for rename, so we don't bother.
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "Usage: mv SRC DST\n");
        return 1;
    }
    if (rename(argv[1], argv[2]) != 0) {
        perror(argv[1]);
        return 1;
    }
    return 0;
}
