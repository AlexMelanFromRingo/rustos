/* echo(1) — print arguments separated by spaces, then newline.
 *
 * Supports -n (suppress trailing newline) per POSIX/coreutils.  Does NOT
 * interpret escape sequences (the GNU `-e` flag) — keeps the binary tiny
 * and is what `printf` is for. */
#include "syscalls.h"

int main(int argc, char **argv) {
    int newline = 1;
    int start = 1;
    if (argc >= 2 && argv[1][0] == '-' && argv[1][1] == 'n' && argv[1][2] == 0) {
        newline = 0;
        start = 2;
    }
    for (int i = start; i < argc; i++) {
        size_t n = my_strlen(argv[i]);
        sys_write(STDOUT, argv[i], n);
        if (i + 1 < argc) sys_write(STDOUT, " ", 1);
    }
    if (newline) sys_write(STDOUT, "\n", 1);
    return 0;
}
