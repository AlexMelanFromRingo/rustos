/* date(1) — print current time.
 *
 * Format: YYYY-MM-DD HH:MM:SS (ISO 8601 simple).  We get the epoch
 * seconds from sys_time() and split using the standard "days since
 * Unix epoch" algorithm — see Howard Hinnant's date paper for the
 * proper-leap-year math we lift here.
 *
 * Single-line output, newline-terminated.  No format string yet
 * (POSIX `date +%F` could be added later).
 */
#include "libc.h"

/* Days-since-civil-day to (y, m, d) per Hinnant 2012 — handles all
 * leap-year cases inc. centuries.  Domain: -678881..2147483647 days
 * from 1970-01-01.  Returns y in -32768..32767 range. */
static void civil_from_days(long z, int *yp, int *mp, int *dp) {
    z += 719468;
    long era = (z >= 0 ? z : z - 146096) / 146097;
    long doe = z - era * 146097;                                 /* 0..146096 */
    long yoe = (doe - doe/1460 + doe/36524 - doe/146096) / 365;  /* 0..399 */
    long y   = yoe + era * 400;
    long doy = doe - (365*yoe + yoe/4 - yoe/100);                /* 0..365 */
    long mp_ = (5*doy + 2) / 153;                                /* 0..11  */
    int  d   = (int)(doy - (153*mp_ + 2)/5 + 1);                 /* 1..31  */
    int  m   = (int)(mp_ + (mp_ < 10 ? 3 : -9));                 /* 1..12  */
    *yp = (int)(y + (m <= 2 ? 1 : 0));
    *mp = m;
    *dp = d;
}

int main(int argc, char **argv) {
    (void)argc; (void)argv;
    time_t t = time(NULL);
    if (t < 0) { perror("date"); return 1; }
    long secs = (long)t;
    long days = secs / 86400;
    long rem  = secs % 86400;
    if (rem < 0) { rem += 86400; days--; }
    int y, m, d;
    civil_from_days(days, &y, &m, &d);
    int hh = (int)(rem / 3600);
    int mm = (int)((rem % 3600) / 60);
    int ss = (int)(rem % 60);
    printf("%d-%02d-%02d %02d:%02d:%02d\n", y, m, d, hh, mm, ss);
    return 0;
}
