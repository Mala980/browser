/* astra/main.c - command line interface */
#include "cdp.h"
#include "engine.h"
#include "http.h"
#include "json.h"
#include "optimizer.h"
#include "stats.h"
#include "ws.h"

#include <getopt.h>
#include <stdlib.h>
#include <unistd.h>

static void usage(void) {
    printf(
        "astra %s - lightweight CDP browser control plane with a bandwidth saving proxy\n"
        "\n"
        "usage:\n"
        "  astra serve [options]              run the CDP control plane (Puppeteer/go-rod connect here)\n"
        "  astra open <url> [options]         one shot: load a page, optionally screenshot/dump\n"
        "  astra bench <url> [options]        measure real byte savings: lite mode on vs off\n"
        "  astra info                         show resolved configuration and detected engine\n"
        "  astra cache-clear                  drop the on-disk cache\n"
        "  astra version                      print version\n"
        "\n"
        "engine / window:\n"
        "  --mode=headless|full   headless (default) or windowed browser\n"
        "  --engine=PATH          chromium-family binary to drive (default: autodetect)\n"
        "  --engine-url=ws://..   attach to an already running browser instead of launching\n"
        "  --profile=DIR          user data dir (default ~/.config/astra/profile)\n"
        "  --width=N --height=N   window / viewport size (default 1280x800)\n"
        "  --gpu=on|off           GPU rasterization flags (default on)\n"
        "  --no-sandbox           run the engine without its sandbox (needed as root / Android)\n"
        "  --user-agent=UA        override the user agent\n"
        "  --extra-flags=\"..\"     extra flags passed to the engine\n"
        "\n"
        "control plane:\n"
        "  --port=N --bind=ADDR   CDP endpoint (default 127.0.0.1:9222)\n"
        "  --idle-exit-ms=N       exit when no client activity for N ms (0 = never)\n"
        "\n"
        "saving / lite mode (all default on unless stated):\n"
        "  --lite=on|off          master switch\n"
        "  --block-ads=on|off     block ad/tracker requests (builtin list or --filter-list)\n"
        "  --filter-list=FILE     EasyList style filter file\n"
        "  --optimize-images=on|off   re-encode + downscale images\n"
        "  --max-image-width=N    downscale cap (default 1280)\n"
        "  --image-quality=N      jpeg quality (default 72)\n"
        "  --image-min-bytes=N    ignore images smaller than N (default 8k)\n"
        "  --minify-html/css/js   minify text resources (js default off, it is heuristic)\n"
        "  --lazy-load=on|off     inject loading=\"lazy\" on <img>/<iframe>\n"
        "  --save-data=on|off     send Save-Data: on for documents and images\n"
        "  --strip-metadata=on|off  strip EXIF/ICC/PNG ancillary chunks\n"
        "  --video=auto|block     block video downloads (huge bandwidth win)\n"
        "  --cache=on|off --cache-dir=DIR --cache-max-bytes=256M\n"
        "\n"
        "one shot options (astra open / bench):\n"
        "  --screenshot=FILE.png  capture the page\n"
        "  --pdf=FILE.pdf         print to pdf\n"
        "  --dump-dom             print document.documentElement.outerHTML\n"
        "  --eval=EXPR            evaluate JS and print the result\n"
        "  --wait=MS              extra settle time after load (default 0)\n"
        "  --timeout=MS           navigation timeout (default 30000)\n"
        "  --stats                print the bandwidth report\n"
        "\n"
        "misc:\n"
        "  --config=FILE          load a config file (key = value)\n"
        "  --log-level=N          0=error 1=warn 2=info 3=debug 4=trace\n"
        "  -v, --verbose          same as --log-level=4\n"
        "  -q, --quiet            same as --log-level=0\n",
        ASTRA_VERSION);
}

static int opt_value(int argc, char **argv, int *i, const char *name, const char **out) {
    /* supports --name=value and --name value */
    size_t nl = strlen(name);
    const char *a = argv[*i];
    if (strncmp(a, name, nl) != 0) return 0;
    if (a[nl] == '=') {
        *out = a + nl + 1;
        return 1;
    }
    if (a[nl] == 0) {
        if (*i + 1 < argc) {
            *out = argv[++(*i)];
            return 1;
        }
        *out = "";
        return 1;
    }
    return 0;
}

static int opt_bool(int argc, char **argv, int *i, const char *name, int *out) {
    const char *v = NULL;
    if (!opt_value(argc, argv, i, name, &v)) return 0;
    *out = parse_bool(v, 1);
    return 1;
}

