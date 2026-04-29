/* false(1) — exit with status 1.  POSIX-conformant: ignore all args. */
#include "syscalls.h"

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    return 1;
}
