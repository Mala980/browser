/* astra/config.c */
#include "config.h"

#include <stdlib.h>

int parse_bool(const char *s, int def) {
    if (!s) return def;
    if (!strcasecmp(s, "1") || !strcasecmp(s, "on") || !strcasecmp(s, "true") ||
        !strcasecmp(s, "yes"))
        return 1;
    if (!strcasecmp(s, "0") || !strcasecmp(s, "off") || !strcasecmp(s, "false") ||
        !strcasecmp(s, "no"))
        return 0;
    return def;
}

long long parse_size(const char *s, long long def) {
    if (!s || !*s) return def;
    char *end = NULL;
    double v = strtod(s, &end);
    if (end == s) return def;
    if (end && *end) {
        if (!strcasecmp(end, "k") || !strcasecmp(end, "kb"))
            v *= 1024;
        else if (!strcasecmp(end, "m") || !strcasecmp(end, "mb"))
            v *= 1024 * 1024;
        else if (!strcasecmp(end, "g") || !strcasecmp(end, "gb"))
            v *= 1024 * 1024 * 1024;
    }
    return (long long)v;
}

void config_defaults(astra_config *cfg) {
    memset(cfg, 0, sizeof(*cfg));
    cfg->mode = MODE_HEADLESS;
    cfg->width = 1280;
    cfg->height = 800;
    cfg->gpu = 1;
    cfg->no_sandbox = -1; /* auto: enabled when running as root or on Android */
    cfg->port = ASTRA_DEFAULT_PORT;
    snprintf(cfg->bind_addr, sizeof(cfg->bind_addr), "127.0.0.1");
    cfg->idle_exit_ms = 0;
    cfg->lite = 1;
    cfg->block_ads = 1;
    cfg->optimize_images = 1;
    cfg->max_image_width = 1280;
    cfg->image_quality = 72;
    cfg->image_min_bytes = 8 * 1024;
    cfg->minify_html = 1;
    cfg->minify_css = 1;
    cfg->minify_js = 0;
    cfg->lazy_load = 1;
    cfg->save_data = 1;
    cfg->strip_metadata = 1;
    cfg->block_third_party_cookies = 1;
    cfg->video = VIDEO_AUTO;
    cfg->cache_enabled = 1;
    cfg->cache_max_bytes = (size_t)256 * 1024 * 1024;
    cfg->log_level = L_INFO;
    const char *home = getenv("HOME");
    if (!home) home = "/tmp";
    snprintf(cfg->cache_dir, sizeof(cfg->cache_dir), "%s/.cache/astra", home);
    snprintf(cfg->profile_dir, sizeof(cfg->profile_dir), "%s/.config/astra/profile", home);
}

