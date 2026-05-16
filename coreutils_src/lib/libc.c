/* libc.c — implementation of the POSIX subset declared in libc.h.
 *
 * Single translation unit for simplicity (linked into every coreutil).
 * No external dependencies — every function is implemented here on top
 * of the raw syscall ABI.
 *
 * Allocator: linked-list first-fit on top of a 1 MiB initial brk pool.
 * Honest free + malloc-coalesce-on-free, no fragmentation tracking.
 * Works for typical coreutil workloads (a few KiB peak).
 *
 * stdio: line-buffered when fd is a terminal, fully-buffered otherwise.
 * 4 KiB FILE buffer.  fflush is honoured on putchar('\n') for terminal
 * fds.
 *
 * printf: handles %d, %u, %x, %X, %o, %s, %c, %%, %p with width and a
 * '0' / '-' / '+' / ' ' flag set, plus 'l' / 'll' length modifiers and
 * '*' for argument-supplied width.  No floats.
 */

#include "libc.h"

/* ────────────────────────────────────────────────────────────
 *  raw syscall wrappers
 * ──────────────────────────────────────────────────────────── */

static inline long _sc0(long n) {
    long r;
    __asm__ volatile ("syscall" : "=a"(r) : "a"(n) : "rcx", "r11", "memory");
    return r;
}
static inline long _sc1(long n, long a) {
    long r;
    __asm__ volatile ("syscall" : "=a"(r) : "a"(n), "D"(a) : "rcx", "r11", "memory");
    return r;
}
static inline long _sc2(long n, long a, long b) {
    long r;
    __asm__ volatile ("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b) : "rcx", "r11", "memory");
    return r;
}
static inline long _sc3(long n, long a, long b, long c) {
    long r;
    __asm__ volatile ("syscall"
        : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return r;
}

#define SYS_read         0
#define SYS_write        1
#define SYS_open         2
#define SYS_close        3
#define SYS_lseek        8
#define SYS_brk         12
#define SYS_exit        60
#define SYS_getpid      39
#define SYS_getcwd      79
#define SYS_time       201
#define SYS_unlink      87
#define SYS_rmdir       84
#define SYS_mkdir       83
#define SYS_rename      82
#define SYS_chdir       80
#define SYS_getdents64 217

/* ────────────────────────────────────────────────────────────
 *  errno
 * ──────────────────────────────────────────────────────────── */

int errno = 0;

/* Convert a kernel return (negative = -errno) to (rc, errno). */
static long _ret(long r) {
    if (r < 0 && r > -4096) { errno = (int)-r; return -1; }
    return r;
}

const char *strerror(int e) {
    switch (e) {
        case 0:        return "Success";
        case EPERM:    return "Operation not permitted";
        case ENOENT:   return "No such file or directory";
        case EIO:      return "I/O error";
        case EBADF:    return "Bad file descriptor";
        case EAGAIN:   return "Resource temporarily unavailable";
        case ENOMEM:   return "Cannot allocate memory";
        case EACCES:   return "Permission denied";
        case EEXIST:   return "File exists";
        case ENOTDIR:  return "Not a directory";
        case EISDIR:   return "Is a directory";
        case EINVAL:   return "Invalid argument";
        case ENFILE:   return "Too many open files in system";
        case EMFILE:   return "Too many open files";
        case ENOSPC:   return "No space left on device";
        case EPIPE:    return "Broken pipe";
        case ERANGE:   return "Numerical result out of range";
        default:       return "Unknown error";
    }
}

void perror(const char *prefix) {
    if (prefix && *prefix) {
        write(STDERR_FILENO, prefix, strlen(prefix));
        write(STDERR_FILENO, ": ", 2);
    }
    const char *m = strerror(errno);
    write(STDERR_FILENO, m, strlen(m));
    write(STDERR_FILENO, "\n", 1);
}

/* ────────────────────────────────────────────────────────────
 *  unistd.h
 * ──────────────────────────────────────────────────────────── */

ssize_t read(int fd, void *buf, size_t n) {
    return _ret(_sc3(SYS_read, fd, (long)buf, (long)n));
}
ssize_t write(int fd, const void *buf, size_t n) {
    return _ret(_sc3(SYS_write, fd, (long)buf, (long)n));
}
int open(const char *path, int flags) {
    return (int)_ret(_sc2(SYS_open, (long)path, flags));
}
int close(int fd) { return (int)_ret(_sc1(SYS_close, fd)); }
off_t lseek(int fd, off_t off, int whence) {
    return _ret(_sc3(SYS_lseek, fd, off, whence));
}
int getpid(void) { return (int)_sc0(SYS_getpid); }

char *getcwd(char *buf, size_t sz) {
    long r = _ret(_sc2(SYS_getcwd, (long)buf, (long)sz));
    return r < 0 ? NULL : buf;
}

long getdents64(int fd, void *buf, unsigned long count) {
    return _ret(_sc3(SYS_getdents64, fd, (long)buf, (long)count));
}

int mkdir(const char *path, unsigned mode) {
    return (int)_ret(_sc2(SYS_mkdir, (long)path, (long)mode));
}
int rmdir(const char *path) {
    return (int)_ret(_sc1(SYS_rmdir, (long)path));
}
int unlink(const char *path) {
    return (int)_ret(_sc1(SYS_unlink, (long)path));
}
int rename(const char *oldp, const char *newp) {
    return (int)_ret(_sc2(SYS_rename, (long)oldp, (long)newp));
}
int chdir(const char *path) {
    return (int)_ret(_sc1(SYS_chdir, (long)path));
}

int isatty(int fd) {
    /* No tty ioctl yet in our kernel.  Approximate: stdin/stdout/stderr
     * are TTYs unless explicitly redirected.  Coreutils mostly use
     * isatty to decide whether to colour output; returning 1 for the
     * standard fds matches sane shell behaviour. */
    return (fd >= 0 && fd <= 2);
}

/* ────────────────────────────────────────────────────────────
 *  time.h
 * ──────────────────────────────────────────────────────────── */

time_t time(time_t *out) {
    long r = _sc1(SYS_time, (long)out);
    if (r < 0 && r > -4096) { errno = (int)-r; return -1; }
    return r;
}

/* ────────────────────────────────────────────────────────────
 *  ctype.h
 * ──────────────────────────────────────────────────────────── */

int isalpha(int c) { return (c>='a'&&c<='z')||(c>='A'&&c<='Z'); }
int isdigit(int c) { return c>='0'&&c<='9'; }
int isspace(int c) { return c==' '||c=='\t'||c=='\n'||c=='\r'||c=='\v'||c=='\f'; }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isupper(int c) { return c>='A'&&c<='Z'; }
int islower(int c) { return c>='a'&&c<='z'; }
int toupper(int c) { return islower(c) ? c - 'a' + 'A' : c; }
int tolower(int c) { return isupper(c) ? c - 'A' + 'a' : c; }

/* ────────────────────────────────────────────────────────────
 *  string.h
 * ──────────────────────────────────────────────────────────── */

size_t strlen(const char *s) { size_t n=0; while (s[n]) n++; return n; }
int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}
int strncmp(const char *a, const char *b, size_t n) {
    while (n && *a && *a == *b) { a++; b++; n--; }
    if (n == 0) return 0;
    return (unsigned char)*a - (unsigned char)*b;
}
char *strchr(const char *s, int c) {
    char ch = (char)c;
    for (; *s; s++) if (*s == ch) return (char *)s;
    return ch == 0 ? (char *)s : NULL;
}
char *strrchr(const char *s, int c) {
    char ch = (char)c;
    const char *last = NULL;
    for (; *s; s++) if (*s == ch) last = s;
    return ch == 0 ? (char *)s : (char *)last;
}
char *strcpy(char *dst, const char *src) {
    char *r = dst;
    while ((*dst++ = *src++)) {}
    return r;
}
char *strncpy(char *dst, const char *src, size_t n) {
    char *r = dst;
    while (n && (*dst++ = *src++)) n--;
    while (n--) *dst++ = 0;
    return r;
}
char *strcat(char *dst, const char *src) {
    char *r = dst;
    while (*dst) dst++;
    while ((*dst++ = *src++)) {}
    return r;
}
size_t strspn(const char *s, const char *accept) {
    size_t n = 0;
    for (; *s; s++) {
        const char *a;
        for (a = accept; *a; a++) if (*a == *s) break;
        if (!*a) break;
        n++;
    }
    return n;
}
size_t strcspn(const char *s, const char *reject) {
    size_t n = 0;
    for (; *s; s++) {
        for (const char *r = reject; *r; r++) if (*r == *s) return n;
        n++;
    }
    return n;
}
void *memset(void *s, int c, size_t n) {
    unsigned char *p = s;
    for (size_t i = 0; i < n; i++) p[i] = (unsigned char)c;
    return s;
}
void *memcpy(void *d, const void *s, size_t n) {
    unsigned char *dp = d;
    const unsigned char *sp = s;
    for (size_t i = 0; i < n; i++) dp[i] = sp[i];
    return d;
}
void *memmove(void *d, const void *s, size_t n) {
    unsigned char *dp = d;
    const unsigned char *sp = s;
    if (dp < sp) {
        for (size_t i = 0; i < n; i++) dp[i] = sp[i];
    } else if (dp > sp) {
        for (size_t i = n; i; i--) dp[i-1] = sp[i-1];
    }
    return d;
}
int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *p = a, *q = b;
    for (size_t i = 0; i < n; i++) if (p[i] != q[i]) return (int)p[i] - (int)q[i];
    return 0;
}
void *memchr(const void *s, int c, size_t n) {
    const unsigned char *p = s;
    unsigned char ch = (unsigned char)c;
    for (size_t i = 0; i < n; i++) if (p[i] == ch) return (void *)(p + i);
    return NULL;
}

