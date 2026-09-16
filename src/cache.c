/* astra/cache.c */
#include "cache.h"

#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>
#include <utime.h>

#define CACHE_MAGIC "astra-cache-v1"

static void key_path(const cache_t *c, const char *url, char *body, size_t bn, char *meta,
                     size_t mn) {
    char hex[41];
    astra_sha1_hex((const uint8_t *)url, strlen(url), hex);
    snprintf(body, bn, "%s/%s.body", c->dir, hex);
    snprintf(meta, mn, "%s/%s.meta", c->dir, hex);
}

int cache_init(cache_t *c, const char *dir, size_t max_bytes) {
    memset(c, 0, sizeof(*c));
    if (!dir || !*dir) {
        c->enabled = 0;
        return 0;
    }
    snprintf(c->dir, sizeof(c->dir), "%s", dir);
    c->max_bytes = max_bytes ? max_bytes : (size_t)256 * 1024 * 1024;
    if (mkdir_p(c->dir, 0755) != 0) {
        LOGW("cache: cannot create %s", c->dir);
        c->enabled = 0;
        return -1;
    }
    c->enabled = 1;
    return 0;
}

static int read_meta_line(FILE *f, char *key, size_t kn, char *val, size_t vn) {
    char line[2048];
    if (!fgets(line, sizeof(line), f)) return 0;
    char *tab = strchr(line, '\t');
    if (!tab) return 0;
    *tab = 0;
    snprintf(key, kn, "%s", line);
    char *v = tab + 1;
    size_t vl = strlen(v);
    while (vl && (v[vl - 1] == '\n' || v[vl - 1] == '\r')) v[--vl] = 0;
    snprintf(val, vn, "%s", v);
    return 1;
}

int cache_get(cache_t *c, const char *url, cache_entry_t *out) {
    memset(out, 0, sizeof(*out));
    if (!c || !c->enabled || !url) return 0;
    char bpath[1200], mpath[1200];
    key_path(c, url, bpath, sizeof(bpath), mpath, sizeof(mpath));
    FILE *f = fopen(mpath, "r");
    if (!f) return 0;
    char url_stored[4096];
    url_stored[0] = 0;
    out->max_age = -1;
    char key[128], val[2048];
    while (read_meta_line(f, key, sizeof(key), val, sizeof(val))) {
        if (!strcmp(key, "url")) {
            snprintf(url_stored, sizeof(url_stored), "%s", val);
        } else if (!strcmp(key, "status")) {
            out->status = atoi(val);
        } else if (!strcmp(key, "ctype")) {
            snprintf(out->ctype, sizeof(out->ctype), "%s", val);
        } else if (!strcmp(key, "etag")) {
            snprintf(out->etag, sizeof(out->etag), "%s", val);
        } else if (!strcmp(key, "last_modified")) {
            snprintf(out->last_modified, sizeof(out->last_modified), "%s", val);
        } else if (!strcmp(key, "stored_at")) {
            out->stored_at = strtoull(val, NULL, 10);
        } else if (!strcmp(key, "expires_at")) {
            out->expires_at = strtoull(val, NULL, 10);
        } else if (!strcmp(key, "magic")) {
            if (strcmp(val, CACHE_MAGIC) != 0) {
                fclose(f);
                return 0;
            }
        }
    }
    fclose(f);
    if (url_stored[0] && strcmp(url_stored, url) != 0) return 0;
    if (file_read(bpath, &out->body, &out->body_len) != 0) return 0;
    out->found = 1;
    out->fresh = out->expires_at > wall_ms() ? 1 : 0;
    return 1;
}

void cache_entry_free(cache_entry_t *e) {
    if (!e) return;
    free(e->body);
    e->body = NULL;
    e->body_len = 0;
}

