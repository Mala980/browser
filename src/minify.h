/* astra/minify.h - safe, conservative HTML/CSS/JS size reduction */
#ifndef ASTRA_MINIFY_H
#define ASTRA_MINIFY_H

#include "util.h"

/* Conservative HTML minification: drops comments, collapses whitespace outside of
 * <pre>/<textarea>/<script>/<style>, and (optionally) injects lazy-loading
 * attributes on <img>/<iframe>.  Markup semantics are preserved. */
void minify_html(const char *in, size_t len, buf_t *out, int inject_lazy);

/* CSS minification: strips comments, collapses whitespace, drops redundant
 * semicolons, shortens safe zero-values and #rrggbb colors. */
void minify_css(const char *in, size_t len, buf_t *out);

/* JS comment stripper (opt-in, off by default: it is heuristic by nature). */
void minify_js(const char *in, size_t len, buf_t *out);

/* Counts how many bytes were removed by the last call on this thread (diagnostics). */
size_t minify_last_saved(void);

#endif /* ASTRA_MINIFY_H */