/* ────────────────────────────────────────────────────────────
 *  stdlib.h
 * ──────────────────────────────────────────────────────────── */

long strtol(const char *s, char **endptr, int base) {
    while (*s == ' ' || (*s >= '\t' && *s <= '\r')) s++;
    int neg = 0;
    if (*s == '-') { neg = 1; s++; }
    else if (*s == '+') s++;
    if ((base == 0 || base == 16) && s[0] == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2; base = 16;
    } else if (base == 0 && s[0] == '0') {
        base = 8; s++;
    } else if (base == 0) {
        base = 10;
    }
    long acc = 0;
    int any = 0;
    while (*s) {
        int d;
        if (*s >= '0' && *s <= '9') d = *s - '0';
        else if (*s >= 'a' && *s <= 'z') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'Z') d = *s - 'A' + 10;
        else break;
        if (d >= base) break;
        acc = acc * base + d;
        any = 1;
        s++;
    }
    if (endptr) *endptr = (char *)(any ? s : s);
    return neg ? -acc : acc;
}

int atoi(const char *s) { return (int)strtol(s, NULL, 10); }
long atol(const char *s) { return strtol(s, NULL, 10); }

/* ---- Allocator: free-list first-fit ------------------------- */

#define HEAP_ALIGN 16
#define HEAP_MIN_SPLIT 32

