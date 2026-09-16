/* astra/util.c */
#include "util.h"

#include <ctype.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/types.h>
#include <unistd.h>

log_level_t g_log_level = L_INFO;
int g_log_colors = 0;

/* ------------------------------------------------------------------ buffers */

void buf_init(buf_t *b) {
    b->data = NULL;
    b->len = 0;
    b->cap = 0;
}

void buf_free(buf_t *b) {
    free(b->data);
    b->data = NULL;
    b->len = b->cap = 0;
}

void buf_reserve(buf_t *b, size_t extra) {
    if (b->len + extra + 1 <= b->cap) return;
    size_t nc = b->cap ? b->cap * 2 : 256;
    while (nc < b->len + extra + 1) nc *= 2;
    uint8_t *nd = (uint8_t *)realloc(b->data, nc);
    if (!nd) {
        LOGE("out of memory (realloc %zu bytes)", nc);
        abort();
    }
    b->data = nd;
    b->cap = nc;
}

void buf_append(buf_t *b, const void *p, size_t n) {
    if (!n) return;
    buf_reserve(b, n);
    memcpy(b->data + b->len, p, n);
    b->len += n;
    b->data[b->len] = 0;
}

void buf_appendc(buf_t *b, char c) { buf_append(b, &c, 1); }

void buf_appendstr(buf_t *b, const char *s) {
    if (s) buf_append(b, s, strlen(s));
}

void buf_appendf(buf_t *b, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    char tmp[1024];
    int n = vsnprintf(tmp, sizeof(tmp), fmt, ap);
    va_end(ap);
    if (n < 0) return;
    if ((size_t)n < sizeof(tmp)) {
        buf_append(b, tmp, (size_t)n);
        return;
    }
    char *big = (char *)malloc((size_t)n + 1);
    if (!big) return;
    va_start(ap, fmt);
    vsnprintf(big, (size_t)n + 1, fmt, ap);
    va_end(ap);
    buf_append(b, big, (size_t)n);
    free(big);
}

void buf_clear(buf_t *b) {
    b->len = 0;
    if (b->data) b->data[0] = 0;
}

void buf_consume(buf_t *b, size_t n) {
    if (n >= b->len) {
        b->len = 0;
        if (b->data) b->data[0] = 0;
        return;
    }
    memmove(b->data, b->data + n, b->len - n);
    b->len -= n;
    b->data[b->len] = 0;
}

/* ------------------------------------------------------------------ logging */

static const char *lvl_name(log_level_t l) {
    switch (l) {
        case L_ERROR: return "ERROR";
        case L_WARN: return "WARN ";
        case L_INFO: return "INFO ";
        case L_DEBUG: return "DEBUG";
        case L_TRACE: return "TRACE";
    }
    return "?????";
}

void astra_log(log_level_t lvl, const char *fmt, ...) {
    if (lvl > g_log_level) return;
    char msg[4096];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(msg, sizeof(msg), fmt, ap);
    va_end(ap);
    uint64_t t = wall_ms();
    time_t sec = (time_t)(t / 1000);
    struct tm tmv;
    localtime_r(&sec, &tmv);
    char ts[32];
    strftime(ts, sizeof(ts), "%H:%M:%S", &tmv);
    fprintf(stderr, "%s.%03d [%s] %s\n", ts, (int)(t % 1000), lvl_name(lvl), msg);
    fflush(stderr);
}

/* --------------------------------------------------------------------- time */

uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ull + (uint64_t)(ts.tv_nsec / 1000000ull);
}

uint64_t wall_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return (uint64_t)ts.tv_sec * 1000ull + (uint64_t)(ts.tv_nsec / 1000000ull);
}

void sleep_ms(int ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
}

/* ------------------------------------------------------------------ strings */

char *astr_dupn(const char *s, size_t n) {
    char *r = (char *)malloc(n + 1);
    if (!r) return NULL;
    memcpy(r, s, n);
    r[n] = 0;
    return r;
}

char *astr_dup(const char *s) { return s ? astr_dupn(s, strlen(s)) : NULL; }

char *astr_lower(char *s) {
    for (char *p = s; p && *p; ++p) *p = (char)tolower((unsigned char)*p);
    return s;
}

