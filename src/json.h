/* astra/json.h - minimal JSON parser / serializer (no dependencies) */
#ifndef ASTRA_JSON_H
#define ASTRA_JSON_H

#include "util.h"

typedef enum { J_NULL = 0, J_BOOL, J_NUM, J_STR, J_ARR, J_OBJ } jtype_t;

typedef struct json json_t;

struct json {
    jtype_t type;
    double num;
    int bval;
    char *str; /* J_STR: NUL terminated */
    json_t **items;
    char **keys; /* J_OBJ only, parallel to items */
    size_t n, cap;
};

/* parsing */
json_t *json_parse(const char *s, size_t len);
json_t *json_parse_cstr(const char *s);
void json_free(json_t *v);

/* accessors */
json_t *json_get(json_t *obj, const char *key); /* NULL if not found */
const char *json_get_str(json_t *obj, const char *key, const char *def);
double json_get_num(json_t *obj, const char *key, double def);
int json_get_bool(json_t *obj, const char *key, int def);
json_t *json_at(json_t *arr, size_t i);
size_t json_len(json_t *v);
const char *json_as_str(json_t *v, const char *def);
double json_as_num(json_t *v, double def);
int json_as_bool(json_t *v, int def);

/* deep copy */
json_t *json_clone(const json_t *v);

/* builders (take ownership of values) */
json_t *jnull(void);
json_t *jbool(int v);
json_t *jnum(double v);
json_t *jstr(const char *s);
json_t *jstrn(const char *s, size_t n);
json_t *jarr(void);
json_t *jobj(void);
void jset(json_t *obj, const char *key, json_t *val);
void jpush(json_t *arr, json_t *val);

/* serialization */
void json_write(json_t *v, buf_t *out);
char *json_stringify(json_t *v); /* malloc'd */

#endif /* ASTRA_JSON_H */