typedef struct block {
    size_t        size;       /* user-payload size, not counting header */
    int           free;
    struct block *next;
} block_t;

static block_t *heap_head = NULL;
static unsigned long brk_base = 0;
static unsigned long brk_top = 0;

static unsigned long sys_brk_call(unsigned long addr) {
    return (unsigned long)_sc1(SYS_brk, (long)addr);
}

static void heap_init(void) {
    if (brk_base) return;
    brk_base = sys_brk_call(0);
    /* Reserve 1 MiB. */
    brk_top = sys_brk_call(brk_base + (1UL << 20));
    if (brk_top <= brk_base) { brk_base = 0; return; }
    heap_head = (block_t *)brk_base;
    heap_head->size = brk_top - brk_base - sizeof(block_t);
    heap_head->free = 1;
    heap_head->next = NULL;
}

static size_t align_up(size_t n) { return (n + HEAP_ALIGN - 1) & ~(size_t)(HEAP_ALIGN - 1); }

void *malloc(size_t n) {
    if (n == 0) return NULL;
    if (!heap_head) heap_init();
    if (!heap_head) { errno = ENOMEM; return NULL; }
    n = align_up(n);
    for (block_t *b = heap_head; b; b = b->next) {
        if (!b->free || b->size < n) continue;
        /* Found.  Split if there's room. */
        if (b->size >= n + sizeof(block_t) + HEAP_MIN_SPLIT) {
            block_t *next = (block_t *)((unsigned char *)b + sizeof(block_t) + n);
            next->size = b->size - n - sizeof(block_t);
            next->free = 1;
            next->next = b->next;
            b->size = n;
            b->next = next;
        }
        b->free = 0;
        return (unsigned char *)b + sizeof(block_t);
    }
    errno = ENOMEM;
    return NULL;
}

