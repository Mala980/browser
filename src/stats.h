/* astra/stats.h - bandwidth saving counters */
#ifndef ASTRA_STATS_H
#define ASTRA_STATS_H

#include "json.h"
#include "util.h"

typedef struct {
    uint64_t requests;         /* requests inspected */
    uint64_t blocked;          /* blocked by filters */
    uint64_t cache_hits;       /* served from cache without network */
    uint64_t revalidated;      /* 304 -> served from cache */
    uint64_t images_optimized; /* images re-encoded / downscaled */
    uint64_t text_minified;    /* html/css minified */
    uint64_t passthrough;
    uint64_t bytes_original;   /* bytes the page would have downloaded */
    uint64_t bytes_delivered;  /* bytes actually handed to the renderer */
    uint64_t bytes_blocked;
    uint64_t bytes_saved_images;
    uint64_t bytes_saved_text;
    uint64_t bytes_saved_cache;
    uint64_t bytes_saved_metadata;
    double image_ms;
    double minify_ms;
    uint64_t started_at;
} astra_stats_t;

extern astra_stats_t g_stats;

void stats_reset(void);
void stats_json(json_t *out);
char *stats_json_text(void);
void stats_print(FILE *f);
double stats_saving_pct(void);

#endif /* ASTRA_STATS_H */
