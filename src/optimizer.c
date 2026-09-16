/* astra/optimizer.c */
#include "optimizer.h"

#include "image.h"
#include "minify.h"
#include "stats.h"

#include <stdlib.h>

typedef struct opt_pending {
    int id; /* CDP id of our Fetch.getResponseBody, 0 if none */
    char request_id[128];
    char session[128];
    char url[2048];
    char method[16];
    char resource_type[32];
    char page_host[256];
    int status;
    int inm_injected;
    long long content_len;
    long long max_age;
    int stage;
    struct opt_pending *next;
} opt_pending_t;

typedef struct {
    char session[128];
    char host[256];
} page_host_t;

struct optimizer {
    const astra_config *cfg;
    filters_t filters;
    cache_t cache;
    cdp_send_fn send;
    void *send_ud;
    opt_pending_t *pending;
    page_host_t hosts[64];
    int nhosts;
    int next_id;
};

/* ------------------------------------------------------------------ helpers */

static void send_cmd(optimizer_t *o, const char *session, const char *method, json_t *params) {
    json_t *msg = jobj();
    jset(msg, "id", jnum((double)(ASTRA_OPTIMIZER_ID_BASE + (o->next_id++ % 99999))));
    jset(msg, "method", jstr(method));
    if (params) jset(msg, "params", params);
    if (session) jset(msg, "sessionId", jstr(session));
    o->send(o->send_ud, session, msg);
    json_free(msg);
}

static void fail_request(optimizer_t *o, const char *session, const char *request_id,
                         const char *reason) {
    json_t *p = jobj();
    jset(p, "requestId", jstr(request_id));
    jset(p, "errorReason", jstr(reason));
    send_cmd(o, session, "Fetch.failRequest", p);
}

static json_t *headers_to_array(json_t *headers_obj) {
    json_t *arr = jarr();
    if (!headers_obj || headers_obj->type != J_OBJ) return arr;
    for (size_t i = 0; i < headers_obj->n; i++) {
        json_t *h = jobj();
        jset(h, "name", jstr(headers_obj->keys[i]));
        jset(h, "value", jstr(json_as_str(headers_obj->items[i], "")));
        jpush(arr, h);
    }
    return arr;
}

static void array_set_header(json_t *arr, const char *name, const char *value) {
    int found = 0;
    for (size_t i = 0; i < arr->n; i++) {
        json_t *h = arr->items[i];
        const char *hn = json_get_str(h, "name", NULL);
        if (hn && !strcasecmp(hn, name)) {
            jset(h, "value", jstr(value));
            found = 1;
            break;
        }
    }
    if (!found) {
        json_t *h = jobj();
        jset(h, "name", jstr(name));
        jset(h, "value", jstr(value));
        jpush(arr, h);
    }
}

static void array_del_header(json_t *arr, const char *name) {
    for (size_t i = 0; i < arr->n; i++) {
        json_t *h = arr->items[i];
        const char *hn = json_get_str(h, "name", NULL);
        if (hn && !strcasecmp(hn, name)) {
            json_free(h);
            for (size_t k = i + 1; k < arr->n; k++) {
                arr->items[k - 1] = arr->items[k];
                arr->keys[k - 1] = arr->keys[k];
            }
            arr->n--;
            return;
        }
    }
}

static const char *array_get_header(json_t *arr, const char *name) {
    for (size_t i = 0; i < arr->n; i++) {
        json_t *h = arr->items[i];
        const char *hn = json_get_str(h, "name", NULL);
        if (hn && !strcasecmp(hn, name)) return json_get_str(h, "value", NULL);
    }
    return NULL;
}

static void continue_request(optimizer_t *o, const char *session, const char *request_id,
                             json_t *headers, int intercept_response) {
    json_t *p = jobj();
    jset(p, "requestId", jstr(request_id));
    if (headers) jset(p, "headers", json_clone(headers)); /* send_cmd takes ownership */
    if (intercept_response) jset(p, "interceptResponse", jbool(1));
    send_cmd(o, session, "Fetch.continueRequest", p);
}

