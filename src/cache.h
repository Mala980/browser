/* astra/cache.h - on-disk HTTP response cache with revalidation support */
#ifndef ASTRA_CACHE_H
#define ASTRA_CACHE_H

#include "util.h"

typedef struct {
    char dir[1024];
    size_t max_bytes;
    int enabled;
} cache_t;

typedef struct {
    int found;
    int fresh;
    int status;
    char ctype[128];
    char etag[512];
    char last_modified[512];
    uint8_t *body;
    size_t body_len;
    uint64_t stored_at;
    uint64_t expires_at;
    long long max_age;
} cache_entry_t;

int cache_init(cache_t *c, const char *dir, size_t max_bytes);
int cache_get(cache_t *c, const char *url, cache_entry_t *out);
void cache_entry_free(cache_entry_t *e);
int cache_put(cache_t *c, const char *url, int status, const char *ctype, const uint8_t *body,
              size_t len, const char *etag, const char *last_modified, long long max_age);
int cache_touch(cache_t *c, const char *url);
int cache_stats(cache_t *c, size_t *entries, uint64_t *bytes);
int cache_clear(cache_t *c);
int cache_enforce_limit(cache_t *c);

#endif /* ASTRA_CACHE_H */