static void set_kv(astra_config *cfg, const char *key, const char *val) {
    if (!key || !val) return;
    if (!strcmp(key, "port"))
        cfg->port = atoi(val);
    else if (!strcmp(key, "bind"))
        snprintf(cfg->bind_addr, sizeof(cfg->bind_addr), "%s", val);
    else if (!strcmp(key, "mode"))
        cfg->mode = !strcasecmp(val, "full") ? MODE_FULL : MODE_HEADLESS;
    else if (!strcmp(key, "headless"))
        cfg->mode = parse_bool(val, 1) ? MODE_HEADLESS : MODE_FULL;
    else if (!strcmp(key, "width"))
        cfg->width = atoi(val);
    else if (!strcmp(key, "height"))
        cfg->height = atoi(val);
    else if (!strcmp(key, "engine"))
        snprintf(cfg->engine_path, sizeof(cfg->engine_path), "%s", val);
    else if (!strcmp(key, "profile"))
        snprintf(cfg->profile_dir, sizeof(cfg->profile_dir), "%s", val);
    else if (!strcmp(key, "lite"))
        cfg->lite = parse_bool(val, cfg->lite);
    else if (!strcmp(key, "block-ads"))
        cfg->block_ads = parse_bool(val, cfg->block_ads);
    else if (!strcmp(key, "optimize-images"))
        cfg->optimize_images = parse_bool(val, cfg->optimize_images);
    else if (!strcmp(key, "max-image-width"))
        cfg->max_image_width = atoi(val);
    else if (!strcmp(key, "image-quality"))
        cfg->image_quality = atoi(val);
    else if (!strcmp(key, "image-min-bytes"))
        cfg->image_min_bytes = (size_t)parse_size(val, (long long)cfg->image_min_bytes);
    else if (!strcmp(key, "minify-html"))
        cfg->minify_html = parse_bool(val, cfg->minify_html);
    else if (!strcmp(key, "minify-css"))
        cfg->minify_css = parse_bool(val, cfg->minify_css);
    else if (!strcmp(key, "minify-js"))
        cfg->minify_js = parse_bool(val, cfg->minify_js);
    else if (!strcmp(key, "lazy-load"))
        cfg->lazy_load = parse_bool(val, cfg->lazy_load);
    else if (!strcmp(key, "save-data"))
        cfg->save_data = parse_bool(val, cfg->save_data);
    else if (!strcmp(key, "strip-metadata"))
        cfg->strip_metadata = parse_bool(val, cfg->strip_metadata);
    else if (!strcmp(key, "block-third-party-cookies"))
        cfg->block_third_party_cookies = parse_bool(val, cfg->block_third_party_cookies);
    else if (!strcmp(key, "video"))
        cfg->video = !strcasecmp(val, "block") ? VIDEO_BLOCK : VIDEO_AUTO;
    else if (!strcmp(key, "cache"))
        cfg->cache_enabled = parse_bool(val, cfg->cache_enabled);
    else if (!strcmp(key, "cache-max-bytes"))
        cfg->cache_max_bytes = (size_t)parse_size(val, (long long)cfg->cache_max_bytes);
    else if (!strcmp(key, "cache-dir"))
        snprintf(cfg->cache_dir, sizeof(cfg->cache_dir), "%s", val);
    else if (!strcmp(key, "filter-list"))
        snprintf(cfg->filter_list, sizeof(cfg->filter_list), "%s", val);
    else if (!strcmp(key, "user-agent"))
        snprintf(cfg->user_agent, sizeof(cfg->user_agent), "%s", val);
    else if (!strcmp(key, "gpu"))
        cfg->gpu = parse_bool(val, cfg->gpu);
    else if (!strcmp(key, "no-sandbox"))
        cfg->no_sandbox = parse_bool(val, cfg->no_sandbox);
    else if (!strcmp(key, "log-level"))
        cfg->log_level = (log_level_t)atoi(val);
    else if (!strcmp(key, "idle-exit-ms"))
        cfg->idle_exit_ms = atoi(val);
    else if (!strcmp(key, "extra-flags"))
        snprintf(cfg->extra_flags, sizeof(cfg->extra_flags), "%s", val);
}

int config_load_file(astra_config *cfg, const char *path) {
    uint8_t *data = NULL;
    size_t n = 0;
    if (file_read(path, &data, &n) != 0) return -1;
    char *text = (char *)data;
    size_t pos = 0;
    while (pos < n) {
        size_t eol = pos;
        while (eol < n && text[eol] != '\n') eol++;
        size_t len = eol - pos;
        while (len && (text[pos + len - 1] == '\r')) len--;
        if (len) {
            char *line = astr_dupn(text + pos, len);
            char *c = strchr(line, '#');
            if (c) *c = 0;
            char *trimmed = astr_trim(line);
            if (*trimmed) {
                char *eq = strchr(trimmed, '=');
                if (eq) {
                    *eq = 0;
                    char *k = astr_trim(trimmed);
                    char *v = astr_trim(eq + 1);
                    set_kv(cfg, k, v);
                }
            }
            free(line);
        }
        pos = eol < n ? eol + 1 : n;
    }
    free(data);
    return 0;
}