void free(void *p) {
    if (!p) return;
    block_t *b = (block_t *)((unsigned char *)p - sizeof(block_t));
    b->free = 1;
    /* Coalesce with next if also free. */
    if (b->next && b->next->free) {
        b->size += sizeof(block_t) + b->next->size;
        b->next = b->next->next;
    }
    /* Coalesce with prev: O(n) walk. */
    block_t *prev = NULL;
    for (block_t *c = heap_head; c && c != b; c = c->next) prev = c;
    if (prev && prev->free) {
        prev->size += sizeof(block_t) + b->size;
        prev->next = b->next;
    }
}

void *calloc(size_t nmemb, size_t size) {
    size_t total = nmemb * size;
    if (size && total / size != nmemb) { errno = ENOMEM; return NULL; }
    void *p = malloc(total);
    if (p) memset(p, 0, total);
    return p;
}

void *realloc(void *p, size_t n) {
    if (!p) return malloc(n);
    if (n == 0) { free(p); return NULL; }
    block_t *b = (block_t *)((unsigned char *)p - sizeof(block_t));
    if (b->size >= n) return p; /* shrink → no-op */
    void *q = malloc(n);
    if (!q) return NULL;
    memcpy(q, p, b->size);
    free(p);
    return q;
}

__attribute__((noreturn)) void exit(int code) {
    /* fflush stdio buffers before exit. */
    extern void _stdio_flush_all(void);
    _stdio_flush_all();
    _sc1(SYS_exit, code);
    __builtin_unreachable();
}

__attribute__((noreturn)) void abort(void) {
    /* Best we can do without signals: write a marker and exit(127). */
    static const char m[] = "abort()\n";
    write(STDERR_FILENO, m, sizeof(m) - 1);
    exit(127);
}

/* ────────────────────────────────────────────────────────────
 *  stdio.h
 * ──────────────────────────────────────────────────────────── */

#define BUF_SZ 4096

struct __FILE {
    int    fd;
    int    flags;       /* bit 0 = read, 1 = write, 2 = error, 3 = eof */
    int    line_buf;    /* 1 = flush on '\n' */
    size_t buf_pos;
    size_t buf_end;     /* for read: bytes available; for write: 0 unless we add unread */
    unsigned char buf[BUF_SZ];
};

#define F_READ   1
#define F_WRITE  2
#define F_ERROR  4
#define F_EOF    8

