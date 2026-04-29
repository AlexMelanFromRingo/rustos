/* hostname(1) — print the system hostname read from /etc/hostname. */
#include "syscalls.h"

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    int fd = sys_open("/etc/hostname", O_RDONLY);
    if (fd < 0) {
        /* Fall back to a hardcoded name — keeps `hostname` working
         * even if the file doesn't exist yet (very early boot). */
        const char *fallback = "rustos\n";
        sys_write(STDOUT, fallback, my_strlen(fallback));
        return 0;
    }
    char buf[256];
    ssize_t n = sys_read(fd, buf, sizeof(buf));
    sys_close(fd);
    if (n <= 0) return 1;
    /* Trim trailing newline if present, then re-emit our own. */
    size_t len = (size_t)n;
    if (buf[len - 1] == '\n') len--;
    sys_write(STDOUT, buf, len);
    sys_write(STDOUT, "\n", 1);
    return 0;
}
