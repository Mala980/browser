/* astra/filters.c */
#include "filters.h"

#include "filters_builtin.h"

#include <stdlib.h>

int res_type_mask(const char *resource_type) {
    if (!resource_type) return RES_OTHER;
    if (!strcmp(resource_type, "Image")) return RES_IMAGE;
    if (!strcmp(resource_type, "Script")) return RES_SCRIPT;
    if (!strcmp(resource_type, "Stylesheet")) return RES_STYLE;
    if (!strcmp(resource_type, "XHR") || !strcmp(resource_type, "Fetch")) return RES_XHR;
    if (!strcmp(resource_type, "Font")) return RES_FONT;
    if (!strcmp(resource_type, "Media")) return RES_MEDIA;
    if (!strcmp(resource_type, "Document")) return RES_DOC;
    return RES_OTHER;
}

void filters_init(filters_t *f) { memset(f, 0, sizeof(*f)); }

void filters_free(filters_t *f) {
    for (size_t i = 0; i < f->n; i++) free(f->rules[i].domain);
    free(f->rules);
    memset(f, 0, sizeof(*f));
}

static void filters_add(filters_t *f, const char *domain, int third_party_only, int exception,
                        int types) {
    if (!domain || !*domain) return;
    if (f->n == f->cap) {
        f->cap = f->cap ? f->cap * 2 : 128;
        f->rules = (filter_rule_t *)realloc(f->rules, f->cap * sizeof(filter_rule_t));
        if (!f->rules) return;
    }
    filter_rule_t *r = &f->rules[f->n++];
    r->domain = astr_lower_dup(domain);
    r->third_party_only = third_party_only;
    r->exception = exception;
    r->types = types;
}

void filters_load_builtin(filters_t *f) {
    for (int i = 0; ASTRA_BUILTIN_FILTERS[i]; i++)
        filters_load_text(f, ASTRA_BUILTIN_FILTERS[i], strlen(ASTRA_BUILTIN_FILTERS[i]));
}

int filters_load_text(filters_t *f, const char *text, size_t len) {
    if (!f || !text) return -1;
    size_t pos = 0;
    while (pos < len) {
        size_t eol = pos;
        while (eol < len && text[eol] != '\n') eol++;
        size_t ll = eol - pos;
        while (ll && (text[pos + ll - 1] == '\r' || text[pos + ll - 1] == ' ')) ll--;
        if (ll) {
            char *line = astr_dupn(text + pos, ll);
            f->parsed_lines++;
            char *trimmed = astr_trim(line);
            if (!*trimmed || *trimmed == '!' || *trimmed == '#') {
                free(line);
                goto next;
            }
            /* cosmetic / scriptlet rules: ignore */
            if (strstr(trimmed, "##") || strstr(trimmed, "#@#") || strstr(trimmed, "#%#") ||
                strstr(trimmed, "#$#")) {
                f->skipped_lines++;
                free(line);
                goto next;
            }
            int exception = 0;
            char *rule = trimmed;
            if (rule[0] == '@' && rule[1] == '@') {
                exception = 1;
                rule += 2;
            }
            /* options */
            int third_party_only = 0, types = 0;
            char *dollar = strchr(rule, '$');
            if (dollar) {
                *dollar = 0;
                char *opts = dollar + 1;
                char *save = NULL;
                for (char *tok = strtok_r(opts, ",", &save); tok;
                     tok = strtok_r(NULL, ",", &save)) {
                    if (!strcmp(tok, "third-party"))
                        third_party_only = 1;
                    else if (!strcmp(tok, "script"))
                        types |= RES_SCRIPT;
                    else if (!strcmp(tok, "image"))
                        types |= RES_IMAGE;
                    else if (!strcmp(tok, "stylesheet"))
                        types |= RES_STYLE;
                    else if (!strcmp(tok, "xmlhttprequest"))
                        types |= RES_XHR;
                    else if (!strcmp(tok, "font"))
                        types |= RES_FONT;
                    else if (!strcmp(tok, "media"))
                        types |= RES_MEDIA;
                }
            }
            /* Supported forms: "||domain^", "||domain^$options", "domain".
             * Wildcards, regexes and path-specific rules are counted as skipped:
             * silently downgrading a path rule to a domain rule would create
             * wrong (over-broad) exceptions. */
            char *dom = rule;
            if (dom[0] == '|' && dom[1] == '|')
                dom += 2;
            else if (dom[0] == '|')
                dom += 1;
            char *path = strchr(dom, '/');
            if (path) *path = 0;
            char *end = strpbrk(dom, "^$");
            if (end) *end = 0;
            if (path || !*dom || strchr(dom, '*') || strchr(dom, '.')) {
                /* "domain" here still contains subdomains; '/' means path rule */
                if (path || strchr(dom, '*')) {
                    f->skipped_lines++;
                    free(line);
                    goto next;
                }
            }
            filters_add(f, dom, third_party_only, exception, types);
            free(line);
        }
    next:
        pos = eol < len ? eol + 1 : len;
    }
    return 0;
}

int filters_load_file(filters_t *f, const char *path) {
    uint8_t *data = NULL;
    size_t n = 0;
    if (file_read(path, &data, &n) != 0) return -1;
    int rc = filters_load_text(f, (const char *)data, n);
    free(data);
    return rc;
}

int filters_is_blocked(const filters_t *f, const char *request_url, const char *page_host,
                       const char *resource_type) {
    if (!f || !request_url) return 0;
    url_t u;
    if (url_parse(request_url, &u) != 0 || !u.host[0]) return 0;
    if (!strcmp(u.scheme, "data") || !strcmp(u.scheme, "blob")) return 0;
    int type = res_type_mask(resource_type);
    int blocked = 0;
    for (size_t i = 0; i < f->n; i++) {
        const filter_rule_t *r = &f->rules[i];
        if (!host_matches_domain(u.host, r->domain)) continue;
        if (r->types && !(r->types & type)) continue;
        if (r->third_party_only && page_host && url_same_site(page_host, u.host)) continue;
        if (r->exception) return 0; /* @@ exception always wins */
        blocked = 1;
    }
    return blocked;
}
