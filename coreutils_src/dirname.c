/* dirname(1) — strip last path component from PATH.
 *
 * Examples:
 *   dirname /etc/passwd     -> /etc
 *   dirname src/main.c      -> src
 *   dirname single          -> .
 *   dirname /single          -> /
 *   dirname ""              -> .
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "Usage: dirname PATH\n");
        return 1;
    }
    char *p = argv[1];
    if (!p[0]) { puts("."); return 0; }
    size_t len = strlen(p);
    /* Strip trailing slashes (POSIX). */
    while (len > 1 && p[len - 1] == '/') p[--len] = 0;
    char *base = strrchr(p, '/');
    if (!base) { puts("."); return 0; }
    if (base == p) { puts("/"); return 0; }
    /* Strip trailing slashes between the dir and base, then NUL the
     * first slash that separates dir from base. */
    while (base > p && base[-1] == '/') base--;
    *base = 0;
    puts(p);
    return 0;
}
