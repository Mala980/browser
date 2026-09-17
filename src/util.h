/* astra/util.h - small zero-dependency utilities (buffers, logging, sha1, base64, url) */
#ifndef ASTRA_UTIL_H
#define ASTRA_UTIL_H

#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* ------------------------------------------------------------------ buffers */
typedef struct {
    uint8_t *data;
    size_t len;
    size_t cap;
} buf_t;

void buf_init(buf_t *b);
void buf_free(buf_t *b);
void buf_reserve(buf_t *b, size_t extra);
void buf_append(buf_t *b, const void *p, size_t n);
void buf_appendc(buf_t *b, char c);
void buf_appendstr(buf_t *b, const char *s);
void buf_appendf(buf_t *b, const char *fmt, ...);
void buf_clear(buf_t *b);
void buf_consume(buf_t *b, size_t n); /* drop n bytes from the front */

/* ------------------------------------------------------------------ logging */
typedef enum { L_ERROR = 0, L_WARN, L_INFO, L_DEBUG, L_TRACE } log_level_t;

extern log_level_t g_log_level;
extern int g_log_colors;

void astra_log(log_level_t lvl, const char *fmt, ...);
#define LOGE(...) astra_log(L_ERROR, __VA_ARGS__)
#define LOGW(...) astra_log(L_WARN, __VA_ARGS__)
#define LOGI(...) astra_log(L_INFO, __VA_ARGS__)
#define LOGD(...) astra_log(L_DEBUG, __VA_ARGS__)
#define LOGT(...) astra_log(L_TRACE, __VA_ARGS__)

/* --------------------------------------------------------------------- time */
uint64_t now_ms(void);      /* monotonic */
uint64_t wall_ms(void);     /* epoch, milliseconds */
void sleep_ms(int ms);

/* ------------------------------------------------------------------ strings */
char *astr_dup(const char *s);
char *astr_dupn(const char *s, size_t n);
char *astr_lower(char *s);
char *astr_lower_dup(const char *s);
int astr_case_eq(const char *a, const char *b);
int astr_starts_ci(const char *s, const char *prefix);
int astr_ends_ci(const char *s, const char *suffix);
char *astr_trim(char *s);
int astr_contains_ci(const char *hay, const char *needle);
void *astr_memmem(const void *hay, size_t haylen, const void *needle, size_t needlelen);
void astr_hex(const uint8_t *data, size_t len, char *out);

/* ------------------------------------------------------------------ crypto */
void astra_sha1(const uint8_t *data, size_t len, uint8_t out[20]);
void astra_sha1_hex(const uint8_t *data, size_t len, char out[41]);
char *b64_encode(const uint8_t *data, size_t len);              /* malloc'd, NUL terminated */
uint8_t *b64_decode(const char *s, size_t len, size_t *out_len); /* malloc'd  */
int random_bytes(uint8_t *out, size_t n);

/* --------------------------------------------------------------------- urls */
typedef struct {
    char scheme[16];
    char host[256];
    int port;
    char path[2048];
    char query[2048];
} url_t;

int url_parse(const char *url, url_t *out);
int url_host(const char *url, char *out, size_t n); /* host only, no port */
int url_is_http_family(const char *url);
const char *url_path_ptr(const url_t *u);
void url_registrable_domain(const char *host, char *out, size_t n); /* eTLD+1 approximation */
int url_same_site(const char *hosta, const char *hostb);
int host_matches_domain(const char *host, const char *domain); /* host == d || host ends ".d" */

/* -------------------------------------------------------------------- files */
int file_read(const char *path, uint8_t **out, size_t *out_len);
int file_write(const char *path, const void *data, size_t len);
int file_append(const char *path, const void *data, size_t len);
int file_exists(const char *path);
int64_t file_size(const char *path);
int mkdir_p(const char *path, int mode);
void path_join(char *out, size_t n, const char *a, const char *b);
char *expand_home(const char *path); /* malloc'd */
int rm_rf(const char *path);

/* Directory for temporary files.  Android has no /tmp, so honour TMPDIR first
 * and only use /tmp when it is really there - a hardcoded /tmp breaks every
 * test and every temporary profile dir on a phone. */
const char *astra_tmpdir(void);

#endif /* ASTRA_UTIL_H */
