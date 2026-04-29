/* cat(1) — concatenate files and print to stdout.
 *
 * If no files given (or "-"), read from stdin.  Per POSIX: stop at the
 * first read error and propagate the exit code, but continue with the
 * next argument so partial output is preserved. */
#include "syscalls.h"

static int copy_fd(int fd) {
    char buf[4096];
    for (;;) {
        ssize_t n = sys_read(fd, buf, sizeof(buf));
        if (n == 0) return 0;
        if (n < 0)  return 1;
        ssize_t off = 0;
        while (off < n) {
            ssize_t w = sys_write(STDOUT, buf + off, (size_t)(n - off));
            if (w <= 0) return 1;
            off += w;
        }
    }
}

int main(int argc, char **argv) {
    if (argc < 2) return copy_fd(STDIN);
    int rc = 0;
    for (int i = 1; i < argc; i++) {
        int fd;
        if (argv[i][0] == '-' && argv[i][1] == 0) {
            fd = STDIN;
        } else {
            fd = sys_open(argv[i], O_RDONLY);
            if (fd < 0) {
                const char *err = "cat: cannot open file\n";
                sys_write(STDERR, err, my_strlen(err));
                rc = 1;
                continue;
            }
        }
        if (copy_fd(fd) != 0) rc = 1;
        if (fd != STDIN) sys_close(fd);
    }
    return rc;
}
