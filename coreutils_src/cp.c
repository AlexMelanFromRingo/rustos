/* cp(1) — copy a single file.  POSIX subset.
 *
 *   cp SRC DST     — copy SRC to DST, overwriting if DST exists.
 *
 * No -r yet (would need an in-tree readdir/recurse).  Reads SRC into
 * a 4 KiB buffer in a loop, writes to DST opened O_WRONLY|O_CREAT|O_TRUNC.
 */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "Usage: cp SRC DST\n");
        return 1;
    }
    int sfd = open(argv[1], O_RDONLY);
    if (sfd < 0) { perror(argv[1]); return 1; }
    int dfd = open(argv[2], O_WRONLY | O_CREAT | O_TRUNC);
    if (dfd < 0) { perror(argv[2]); close(sfd); return 1; }
    char buf[4096];
    for (;;) {
        ssize_t n = read(sfd, buf, sizeof(buf));
        if (n == 0) break;
        if (n < 0) { perror(argv[1]); close(sfd); close(dfd); return 1; }
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = write(dfd, buf + off, (size_t)(n - off));
            if (w <= 0) { perror(argv[2]); close(sfd); close(dfd); return 1; }
            off += w;
        }
    }
    close(sfd);
    close(dfd);
    return 0;
}
