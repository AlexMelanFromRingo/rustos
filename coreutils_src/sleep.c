/* sleep(1) — pause for the given number of seconds. */
#include "libc.h"

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "Usage: sleep SECONDS\n");
        return 1;
    }
    long secs = atol(argv[1]);
    if (secs < 0) secs = 0;
    sleep((unsigned int)secs);
    return 0;
}