static int opt_int(int argc, char **argv, int *i, const char *name, int *out) {
    const char *v = NULL;
    if (!opt_value(argc, argv, i, name, &v)) return 0;
    *out = atoi(v);
    return 1;
}

struct one_shot {
    const char *screenshot;
    const char *pdf;
    const char *eval;
    int dump_dom;
    int wait_ms;
    int timeout_ms;
    int show_stats;
    int stats_json;
};

static void parse_common(int argc, char **argv, int start, astra_config *cfg,
                         struct one_shot *os);

static void parse_common(int argc, char **argv, int start, astra_config *cfg,
                         struct one_shot *os) {
    for (int i = start; i < argc; i++) {
        const char *v;
        if (opt_bool(argc, argv, &i, "--lite", &cfg->lite)) continue;
        if (opt_bool(argc, argv, &i, "--block-ads", &cfg->block_ads)) continue;
        if (opt_bool(argc, argv, &i, "--optimize-images", &cfg->optimize_images)) continue;
        if (opt_bool(argc, argv, &i, "--minify-html", &cfg->minify_html)) continue;
        if (opt_bool(argc, argv, &i, "--minify-css", &cfg->minify_css)) continue;
        if (opt_bool(argc, argv, &i, "--minify-js", &cfg->minify_js)) continue;
        if (opt_bool(argc, argv, &i, "--lazy-load", &cfg->lazy_load)) continue;
        if (opt_bool(argc, argv, &i, "--save-data", &cfg->save_data)) continue;
        if (opt_bool(argc, argv, &i, "--strip-metadata", &cfg->strip_metadata)) continue;
        if (opt_bool(argc, argv, &i, "--cache", &cfg->cache_enabled)) continue;
        if (opt_bool(argc, argv, &i, "--gpu", &cfg->gpu)) continue;
        if (opt_bool(argc, argv, &i, "--no-sandbox", &cfg->no_sandbox)) continue;
        if (opt_int(argc, argv, &i, "--max-image-width", &cfg->max_image_width)) continue;
        if (opt_int(argc, argv, &i, "--image-quality", &cfg->image_quality)) continue;
        if (opt_int(argc, argv, &i, "--port", &cfg->port)) continue;
        if (opt_int(argc, argv, &i, "--width", &cfg->width)) continue;
        if (opt_int(argc, argv, &i, "--height", &cfg->height)) continue;
        if (opt_int(argc, argv, &i, "--idle-exit-ms", &cfg->idle_exit_ms)) continue;
        if (opt_int(argc, argv, &i, "--log-level", (int *)&cfg->log_level)) continue;
        if (opt_value(argc, argv, &i, "--bind", &v)) {
            snprintf(cfg->bind_addr, sizeof(cfg->bind_addr), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--mode", &v)) {
            cfg->mode = !strcasecmp(v, "full") ? MODE_FULL : MODE_HEADLESS;
            continue;
        }
        if (opt_value(argc, argv, &i, "--engine", &v)) {
            snprintf(cfg->engine_path, sizeof(cfg->engine_path), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--engine-url", &v)) {
            snprintf(cfg->engine_url, sizeof(cfg->engine_url), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--profile", &v)) {
            snprintf(cfg->profile_dir, sizeof(cfg->profile_dir), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--user-agent", &v)) {
            snprintf(cfg->user_agent, sizeof(cfg->user_agent), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--extra-flags", &v)) {
            snprintf(cfg->extra_flags, sizeof(cfg->extra_flags), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--filter-list", &v)) {
            snprintf(cfg->filter_list, sizeof(cfg->filter_list), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--cache-dir", &v)) {
            snprintf(cfg->cache_dir, sizeof(cfg->cache_dir), "%s", v);
            continue;
        }
        if (opt_value(argc, argv, &i, "--cache-max-bytes", &v)) {
            cfg->cache_max_bytes = (size_t)parse_size(v, (long long)cfg->cache_max_bytes);
            continue;
        }
        if (opt_value(argc, argv, &i, "--image-min-bytes", &v)) {
            cfg->image_min_bytes = (size_t)parse_size(v, (long long)cfg->image_min_bytes);
            continue;
        }
        if (opt_value(argc, argv, &i, "--video", &v)) {
            cfg->video = !strcasecmp(v, "block") ? VIDEO_BLOCK : VIDEO_AUTO;
            continue;
        }
        if (os && opt_value(argc, argv, &i, "--screenshot", &v)) {
            os->screenshot = v;
            continue;
        }
        if (os && opt_value(argc, argv, &i, "--pdf", &v)) {
            os->pdf = v;
            continue;
        }
        if (os && opt_value(argc, argv, &i, "--eval", &v)) {
            os->eval = v;
            continue;
        }
        if (os && opt_value(argc, argv, &i, "--wait", &v)) {
            os->wait_ms = atoi(v);
            continue;
        }
        if (os && opt_value(argc, argv, &i, "--timeout", &v)) {
            os->timeout_ms = atoi(v) > 0 ? atoi(v) : 30000;
            continue;
        }
        if (!strcmp(argv[i], "--dump-dom") && os) {
            os->dump_dom = 1;
            continue;
        }
        if (!strcmp(argv[i], "--stats")) {
            if (os) os->show_stats = 1;
            continue;
        }
        if (!strcmp(argv[i], "--stats-json")) {
            if (os) {
                os->show_stats = 1;
                os->stats_json = 1;
            }
            continue;
        }
        if (!strcmp(argv[i], "-v") || !strcmp(argv[i], "--verbose")) {
            cfg->log_level = L_TRACE;
            continue;
        }
        if (!strcmp(argv[i], "-q") || !strcmp(argv[i], "--quiet")) {
            cfg->log_level = L_ERROR;
            continue;
        }
        if (!strcmp(argv[i], "-h") || !strcmp(argv[i], "--help")) {
            usage();
            exit(0);
        }
        /* unknown: ignore (keeps scripts tolerant) */
    }
}

static int navigate_and_wait(const char *session, const char *url, int timeout_ms) {
    json_t *p = jobj();
    jset(p, "url", jstr(url));
    cdp_result_t r;
    int rc = cdp_cmd(session, "Page.enable", NULL, 10000, &r);
    cdp_release_result(&r);
    if (rc != 0) LOGW("Page.enable failed (%d)", rc);
    cdp_arm_event("Page.loadEventFired");
    rc = cdp_cmd(session, "Page.navigate", p, timeout_ms, &r);
    json_free(p);
    cdp_release_result(&r);
    if (rc != 0) {
        LOGE("Page.navigate failed (%d)", rc);
        return -1;
    }
    if (cdp_wait_armed(timeout_ms) != 0) {
        LOGW("timeout waiting for Page.loadEventFired");
        return -2;
    }
    return 0;
}

static int cmd_open(const astra_config *cfg, const char *url, struct one_shot *os) {
    engine_t eng;
    if (engine_launch(cfg, &eng) != 0) return 1;
    if (cdp_attach(cfg, &eng) != 0) {
        engine_stop(&eng);
        return 1;
    }
    if (cdp_attach_to_first_page(15000) != 0) {
        LOGE("cannot attach to a page target");
        cdp_detach();
        engine_stop(&eng);
        return 1;
    }
    const char *sess = cdp_page_session();

    json_t *m = jobj();
    jset(m, "width", jnum((double)cfg->width));
    jset(m, "height", jnum((double)cfg->height));
    jset(m, "deviceScaleFactor", jnum(1));
    jset(m, "mobile", jbool(0));
    cdp_cmd_simple(sess, "Emulation.setDeviceMetricsOverride", m, 10000);
    json_free(m);

    int rc = navigate_and_wait(sess, url, os->timeout_ms);
    if (os->wait_ms > 0) sleep_ms(os->wait_ms);

    if (os->eval) {
        json_t *p = jobj();
        jset(p, "expression", jstr(os->eval));
        jset(p, "returnByValue", jbool(1));
        jset(p, "awaitPromise", jbool(1));
        cdp_result_t r;
        if (cdp_cmd(sess, "Runtime.evaluate", p, os->timeout_ms, &r) == 0) {
            json_t *res = json_get(r.result, "result");
            json_t *val = res ? json_get(res, "value") : NULL;
            if (val) {
                char *s = json_stringify(val);
                printf("%s\n", s ? s : "");
                free(s);
            }
        } else {
            LOGE("eval failed: %s", r.error);
        }
        cdp_release_result(&r);
        json_free(p);
    }
    if (os->dump_dom) {
        json_t *p = jobj();
        jset(p, "expression", jstr("document.documentElement.outerHTML"));
        jset(p, "returnByValue", jbool(1));
        cdp_result_t r;
        if (cdp_cmd(sess, "Runtime.evaluate", p, os->timeout_ms, &r) == 0) {
            json_t *res = json_get(r.result, "result");
            const char *html = res ? json_get_str(res, "value", NULL) : NULL;
            if (html) printf("%s\n", html);
        }
        cdp_release_result(&r);
        json_free(p);
    }
    if (os->screenshot) {
        json_t *p = jobj();
        jset(p, "format", jstr("png"));
        jset(p, "captureBeyondViewport", jbool(0));
        cdp_result_t r;
        if (cdp_cmd(sess, "Page.captureScreenshot", p, os->timeout_ms, &r) == 0) {
            const char *b64 = json_get_str(r.result, "data", NULL);
            size_t len = 0;
            uint8_t *png = b64 ? b64_decode(b64, strlen(b64), &len) : NULL;
            if (png) {
                file_write(os->screenshot, png, len);
                printf("screenshot: %s (%zu bytes)\n", os->screenshot, len);
                free(png);
            }
        } else {
            LOGE("screenshot failed: %s", r.error);
        }
        cdp_release_result(&r);
        json_free(p);
    }
    if (os->pdf) {
        cdp_result_t r;
        if (cdp_cmd(sess, "Page.printToPDF", NULL, os->timeout_ms, &r) == 0) {
            const char *b64 = json_get_str(r.result, "data", NULL);
            size_t len = 0;
            uint8_t *pdf = b64 ? b64_decode(b64, strlen(b64), &len) : NULL;
            if (pdf) {
                file_write(os->pdf, pdf, len);
                printf("pdf: %s (%zu bytes)\n", os->pdf, len);
                free(pdf);
            }
        }
        cdp_release_result(&r);
    }
    if (os->show_stats) {
        if (os->stats_json) {
            char *s = stats_json_text();
            printf("%s\n", s ? s : "{}");
            free(s);
        } else {
            stats_print(stdout);
        }
    }
    cdp_detach();
    engine_stop(&eng);
    return rc == 0 ? 0 : 1;
}

static int cmd_bench(const astra_config *cfg, const char *url, struct one_shot *os) {
    engine_t eng;
    if (engine_launch(cfg, &eng) != 0) return 1;
    if (cdp_attach(cfg, &eng) != 0) {
        engine_stop(&eng);
        return 1;
    }
    if (cdp_attach_to_first_page(15000) != 0) {
        cdp_detach();
        engine_stop(&eng);
        return 1;
    }
    const char *sess = cdp_page_session();

    /* Chrome's own byte counters (Network.loadingFinished) are the only numbers
     * an outside observer can check; without Network.enable astra would have
     * nothing to report for "bytes over the wire". */
    cdp_cmd_simple(sess, "Network.enable", NULL, 5000);

    /* pass 1: lite mode on */
    stats_reset();
    cdp_net_rx_reset();
    navigate_and_wait(sess, url, os->timeout_ms);
    sleep_ms(os->wait_ms > 0 ? os->wait_ms : 1500);
    astra_stats_t on = g_stats;
    uint64_t on_net = cdp_net_rx();

    /* pass 2: same page, optimizers off (hard reload, cold cache).  Interception
     * stays on so the byte counters work identically in both passes - that is
     * what makes this an honest A/B and not "0 vs something". */
    astra_config plain = *cfg;
    plain.block_ads = 0;
    plain.optimize_images = 0;
    plain.minify_html = 0;
    plain.minify_css = 0;
    plain.minify_js = 0;
    plain.strip_metadata = 0;
    plain.lazy_load = 0;
    plain.cache_enabled = 0;
    cdp_set_config(&plain);
    stats_reset();
    cdp_net_rx_reset();
    json_t *p = jobj();
    jset(p, "ignoreCache", jbool(1));
    cdp_arm_event("Page.loadEventFired");
    cdp_cmd_simple(sess, "Page.reload", p, os->timeout_ms);
    json_free(p);
    cdp_wait_armed(os->timeout_ms);
    sleep_ms(os->wait_ms > 0 ? os->wait_ms : 1500);
    astra_stats_t off = g_stats;
    uint64_t off_net = cdp_net_rx();

    printf("\nastra benchmark: %s\n", url);
    printf("  %-26s %11s %11s %11s\n", "metric", "lite on", "lite off", "delta");
    printf("  %-26s %11llu %11llu %+11lld\n", "requests",
           (unsigned long long)on.requests, (unsigned long long)off.requests,
           (long long)off.requests - (long long)on.requests);
    printf("  %-26s %11llu %11llu\n", "blocked (ads)", (unsigned long long)on.blocked,
           (unsigned long long)off.blocked);
    printf("  %-26s %11llu %11llu\n", "images optimized", (unsigned long long)on.images_optimized,
           (unsigned long long)off.images_optimized);
    printf("  %-26s %11llu %11llu\n", "bytes on the wire (chrome)",
           (unsigned long long)on_net, (unsigned long long)off_net);
    /* Astra's own counters only exist for the lite pass: with the optimizers off
     * it forwards every body untouched, so it neither sees nor counts them. */
    printf("  %-26s %11llu %11s\n", "bytes handed to the renderer",
           (unsigned long long)on.bytes_delivered, "n/a");
    printf("  %-26s %11llu %11s\n", "bytes before optimizing",
           (unsigned long long)on.bytes_original, "n/a");
    /* Both passes are measured with Chrome's own network counter: it counts for
     * every request whether or not astra rewrites the body, so the two numbers
     * are comparable.  Astra's counters (what it handed to the renderer) are the
     * fallback for engines that do not report Network.loadingFinished. */
    uint64_t lite_bytes = on_net > 0 ? on_net : on.bytes_delivered;
    uint64_t baseline = off_net > 0 ? off_net
                      : (off.bytes_delivered > 0 ? off.bytes_delivered : on.bytes_original);
    if (baseline > 0) {
        double pct = 100.0 * ((double)baseline - (double)lite_bytes) / (double)baseline;
        printf("\n  => lite mode moved %.1f%% fewer bytes for the same page (%.1f KB vs %.1f KB)\n",
               pct, lite_bytes / 1024.0, baseline / 1024.0);
    } else {
        printf("\n  (no baseline to compare against)\n");
    }
    printf("\n");
    cdp_detach();
    engine_stop(&eng);
    return 0;
}

int main(int argc, char **argv) {
    astra_config cfg;
    struct one_shot os;
    memset(&os, 0, sizeof(os));
    os.timeout_ms = 30000;
    config_defaults(&cfg);

    /* config file: --config=FILE, else ~/.config/astra/astra.conf */
    const char *cfgpath = NULL;
    for (int i = 1; i < argc; i++) {
        const char *v;
        int j = i;
        if (opt_value(argc, argv, &j, "--config", &v)) {
            cfgpath = v;
            break;
        }
    }
    if (cfgpath) {
        char *exp = expand_home(cfgpath);
        if (config_load_file(&cfg, exp) != 0) LOGW("cannot read config %s", exp);
        free(exp);
    } else {
        char *home = expand_home("~/.config/astra/astra.conf");
        if (file_exists(home)) config_load_file(&cfg, home);
        free(home);
    }
    config_apply_env(&cfg);

    if (argc < 2) {
        usage();
        return 0;
    }
    const char *cmd = argv[1];
    if (!strcmp(cmd, "version") || !strcmp(cmd, "--version")) {
        printf("astra %s\n", ASTRA_VERSION);
        return 0;
    }
    if (!strcmp(cmd, "help") || !strcmp(cmd, "-h") || !strcmp(cmd, "--help")) {
        usage();
        return 0;
    }
    parse_common(argc, argv, 2, &cfg, &os);
    g_log_level = cfg.log_level;

    if (!strcmp(cmd, "info")) {
        config_print(&cfg);
        char bin[1024];
        if (engine_autodetect(bin, sizeof(bin)) == 0)
            printf("detected engine: %s\n", bin);
        else
            printf("detected engine: (none found)\n");
        printf("lite rules     : %zu\n", (size_t)0);
        return 0;
    }
    if (!strcmp(cmd, "cache-clear")) {
        cache_t c;
        char *dir = expand_home(cfg.cache_dir);
        if (cache_init(&c, dir, cfg.cache_max_bytes) == 0) {
            cache_clear(&c);
            printf("cache cleared: %s\n", dir);
        }
        free(dir);
        return 0;
    }
    if (!strcmp(cmd, "serve")) {
        engine_t eng;
        if (engine_launch(&cfg, &eng) != 0) return 1;
        int rc = cdp_serve(&cfg, &eng);
        stats_print(stdout);
        cdp_detach();
        engine_stop(&eng);
        return rc;
    }
    if (!strcmp(cmd, "open")) {
        const char *url = NULL;
        for (int i = 2; i < argc; i++)
            if (argv[i][0] != '-') {
                url = argv[i];
                break;
            }
        if (!url) {
            LOGE("usage: astra open <url> [options]");
            return 1;
        }
        return cmd_open(&cfg, url, &os);
    }
    if (!strcmp(cmd, "bench")) {
        const char *url = NULL;
        for (int i = 2; i < argc; i++)
            if (argv[i][0] != '-') {
                url = argv[i];
                break;
            }
        if (!url) {
            LOGE("usage: astra bench <url> [options]");
            return 1;
        }
        return cmd_bench(&cfg, url, &os);
    }
    usage();
    return 1;
}
