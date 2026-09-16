/* astra unit tests - no external dependencies, links against the astra objects */
#include "cache.h"
#include "cdp.h"
#include "config.h"
#include "engine.h"
#include "filters.h"
#include "http.h"
#include "image.h"
#include "json.h"
#include "minify.h"
#include "optimizer.h"
#include "stats.h"
#include "util.h"
#include "ws.h"

#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <string.h>

#define STBI_NO_STDIO
#include "../../vendor/stb/stb_image.h"
#include "../../vendor/stb/stb_image_write.h"

static int g_tests = 0, g_failed = 0;

#define CHECK(cond, ...)                                 \
    do {                                                 \
        g_tests++;                                       \
        if (!(cond)) {                                   \
            g_failed++;                                  \
            printf("  FAIL %s:%d: ", __FILE__, __LINE__); \
            printf(__VA_ARGS__);                         \
            printf("\n");                                \
        }                                                \
    } while (0)

#define CHECK_STR(a, b)                                                    \
    CHECK(strcmp((const char *)(a), (const char *)(b)) == 0, "expected \"%s\" got \"%s\"", \
          (const char *)(b), (const char *)(a))

/* ------------------------------------------------------------------- json */

static void test_json(void) {
    const char *src = "{\"a\":1,\"b\":\"x\\\"y\",\"c\":[1,2,{\"d\":true}],\"e\":null,\"f\":1.5}";
    json_t *v = json_parse_cstr(src);
    CHECK(v != NULL, "parse failed");
    if (!v) return;
    CHECK(json_get_num(v, "a", 0) == 1, "a");
    CHECK_STR(json_get_str(v, "b", ""), "x\"y");
    json_t *c = json_get(v, "c");
    CHECK(c && c->type == J_ARR && c->n == 3, "array size");
    CHECK(json_get_bool(json_at(c, 2), "d", 0) == 1, "nested bool");
    CHECK(json_get(v, "e")->type == J_NULL, "null type");
    CHECK(json_get_num(v, "f", 0) == 1.5, "float");
    char *s = json_stringify(v);
    json_t *v2 = json_parse_cstr(s);
    char *s2 = json_stringify(v2);
    CHECK_STR(s, s2);
    free(s);
    free(s2);
    json_free(v);
    json_free(v2);

    json_t *o = jobj();
    jset(o, "hello", jstr("world"));
    jset(o, "n", jnum(42));
    json_t *arr = jarr();
    jpush(arr, jnum(1));
    jpush(arr, jstr("two"));
    jset(o, "arr", arr);
    char *js = json_stringify(o);
    CHECK_STR(js, "{\"hello\":\"world\",\"n\":42,\"arr\":[1,\"two\"]}");
    json_t *clone = json_clone(o);
    char *js2 = json_stringify(clone);
    CHECK_STR(js, js2);
    free(js);
    free(js2);
    json_free(o);
    json_free(clone);
    CHECK(json_parse_cstr("{bad json") == NULL, "invalid json must be rejected");
}

/* -------------------------------------------------------- crypto / base64 */

static void test_crypto(void) {
    uint8_t d[20];
    astra_sha1((const uint8_t *)"abc", 3, d);
    char hex[41];
    astr_hex(d, 20, hex);
    CHECK_STR(hex, "a9993e364706816aba3e25717850c26c9cd0d89d");

    char *b64 = b64_encode((const uint8_t *)"Hello, World!", 13);
    CHECK_STR(b64, "SGVsbG8sIFdvcmxkIQ==");
    free(b64);
    size_t n = 0;
    uint8_t *raw = b64_decode("SGVsbG8sIFdvcmxkIQ==", strlen("SGVsbG8sIFdvcmxkIQ=="), &n);
    CHECK(raw && n == 13 && !memcmp(raw, "Hello, World!", 13), "base64 roundtrip");
    free(raw);

    uint8_t rnd[32];
    CHECK(random_bytes(rnd, sizeof(rnd)) == 0, "random_bytes");
    int nonzero = 0;
    for (int i = 0; i < 32; i++)
        if (rnd[i]) nonzero = 1;
    CHECK(nonzero, "random data not all zero");
}