char *astr_lower_dup(const char *s) {
    char *d = astr_dup(s);
    return astr_lower(d);
}

int astr_case_eq(const char *a, const char *b) {
    if (!a || !b) return 0;
    while (*a && *b) {
        if (tolower((unsigned char)*a) != tolower((unsigned char)*b)) return 0;
        a++;
        b++;
    }
    return *a == 0 && *b == 0;
}

int astr_starts_ci(const char *s, const char *prefix) {
    if (!s || !prefix) return 0;
    size_t n = strlen(prefix);
    return strlen(s) >= n && strncasecmp(s, prefix, n) == 0;
}

int astr_ends_ci(const char *s, const char *suffix) {
    if (!s || !suffix) return 0;
    size_t n = strlen(suffix), m = strlen(s);
    return m >= n && strncasecmp(s + m - n, suffix, n) == 0;
}

char *astr_trim(char *s) {
    if (!s) return NULL;
    while (*s && isspace((unsigned char)*s)) s++;
    size_t n = strlen(s);
    while (n > 0 && isspace((unsigned char)s[n - 1])) s[--n] = 0;
    return s;
}

int astr_contains_ci(const char *hay, const char *needle) {
    if (!hay || !needle || !*needle) return 0;
    size_t n = strlen(hay), m = strlen(needle);
    for (size_t i = 0; i + m <= n; i++)
        if (strncasecmp(hay + i, needle, m) == 0) return 1;
    return 0;
}

void astr_hex(const uint8_t *data, size_t len, char *out) {
    static const char *hexd = "0123456789abcdef";
    for (size_t i = 0; i < len; i++) {
        out[i * 2] = hexd[data[i] >> 4];
        out[i * 2 + 1] = hexd[data[i] & 15];
    }
    out[len * 2] = 0;
}

/* -------------------------------------------------------------------- sha1 */

typedef struct {
    uint32_t h[5];
    uint8_t block[64];
    size_t blocklen;
    uint64_t bitlen;
} sha1_ctx;

static uint32_t rol32(uint32_t v, int bits) { return (v << bits) | (v >> (32 - bits)); }

static void sha1_compress(sha1_ctx *c) {
    uint32_t w[80];
    for (int i = 0; i < 16; i++) {
        w[i] = ((uint32_t)c->block[i * 4] << 24) | ((uint32_t)c->block[i * 4 + 1] << 16) |
               ((uint32_t)c->block[i * 4 + 2] << 8) | ((uint32_t)c->block[i * 4 + 3]);
    }
    for (int i = 16; i < 80; i++) w[i] = rol32(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    uint32_t a = c->h[0], b = c->h[1], cc = c->h[2], d = c->h[3], e = c->h[4];
    for (int i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20) {
            f = (b & cc) | ((~b) & d);
            k = 0x5A827999u;
        } else if (i < 40) {
            f = b ^ cc ^ d;
            k = 0x6ED9EBA1u;
        } else if (i < 60) {
            f = (b & cc) | (b & d) | (cc & d);
            k = 0x8F1BBCDCu;
        } else {
            f = b ^ cc ^ d;
            k = 0xCA62C1D6u;
        }
        uint32_t tmp = rol32(a, 5) + f + e + k + w[i];
        e = d;
        d = cc;
        cc = rol32(b, 30);
        b = a;
        a = tmp;
    }
    c->h[0] += a;
    c->h[1] += b;
    c->h[2] += cc;
    c->h[3] += d;
    c->h[4] += e;
}

static void sha1_update(sha1_ctx *c, const uint8_t *data, size_t len) {
    for (size_t i = 0; i < len; i++) {
        c->block[c->blocklen++] = data[i];
        c->bitlen += 8;
        if (c->blocklen == 64) {
            sha1_compress(c);
            c->blocklen = 0;
        }
    }
}

