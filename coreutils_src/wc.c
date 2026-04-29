/* wc(1) — count lines, words, bytes.  POSIX-compliant subset.
 *
 * Flags: -l (lines only), -w (words only), -c (bytes only).  Without
 * flags, prints all three.  Multiple files: prints per-file counts plus
 * a "total" footer.  Without args: reads stdin.
 *
 * Uses libc fopen/fread/feof, printf, and isspace — exercises the
 * stdio + ctype + string layer in our in-tree libc subset.
 */
#include "libc.h"

typedef struct { long lines, words, bytes; } counts_t;

static counts_t count_fp(FILE *f) {
    counts_t c = {0,0,0};
    int prev_space = 1;
    int ch;
    while ((ch = fgetc(f)) != EOF) {
        c.bytes++;
        if (ch == '\n') c.lines++;
        if (isspace(ch)) {
            prev_space = 1;
        } else if (prev_space) {
            c.words++;
            prev_space = 0;
        }
    }
    return c;
}

static int show_lines = 0, show_words = 0, show_bytes = 0;

static void print_counts(const counts_t *c, const char *label) {
    int any = show_lines | show_words | show_bytes;
    if (!any) { show_lines = show_words = show_bytes = 1; }
    if (show_lines) printf(" %7ld", c->lines);
    if (show_words) printf(" %7ld", c->words);
    if (show_bytes) printf(" %7ld", c->bytes);
    if (label) printf(" %s", label);
    putchar('\n');
}

int main(int argc, char **argv) {
    int files_start = 1;
    while (files_start < argc && argv[files_start][0] == '-' && argv[files_start][1]) {
        const char *p = argv[files_start] + 1;
        for (; *p; p++) {
            switch (*p) {
                case 'l': show_lines = 1; break;
                case 'w': show_words = 1; break;
                case 'c': show_bytes = 1; break;
                default:
                    fprintf(stderr, "wc: unknown flag '%c'\n", *p);
                    return 1;
            }
        }
        files_start++;
    }
    counts_t total = {0,0,0};
    int n_files = argc - files_start;
    if (n_files == 0) {
        counts_t c = count_fp(stdin);
        print_counts(&c, NULL);
        return 0;
    }
    for (int i = files_start; i < argc; i++) {
        FILE *f = fopen(argv[i], "r");
        if (!f) { perror(argv[i]); continue; }
        counts_t c = count_fp(f);
        fclose(f);
        print_counts(&c, argv[i]);
        total.lines += c.lines; total.words += c.words; total.bytes += c.bytes;
    }
    if (n_files > 1) print_counts(&total, "total");
    return 0;
}
