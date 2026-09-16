/* astra/filters.h - ad / tracker request blocking (EasyList style subset) */
#ifndef ASTRA_FILTERS_H
#define ASTRA_FILTERS_H

#include "util.h"

#define RES_IMAGE 0x001
#define RES_SCRIPT 0x002
#define RES_STYLE 0x004
#define RES_XHR 0x008
#define RES_FONT 0x010
#define RES_MEDIA 0x020
#define RES_OTHER 0x040
#define RES_DOC 0x080

typedef struct {
    char *domain; /* host pattern, e.g. "doubleclick.net" */
    int third_party_only;
    int exception;
    int types; /* mask of RES_* ; 0 = any */
} filter_rule_t;

typedef struct {
    filter_rule_t *rules;
    size_t n, cap;
    size_t parsed_lines;
    size_t skipped_lines;
} filters_t;

void filters_init(filters_t *f);
void filters_free(filters_t *f);
void filters_load_builtin(filters_t *f);
int filters_load_text(filters_t *f, const char *text, size_t len);
int filters_load_file(filters_t *f, const char *path);
int filters_is_blocked(const filters_t *f, const char *request_url, const char *page_host,
                       const char *resource_type);
int res_type_mask(const char *resource_type);

#endif /* ASTRA_FILTERS_H */