/* ------------------------------------------------------------------ urls */

static void test_urls(void) {
    url_t u;
    CHECK(url_parse("https://Example.COM:8443/a/b?x=1#frag", &u) == 0, "parse");
    CHECK_STR(u.scheme, "https");
    CHECK_STR(u.host, "example.com");
    CHECK(u.port == 8443, "port %d", u.port);
    CHECK_STR(u.path, "/a/b");
    CHECK_STR(u.query, "x=1");

    CHECK(host_matches_domain("ads.doubleclick.net", "doubleclick.net") == 1, "subdomain match");
    CHECK(host_matches_domain("notdoubleclick.net", "doubleclick.net") == 0, "suffix trap");
    CHECK(host_matches_domain("doubleclick.net", "doubleclick.net") == 1, "exact match");

    char reg[256];
    url_registrable_domain("www.example.co.id", reg, sizeof(reg));
    CHECK_STR(reg, "example.co.id");
    url_registrable_domain("a.b.example.com", reg, sizeof(reg));
    CHECK_STR(reg, "example.com");
    url_registrable_domain("localhost", reg, sizeof(reg));
    CHECK_STR(reg, "localhost");

    CHECK(url_same_site("www.google-analytics.com", "google-analytics.com") == 1, "same site");
    CHECK(url_same_site("example.com", "google-analytics.com") == 0, "different site");
}

/* --------------------------------------------------------------- filters */

static void test_filters(void) {
    filters_t f;
    filters_init(&f);
    filters_load_builtin(&f);
    CHECK(f.n > 50, "builtin rules loaded: %zu", f.n);

    CHECK(filters_is_blocked(&f, "https://googleads.g.doubleclick.net/pagead/id", "example.com",
                             "Image") == 1,
          "doubleclick blocked");
    CHECK(filters_is_blocked(&f, "https://www.google-analytics.com/collect", "example.com",
                             "Image") == 1,
          "analytics blocked");
    CHECK(filters_is_blocked(&f, "https://example.com/app.js", "example.com", "Script") == 0,
          "first party script allowed");
    CHECK(filters_is_blocked(&f, "https://connect.facebook.net/en_US/sdk.js", "example.com",
                             "Script") == 1,
          "third party fb sdk blocked");
    CHECK(filters_is_blocked(&f, "https://connect.facebook.net/en_US/sdk.js",
                             "www.facebook.net", "Script") == 0,
          "same-site fb sdk allowed (third-party only rule)");
    CHECK(filters_is_blocked(&f, "data:image/png;base64,AAA", "example.com", "Image") == 0,
          "data urls never blocked");

    /* custom list: block rule, exception rule, unsupported path rule */
    filters_t g;
    filters_init(&g);
    filters_load_text(&g, "||ads.example.com^\n", strlen("||ads.example.com^\n"));
    CHECK(g.n == 1, "block rule parsed (%zu)", g.n);
    CHECK(filters_is_blocked(&g, "https://ads.example.com/x.js", "site.test", "Script") == 1,
          "blocked by custom rule");
    filters_load_text(&g, "@@||ads.example.com^\n", strlen("@@||ads.example.com^\n"));
    CHECK(filters_is_blocked(&g, "https://ads.example.com/x.js", "site.test", "Script") == 0,
          "exception rule wins over block rule");
    filters_load_text(&g, "@@||ads.example.com/allowed^\n!comment\n##.ad\n",
                      strlen("@@||ads.example.com/allowed^\n!comment\n##.ad\n"));
    CHECK(g.skipped_lines >= 1, "path rules skipped, not downgraded (%zu)", g.skipped_lines);
    CHECK(filters_is_blocked(&g, "https://ads.example.com/x.js", "site.test", "Script") == 0,
          "path exception does not whitelist the whole domain");
    CHECK(res_type_mask("Image") == RES_IMAGE, "resource type Image");
    CHECK(res_type_mask("XHR") == RES_XHR, "resource type XHR");
    filters_free(&f);
    filters_free(&g);
}

