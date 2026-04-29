/* pwd(1) — print working directory.  POSIX: 4096-byte path max. */
#include "syscalls.h"

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    char buf[4096];
    long n = sys_getcwd(buf, sizeof(buf));
    if (n < 0) {
        const char *err = "pwd: getcwd failed\n";
        sys_write(STDERR, err, my_strlen(err));
        return 1;
    }
    /* getcwd returns the length including a trailing NUL when it
     * succeeds (Linux convention).  Strip the NUL so we don't print
     * it, then add a newline. */
    size_t len = (size_t)n;
    if (len > 0 && buf[len - 1] == 0) len--;
    sys_write(STDOUT, buf, len);
    sys_write(STDOUT, "\n", 1);
    return 0;
}
