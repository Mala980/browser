/* astra/minify.c */
#include "minify.h"

#include <ctype.h>
#include <stdlib.h>

static size_t g_last_saved = 0;
size_t minify_last_saved(void) { return g_last_saved; }

static int tag_named(const char *p, size_t avail, const char *name) {
    size_t n = strlen(name);
    if (avail < n + 1) return 0;
    if (strncasecmp(p, name, n) != 0) return 0;
    char c = p[n];
    return c == ' ' || c == '>' || c == '\t' || c == '\n' || c == '\r' || c == '/' || c == '<';
}

/* copies a tag (from '<' up to and including '>'), honouring quoted attribute values */
static size_t tag_len(const char *p, size_t avail) {
    size_t i = 1;
    char quote = 0;
    while (i < avail) {
        char c = p[i];
        if (quote) {
            if (c == quote) quote = 0;
        } else if (c == '"' || c == '\'') {
            quote = c;
        } else if (c == '>') {
            return i + 1;
        }
        i++;
    }
    return avail;
}

static int tag_has_attr(const char *tag, size_t len, const char *attr) {
    size_t n = strlen(attr);
    for (size_t i = 0; i + n < len; i++) {
        if (tag[i] == '"' || tag[i] == '\'' || tag[i] == '=') continue;
        if (strncasecmp(tag + i, attr, n) == 0) {
            char prev = i ? tag[i - 1] : ' ';
            if (isspace((unsigned char)prev) || prev == '<') return 1;
        }
    }
    return 0;
}

void minify_html(const char *in, size_t len, buf_t *out, int inject_lazy) {
    if (!in) return;
    size_t i = 0;
    int prev_was_gt = 0;
    while (i < len) {
        if (in[i] == '<') {
            size_t avail = len - i;
            /* comments */
            if (avail >= 4 && in[i + 1] == '!' && in[i + 2] == '-' && in[i + 3] == '-') {
                int conditional = avail > 8 && strncasecmp(in + i + 4, "[if", 3) == 0;
                const char *end = NULL;
                for (size_t j = i + 4; j + 2 < len; j++) {
                    if (in[j] == '-' && in[j + 1] == '-' && in[j + 2] == '>') {
                        end = in + j + 3;
                        break;
                    }
                }
                if (!end) end = in + len;
                if (conditional) buf_append(out, in + i, (size_t)(end - (in + i)));
                i = (size_t)(end - in);
                continue;
            }
            size_t tl = tag_len(in + i, avail);
            /* raw text elements */
            if (tag_named(in + i + 1, avail - 1, "script") ||
                tag_named(in + i + 1, avail - 1, "style") ||
                tag_named(in + i + 1, avail - 1, "pre") ||
                tag_named(in + i + 1, avail - 1, "textarea")) {
                buf_append(out, in + i, tl);
                i += tl;
                const char *closers[4] = {"</script", "</style", "</pre", "</textarea"};
                int which = tag_named(in + i - tl + 1, avail - 1, "script")    ? 0
                            : tag_named(in + i - tl + 1, avail - 1, "style")   ? 1
                            : tag_named(in + i - tl + 1, avail - 1, "pre")     ? 2
                                                                               : 3;
                const char *closer = closers[which];
                size_t cl = strlen(closer);
                const char *found = NULL;
                for (size_t j = i; j + cl < len; j++) {
                    if (strncasecmp(in + j, closer, cl) == 0) {
                        found = in + j;
                        break;
                    }
                }
                size_t rawlen = found ? (size_t)(found - (in + i)) : len - i;
                buf_append(out, in + i, rawlen);
                i += rawlen;
                continue;
            }
            /* inject lazy loading */
            if (inject_lazy && tl > 4 &&
                (tag_named(in + i + 1, avail - 1, "img") ||
                 tag_named(in + i + 1, avail - 1, "iframe")) &&
                !tag_has_attr(in + i, tl, "loading=")) {
                if (in[i + tl - 2] == '/') {
                    buf_append(out, in + i, tl - 2);
                    buf_appendstr(out, " loading=\"lazy\" decoding=\"async\"/>");
                } else {
                    buf_append(out, in + i, tl - 1);
                    buf_appendstr(out, " loading=\"lazy\" decoding=\"async\">");
                }
                i += tl;
                prev_was_gt = 1;
                continue;
            }
            buf_append(out, in + i, tl);
            i += tl;
            prev_was_gt = 1;
            continue;
        }
        /* text run: collapse whitespace */
        size_t start = i;
        while (i < len && in[i] != '<') i++;
        size_t tlen = i - start;
        int all_ws = 1;
        for (size_t k = 0; k < tlen; k++)
            if (!isspace((unsigned char)in[start + k])) {
                all_ws = 0;
                break;
            }
        if (all_ws) {
            /* drop whitespace between tags, keep a single space inside text */
            int next_is_lt = (i < len && in[i] == '<');
            if (prev_was_gt && next_is_lt) {
                continue;
            }
            buf_appendc(out, ' ');
            continue;
        }
        int pending_space = 0;
        for (size_t k = 0; k < tlen; k++) {
            char c = in[start + k];
            if (isspace((unsigned char)c)) {
                pending_space = 1;
                continue;
            }
            if (pending_space) {
                buf_appendc(out, ' ');
                pending_space = 0;
            }
            buf_appendc(out, c);
        }
        if (pending_space) buf_appendc(out, ' ');
        prev_was_gt = 0;
    }
    if (len >= out->len) g_last_saved = len - out->len;
}

/* --------------------------------------------------------------------- CSS */