/* ---------------------------------------------------------------- minify */

static void test_minify(void) {
    const char *html =
        "<!DOCTYPE html>\n<html>\n  <head>\n    <!-- a comment -->\n    <title>Hi</title>\n"
        "    <style>  body { color : red ; }  </style>\n  </head>\n  <body>\n"
        "    <pre>  keep   this  </pre>\n"
        "    <img src=\"a.png\">\n    <img loading=\"eager\" src=\"b.png\">\n"
        "    <script>var x = 1;   // keep\n</script>\n  </body>\n</html>\n";
    buf_t out;
    buf_init(&out);
    minify_html(html, strlen(html), &out, 1);
    CHECK(out.len > 0 && out.len < strlen(html), "html shrank: %zu -> %zu", strlen(html), out.len);
    CHECK(strstr((char *)out.data, "<!--") == NULL, "comments removed");
    CHECK(strstr((char *)out.data, "keep   this") != NULL, "pre content preserved");
    CHECK(strstr((char *)out.data, "var x = 1;   // keep") != NULL, "script content preserved");
    CHECK(strstr((char *)out.data, "<img src=\"a.png\" loading=\"lazy\" decoding=\"async\">") != NULL,
          "lazy attributes injected: %s", (char *)out.data);
    CHECK(strstr((char *)out.data, "loading=\"eager\"") != NULL &&
              strstr((char *)out.data, "loading=\"eager\" loading=") == NULL,
          "existing loading attr untouched");
    CHECK(strstr((char *)out.data, "<!DOCTYPE html>") != NULL ||
              strstr((char *)out.data, "<!doctype html>") != NULL,
          "doctype preserved");
    buf_free(&out);

    const char *css = "/* comment */\nbody {\n  color : #ffffff ;\n  margin : 0px ;\n}\n";
    buf_init(&out);
    minify_css(css, strlen(css), &out);
    CHECK(strstr((char *)out.data, "comment") == NULL, "css comment removed");
    CHECK(strstr((char *)out.data, "#fff") != NULL, "css color shortened");
    CHECK(strstr((char *)out.data, "margin:0;") != NULL || strstr((char *)out.data, "margin:0}") != NULL,
          "zero unit dropped: %s", (char *)out.data);
    CHECK(out.len < strlen(css), "css shrank");
    buf_free(&out);

    const char *js = "var s = \"// not a comment\"; // real comment\n/* block */ var t = /re\\/gex/;";
    buf_init(&out);
    minify_js(js, strlen(js), &out);
    CHECK(strstr((char *)out.data, "not a comment") != NULL, "string preserved");
    CHECK(strstr((char *)out.data, "real comment") == NULL, "line comment stripped");
    CHECK(strstr((char *)out.data, "block") == NULL, "block comment stripped");
    CHECK(strstr((char *)out.data, "/re\\/gex/") != NULL, "regex preserved");
    buf_free(&out);
}

/* ----------------------------------------------------------------- image */

static void stb_write_cb(void *ctx, void *data, int size) {
    buf_t *b = (buf_t *)ctx;
    buf_append(b, data, (size_t)size);
}

