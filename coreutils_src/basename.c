/* basename(1) — strip directory + optional suffix from PATH.
 *
 *   basename PATH [SUFFIX]
 *
 * Examples:
 *   basename /etc/passwd       -> passwd
 *   basename src/main.c .c     -> main
 *   basename /                 -> /
 *   basename ""                -> ""
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc < 2 || argc > 3) {
        fprintf(stderr, "Usage: basename PATH [SUFFIX]\n");
        return 1;
    }
    char *p = argv[1];
    if (!p[0]) { putchar('\n'); return 0; }
    /* Strip trailing slashes (POSIX, except when whole string is "/"). */
    size_t len = strlen(p);
    while (len > 1 && p[len - 1] == '/') p[--len] = 0;
    /* Find last '/'. */
    char *base = strrchr(p, '/');
    base = base ? base + 1 : p;
    /* Optional suffix removal — must not equal the whole basename. */
    if (argc == 3) {
        size_t bl = strlen(base);
        size_t sl = strlen(argv[2]);
        if (sl < bl && strcmp(base + bl - sl, argv[2]) == 0) {
            base[bl - sl] = 0;
        }
    }
    fputs(base, stdout);
    putchar('\n');
    return 0;
}