static void fulfill_request(optimizer_t *o, const char *session, const char *request_id, int status,
                            json_t *headers, const uint8_t *body, size_t len) {
    json_t *p = jobj();
    jset(p, "requestId", jstr(request_id));
    jset(p, "responseCode", jnum((double)status));
    if (headers) jset(p, "responseHeaders", json_clone(headers));
    char *b64 = b64_encode(body, len);
    jset(p, "body", jstr(b64));
    free(b64);
    send_cmd(o, session, "Fetch.fulfillRequest", p);
}

static opt_pending_t *pending_add(optimizer_t *o, const char *request_id, const char *session) {
    opt_pending_t *p = (opt_pending_t *)calloc(1, sizeof(opt_pending_t));
    snprintf(p->request_id, sizeof(p->request_id), "%s", request_id);
    snprintf(p->session, sizeof(p->session), "%s", session ? session : "");
    p->next = o->pending;
    o->pending = p;
    return p;
}

static opt_pending_t *pending_find(optimizer_t *o, const char *request_id) {
    for (opt_pending_t *p = o->pending; p; p = p->next)
        if (!strcmp(p->request_id, request_id)) return p;
    return NULL;
}

static void pending_del(optimizer_t *o, const char *request_id) {
    opt_pending_t **pp = &o->pending;
    while (*pp) {
        if (!strcmp((*pp)->request_id, request_id)) {
            opt_pending_t *d = *pp;
            *pp = d->next;
            free(d);
            return;
        }
        pp = &(*pp)->next;
    }
}

/* --------------------------------------------------------------- lifecycle */

optimizer_t *optimizer_new(const astra_config *cfg, cdp_send_fn send, void *send_ud) {
    optimizer_t *o = (optimizer_t *)calloc(1, sizeof(optimizer_t));
    o->cfg = cfg;
    o->send = send;
    o->send_ud = send_ud;
    o->next_id = 1;
    filters_init(&o->filters);
    filters_load_builtin(&o->filters);
    if (cfg->filter_list[0] && filters_load_file(&o->filters, cfg->filter_list) == 0)
        LOGI("filters: loaded %s (%zu rules, %zu unsupported lines skipped)", cfg->filter_list,
             o->filters.n, o->filters.skipped_lines);
    else
        LOGI("filters: %zu builtin rules active", o->filters.n);
    if (cfg->cache_enabled) {
        char dir[1200];
        snprintf(dir, sizeof(dir), "%s", cfg->cache_dir);
        char *exp = expand_home(dir);
        snprintf(dir, sizeof(dir), "%s", exp ? exp : cfg->cache_dir);
        free(exp);
        if (cache_init(&o->cache, dir, cfg->cache_max_bytes) == 0) {
            size_t entries = 0;
            uint64_t bytes = 0;
            cache_stats(&o->cache, &entries, &bytes);
            LOGI("cache: %s (%zu entries, %.1f MB)", dir, entries, bytes / 1048576.0);
        }
    }
    return o;
}

void optimizer_free(optimizer_t *o) {
    if (!o) return;
    filters_free(&o->filters);
    opt_pending_t *p = o->pending;
    while (p) {
        opt_pending_t *nx = p->next;
        free(p);
        p = nx;
    }
    free(o);
}

size_t optimizer_rule_count(const optimizer_t *o) { return o ? o->filters.n : 0; }
size_t optimizer_session_count(const optimizer_t *o) {
    size_t n = 0;
    for (opt_pending_t *p = o->pending; p; p = p->next) n++;
    return n;
}

void optimizer_set_page_host(optimizer_t *o, const char *engine_session, const char *url) {
    if (!o || !url) return;
    char host[256];
    if (url_host(url, host, sizeof(host)) != 0) return;
    const char *sess = engine_session ? engine_session : "";
    for (int i = 0; i < o->nhosts; i++)
        if (!strcmp(o->hosts[i].session, sess)) {
            snprintf(o->hosts[i].host, sizeof(o->hosts[i].host), "%s", host);
            return;
        }
    if (o->nhosts < 64) {
        snprintf(o->hosts[o->nhosts].session, sizeof(o->hosts[o->nhosts].session), "%s", sess);
        snprintf(o->hosts[o->nhosts].host, sizeof(o->hosts[o->nhosts].host), "%s", host);
        o->nhosts++;
    }
}

static const char *page_host_for(optimizer_t *o, const char *session) {
    const char *sess = session ? session : "";
    for (int i = 0; i < o->nhosts; i++)
        if (!strcmp(o->hosts[i].session, sess) && o->hosts[i].host[0]) return o->hosts[i].host;
    return NULL;
}