void config_apply_env(astra_config *cfg) {
    struct {
        const char *env;
        const char *key;
    } map[] = {{"ASTRA_PORT", "port"},
               {"ASTRA_BIND", "bind"},
               {"ASTRA_MODE", "mode"},
               {"ASTRA_ENGINE", "engine"},
               {"ASTRA_PROFILE", "profile"},
               {"ASTRA_LITE", "lite"},
               {"ASTRA_BLOCK_ADS", "block-ads"},
               {"ASTRA_OPTIMIZE_IMAGES", "optimize-images"},
               {"ASTRA_MAX_IMAGE_WIDTH", "max-image-width"},
               {"ASTRA_IMAGE_QUALITY", "image-quality"},
               {"ASTRA_IMAGE_MIN_BYTES", "image-min-bytes"},
               {"ASTRA_MINIFY_HTML", "minify-html"},
               {"ASTRA_MINIFY_CSS", "minify-css"},
               {"ASTRA_MINIFY_JS", "minify-js"},
               {"ASTRA_LAZY_LOAD", "lazy-load"},
               {"ASTRA_SAVE_DATA", "save-data"},
               {"ASTRA_STRIP_METADATA", "strip-metadata"},
               {"ASTRA_VIDEO", "video"},
               {"ASTRA_CACHE", "cache"},
               {"ASTRA_CACHE_DIR", "cache-dir"},
               {"ASTRA_CACHE_MAX_BYTES", "cache-max-bytes"},
               {"ASTRA_FILTER_LIST", "filter-list"},
               {"ASTRA_USER_AGENT", "user-agent"},
               {"ASTRA_GPU", "gpu"},
               {"ASTRA_NO_SANDBOX", "no-sandbox"},
               {"ASTRA_LOG_LEVEL", "log-level"},
               {"ASTRA_IDLE_EXIT_MS", "idle-exit-ms"},
               {"ASTRA_EXTRA_FLAGS", "extra-flags"},
               {NULL, NULL}};
    for (int i = 0; map[i].env; i++) {
        const char *v = getenv(map[i].env);
        if (v) set_kv(cfg, map[i].key, v);
    }
    if (getenv("ASTRA_ENGINE_URL"))
        snprintf(cfg->engine_url, sizeof(cfg->engine_url), "%s", getenv("ASTRA_ENGINE_URL"));
}

void config_print(const astra_config *cfg) {
    printf("mode           : %s\n", cfg->mode == MODE_HEADLESS ? "headless" : "full");
    printf("viewport       : %dx%d\n", cfg->width, cfg->height);
    printf("engine         : %s%s\n", cfg->engine_path[0] ? cfg->engine_path : "(autodetect)",
           cfg->engine_url[0] ? " (attached)" : "");
    printf("cdp endpoint   : %s:%d\n", cfg->bind_addr, cfg->port);
    printf("lite mode      : %s\n", cfg->lite ? "on" : "off");
    printf("  block ads    : %s\n", cfg->block_ads ? "on" : "off");
    printf("  images       : %s (max width %d, q%d, min %zu B)\n",
           cfg->optimize_images ? "on" : "off", cfg->max_image_width, cfg->image_quality,
           cfg->image_min_bytes);
    printf("  minify       : html=%s css=%s js=%s\n", cfg->minify_html ? "on" : "off",
           cfg->minify_css ? "on" : "off", cfg->minify_js ? "on" : "off");
    printf("  lazy-load    : %s\n", cfg->lazy_load ? "on" : "off");
    printf("  save-data    : %s\n", cfg->save_data ? "on" : "off");
    printf("  video        : %s\n", cfg->video == VIDEO_BLOCK ? "block" : "auto");
    printf("cache          : %s (%s, max %zu MB)\n", cfg->cache_enabled ? "on" : "off",
           cfg->cache_dir, cfg->cache_max_bytes / (1024 * 1024));
}