static FILE _stdin  = { 0, F_READ,  0, 0, 0, {0} };
static FILE _stdout = { 1, F_WRITE, 1, 0, 0, {0} };
static FILE _stderr = { 2, F_WRITE, 1, 0, 0, {0} };

FILE *stdin  = &_stdin;
FILE *stdout = &_stdout;
FILE *stderr = &_stderr;

static FILE *_files[8] = { &_stdin, &_stdout, &_stderr, NULL, NULL, NULL, NULL, NULL };

void _stdio_flush_all(void) {
    for (int i = 0; i < 8; i++) if (_files[i]) fflush(_files[i]);
}

int fflush(FILE *f) {
    if (!f) return 0;
    if (!(f->flags & F_WRITE)) return 0;
    if (f->buf_pos == 0) return 0;
    ssize_t n = write(f->fd, f->buf, f->buf_pos);
    if (n < 0 || (size_t)n != f->buf_pos) {
        f->flags |= F_ERROR;
        return EOF;
    }
    f->buf_pos = 0;
    return 0;
}

static int _putc_buffered(int c, FILE *f) {
    if (f->buf_pos >= BUF_SZ) {
        if (fflush(f) == EOF) return EOF;
    }
    f->buf[f->buf_pos++] = (unsigned char)c;
    if (f->line_buf && c == '\n') {
        if (fflush(f) == EOF) return EOF;
    }
    return c;
}

int fputc(int c, FILE *f) { return _putc_buffered(c, f); }
int fputs(const char *s, FILE *f) {
    while (*s) if (fputc(*s++, f) == EOF) return EOF;
    return 0;
}
int putchar(int c) { return fputc(c, stdout); }
int puts(const char *s) {
    if (fputs(s, stdout) == EOF) return EOF;
    if (fputc('\n', stdout) == EOF) return EOF;
    return 0;
}

int fgetc(FILE *f) {
    if (!(f->flags & F_READ)) return EOF;
    if (f->buf_pos >= f->buf_end) {
        ssize_t n = read(f->fd, f->buf, BUF_SZ);
        if (n <= 0) {
            f->flags |= (n == 0) ? F_EOF : F_ERROR;
            return EOF;
        }
        f->buf_pos = 0;
        f->buf_end = (size_t)n;
    }
    return (int)f->buf[f->buf_pos++];
}

char *fgets(char *buf, int sz, FILE *f) {
    if (sz <= 0) return NULL;
    int n = 0;
    while (n < sz - 1) {
        int c = fgetc(f);
        if (c == EOF) {
            if (n == 0) return NULL;
            break;
        }
        buf[n++] = (char)c;
        if (c == '\n') break;
    }
    buf[n] = 0;
    return buf;
}

int feof(FILE *f)   { return (f->flags & F_EOF)   != 0; }
int ferror(FILE *f) { return (f->flags & F_ERROR) != 0; }
int getchar(void)   { return fgetc(stdin); }

FILE *fopen(const char *path, const char *mode) {
    int flags;
    int reading = 0, writing = 0;
    if (mode[0] == 'r') { flags = O_RDONLY; reading = 1; }
    else if (mode[0] == 'w') { flags = O_WRONLY | O_CREAT | O_TRUNC; writing = 1; }
    else if (mode[0] == 'a') { flags = O_WRONLY | O_CREAT | O_APPEND; writing = 1; }
    else { errno = EINVAL; return NULL; }

    int fd = open(path, flags);
    if (fd < 0) return NULL;

    int slot = -1;
    for (int i = 3; i < 8; i++) if (!_files[i]) { slot = i; break; }
    if (slot < 0) { close(fd); errno = EMFILE; return NULL; }

    FILE *f = malloc(sizeof(FILE));
    if (!f) { close(fd); return NULL; }
    f->fd = fd;
    f->flags = (reading ? F_READ : 0) | (writing ? F_WRITE : 0);
    f->line_buf = 0;
    f->buf_pos = 0;
    f->buf_end = 0;
    _files[slot] = f;
    return f;
}