/* ------------------------------------------------------------ request stage */

/* strcasestr is a GNU extension; local bounded version */
static const char *strcasestr_len_static(const char *hay, const char *needle) {
    size_t n = strlen(needle), m = strlen(hay);
    if (n > m) return NULL;
    for (size_t i = 0; i + n <= m; i++)
        if (strncasecmp(hay + i, needle, n) == 0) return hay + i;
    return NULL;
}

static long long max_age_from_headers(json_t *headers) {
    const char *cc = array_get_header(headers, "Cache-Control");
    if (!cc) cc = array_get_header(headers, "cache-control");
    if (!cc) return -1;
    const char *p = strcasestr_len_static(cc, "max-age=");
    if (!p) return -1;
    return atoll(p + strlen("max-age="));
}

static void handle_request_stage(optimizer_t *o, const char *session, json_t *params) {
    json_t *req = json_get(params, "request");
    if (!req) return;
    const char *url = json_get_str(req, "url", "");
    const char *method = json_get_str(req, "method", "GET");
    const char *rtype = json_get_str(params, "resourceType", "Other");
    const char *request_id = json_get_str(params, "requestId", "");
    if (!request_id[0]) return;

    g_stats.requests++;

    /* page host bookkeeping */
    char host[256];
    url_host(url, host, sizeof(host));
    if (!strcmp(rtype, "Document")) optimizer_set_page_host(o, session, url);
    const char *page_host = page_host_for(o, session);
    if (!page_host) {
        json_t *hdrs = json_get(req, "headers");
        const char *ref = hdrs ? json_get_str(hdrs, "Referer", NULL) : NULL;
        static char refhost[256];
        if (ref && url_host(ref, refhost, sizeof(refhost)) == 0) page_host = refhost;
        else page_host = host;
    }

    /* 1. ad / tracker blocking */
    if (o->cfg->lite && o->cfg->block_ads &&
        filters_is_blocked(&o->filters, url, page_host, rtype)) {
        g_stats.blocked++;
        LOGD("block %s (%s)", url, rtype);
        fail_request(o, session, request_id, "BlockedByClient");
        return;
    }
    /* 2. video policy */
    if (o->cfg->lite && o->cfg->video == VIDEO_BLOCK && !strcmp(rtype, "Media")) {
        g_stats.blocked++;
        fail_request(o, session, request_id, "BlockedByClient");
        return;
    }

    opt_pending_t *p = pending_add(o, request_id, session);
    snprintf(p->url, sizeof(p->url), "%s", url);
    snprintf(p->method, sizeof(p->method), "%s", method);
    snprintf(p->resource_type, sizeof(p->resource_type), "%s", rtype);
    snprintf(p->page_host, sizeof(p->page_host), "%s", page_host);
    p->stage = 1;

    json_t *headers = headers_to_array(json_get(req, "headers"));

    /* 3. cache */
    if (o->cfg->lite && o->cfg->cache_enabled && o->cache.enabled && !strcmp(method, "GET")) {
        cache_entry_t e;
        if (cache_get(&o->cache, url, &e)) {
            if (e.fresh && e.body_len) {
                json_t *rh = jarr();
                array_set_header(rh, "Content-Type", e.ctype[0] ? e.ctype : "application/octet-stream");
                array_set_header(rh, "Content-Length", "");
                char clen[32];
                snprintf(clen, sizeof(clen), "%zu", e.body_len);
                array_set_header(rh, "Content-Length", clen);
                array_set_header(rh, "X-Astra-Cache", "HIT");
                fulfill_request(o, session, request_id, e.status ? e.status : 200, rh, e.body,
                                e.body_len);
                g_stats.cache_hits++;
                g_stats.bytes_original += e.body_len;
                g_stats.bytes_saved_cache += e.body_len;
                LOGD("cache hit %s (%zu B)", url, e.body_len);
                cache_entry_free(&e);
                json_free(rh);
                pending_del(o, request_id);
                return;
            }
            if (e.etag[0]) {
                array_set_header(headers, "If-None-Match", e.etag);
                p->inm_injected = 1;
            }
            if (e.last_modified[0]) {
                array_set_header(headers, "If-Modified-Since", e.last_modified);
                p->inm_injected = 1;
            }
            cache_entry_free(&e);
        }
    }

    /* 4. header tuning */
    if (o->cfg->lite) {
        int is_image = !strcmp(rtype, "Image");
        int is_doc = !strcmp(rtype, "Document");
        int is_xhr = !strcmp(rtype, "XHR") || !strcmp(rtype, "Fetch");
        if (o->cfg->save_data && (is_image || is_doc) && !is_xhr)
            array_set_header(headers, "Save-Data", "on");
        if (is_image)
            array_set_header(headers, "Accept",
                             "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8");
        if (o->cfg->block_third_party_cookies && !url_same_site(page_host, host))
            array_del_header(headers, "Cookie");
        array_set_header(headers, "Accept-Encoding", "gzip, deflate, br, zstd");
    }

    /* 5. continue; intercept the response when we may transform it */
    int intercept = 0;
    if (o->cfg->lite && !strcmp(method, "GET")) {
        if (o->cfg->optimize_images && !strcmp(rtype, "Image"))
            intercept = 1;
        else if (o->cfg->minify_html && !strcmp(rtype, "Document"))
            intercept = 1;
        else if (o->cfg->minify_css && !strcmp(rtype, "Stylesheet"))
            intercept = 1;
        else if (o->cfg->minify_js && !strcmp(rtype, "Script"))
            intercept = 1;
        else if (o->cfg->cache_enabled || o->cfg->strip_metadata)
            intercept = 1; /* we may want to cache / strip metadata */
    }
    continue_request(o, session, request_id, headers, intercept);
    json_free(headers);
}

