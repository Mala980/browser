/* astra/config.h - runtime configuration */
#ifndef ASTRA_CONFIG_H
#define ASTRA_CONFIG_H

#include "util.h"

#define ASTRA_VERSION "0.1.0"
#define ASTRA_DEFAULT_PORT 9222
#define ASTRA_OPTIMIZER_ID_BASE 900000

typedef enum { MODE_HEADLESS = 0, MODE_FULL = 1 } run_mode_t;
typedef enum { VIDEO_AUTO = 0, VIDEO_BLOCK = 1 } video_policy_t;

typedef struct {
    /* engine */
    char engine_path[1024]; /* explicit binary; empty = autodetect */
    char engine_url[512];   /* attach to an already running browser (ws://...) */
    run_mode_t mode;
    int width, height;
    int gpu;
    int no_sandbox;
    int single_process;    /* only used when explicitly requested */
    char profile_dir[1024];/* empty = auto temp dir */
    char extra_flags[1024];

    /* local CDP server */
    int port;
    char bind_addr[64];
    int idle_exit_ms;

    /* optimizing proxy */
    int lite;
    int block_ads;
    int optimize_images;
    int max_image_width;
    int image_quality;
    size_t image_min_bytes;
    int minify_html;
    int minify_css;
    int minify_js;
    int lazy_load;
    int save_data;
    int strip_metadata;
    int block_third_party_cookies;
    video_policy_t video;
    char filter_list[1024];
    char user_agent[512];

    /* cache */
    int cache_enabled;
    size_t cache_max_bytes;
    char cache_dir[1024];

    /* misc */
    log_level_t log_level;
} astra_config;

void config_defaults(astra_config *cfg);
int config_load_file(astra_config *cfg, const char *path);
void config_apply_env(astra_config *cfg);
void config_print(const astra_config *cfg);

/* small helpers */
int parse_bool(const char *s, int def);
long long parse_size(const char *s, long long def);

#endif /* ASTRA_CONFIG_H */
