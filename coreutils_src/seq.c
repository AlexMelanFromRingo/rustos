/* seq(1) — print a sequence of integers.
 *
 *   seq LAST              prints 1..LAST
 *   seq FIRST LAST        prints FIRST..LAST
 *   seq FIRST STEP LAST   prints FIRST, FIRST+STEP, ... LAST
 *
 * Each on its own line.  Empty output if the sequence would be empty
 * (e.g., seq 5 -1 10).
 */
#include "libc.h"

int main(int argc, char **argv) {
    long first = 1, step = 1, last;
    if (argc == 2) { last = atol(argv[1]); }
    else if (argc == 3) { first = atol(argv[1]); last = atol(argv[2]); }
    else if (argc == 4) {
        first = atol(argv[1]); step = atol(argv[2]); last = atol(argv[3]);
    } else {
        fprintf(stderr, "Usage: seq [FIRST [STEP]] LAST\n");
        return 1;
    }
    if (step == 0) {
        fprintf(stderr, "seq: STEP cannot be 0\n");
        return 1;
    }
    if (step > 0) {
        for (long i = first; i <= last; i += step) printf("%d\n", (int)i);
    } else {
        for (long i = first; i >= last; i += step) printf("%d\n", (int)i);
    }
    return 0;
}