/* ----------------------------------------------------------- response stage */

static void handle_response_stage(optimizer_t *o, const char *session, json_t *params) {
    const char *request_id = json_get_str(params, "requestId", "");
    if (!request_id[0]) return;
    int status = (int)json_get_num(params, "responseStatusCode", 200);
    const char *rtype = json_get_str(params, "resourceType", "Other");
    json_t *rh = json_get(params, "responseHeaders");
    long long clen = 0;
    const char *cl = rh ? array_get_header(rh, "Content-Length") : NULL;
    if (!cl && rh) cl = array_get_header(rh, "content-length");
    if (cl) clen = atoll(cl);

    opt_pending_t *p = pending_find(o, request_id);
    if (!p) {
        /* we only saw the Response stage (interception started late) */
        p = pending_add(o, request_id, session);
        p->url[0] = 0;
        g_stats.requests++;
    }
    p->stage = 2;
    p->status = status;
    p->content_len = clen;
    p->max_age = max_age_from_headers(rh);
    if (rtype[0]) snprintf(p->resource_type, sizeof(p->resource_type), "%s", rtype);

    /* 304 -> serve the cached copy */
    if (status == 304 && p->inm_injected && o->cfg->cache_enabled && o->cache.enabled) {
        cache_entry_t e;
        if (cache_get(&o->cache, p->url, &e) && e.body_len) {
            json_t *hdrs = jarr();
            char clenbuf[32];
            snprintf(clenbuf, sizeof(clenbuf), "%zu", e.body_len);
            array_set_header(hdrs, "Content-Type", e.ctype[0] ? e.ctype : "application/octet-stream");
            array_set_header(hdrs, "Content-Length", clenbuf);
            array_set_header(hdrs, "X-Astra-Cache", "REVALIDATED");
            fulfill_request(o, session, request_id, 200, hdrs, e.body, e.body_len);
            g_stats.revalidated++;
            g_stats.bytes_original += e.body_len;
            g_stats.bytes_saved_cache += e.body_len;
            cache_entry_free(&e);
            json_free(hdrs);
            pending_del(o, request_id);
            return;
        }
    }

    if (!o->cfg->lite || status < 200 || status >= 300) {
        g_stats.passthrough++;
        g_stats.bytes_original += (uint64_t)(clen > 0 ? clen : 0);
        g_stats.bytes_delivered += (uint64_t)(clen > 0 ? clen : 0);
        continue_request(o, session, request_id, NULL, 0);
        return;
    }

    int want_body = 0;
    if (o->cfg->optimize_images && !strcmp(p->resource_type, "Image"))
        want_body = 1;
    else if (o->cfg->minify_html && !strcmp(p->resource_type, "Document"))
        want_body = 1;
    else if (o->cfg->minify_css && !strcmp(p->resource_type, "Stylesheet"))
        want_body = 1;
    else if (o->cfg->minify_js && !strcmp(p->resource_type, "Script"))
        want_body = 1;
    else if (o->cfg->strip_metadata && !strcmp(p->resource_type, "Image"))
        want_body = 1;
    else if (o->cfg->cache_enabled && (clen > 0 && clen < 8L * 1024 * 1024))
        want_body = 1;

    if (!want_body) {
        g_stats.passthrough++;
        g_stats.bytes_original += (uint64_t)(clen > 0 ? clen : 0);
        g_stats.bytes_delivered += (uint64_t)(clen > 0 ? clen : 0);
        continue_request(o, session, request_id, NULL, 0);
        pending_del(o, request_id);
        return;
    }

    int id = ASTRA_OPTIMIZER_ID_BASE + (o->next_id++ % 99999);
    p->id = id;
    json_t *gp = jobj();
    jset(gp, "requestId", jstr(request_id));
    json_t *msg = jobj();
    jset(msg, "id", jnum((double)id));
    jset(msg, "method", jstr("Fetch.getResponseBody"));
    jset(msg, "params", gp);
    if (session) jset(msg, "sessionId", jstr(session));
    o->send(o->send_ud, session, msg);
    json_free(msg);
}