static int hexval(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

void minify_css(const char *in, size_t len, buf_t *out) {
    if (!in) return;
    size_t i = 0;
    int ws = 0;
    while (i < len) {
        char c = in[i];
        if (c == '/' && i + 1 < len && in[i + 1] == '*') {
            size_t j = i + 2;
            while (j + 1 < len && !(in[j] == '*' && in[j + 1] == '/')) j++;
            i = (j + 1 < len) ? j + 2 : len;
            ws = 0;
            continue;
        }
        if (isspace((unsigned char)c)) {
            ws = 1;
            i++;
            continue;
        }
        if (c == '{' || c == '}' || c == ';' || c == ',' || c == ':') {
            /* drop a trailing ';' before '}' */
            if (c == '}' && out->len && out->data[out->len - 1] == ';') out->len--;
            buf_appendc(out, c);
            i++;
            ws = 0;
            continue;
        }
        if (c == '#') { /* try #rrggbb -> #rgb */
            if (i + 6 < len) {
                int a = hexval(in[i + 1]), b = hexval(in[i + 2]), cc = hexval(in[i + 3]),
                    d = hexval(in[i + 4]), e = hexval(in[i + 5]), f = hexval(in[i + 6]);
                if (a >= 0 && b >= 0 && cc >= 0 && d >= 0 && e >= 0 && f >= 0 && a == b &&
                    cc == d && e == f) {
                    buf_appendc(out, '#');
                    buf_appendc(out, in[i + 1]);
                    buf_appendc(out, in[i + 3]);
                    buf_appendc(out, in[i + 5]);
                    i += 7;
                    ws = 0;
                    continue;
                }
            }
            buf_appendc(out, c);
            i++;
            ws = 0;
            continue;
        }
        if (c == '0' && i + 2 < len) { /* 0px, 0em, 0rem, 0pt, 0% stays */
            char prev = out->len ? (char)out->data[out->len - 1] : ' ';
            if (prev == ':' || prev == ' ' || prev == ',' || prev == '(') {
                if ((in[i + 1] == 'p' && in[i + 2] == 'x') ||
                    (in[i + 1] == 'e' && in[i + 2] == 'm') ||
                    (in[i + 1] == 'p' && in[i + 2] == 't') ||
                    (in[i + 1] == 'c' && in[i + 2] == 'm') ||
                    (in[i + 1] == 'i' && in[i + 2] == 'n')) {
                    buf_appendc(out, '0');
                    i += 3;
                    ws = 0;
                    continue;
                }
                if ((in[i + 1] == 'r' && i + 3 < len && in[i + 2] == 'e' && in[i + 3] == 'm')) {
                    buf_appendc(out, '0');
                    i += 4;
                    ws = 0;
                    continue;
                }
            }
        }
        if (ws && out->len) {
            char prev = (char)out->data[out->len - 1];
            if (!isspace((unsigned char)prev) && prev != '{' && prev != '}' && prev != ';' &&
                prev != ',' && prev != ':')
                buf_appendc(out, ' ');
        }
        buf_appendc(out, c);
        i++;
        ws = 0;
    }
    if (len >= out->len) g_last_saved = len - out->len;
}

/* ---------------------------------------------------------------------- JS */

void minify_js(const char *in, size_t len, buf_t *out) {
    if (!in) return;
    size_t i = 0;
    enum { S_CODE, S_STR, S_STR2, S_TEMPLATE, S_LINE, S_BLOCK, S_REGEX } st = S_CODE;
    while (i < len) {
        char c = in[i];
        switch (st) {
            case S_CODE:
                if (c == '/' && i + 1 < len && in[i + 1] == '/') {
                    st = S_LINE;
                    i += 2;
                    continue;
                }
                if (c == '/' && i + 1 < len && in[i + 1] == '*') {
                    st = S_BLOCK;
                    i += 2;
                    continue;
                }
                if (c == '"') {
                    st = S_STR;
                } else if (c == '\'') {
                    st = S_STR2;
                } else if (c == '`') {
                    st = S_TEMPLATE;
                } else if (c == '/') {
                    /* heuristic: '/' after an operator/brace starts a regex */
                    char prev = out->len ? (char)out->data[out->len - 1] : ';';
                    if (strchr("(,=:[!&|?{};+*%~^<>", prev) != NULL) {
                        st = S_REGEX;
                        buf_appendc(out, c);
                        i++;
                        continue;
                    }
                }
                buf_appendc(out, c);
                i++;
                continue;
            case S_STR:
            case S_STR2: {
                char q = (st == S_STR) ? '"' : '\'';
                buf_appendc(out, c);
                i++;
                if (c == '\\' && i < len) {
                    buf_appendc(out, in[i]);
                    i++;
                } else if (c == q) {
                    st = S_CODE;
                }
                continue;
            }
            case S_TEMPLATE:
                buf_appendc(out, c);
                i++;
                if (c == '\\' && i < len) {
                    buf_appendc(out, in[i]);
                    i++;
                } else if (c == '`') {
                    st = S_CODE;
                }
                continue;
            case S_LINE:
                if (c == '\n') {
                    st = S_CODE;
                    buf_appendc(out, '\n');
                }
                i++;
                continue;
            case S_BLOCK:
                if (c == '*' && i + 1 < len && in[i + 1] == '/') {
                    st = S_CODE;
                    i += 2;
                    continue;
                }
                if (c == '\n') buf_appendc(out, '\n');
                i++;
                continue;
            case S_REGEX:
                buf_appendc(out, c);
                i++;
                if (c == '\\' && i < len) {
                    buf_appendc(out, in[i]);
                    i++;
                } else if (c == '/') {
                    st = S_CODE;
                } else if (c == '\n') {
                    st = S_CODE;
                }
                continue;
        }
    }
    if (len >= out->len) g_last_saved = len - out->len;
}