static void sha1_final(sha1_ctx *c, uint8_t out[20]) {
    uint64_t bits = c->bitlen;
    uint8_t pad = 0x80;
    sha1_update(c, &pad, 1);
    while (c->blocklen != 56) {
        uint8_t z = 0;
        sha1_update(c, &z, 1);
    }
    uint8_t lenbytes[8];
    for (int i = 0; i < 8; i++) lenbytes[i] = (uint8_t)(bits >> (56 - i * 8));
    sha1_update(c, lenbytes, 8);
    for (int i = 0; i < 5; i++) {
        out[i * 4] = (uint8_t)(c->h[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(c->h[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(c->h[i] >> 8);
        out[i * 4 + 3] = (uint8_t)(c->h[i]);
    }
}

void astra_sha1(const uint8_t *data, size_t len, uint8_t out[20]) {
    sha1_ctx c;
    c.h[0] = 0x67452301u;
    c.h[1] = 0xEFCDAB89u;
    c.h[2] = 0x98BADCFEu;
    c.h[3] = 0x10325476u;
    c.h[4] = 0xC3D2E1F0u;
    c.blocklen = 0;
    c.bitlen = 0;
    sha1_update(&c, data, len);
    sha1_final(&c, out);
}

void astra_sha1_hex(const uint8_t *data, size_t len, char out[41]) {
    uint8_t d[20];
    astra_sha1(data, len, d);
    astr_hex(d, 20, out);
}

/* ------------------------------------------------------------------ base64 */

static const char B64T[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

char *b64_encode(const uint8_t *data, size_t len) {
    size_t olen = 4 * ((len + 2) / 3);
    char *out = (char *)malloc(olen + 1);
    if (!out) return NULL;
    size_t i = 0, o = 0;
    while (i < len) {
        uint32_t v = 0;
        int nb = 0;
        for (int k = 0; k < 3 && i < len; k++, i++) {
            v = (v << 8) | data[i];
            nb++;
        }
        v <<= (3 - nb) * 8;
        out[o++] = B64T[(v >> 18) & 63];
        out[o++] = B64T[(v >> 12) & 63];
        out[o++] = nb > 1 ? B64T[(v >> 6) & 63] : '=';
        out[o++] = nb > 2 ? B64T[v & 63] : '=';
    }
    out[o] = 0;
    return out;
}

static int b64_val(char c) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

uint8_t *b64_decode(const char *s, size_t len, size_t *out_len) {
    if (!s) return NULL;
    if (len == (size_t)-1) len = strlen(s);
    uint8_t *out = (uint8_t *)malloc(len / 4 * 3 + 4);
    if (!out) return NULL;
    size_t o = 0;
    uint32_t acc = 0;
    int nb = 0;
    for (size_t i = 0; i < len; i++) {
        char c = s[i];
        if (c == '\r' || c == '\n' || c == ' ' || c == '\t') continue;
        if (c == '=') break;
        int v = b64_val(c);
        if (v < 0) {
            free(out);
            return NULL;
        }
        acc = (acc << 6) | (uint32_t)v;
        nb++;
        if (nb == 4) {
            out[o++] = (uint8_t)(acc >> 16);
            out[o++] = (uint8_t)(acc >> 8);
            out[o++] = (uint8_t)(acc);
            acc = 0;
            nb = 0;
        }
    }
    if (nb == 3) {
        acc <<= 6;
        out[o++] = (uint8_t)(acc >> 16);
        out[o++] = (uint8_t)(acc >> 8);
    } else if (nb == 2) {
        acc <<= 12;
        out[o++] = (uint8_t)(acc >> 16);
    }
    *out_len = o;
    return out;
}

int random_bytes(uint8_t *out, size_t n) {
    static int seeded = 0;
    if (!seeded) {
        srand((unsigned)(wall_ms() ^ (uintptr_t)&seeded));
        seeded = 1;
    }
    int fd = open("/dev/urandom", O_RDONLY);
    if (fd >= 0) {
        ssize_t r = read(fd, out, n);
        close(fd);
        if (r == (ssize_t)n) return 0;
    }
    for (size_t i = 0; i < n; i++) out[i] = (uint8_t)(rand() & 0xff);
    return 0;
}

/* --------------------------------------------------------------------- urls */

int url_parse(const char *url, url_t *out) {
    if (!url || !out) return -1;
    memset(out, 0, sizeof(*out));
    const char *p = url;
    const char *sc = strstr(p, "://");
    if (sc) {
        size_t n = (size_t)(sc - p);
        if (n < sizeof(out->scheme)) {
            memcpy(out->scheme, p, n);
            out->scheme[n] = 0;
            astr_lower(out->scheme);
        }
        p = sc + 3;
    } else {
        snprintf(out->scheme, sizeof(out->scheme), "http");
    }
    const char *hostend = p + strcspn(p, "/?#");
    const char *at = memchr(p, '@', (size_t)(hostend - p));
    if (at) p = at + 1; /* drop userinfo */
    const char *colon = NULL;
    if (*p == '[') { /* ipv6 */
        const char *br = strchr(p, ']');
        if (br) {
            size_t n = (size_t)(br - p + 1);
            if (n < sizeof(out->host)) memcpy(out->host, p, n);
            p = br + 1;
        }
    } else {
        colon = memchr(p, ':', (size_t)(hostend - p));
        if (colon) {
            size_t n = (size_t)(colon - p);
            if (n < sizeof(out->host)) {
                memcpy(out->host, p, n);
                out->host[n] = 0;
            }
            out->port = atoi(colon + 1);
            p = colon;
        }
    }
    if (!out->host[0]) {
        size_t n = (size_t)(hostend - p);
        if (n >= sizeof(out->host)) n = sizeof(out->host) - 1;
        memcpy(out->host, p, n);
        out->host[n] = 0;
    }
    astr_lower(out->host);
    if (!out->port) out->port = strcmp(out->scheme, "https") == 0 ? 443 : 80;
    const char *rest = hostend;
    if (*rest == '/') {
        const char *q = strchr(rest, '?');
        size_t n = q ? (size_t)(q - rest) : strlen(rest);
        if (n >= sizeof(out->path)) n = sizeof(out->path) - 1;
        memcpy(out->path, rest, n);
        out->path[n] = 0;
        if (q) {
            const char *f = strchr(q + 1, '#');
            size_t m = f ? (size_t)(f - q - 1) : strlen(q + 1);
            if (m >= sizeof(out->query)) m = sizeof(out->query) - 1;
            memcpy(out->query, q + 1, m);
            out->query[m] = 0;
        }
    } else if (!out->path[0]) {
        strcpy(out->path, "/");
    }
    return 0;
}

int url_host(const char *url, char *out, size_t n) {
    url_t u;
    if (url_parse(url, &u) != 0) return -1;
    snprintf(out, n, "%s", u.host);
    return 0;
}

int url_is_http_family(const char *url) {
    url_t u;
    if (url_parse(url, &u) != 0) return 0;
    return strcmp(u.scheme, "http") == 0 || strcmp(u.scheme, "https") == 0 ||
           strcmp(u.scheme, "ws") == 0 || strcmp(u.scheme, "wss") == 0;
}

const char *url_path_ptr(const url_t *u) { return u->path[0] ? u->path : "/"; }

/* Very small public-suffix approximation: enough for third-party detection. */
static const char *TWO_LEVEL[] = {
    "co.uk", "org.uk", "gov.uk", "ac.uk", "co.id", "or.id", "ac.id", "sch.id", "go.id",
    "web.id", "my.id", "biz.id", "co.jp", "or.jp", "ne.jp", "ac.jp", "go.jp", "co.kr",
    "or.kr", "ne.kr", "com.au", "net.au", "org.au", "edu.au", "co.nz", "com.br",
    "com.mx", "com.ar", "com.tr", "com.cn", "net.cn", "org.cn", "com.sg", "com.tw",
    "com.hk", "co.za", "co.in", "net.in", "org.in", "com.my", "com.ph", "com.vn",
    "com.pk", "co.th", "com.ua", "com.pl", "com.ng", NULL};

void url_registrable_domain(const char *host, char *out, size_t n) {
    if (!host) {
        if (n) out[0] = 0;
        return;
    }
    /* split host into labels; labels[nl-1] is the TLD */
    const char *labels[8];
    int nl = 0;
    const char *start = host;
    while (nl < 8) {
        labels[nl++] = start;
        const char *dot = strchr(start, '.');
        if (!dot) break;
        start = dot + 1;
    }
    int take = nl >= 2 ? 2 : 1;
    if (nl >= 3) {
        const char *two = labels[nl - 2]; /* e.g. "co.id" */
        for (int i = 0; TWO_LEVEL[i]; i++)
            if (strcmp(two, TWO_LEVEL[i]) == 0) {
                take = 3;
                break;
            }
    }
    if (take > nl) take = nl;
    snprintf(out, n, "%s", labels[nl - take]);
}

int url_same_site(const char *hosta, const char *hostb) {
    if (!hosta || !hostb) return 0;
    char a[256], b[256];
    url_registrable_domain(hosta, a, sizeof(a));
    url_registrable_domain(hostb, b, sizeof(b));
    return a[0] && strcmp(a, b) == 0;
}

int host_matches_domain(const char *host, const char *domain) {
    if (!host || !domain || !*domain) return 0;
    if (strcmp(host, domain) == 0) return 1;
    size_t hl = strlen(host), dl = strlen(domain);
    if (hl > dl && host[hl - dl - 1] == '.' && strncasecmp(host + hl - dl, domain, dl) == 0) return 1;
    return 0;
}

/* -------------------------------------------------------------------- files */

int file_read(const char *path, uint8_t **out, size_t *out_len) {
    FILE *f = fopen(path, "rb");
    if (!f) return -1;
    if (fseek(f, 0, SEEK_END) != 0) {
        fclose(f);
        return -1;
    }
    long sz = ftell(f);
    if (sz < 0) {
        fclose(f);
        return -1;
    }
    fseek(f, 0, SEEK_SET);
    uint8_t *data = (uint8_t *)malloc((size_t)sz + 1);
    if (!data) {
        fclose(f);
        return -1;
    }
    size_t got = fread(data, 1, (size_t)sz, f);
    fclose(f);
    data[got] = 0;
    *out = data;
    *out_len = got;
    return 0;
}

int file_write(const char *path, const void *data, size_t len) {
    FILE *f = fopen(path, "wb");
    if (!f) return -1;
    if (len) fwrite(data, 1, len, f);
    fclose(f);
    return 0;
}

int file_append(const char *path, const void *data, size_t len) {
    FILE *f = fopen(path, "ab");
    if (!f) return -1;
    if (len) fwrite(data, 1, len, f);
    fclose(f);
    return 0;
}

int file_exists(const char *path) {
    struct stat st;
    return stat(path, &st) == 0;
}

int64_t file_size(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) return -1;
    return (int64_t)st.st_size;
}

int mkdir_p(const char *path, int mode) {
    char tmp[PATH_MAX];
    size_t len = strlen(path);
    if (len >= sizeof(tmp)) return -1;
    memcpy(tmp, path, len + 1);
    for (size_t i = 1; i < len; i++) {
        if (tmp[i] == '/') {
            tmp[i] = 0;
            if (mkdir(tmp, (mode_t)mode) != 0 && errno != EEXIST) {
                struct stat st;
                if (stat(tmp, &st) != 0) return -1;
            }
            tmp[i] = '/';
        }
    }
    if (mkdir(tmp, (mode_t)mode) != 0 && errno != EEXIST) {
        struct stat st;
        if (stat(tmp, &st) != 0) return -1;
    }
    return 0;
}

void path_join(char *out, size_t n, const char *a, const char *b) {
    if (!a || !a[0]) {
        snprintf(out, n, "%s", b ? b : "");
        return;
    }
    size_t la = strlen(a);
    if (la > 0 && a[la - 1] == '/')
        snprintf(out, n, "%s%s", a, b ? b : "");
    else
        snprintf(out, n, "%s/%s", a, b ? b : "");
}

char *expand_home(const char *path) {
    if (!path) return NULL;
    if (path[0] == '~' && (path[1] == '/' || path[1] == 0)) {
        const char *home = getenv("HOME");
        if (!home) home = "/tmp";
        char *out = (char *)malloc(strlen(home) + strlen(path) + 2);
        snprintf(out, strlen(home) + strlen(path) + 2, "%s%s", home, path + 1);
        return out;
    }
    return astr_dup(path);
}

int rm_rf(const char *path) {
    /* shallow: files only (we only use it for cache dirs we created) */
    char cmd[PATH_MAX + 32];
    snprintf(cmd, sizeof(cmd), "rm -rf \"%s\" >/dev/null 2>&1", path);
    return system(cmd);
}
