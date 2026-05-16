/* chmod(1) — change file mode bits.
 *
 *   chmod MODE FILE...
 *
 * MODE is an octal number (e.g., 0755, 644) — we parse it as base-8
 * via strtol when it starts with '0', else base-10.  Symbolic forms
 * (u+x, g-w) are out of scope.
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc < 3) {
        fprintf(stderr, "Usage: chmod MODE FILE...\n");
        return 1;
    }
    /* Parse mode in base 8 if leading 0, else base 10. */
    long mode = strtol(argv[1], NULL, argv[1][0] == '0' ? 8 : 10);
    if (mode < 0 || mode > 07777) {
        fprintf(stderr, "chmod: invalid mode '%s'\n", argv[1]);
        return 1;
    }
    int rc = 0;
    for (int i = 2; i < argc; i++) {
        if (chmod(argv[i], (unsigned)mode) != 0) {
            perror(argv[i]);
            rc = 1;
        }
    }
    return rc;
}