int fclose(FILE *f) {
    if (!f) return EOF;
    fflush(f);
    int rc = close(f->fd);
    for (int i = 3; i < 8; i++) if (_files[i] == f) { _files[i] = NULL; break; }
    free(f);
    return rc;
}

size_t fread(void *p, size_t sz, size_t nmemb, FILE *f) {
    unsigned char *out = p;
    size_t total = sz * nmemb;
    size_t got = 0;
    while (got < total) {
        int c = fgetc(f);
        if (c == EOF) break;
        out[got++] = (unsigned char)c;
    }
    return got / sz;
}

size_t fwrite(const void *p, size_t sz, size_t nmemb, FILE *f) {
    const unsigned char *in = p;
    size_t total = sz * nmemb;
    size_t put = 0;
    while (put < total) {
        if (fputc(in[put], f) == EOF) break;
        put++;
    }
    return put / sz;
}

/* ────────────────────────────────────────────────────────────
 *  printf family
 * ──────────────────────────────────────────────────────────── */

#include <stdarg.h>

typedef struct {
    char *buf;
    size_t cap;
    size_t pos;
    FILE *f;        /* for fprintf path */
} pfctx_t;

static void _put(pfctx_t *c, char ch) {
    if (c->buf) {
        if (c->pos + 1 < c->cap) c->buf[c->pos] = ch;
        c->pos++;
    } else if (c->f) {
        fputc(ch, c->f);
        c->pos++;
    }
}

static void _puts_n(pfctx_t *c, const char *s, size_t n) {
    for (size_t i = 0; i < n; i++) _put(c, s[i]);
}

static int _itoa(unsigned long v, int base, int upper, char *out) {
    static const char *low = "0123456789abcdef";
    static const char *hi  = "0123456789ABCDEF";
    const char *t = upper ? hi : low;
    char tmp[32]; int n = 0;
    if (v == 0) tmp[n++] = '0';
    while (v) { tmp[n++] = t[v % base]; v /= base; }
    for (int i = 0; i < n; i++) out[i] = tmp[n - 1 - i];
    return n;
}

