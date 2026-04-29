/* true(1) — exit successfully.  POSIX-conformant: ignore all args. */
#include "syscalls.h"

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    return 0;
}
