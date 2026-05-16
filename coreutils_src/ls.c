/* ls(1) — list directory contents.  POSIX subset:
 *
 *   ls            list current directory
 *   ls DIR        list DIR
 *   ls -1         one entry per line (default)
 *   ls -a         include entries starting with '.'  (we keep "." quiet
 *                 because our VFS doesn't synthesise "."/"..", but the
 *                 flag is accepted for compatibility)
 *
 * Uses getdents64 + the linux_dirent64 layout.  Exercises the full
 * userspace → kernel → VFS path for directory enumeration, which is
 * what enables `ls /bin` to show the coreutils we installed.
 */
#include "libc.h"

static int show_all = 0;

static int do_ls(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        perror(path);
        return 1;
    }
    char buf[4096];
    long n = getdents64(fd, buf, sizeof(buf));
    close(fd);
    if (n < 0) {
        perror(path);
        return 1;
    }
    if (n == 0) {
        /* Empty directory — nothing to print. */
        return 0;
    }
    long off = 0;
    while (off < n) {
        struct linux_dirent64 *d = (struct linux_dirent64 *)(buf + off);
        if (d->d_reclen == 0) break;
        const char *name = d->d_name;
        if (!show_all && name[0] == '.') {
            off += d->d_reclen;
            continue;
        }
        fputs(name, stdout);
        if (d->d_type == DT_DIR) fputc('/', stdout);
        fputc('\n', stdout);
        off += d->d_reclen;
    }
    return 0;
}

int main(int argc, char **argv) {
    int idx = 1;
    while (idx < argc && argv[idx][0] == '-' && argv[idx][1]) {
        const char *p = argv[idx] + 1;
        for (; *p; p++) {
            switch (*p) {
                case 'a': show_all = 1; break;
                case '1': /* default; accept */ break;
                default:
                    fprintf(stderr, "ls: unknown flag '%c'\n", *p);
                    return 1;
            }
        }
        idx++;
    }
    if (idx == argc) return do_ls(".");
    int rc = 0;
    int multi = (argc - idx > 1);
    for (int i = idx; i < argc; i++) {
        if (multi) printf("%s:\n", argv[i]);
        if (do_ls(argv[i]) != 0) rc = 1;
        if (multi && i + 1 < argc) putchar('\n');
    }
    return rc;
}
