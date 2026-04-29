/* Minimal POSIX-ish C library for the in-tree coreutils.
 *
 * This is *not* a port of musl — porting musl-libc is a multi-thousand-
 * line undertaking that's out of scope for the autonomous pass.  What
 * this header *does* provide is the subset every realistic POSIX C
 * program actually uses: string ops, formatted I/O, malloc/free with a
 * real free list (not a brk-bump), file I/O via FILE*, errno, and a
 * handful of unistd / time / sys/types pieces.
 *
 * All implementations live in libc.c; this header is what the coreutils
 * include.  Linked into every binary by build.rs.
 *
 * Reference points:
 *   * POSIX.1-2017 (IEEE Std 1003.1) — function semantics
 *   * musl's src/string/, src/stdio/ (license: MIT) — algorithm shapes
 *   * dlmalloc / wikipedia "Buddy allocator" — free list shape
 *
 * Where this differs from glibc/musl:
 *   * No locale / NLS — printf %d is decimal in ASCII, period.
 *   * No threads / TLS — single-threaded user programs only.
 *   * No DSO loading — coreutils are statically linked; nothing
 *     dynamic to resolve.
 *   * No floating-point printf %f/%e/%g — integer-only.  Adding this
 *     means importing dragon4 / grisu, which is bigger than the rest
 *     of libc combined.  Not worth it for coreutils.
 */
#ifndef RUSTOS_COREUTILS_LIBC_H
#define RUSTOS_COREUTILS_LIBC_H

/* ---- size_t, ssize_t, etc. ---- */

typedef unsigned long  size_t;
typedef long           ssize_t;
typedef int            pid_t;
typedef long           off_t;
typedef long           time_t;

#define NULL  ((void *)0)
#define EOF   (-1)

/* ---- File descriptors / open flags (Linux x86-64 ABI) ---- */

#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

#define O_RDONLY   0
#define O_WRONLY   1
#define O_RDWR     2
#define O_CREAT  0100
#define O_EXCL   0200
#define O_TRUNC 01000
#define O_APPEND 02000

/* ---- errno (POSIX errno values, subset) ---- */

extern int errno;

#define EPERM    1
#define ENOENT   2
#define ESRCH    3
#define EINTR    4
#define EIO      5
#define ENXIO    6
#define E2BIG    7
#define ENOEXEC  8
#define EBADF    9
#define EAGAIN  11
#define ENOMEM  12
#define EACCES  13
#define EFAULT  14
#define EBUSY   16
#define EEXIST  17
#define ENOTDIR 20
#define EISDIR  21
#define EINVAL  22
#define ENFILE  23
#define EMFILE  24
#define ENOSPC  28
#define EPIPE   32
#define ERANGE  34

/* ---- string.h ---- */

size_t strlen(const char *s);
int    strcmp(const char *a, const char *b);
int    strncmp(const char *a, const char *b, size_t n);
char  *strchr(const char *s, int c);
char  *strrchr(const char *s, int c);
char  *strcpy(char *dst, const char *src);
char  *strncpy(char *dst, const char *src, size_t n);
char  *strcat(char *dst, const char *src);
size_t strspn(const char *s, const char *accept);
size_t strcspn(const char *s, const char *reject);
void  *memset(void *s, int c, size_t n);
void  *memcpy(void *d, const void *s, size_t n);
void  *memmove(void *d, const void *s, size_t n);
int    memcmp(const void *a, const void *b, size_t n);
void  *memchr(const void *s, int c, size_t n);

/* ---- stdlib.h ---- */

void  *malloc(size_t n);
void  *calloc(size_t nmemb, size_t size);
void  *realloc(void *p, size_t n);
void   free(void *p);
int    atoi(const char *s);
long   atol(const char *s);
long   strtol(const char *s, char **endptr, int base);
__attribute__((noreturn)) void exit(int code);
__attribute__((noreturn)) void abort(void);

/* ---- unistd.h ---- */

ssize_t read(int fd, void *buf, size_t n);
ssize_t write(int fd, const void *buf, size_t n);
int     open(const char *path, int flags);
int     close(int fd);
off_t   lseek(int fd, off_t off, int whence);
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2
int     getpid(void);
char   *getcwd(char *buf, size_t sz);
int     isatty(int fd);

/* ---- time.h ---- */

time_t time(time_t *out);

/* ---- ctype.h (subset) ---- */

int isalpha(int c);
int isdigit(int c);
int isspace(int c);
int isalnum(int c);
int isupper(int c);
int islower(int c);
int toupper(int c);
int tolower(int c);

/* ---- stdio.h: a real FILE* layer with buffering ---- */

typedef struct __FILE FILE;

extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

FILE  *fopen(const char *path, const char *mode);
int    fclose(FILE *f);
size_t fread(void *p, size_t sz, size_t nmemb, FILE *f);
size_t fwrite(const void *p, size_t sz, size_t nmemb, FILE *f);
int    fflush(FILE *f);
int    fputc(int c, FILE *f);
int    fputs(const char *s, FILE *f);
int    fgetc(FILE *f);
char  *fgets(char *buf, int sz, FILE *f);
int    feof(FILE *f);
int    ferror(FILE *f);

int    putchar(int c);
int    puts(const char *s);
int    getchar(void);

/* printf family.  Integer-only — no %f/%e/%g. */
int    printf(const char *fmt, ...);
int    fprintf(FILE *f, const char *fmt, ...);
int    sprintf(char *buf, const char *fmt, ...);
int    snprintf(char *buf, size_t sz, const char *fmt, ...);

/* perror — print current strerror to stderr. */
const char *strerror(int e);
void        perror(const char *prefix);

#endif /* RUSTOS_COREUTILS_LIBC_H */