void optimizer_handle_paused(optimizer_t *o, const char *engine_session, json_t *params) {
    if (!o || !params) return;
    if (json_get(params, "responseStatusCode") || json_get(params, "responseHeaders"))
        handle_response_stage(o, engine_session, params);
    else
        handle_request_stage(o, engine_session, params);
}

/* --------------------------------------------------- getResponseBody result */

static int is_cacheable_type(const char *ctype) {
    if (!ctype || !*ctype) return 0;
    if (strstr(ctype, "text/") || strstr(ctype, "application/javascript") ||
        strstr(ctype, "application/json") || strstr(ctype, "image/") || strstr(ctype, "font/") ||
        strstr(ctype, "application/font") || strstr(ctype, "video/") || strstr(ctype, "audio/"))
        return 1;
    return 0;
}

void optimizer_handle_response(optimizer_t *o, const char *engine_session, int id, json_t *result,
                               json_t *error) {
    (void)engine_session;
    if (!o || !result) return;
    opt_pending_t *p = NULL;
    for (opt_pending_t *q = o->pending; q; q = q->next)
        if (q->id == id) {
            p = q;
            break;
        }
    if (!p) return;
    if (error) {
        continue_request(o, p->session, p->request_id, NULL, 0);
        pending_del(o, p->request_id);
        return;
    }
    const char *b64 = json_get_str(result, "body", NULL);
    int is_b64 = json_get_bool(result, "base64Encoded", 0);
    if (!b64) {
        continue_request(o, p->session, p->request_id, NULL, 0);
        pending_del(o, p->request_id);
        return;
    }
    uint8_t *body = NULL;
    size_t blen = 0;
    if (is_b64) {
        body = b64_decode(b64, strlen(b64), &blen);
    } else {
        blen = strlen(b64);
        body = (uint8_t *)astr_dupn(b64, blen);
    }
    if (!body) {
        continue_request(o, p->session, p->request_id, NULL, 0);
        pending_del(o, p->request_id);
        return;
    }
    uint64_t original = (uint64_t)(p->content_len > 0 ? p->content_len : (long long)blen);
    g_stats.bytes_original += original;

    /* content type from stored headers (we stored none) -> infer from resource type */
    const char *ctype = "application/octet-stream";
    if (!strcmp(p->resource_type, "Document")) ctype = "text/html; charset=utf-8";
    else if (!strcmp(p->resource_type, "Stylesheet")) ctype = "text/css";
    else if (!strcmp(p->resource_type, "Script")) ctype = "application/javascript";
    else if (!strcmp(p->resource_type, "Image")) ctype = "image/jpeg";

    uint8_t *out = body;
    size_t outlen = blen;
    uint8_t *owned = NULL;
    const char *outctype = ctype;
    int transformed = 0;

    if (!strcmp(p->resource_type, "Image") && o->cfg->optimize_images) {
        img_opt_cfg ic;
        ic.max_width = o->cfg->max_image_width;
        ic.quality = o->cfg->image_quality;
        ic.min_bytes = o->cfg->image_min_bytes;
        ic.to_webp = 0;
        img_out_t r;
        if (image_optimize(body, blen, ctype, &ic, &r) && r.ok) {
            owned = r.data;
            out = r.data;
            outlen = r.len;
            outctype = r.mime;
            transformed = 1;
            g_stats.images_optimized++;
            g_stats.image_ms += r.ms;
            g_stats.bytes_saved_images += (uint64_t)(blen > r.len ? blen - r.len : 0);
            LOGD("image %s: %zu -> %zu B (%dx%d -> %dx%d)", p->url, blen, r.len, r.ow, r.oh, r.w,
                 r.h);
        } else if (o->cfg->strip_metadata) {
            buf_t sb;
            buf_init(&sb);
            if (image_strip_metadata(body, blen, ctype, &sb) && sb.len && sb.len < blen) {
                owned = sb.data;
                out = sb.data;
                outlen = sb.len;
                transformed = 1;
                g_stats.bytes_saved_metadata += (uint64_t)(blen - sb.len);
            } else {
                buf_free(&sb);
            }
        }
    } else if (!strcmp(p->resource_type, "Document") && o->cfg->minify_html) {
        buf_t mb;
        buf_init(&mb);
        uint64_t t0 = now_ms();
        minify_html((const char *)body, blen, &mb, o->cfg->lazy_load);
        g_stats.minify_ms += (double)(now_ms() - t0);
        if (mb.len && mb.len < blen) {
            owned = mb.data;
            out = mb.data;
            outlen = mb.len;
            transformed = 1;
            g_stats.text_minified++;
            g_stats.bytes_saved_text += (uint64_t)(blen - mb.len);
            LOGD("html %s: %zu -> %zu B", p->url, blen, mb.len);
        } else {
            buf_free(&mb);
        }
    } else if (!strcmp(p->resource_type, "Stylesheet") && o->cfg->minify_css) {
        buf_t mb;
        buf_init(&mb);
        uint64_t t0 = now_ms();
        minify_css((const char *)body, blen, &mb);
        g_stats.minify_ms += (double)(now_ms() - t0);
        if (mb.len && mb.len < blen) {
            owned = mb.data;
            out = mb.data;
            outlen = mb.len;
            transformed = 1;
            g_stats.text_minified++;
            g_stats.bytes_saved_text += (uint64_t)(blen - mb.len);
        } else {
            buf_free(&mb);
        }
    } else if (!strcmp(p->resource_type, "Script") && o->cfg->minify_js) {
        buf_t mb;
        buf_init(&mb);
        minify_js((const char *)body, blen, &mb);
        if (mb.len && mb.len < blen) {
            owned = mb.data;
            out = mb.data;
            outlen = mb.len;
            transformed = 1;
            g_stats.text_minified++;
            g_stats.bytes_saved_text += (uint64_t)(blen - mb.len);
        } else {
            buf_free(&mb);
        }
    }

    /* caching (store the optimized representation: that is what a reload would use) */
    if (o->cfg->cache_enabled && o->cache.enabled && p->status >= 200 && p->status < 300 &&
        is_cacheable_type(outctype) && outlen < 8u * 1024 * 1024) {
        long long ma = p->max_age > 0 ? p->max_age : 300;
        cache_put(&o->cache, p->url, p->status, outctype, out, outlen, NULL, NULL, ma);
    }

    if (transformed) {
        json_t *hdrs = jarr();
        char clenbuf[32];
        snprintf(clenbuf, sizeof(clenbuf), "%zu", outlen);
        array_set_header(hdrs, "Content-Type", outctype);
        array_set_header(hdrs, "Content-Length", clenbuf);
        array_set_header(hdrs, "X-Astra-Optimized", "1");
        fulfill_request(o, p->session, p->request_id, p->status, hdrs, out, outlen);
        g_stats.bytes_delivered += (uint64_t)outlen;
        json_free(hdrs);
    } else {
        continue_request(o, p->session, p->request_id, NULL, 0);
        g_stats.passthrough++;
        g_stats.bytes_delivered += (uint64_t)blen;
    }

    if (owned) free(owned);
    free(body);
    pending_del(o, p->request_id);
}