static void test_image(void) {
    int w = 640, h = 480;
    uint8_t *rgba = (uint8_t *)malloc((size_t)w * h * 4);
    CHECK(rgba != NULL, "alloc");
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            size_t i = ((size_t)y * w + x) * 4;
            rgba[i] = (uint8_t)(x ^ y);
            rgba[i + 1] = (uint8_t)((x * 3 + y) & 0xff);
            rgba[i + 2] = (uint8_t)((y * 7) & 0xff);
            rgba[i + 3] = 255;
        }
    buf_t png;
    buf_init(&png);
    stbi_write_png_to_func(stb_write_cb, &png, w, h, 4, rgba, w * 4);
    CHECK(png.len > 1000, "png encoded (%zu bytes)", png.len);

    img_opt_cfg cfg = {320, 60, 1024, 0};
    img_out_t r;
    int rc = image_optimize(png.data, png.len, "image/png", &cfg, &r);
    CHECK(rc == 1 && r.ok, "image optimized (rc=%d)", rc);
    if (r.ok) {
        CHECK(r.w == 320 && r.h == 240, "downscaled to %dx%d", r.w, r.h);
        CHECK(r.len < png.len, "smaller: %zu -> %zu", png.len, r.len);
        CHECK_STR(r.mime, "image/jpeg");
        int dw = 0, dh = 0, dc = 0;
        uint8_t *dec = stbi_load_from_memory(r.data, (int)r.len, &dw, &dh, &dc, 0);
        CHECK(dec != NULL, "output is decodable");
        CHECK(dw == 320 && dh == 240, "decoded size %dx%d", dw, dh);
        free(dec);
    }
    image_out_free(&r);

    /* tiny images must be left alone */
    img_opt_cfg cfg2 = {320, 60, 10 * 1024 * 1024, 0};
    CHECK(image_optimize(png.data, png.len, "image/png", &cfg2, &r) == 0, "min-bytes respected");
    image_out_free(&r);
    /* svg must never be re-encoded */
    CHECK(image_optimize((const uint8_t *)"<svg xmlns='x'/>", 16, "image/svg+xml", &cfg, &r) == 0,
          "svg skipped");
    /* garbage input must not crash */
    CHECK(image_optimize((const uint8_t *)"not an image at all", 19, "image/jpeg", &cfg, &r) == 0,
          "garbage skipped");

    /* metadata stripper */
    buf_t stripped;
    buf_init(&stripped);
    int changed = image_strip_metadata(png.data, png.len, "image/png", &stripped);
    CHECK(changed == 0 || stripped.len <= png.len, "png stripper output sane");
    buf_free(&stripped);
    buf_free(&png);
    free(rgba);
}

/* ----------------------------------------------------------------- cache */

static void test_cache(void) {
    const char *dir = "/tmp/astra-test-cache";
    rm_rf(dir);
    cache_t c;
    CHECK(cache_init(&c, dir, 4096) == 0, "cache init");
    const char *url = "https://example.com/style.css";
    const char *body = "body{color:red}";
    CHECK(cache_put(&c, url, 200, "text/css", (const uint8_t *)body, strlen(body), "\"etag1\"",
                    "Wed, 21 Oct 2015 07:28:00 GMT", 60) == 0,
          "cache put");
    cache_entry_t e;
    CHECK(cache_get(&c, url, &e) == 1, "cache get");
    CHECK(e.found && e.fresh, "entry fresh");
    CHECK_STR(e.ctype, "text/css");
    CHECK(e.body_len == strlen(body) && !memcmp(e.body, body, e.body_len), "body roundtrip");
    CHECK_STR(e.etag, "\"etag1\"");
    cache_entry_free(&e);

    /* unknown url */
    CHECK(cache_get(&c, "https://example.com/nope", &e) == 0, "miss");

    /* expiration */
    CHECK(cache_put(&c, "https://example.com/old", 200, "text/plain", (const uint8_t *)"x", 1, NULL,
                    NULL, -1) == 0,
          "put expired entry");
    CHECK(cache_get(&c, "https://example.com/old", &e) == 1, "get expired entry");
    CHECK(!e.fresh, "entry stale");
    cache_entry_free(&e);

    /* eviction */
    for (int i = 0; i < 200; i++) {
        char u[128], b[512];
        snprintf(u, sizeof(u), "https://example.com/%d", i);
        memset(b, 'a', sizeof(b) - 1);
        b[sizeof(b) - 1] = 0;
        cache_put(&c, u, 200, "text/plain", (const uint8_t *)b, strlen(b), NULL, NULL, 60);
    }
    size_t entries = 0;
    uint64_t bytes = 0;
    cache_stats(&c, &entries, &bytes);
    cache_enforce_limit(&c);
    size_t e2 = 0;
    uint64_t b2 = 0;
    cache_stats(&c, &e2, &b2);
    CHECK(b2 <= c.max_bytes, "eviction respected limit: %llu <= %zu", (unsigned long long)b2,
          c.max_bytes);
    CHECK(e2 <= entries, "entries evicted (%zu -> %zu)", entries, e2);
    cache_clear(&c);
    size_t e3 = 0;
    cache_stats(&c, &e3, &bytes);
    CHECK(e3 == 0, "cache cleared");
    rm_rf(dir);
}