int cache_put(cache_t *c, const char *url, int status, const char *ctype, const uint8_t *body,
              size_t len, const char *etag, const char *last_modified, long long max_age) {
    if (!c || !c->enabled || !url || !body) return -1;
    char bpath[1200], mpath[1200];
    key_path(c, url, bpath, sizeof(bpath), mpath, sizeof(mpath));
    if (file_write(bpath, body, len) != 0) return -1;
    uint64_t now = wall_ms();
    uint64_t exp = max_age > 0 ? now + (uint64_t)max_age * 1000ull : now;
    buf_t m;
    buf_init(&m);
    buf_appendf(&m, "magic\t%s\n", CACHE_MAGIC);
    buf_appendf(&m, "url\t%s\n", url);
    buf_appendf(&m, "status\t%d\n", status);
    buf_appendf(&m, "ctype\t%s\n", ctype ? ctype : "");
    buf_appendf(&m, "etag\t%s\n", etag ? etag : "");
    buf_appendf(&m, "last_modified\t%s\n", last_modified ? last_modified : "");
    buf_appendf(&m, "stored_at\t%llu\n", (unsigned long long)now);
    buf_appendf(&m, "expires_at\t%llu\n", (unsigned long long)exp);
    buf_appendf(&m, "len\t%zu\n", len);
    int rc = file_write(mpath, m.data, m.len);
    buf_free(&m);
    return rc;
}

int cache_touch(cache_t *c, const char *url) {
    char bpath[1200], mpath[1200];
    key_path(c, url, bpath, sizeof(bpath), mpath, sizeof(mpath));
    struct utimbuf tb;
    time_t now = time(NULL);
    tb.actime = now;
    tb.modtime = now;
    utime(bpath, &tb);
    utime(mpath, &tb);
    return 0;
}

typedef struct {
    char path[1200];
    time_t mtime;
    int64_t size;
} evict_item;

static int cmp_mtime(const void *a, const void *b) {
    const evict_item *x = (const evict_item *)a;
    const evict_item *y = (const evict_item *)b;
    if (x->mtime < y->mtime) return -1;
    if (x->mtime > y->mtime) return 1;
    return 0;
}

int cache_stats(cache_t *c, size_t *entries, uint64_t *bytes) {
    if (!c || !c->enabled) return -1;
    DIR *d = opendir(c->dir);
    if (!d) return -1;
    struct dirent *de;
    size_t n = 0;
    uint64_t total = 0;
    char path[1200];
    struct stat st;
    while ((de = readdir(d))) {
        if (strstr(de->d_name, ".body") == NULL) continue;
        snprintf(path, sizeof(path), "%s/%s", c->dir, de->d_name);
        if (stat(path, &st) == 0) {
            n++;
            total += (uint64_t)st.st_size;
        }
    }
    closedir(d);
    if (entries) *entries = n;
    if (bytes) *bytes = total;
    return 0;
}

static void cache_evict_to(cache_t *c, uint64_t target) {
    DIR *d = opendir(c->dir);
    if (!d) return;
    struct dirent *de;
    evict_item *items = NULL;
    size_t n = 0, cap = 0;
    uint64_t total = 0;
    char path[1200];
    struct stat st;
    while ((de = readdir(d))) {
        if (strstr(de->d_name, ".body") == NULL) continue;
        snprintf(path, sizeof(path), "%s/%s", c->dir, de->d_name);
        if (stat(path, &st) != 0) continue;
        if (n == cap) {
            cap = cap ? cap * 2 : 128;
            items = (evict_item *)realloc(items, cap * sizeof(evict_item));
            if (!items) break;
        }
        snprintf(items[n].path, sizeof(items[n].path), "%s", path);
        items[n].mtime = st.st_mtime;
        items[n].size = (int64_t)st.st_size;
        total += (uint64_t)st.st_size;
        n++;
    }
    closedir(d);
    if (!items) return;
    qsort(items, n, sizeof(evict_item), cmp_mtime);
    for (size_t i = 0; i < n && total > target; i++) {
        unlink(items[i].path);
        char meta[1300];
        snprintf(meta, sizeof(meta), "%s", items[i].path);
        char *dot = strstr(meta, ".body");
        if (dot) {
            strcpy(dot, ".meta");
            unlink(meta);
        }
        total -= (uint64_t)items[i].size;
    }
    free(items);
}

int cache_clear(cache_t *c) {
    if (!c || !c->enabled) return -1;
    cache_evict_to(c, 0);
    return 0;
}

/* exported for tests / periodic maintenance */
int cache_enforce_limit(cache_t *c) {
    uint64_t total = 0;
    size_t n = 0;
    if (cache_stats(c, &n, &total) != 0) return -1;
    if (total > c->max_bytes) cache_evict_to(c, c->max_bytes * 9 / 10);
    return 0;
}
