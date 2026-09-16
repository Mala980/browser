/* astra/stats.c */
#include "stats.h"

#include <stdio.h>

astra_stats_t g_stats;

void stats_reset(void) {
    memset(&g_stats, 0, sizeof(g_stats));
    g_stats.started_at = wall_ms();
}

void stats_json(json_t *out) {
    jset(out, "requests", jnum((double)g_stats.requests));
    jset(out, "blocked", jnum((double)g_stats.blocked));
    jset(out, "cacheHits", jnum((double)g_stats.cache_hits));
    jset(out, "revalidated", jnum((double)g_stats.revalidated));
    jset(out, "imagesOptimized", jnum((double)g_stats.images_optimized));
    jset(out, "textMinified", jnum((double)g_stats.text_minified));
    jset(out, "passthrough", jnum((double)g_stats.passthrough));
    jset(out, "bytesOriginal", jnum((double)g_stats.bytes_original));
    jset(out, "bytesDelivered", jnum((double)g_stats.bytes_delivered));
    jset(out, "bytesBlocked", jnum((double)g_stats.bytes_blocked));
    jset(out, "bytesSavedImages", jnum((double)g_stats.bytes_saved_images));
    jset(out, "bytesSavedText", jnum((double)g_stats.bytes_saved_text));
    jset(out, "bytesSavedCache", jnum((double)g_stats.bytes_saved_cache));
    jset(out, "bytesSavedMetadata", jnum((double)g_stats.bytes_saved_metadata));
    jset(out, "bytesSaved", jnum((double)(g_stats.bytes_original - g_stats.bytes_delivered)));
    jset(out, "savingPct", jnum(stats_saving_pct()));
    jset(out, "imageMs", jnum(g_stats.image_ms));
    jset(out, "minifyMs", jnum(g_stats.minify_ms));
    jset(out, "uptimeMs", jnum((double)(wall_ms() - g_stats.started_at)));
}

char *stats_json_text(void) {
    json_t *o = jobj();
    stats_json(o);
    char *s = json_stringify(o);
    json_free(o);
    return s;
}

double stats_saving_pct(void) {
    if (g_stats.bytes_original == 0) return 0.0;
    double saved = (double)g_stats.bytes_original - (double)g_stats.bytes_delivered +
                   (double)g_stats.bytes_saved_cache + (double)g_stats.bytes_blocked;
    if (saved < 0) saved = 0;
    double pct = saved * 100.0 / (double)(g_stats.bytes_original + g_stats.bytes_blocked);
    if (pct > 100.0) pct = 100.0;
    return pct;
}

static void kb(char *out, size_t n, uint64_t bytes) {
    double v = (double)bytes;
    const char *u = "B";
    if (v >= 1024) {
        v /= 1024;
        u = "KB";
    }
    if (v >= 1024) {
        v /= 1024;
        u = "MB";
    }
    if (v >= 1024) {
        v /= 1024;
        u = "GB";
    }
    snprintf(out, n, "%.2f %s", v, u);
}

void stats_print(FILE *f) {
    char a[64], b[64], c[64];
    kb(a, sizeof(a), g_stats.bytes_original);
    kb(b, sizeof(b), g_stats.bytes_delivered);
    kb(c, sizeof(c), g_stats.bytes_original - g_stats.bytes_delivered + g_stats.bytes_blocked +
                         g_stats.bytes_saved_cache);
    fprintf(f, "\n--- astra bandwidth report -------------------------------\n");
    fprintf(f, " requests inspected : %llu\n", (unsigned long long)g_stats.requests);
    fprintf(f, " blocked (ads)      : %llu\n", (unsigned long long)g_stats.blocked);
    fprintf(f, " cache hits         : %llu (revalidated %llu)\n",
            (unsigned long long)g_stats.cache_hits, (unsigned long long)g_stats.revalidated);
    fprintf(f, " images optimized   : %llu\n", (unsigned long long)g_stats.images_optimized);
    fprintf(f, " text minified      : %llu\n", (unsigned long long)g_stats.text_minified);
    fprintf(f, " would download     : %s\n", a);
    fprintf(f, " actually delivered : %s\n", b);
    fprintf(f, " total saved        : %s (%.1f%%)\n", c, stats_saving_pct());
    fprintf(f, " cpu: image %.1f ms, minify %.1f ms\n", g_stats.image_ms, g_stats.minify_ms);
    fprintf(f, "---------------------------------------------------------\n");
}
