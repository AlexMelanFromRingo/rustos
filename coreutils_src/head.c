/* head(1) — print the first N lines of one or more files (default N=10).
 *
 * Supports `-n N`.  Multiple files: prints "==> name <==" header.  No
 * args: reads stdin.  Exercises stdio fgets + printf + strtol.
 */
#include "libc.h"

static long n_lines = 10;

static void head_fp(FILE *f) {
    char buf[4096];
    long left = n_lines;
    while (left > 0 && fgets(buf, sizeof(buf), f)) {
        fputs(buf, stdout);
        /* fgets only finishes a "line" when it sees '\n' or hits EOF;
         * if our buffer fills mid-line we don't decrement. */
        size_t len = strlen(buf);
        if (len > 0 && buf[len - 1] == '\n') left--;
    }
}

int main(int argc, char **argv) {
    int idx = 1;
    while (idx < argc && argv[idx][0] == '-') {
        if (strcmp(argv[idx], "-n") == 0 && idx + 1 < argc) {
            n_lines = atol(argv[idx + 1]);
            idx += 2;
        } else if (argv[idx][1] == 'n' && argv[idx][2] != 0) {
            n_lines = atol(&argv[idx][2]);
            idx++;
        } else if (argv[idx][1] == '-' && argv[idx][2] == 0) {
            idx++; break;
        } else {
            fprintf(stderr, "head: unknown flag '%s'\n", argv[idx]);
            return 1;
        }
    }
    if (idx == argc) { head_fp(stdin); return 0; }
    int multi = (argc - idx > 1);
    int rc = 0;
    for (int i = idx; i < argc; i++) {
        FILE *f = fopen(argv[i], "r");
        if (!f) { perror(argv[i]); rc = 1; continue; }
        if (multi) printf("==> %s <==\n", argv[i]);
        head_fp(f);
        fclose(f);
        if (multi && i + 1 < argc) putchar('\n');
    }
    return rc;
}