static int vpf(pfctx_t *c, const char *fmt, va_list ap) {
    while (*fmt) {
        if (*fmt != '%') { _put(c, *fmt++); continue; }
        fmt++;
        /* flags */
        int left = 0, zero_pad = 0, plus = 0, sp = 0;
        for (;;) {
            if (*fmt == '-') { left = 1; fmt++; }
            else if (*fmt == '0') { zero_pad = 1; fmt++; }
            else if (*fmt == '+') { plus = 1; fmt++; }
            else if (*fmt == ' ') { sp = 1; fmt++; }
            else break;
        }
        /* width */
        int width = 0;
        if (*fmt == '*') { width = va_arg(ap, int); fmt++; }
        else while (*fmt >= '0' && *fmt <= '9') { width = width * 10 + (*fmt - '0'); fmt++; }
        /* length: l or ll */
        int longflag = 0;
        if (*fmt == 'l') { longflag = 1; fmt++; if (*fmt == 'l') { longflag = 2; fmt++; } }

        char spec = *fmt++;
        char numbuf[32];
        int  numlen = 0;
        const char *prefix = "";
        int  is_signed_neg = 0;

        switch (spec) {
            case 'd': case 'i': {
                long long v = (longflag == 2) ? va_arg(ap, long long)
                            : (longflag == 1) ? va_arg(ap, long)
                            :                    va_arg(ap, int);
                unsigned long long u;
                if (v < 0) { is_signed_neg = 1; u = (unsigned long long)(-v); }
                else u = (unsigned long long)v;
                numlen = _itoa((unsigned long)u, 10, 0, numbuf);
                if (is_signed_neg) prefix = "-";
                else if (plus) prefix = "+";
                else if (sp)   prefix = " ";
                break;
            }
            case 'u': {
                unsigned long long u = (longflag == 2) ? va_arg(ap, unsigned long long)
                                     : (longflag == 1) ? va_arg(ap, unsigned long)
                                     :                    va_arg(ap, unsigned int);
                numlen = _itoa((unsigned long)u, 10, 0, numbuf);
                break;
            }
            case 'x': case 'X': {
                unsigned long long u = (longflag == 2) ? va_arg(ap, unsigned long long)
                                     : (longflag == 1) ? va_arg(ap, unsigned long)
                                     :                    va_arg(ap, unsigned int);
                numlen = _itoa((unsigned long)u, 16, spec == 'X', numbuf);
                break;
            }
            case 'o': {
                unsigned long long u = (longflag == 2) ? va_arg(ap, unsigned long long)
                                     : (longflag == 1) ? va_arg(ap, unsigned long)
                                     :                    va_arg(ap, unsigned int);
                numlen = _itoa((unsigned long)u, 8, 0, numbuf);
                break;
            }
            case 'p': {
                unsigned long u = (unsigned long)va_arg(ap, void *);
                _put(c, '0'); _put(c, 'x');
                numlen = _itoa(u, 16, 0, numbuf);
                break;
            }
            case 'c': {
                int v = va_arg(ap, int);
                int pad = width > 1 ? width - 1 : 0;
                if (!left) for (int i = 0; i < pad; i++) _put(c, ' ');
                _put(c, (char)v);
                if (left)  for (int i = 0; i < pad; i++) _put(c, ' ');
                continue;
            }
            case 's': {
                const char *s = va_arg(ap, const char *);
                if (!s) s = "(null)";
                size_t l = strlen(s);
                int pad = width > (int)l ? width - (int)l : 0;
                if (!left) for (int i = 0; i < pad; i++) _put(c, ' ');
                _puts_n(c, s, l);
                if (left)  for (int i = 0; i < pad; i++) _put(c, ' ');
                continue;
            }
            case '%': _put(c, '%'); continue;
            default:
                _put(c, '%');
                _put(c, spec);
                continue;
        }

        int total = (int)strlen(prefix) + numlen;
        int pad = width > total ? width - total : 0;
        if (!left && !zero_pad) for (int i = 0; i < pad; i++) _put(c, ' ');
        for (const char *p = prefix; *p; p++) _put(c, *p);
        if (!left && zero_pad) for (int i = 0; i < pad; i++) _put(c, '0');
        for (int i = 0; i < numlen; i++) _put(c, numbuf[i]);
        if (left) for (int i = 0; i < pad; i++) _put(c, ' ');
    }
    /* null-terminate sprintf/snprintf output */
    if (c->buf && c->cap > 0) {
        c->buf[c->pos < c->cap ? c->pos : c->cap - 1] = 0;
    }
    return (int)c->pos;
}

int printf(const char *fmt, ...) {
    pfctx_t c = { NULL, 0, 0, stdout };
    va_list ap; va_start(ap, fmt);
    int n = vpf(&c, fmt, ap);
    va_end(ap);
    return n;
}
int fprintf(FILE *f, const char *fmt, ...) {
    pfctx_t c = { NULL, 0, 0, f };
    va_list ap; va_start(ap, fmt);
    int n = vpf(&c, fmt, ap);
    va_end(ap);
    return n;
}
int sprintf(char *buf, const char *fmt, ...) {
    /* SIZE_MAX gives no truncation. */
    pfctx_t c = { buf, (size_t)-1, 0, NULL };
    va_list ap; va_start(ap, fmt);
    int n = vpf(&c, fmt, ap);
    va_end(ap);
    return n;
}
int snprintf(char *buf, size_t sz, const char *fmt, ...) {
    pfctx_t c = { buf, sz, 0, NULL };
    va_list ap; va_start(ap, fmt);
    int n = vpf(&c, fmt, ap);
    va_end(ap);
    return n;
}