/* --------------------------------------------------------------- websocket */

static int ws_got = 0;
static char ws_payload[256];

static void ws_cb(void *ud, int opcode, const uint8_t *data, size_t len) {
    (void)ud;
    if (opcode == WS_TEXT) {
        ws_got++;
        size_t n = len < sizeof(ws_payload) - 1 ? len : sizeof(ws_payload) - 1;
        memcpy(ws_payload, data, n);
        ws_payload[n] = 0;
    }
}

static void test_ws(void) {
    buf_t frame;
    buf_init(&frame);
    const char *msg = "{\"id\":1,\"method\":\"Target.getTargets\"}";
    ws_frame(&frame, WS_TEXT, msg, strlen(msg), 1); /* masked: what a client sends */

    ws_parser_t p;
    ws_parser_init(&p, ws_cb, NULL);
    ws_got = 0;
    CHECK(ws_parser_feed(&p, frame.data, frame.len) == 0, "frame parsed");
    CHECK(ws_got == 1, "one message delivered");
    CHECK_STR(ws_payload, msg);
    ws_parser_free(&p);
    buf_free(&frame);

    /* fragmented / partial delivery */
    buf_t f2;
    buf_init(&f2);
    ws_frame_ex(&f2, WS_TEXT, "hello ", 6, 1, 0); /* fragmented: FIN = 0 */
    ws_frame_ex(&f2, WS_CONT, "world", 5, 1, 1);
    ws_parser_init(&p, ws_cb, NULL);
    ws_got = 0;
    for (size_t i = 0; i < f2.len; i++) {
        CHECK(ws_parser_feed(&p, f2.data + i, 1) == 0, "byte by byte feed");
    }
    CHECK(ws_got == 1, "reassembled one message");
    CHECK_STR(ws_payload, "hello world");
    ws_parser_free(&p);
    buf_free(&f2);

    /* server handshake accept value (RFC 6455 example) */
    const char *req = "GET /devtools/browser/x HTTP/1.1\r\n"
                      "Host: 127.0.0.1:9222\r\n"
                      "Upgrade: websocket\r\n"
                      "Connection: Upgrade\r\n"
                      "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
                      "Sec-WebSocket-Version: 13\r\n\r\n";
    buf_t resp;
    buf_init(&resp);
    CHECK(ws_server_handshake(req, strlen(req), &resp) == 0, "handshake built");
    CHECK(strstr((char *)resp.data, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=") != NULL,
          "accept token correct: %s", (char *)resp.data);
    CHECK(strncmp((char *)resp.data, "HTTP/1.1 101", 12) == 0, "101 response");
    buf_free(&resp);

    char *creq = ws_client_request("127.0.0.1", 9222, "/devtools/browser/x");
    CHECK(creq && strstr(creq, "Sec-WebSocket-Key: ") != NULL, "client request built");
    free(creq);
}

/* ------------------------------------------------------------------ stats */

static void test_stats(void) {
    stats_reset();
    g_stats.requests = 100;
    g_stats.blocked = 10;
    g_stats.bytes_original = 1000;
    g_stats.bytes_delivered = 400;
    g_stats.bytes_saved_cache = 200;
    CHECK(stats_saving_pct() > 60.0, "saving percentage: %.1f", stats_saving_pct());
    json_t *o = jobj();
    stats_json(o);
    CHECK(json_get_num(o, "requests", 0) == 100, "stats json requests");
    char *s = json_stringify(o);
    CHECK(s && strstr(s, "savingPct") != NULL, "stats json has savingPct");
    free(s);
    json_free(o);
    stats_reset();
    CHECK(g_stats.requests == 0, "reset");
}

/* --------------------------------------------------------------- config */

static void test_config(void) {
    astra_config cfg;
    config_defaults(&cfg);
    CHECK(cfg.lite == 1 && cfg.block_ads == 1, "defaults");
    CHECK(cfg.port == 9222, "default port");
    CHECK(parse_bool("on", 0) == 1 && parse_bool("off", 1) == 0, "parse_bool");
    CHECK(parse_size("256M", 0) == 256 * 1024 * 1024, "parse_size MB");
    CHECK(parse_size("8k", 0) == 8192, "parse_size k");
    const char *tmp = "/tmp/astra-test.conf";
    file_write(tmp, "lite = off\nport = 9333\nmax-image-width = 800\n",
               strlen("lite = off\nport = 9333\nmax-image-width = 800\n"));
    CHECK(config_load_file(&cfg, tmp) == 0, "config load");
    CHECK(cfg.lite == 0, "lite off from file");
    CHECK(cfg.port == 9333, "port from file");
    CHECK(cfg.max_image_width == 800, "width from file");
    unlink(tmp);
}

/* ------------------------------------------------------------------- http */

static void test_http_req(void) {
    const char *req = "GET /json/version HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
                      "Sec-WebSocket-Key: abc\r\n\r\n";
    http_req_t r;
    CHECK(http_req_parse(req, strlen(req), &r) == 1, "request parsed");
    CHECK_STR(r.method, "GET");
    CHECK_STR(r.target, "/json/version");
    CHECK_STR(http_req_header(&r, "Sec-WebSocket-Key"), "abc");
    http_req_free(&r);
    CHECK(http_req_parse("GET / HTTP/1.1\r\nHost: x\r\n\r", 25, &r) == 0, "incomplete request");
    http_req_free(&r);

    buf_t out;
    buf_init(&out);
    http_write_response(&out, 200, "application/json", "{}", 2);
    CHECK(strncmp((char *)out.data, "HTTP/1.1 200 OK", 15) == 0, "response line");
    CHECK(strstr((char *)out.data, "Content-Length: 2") != NULL, "content length");
    buf_free(&out);
}

/* --------------------------------------------------------------- buffers */

static void test_buf(void) {
    buf_t b;
    buf_init(&b);
    buf_appendstr(&b, "hello");
    buf_appendf(&b, " %d", 42);
    CHECK_STR((char *)b.data, "hello 42");
    buf_consume(&b, 6);
    CHECK_STR((char *)b.data, "42");
    buf_clear(&b);
    CHECK(b.len == 0, "cleared");
    for (int i = 0; i < 10000; i++) buf_appendf(&b, "%d,", i);
    CHECK(b.len > 40000, "grew to %zu", b.len);
    buf_free(&b);
}

int main(void) {
    g_log_level = L_ERROR;
    printf("astra unit tests\n");
    test_buf();
    test_json();
    test_crypto();
    test_urls();
    test_filters();
    test_minify();
    test_image();
    test_cache();
    test_ws();
    test_stats();
    test_config();
    test_http_req();
    printf("\n%d checks, %d failed\n", g_tests, g_failed);
    return g_failed ? 1 : 0;
}
