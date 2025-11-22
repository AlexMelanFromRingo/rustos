/**
 * Hello World for RustOS
 *
 * This program demonstrates user mode (Ring 3) execution
 * and system calls in RustOS.
 *
 * Compile:
 *   gcc -nostdlib -static -Ttext=0x400000 -o hello.elf hello.c
 *
 * Copy to disk:
 *   mcopy -i disk.img hello.elf ::hello.elf
 *
 * Run in RustOS:
 *   exec hello.elf
 */

// System call numbers (matching RustOS syscall/mod.rs)
#define SYS_READ    0
#define SYS_WRITE   1
#define SYS_OPEN    2
#define SYS_CLOSE   3
#define SYS_EXIT    60
#define SYS_GETPID  39

// File descriptors
#define STDOUT 1

// Inline assembly for syscall instruction
static inline long syscall3(long n, long arg1, long arg2, long arg3) {
    long ret;
    asm volatile(
        "syscall"
        : "=a"(ret)
        : "a"(n), "D"(arg1), "S"(arg2), "d"(arg3)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall1(long n, long arg1) {
    long ret;
    asm volatile(
        "syscall"
        : "=a"(ret)
        : "a"(n), "D"(arg1)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static inline long syscall0(long n) {
    long ret;
    asm volatile(
        "syscall"
        : "=a"(ret)
        : "a"(n)
        : "rcx", "r11", "memory"
    );
    return ret;
}

// Wrapper functions
static long write(int fd, const char* buf, unsigned long count) {
    return syscall3(SYS_WRITE, fd, (long)buf, count);
}

static long getpid(void) {
    return syscall0(SYS_GETPID);
}

static void exit(int status) {
    syscall1(SYS_EXIT, status);
    __builtin_unreachable();
}

// String length helper
static unsigned long strlen(const char* s) {
    unsigned long len = 0;
    while (s[len]) len++;
    return len;
}

// Entry point (not main, because we're freestanding)
void _start(void) {
    const char* msg1 = "Hello from Ring 3 user space!\n";
    const char* msg2 = "System calls are working!\n";
    const char* msg3 = "Exiting from user mode...\n";

    // Print messages
    write(STDOUT, msg1, strlen(msg1));
    write(STDOUT, msg2, strlen(msg2));

    // Get process ID (just to test another syscall)
    getpid();

    // Print exit message
    write(STDOUT, msg3, strlen(msg3));

    // Exit cleanly
    exit(0);
}
