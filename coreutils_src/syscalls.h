/* Minimal Linux x86-64 syscall wrappers for the in-tree coreutils.
 *
 * No glibc, no musl — these binaries link with -nostdlib and call the
 * kernel directly via the syscall instruction.  System V x86-64 ABI:
 *   %rax = syscall number
 *   args: %rdi, %rsi, %rdx, %r10, %r8, %r9
 *   return: %rax (negative = -errno)
 *   %rcx and %r11 clobbered by syscall.
 *
 * Numbers match the kernel's src/syscall/numbers.rs which in turn
 * matches Linux x86_64 (asm-generic/unistd.h).
 */
#ifndef RUSTOS_COREUTILS_SYSCALLS_H
#define RUSTOS_COREUTILS_SYSCALLS_H

typedef long           ssize_t;
typedef unsigned long  size_t;
typedef int            pid_t;

#define SYS_read    0
#define SYS_write   1
#define SYS_open    2
#define SYS_close   3
#define SYS_stat    4
#define SYS_fstat   5
#define SYS_lseek   8
#define SYS_brk     12
#define SYS_exit    60
#define SYS_getcwd  79

#define O_RDONLY    0
#define O_WRONLY    1
#define O_RDWR      2

#define STDIN  0
#define STDOUT 1
#define STDERR 2

static inline long _syscall0(long n) {
    long r;
    __asm__ volatile ("syscall"
        : "=a"(r) : "a"(n) : "rcx", "r11", "memory");
    return r;
}
static inline long _syscall1(long n, long a) {
    long r;
    __asm__ volatile ("syscall"
        : "=a"(r) : "a"(n), "D"(a) : "rcx", "r11", "memory");
    return r;
}
static inline long _syscall2(long n, long a, long b) {
    long r;
    __asm__ volatile ("syscall"
        : "=a"(r) : "a"(n), "D"(a), "S"(b) : "rcx", "r11", "memory");
    return r;
}
static inline long _syscall3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile ("syscall"
        : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

static inline ssize_t sys_write(int fd, const void *buf, size_t n) {
    return (ssize_t)_syscall3(SYS_write, fd, (long)buf, (long)n);
}
static inline ssize_t sys_read(int fd, void *buf, size_t n) {
    return (ssize_t)_syscall3(SYS_read, fd, (long)buf, (long)n);
}
static inline int sys_open(const char *path, int flags) {
    return (int)_syscall2(SYS_open, (long)path, flags);
}
static inline int sys_close(int fd) {
    return (int)_syscall1(SYS_close, fd);
}
static inline long sys_getcwd(char *buf, size_t sz) {
    return _syscall2(SYS_getcwd, (long)buf, (long)sz);
}
__attribute__((noreturn))
static inline void sys_exit(int code) {
    _syscall1(SYS_exit, code);
    __builtin_unreachable();
}

/* Tiny strlen — coreutils need it for printing C strings. */
static inline size_t my_strlen(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}

/* Tiny memset — used for buffer init. */
static inline void *my_memset(void *p, int c, size_t n) {
    unsigned char *b = (unsigned char *)p;
    for (size_t i = 0; i < n; i++) b[i] = (unsigned char)c;
    return p;
}

#endif /* RUSTOS_COREUTILS_SYSCALLS_H */
