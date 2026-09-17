/* astra/json.c */
#include "json.h"

#include <math.h>
#include <stdlib.h>

static json_t *json_new(jtype_t t) {
    json_t *v = (json_t *)calloc(1, sizeof(json_t));
    if (!v) return NULL;
    v->type = t;
    return v;
}

void json_free(json_t *v) {
    if (!v) return;
    free(v->str);
    for (size_t i = 0; i < v->n; i++) {
        json_free(v->items[i]);
        if (v->keys) free(v->keys[i]);
    }
    free(v->items);
    free(v->keys);
    free(v);
}

json_t *jnull(void) { return json_new(J_NULL); }

json_t *jbool(int v) {
    json_t *j = json_new(J_BOOL);
    j->bval = v ? 1 : 0;
    return j;
}

json_t *jnum(double v) {
    json_t *j = json_new(J_NUM);
    j->num = v;
    return j;
}

json_t *jstrn(const char *s, size_t n) {
    json_t *j = json_new(J_STR);
    j->str = astr_dupn(s ? s : "", n);
    return j;
}

json_t *jstr(const char *s) { return jstrn(s, s ? strlen(s) : 0); }

json_t *jarr(void) { return json_new(J_ARR); }

json_t *jobj(void) { return json_new(J_OBJ); }

static void json_reserve(json_t *v, size_t extra) {
    if (v->n + extra <= v->cap) return;
    size_t nc = v->cap ? v->cap * 2 : 8;
    while (nc < v->n + extra) nc *= 2;
    json_t **ni = (json_t **)realloc(v->items, nc * sizeof(json_t *));
    if (!ni) return;
    v->items = ni;
    if (v->type == J_OBJ) {
        char **nk = (char **)realloc(v->keys, nc * sizeof(char *));
        if (!nk) return;
        v->keys = nk;
    }
    v->cap = nc;
}

void jpush(json_t *arr, json_t *val) {
    if (!arr || (arr->type != J_ARR && arr->type != J_OBJ) || !val) return;
    json_reserve(arr, 1);
    arr->items[arr->n] = val;
    if (arr->type == J_OBJ) arr->keys[arr->n] = NULL;
    arr->n++;
}

void jset(json_t *obj, const char *key, json_t *val) {
    if (!obj || obj->type != J_OBJ || !key || !val) return;
    for (size_t i = 0; i < obj->n; i++) {
        if (obj->keys[i] && strcmp(obj->keys[i], key) == 0) {
            json_free(obj->items[i]);
            obj->items[i] = val;
            return;
        }
    }
    json_reserve(obj, 1);
    obj->items[obj->n] = val;
    obj->keys[obj->n] = astr_dup(key);
    obj->n++;
}

void json_del(json_t *obj, const char *key) {
    if (!obj || obj->type != J_OBJ || !key) return;
    for (size_t i = 0; i < obj->n; i++) {
        if (obj->keys[i] && strcmp(obj->keys[i], key) == 0) {
            json_free(obj->items[i]);
            free(obj->keys[i]);
            for (size_t j = i + 1; j < obj->n; j++) {
                obj->items[j - 1] = obj->items[j];
                obj->keys[j - 1] = obj->keys[j];
            }
            obj->n--;
            return;
        }
    }
}

/* --------------------------------------------------------------- accessors */

json_t *json_get(json_t *obj, const char *key) {
    if (!obj || obj->type != J_OBJ || !key) return NULL;
    for (size_t i = 0; i < obj->n; i++)
        if (obj->keys[i] && strcmp(obj->keys[i], key) == 0) return obj->items[i];
    return NULL;
}

json_t *json_at(json_t *arr, size_t i) {
    if (!arr || (arr->type != J_ARR && arr->type != J_OBJ)) return NULL;
    return i < arr->n ? arr->items[i] : NULL;
}

size_t json_len(json_t *v) { return v ? v->n : 0; }

const char *json_as_str(json_t *v, const char *def) {
    return (v && v->type == J_STR) ? v->str : def;
}

double json_as_num(json_t *v, double def) {
    if (!v) return def;
    if (v->type == J_NUM) return v->num;
    if (v->type == J_BOOL) return v->bval ? 1 : 0;
    if (v->type == J_STR) return atof(v->str);
    return def;
}

int json_as_bool(json_t *v, int def) {
    if (!v) return def;
    if (v->type == J_BOOL) return v->bval;
    if (v->type == J_NUM) return v->num != 0;
    if (v->type == J_STR) return strcmp(v->str, "true") == 0;
    return def;
}

const char *json_get_str(json_t *obj, const char *key, const char *def) {
    return json_as_str(json_get(obj, key), def);
}

double json_get_num(json_t *obj, const char *key, double def) {
    return json_as_num(json_get(obj, key), def);
}

int json_get_bool(json_t *obj, const char *key, int def) {
    return json_as_bool(json_get(obj, key), def);
}

json_t *json_clone(const json_t *v) {
    if (!v) return NULL;
    switch (v->type) {
        case J_NULL: return jnull();
        case J_BOOL: return jbool(v->bval);
        case J_NUM: return jnum(v->num);
        case J_STR: return jstr(v->str);
        case J_ARR:
        case J_OBJ: {
            json_t *c = (v->type == J_ARR) ? jarr() : jobj();
            for (size_t i = 0; i < v->n; i++) {
                json_t *item = json_clone(v->items[i]);
                if (v->type == J_OBJ) {
                    jpush(c, item);
                    c->keys[c->n - 1] = astr_dup(v->keys[i] ? v->keys[i] : "");
                } else {
                    jpush(c, item);
                }
            }
            return c;
        }
    }
    return NULL;
}

/* ------------------------------------------------------------------ parser */

typedef struct {
    const char *p;
    const char *end;
    int ok;
} jp_t;

static json_t *jp_value(jp_t *s);

static void jp_skip_ws(jp_t *s) {
    while (s->p < s->end && (*s->p == ' ' || *s->p == '\t' || *s->p == '\n' || *s->p == '\r')) s->p++;
}

static int jp_lit(jp_t *s, const char *lit) {
    size_t n = strlen(lit);
    if ((size_t)(s->end - s->p) < n || memcmp(s->p, lit, n) != 0) return 0;
    s->p += n;
    return 1;
}

static int jp_hex4(jp_t *s, unsigned *out) {
    if (s->end - s->p < 4) return 0;
    unsigned v = 0;
    for (int i = 0; i < 4; i++) {
        char c = s->p[i];
        v <<= 4;
        if (c >= '0' && c <= '9')
            v |= (unsigned)(c - '0');
        else if (c >= 'a' && c <= 'f')
            v |= (unsigned)(c - 'a' + 10);
        else if (c >= 'A' && c <= 'F')
            v |= (unsigned)(c - 'A' + 10);
        else
            return 0;
    }
    s->p += 4;
    *out = v;
    return 1;
}

static void utf8_put(buf_t *b, unsigned cp) {
    if (cp < 0x80) {
        buf_appendc(b, (char)cp);
    } else if (cp < 0x800) {
        buf_appendc(b, (char)(0xC0 | (cp >> 6)));
        buf_appendc(b, (char)(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        buf_appendc(b, (char)(0xE0 | (cp >> 12)));
        buf_appendc(b, (char)(0x80 | ((cp >> 6) & 0x3F)));
        buf_appendc(b, (char)(0x80 | (cp & 0x3F)));
    } else {
        buf_appendc(b, (char)(0xF0 | (cp >> 18)));
        buf_appendc(b, (char)(0x80 | ((cp >> 12) & 0x3F)));
        buf_appendc(b, (char)(0x80 | ((cp >> 6) & 0x3F)));
        buf_appendc(b, (char)(0x80 | (cp & 0x3F)));
    }
}

static json_t *jp_string(jp_t *s) {
    if (s->p >= s->end || *s->p != '"') {
        s->ok = 0;
        return NULL;
    }
    s->p++;
    buf_t b;
    buf_init(&b);
    while (s->p < s->end && *s->p != '"') {
        if (*s->p == '\\') {
            s->p++;
            if (s->p >= s->end) break;
            char c = *s->p++;
            switch (c) {
                case '"': buf_appendc(&b, '"'); break;
                case '\\': buf_appendc(&b, '\\'); break;
                case '/': buf_appendc(&b, '/'); break;
                case 'b': buf_appendc(&b, '\b'); break;
                case 'f': buf_appendc(&b, '\f'); break;
                case 'n': buf_appendc(&b, '\n'); break;
                case 'r': buf_appendc(&b, '\r'); break;
                case 't': buf_appendc(&b, '\t'); break;
                case 'u': {
                    unsigned cp = 0;
                    if (!jp_hex4(s, &cp)) {
                        s->ok = 0;
                        buf_free(&b);
                        return NULL;
                    }
                    if (cp >= 0xD800 && cp <= 0xDBFF && s->end - s->p >= 6 && s->p[0] == '\\' &&
                        s->p[1] == 'u') {
                        const char *save = s->p;
                        s->p += 2;
                        unsigned lo = 0;
                        if (jp_hex4(s, &lo) && lo >= 0xDC00 && lo <= 0xDFFF) {
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        } else {
                            s->p = save;
                        }
                    }
                    utf8_put(&b, cp);
                    break;
                }
                default: buf_appendc(&b, c); break;
            }
        } else {
            buf_appendc(&b, *s->p++);
        }
    }
    if (s->p >= s->end || *s->p != '"') {
        s->ok = 0;
        buf_free(&b);
        return NULL;
    }
    s->p++;
    json_t *v = json_new(J_STR);
    v->str = b.data ? (char *)b.data : astr_dup("");
    if (!b.data) buf_init(&b);
    return v;
}

static json_t *jp_number(jp_t *s) {
    const char *start = s->p;
    if (s->p < s->end && (*s->p == '-' || *s->p == '+')) s->p++;
    int isfloat = 0;
    while (s->p < s->end) {
        char c = *s->p;
        if (c >= '0' && c <= '9')
            s->p++;
        else if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
            if (c == '.' || c == 'e' || c == 'E') isfloat = 1;
            s->p++;
        } else
            break;
    }
    if (s->p == start) {
        s->ok = 0;
        return NULL;
    }
    (void)isfloat;
    json_t *v = json_new(J_NUM);
    v->num = strtod(start, NULL);
    return v;
}

static json_t *jp_value(jp_t *s) {
    jp_skip_ws(s);
    if (s->p >= s->end) {
        s->ok = 0;
        return NULL;
    }
    char c = *s->p;
    if (c == '{') {
        s->p++;
        json_t *o = jobj();
        jp_skip_ws(s);
        if (s->p < s->end && *s->p == '}') {
            s->p++;
            return o;
        }
        for (;;) {
            jp_skip_ws(s);
            if (s->p >= s->end || *s->p != '"') {
                s->ok = 0;
                json_free(o);
                return NULL;
            }
            json_t *k = jp_string(s);
            if (!k) {
                json_free(o);
                return NULL;
            }
            jp_skip_ws(s);
            if (s->p >= s->end || *s->p != ':') {
                s->ok = 0;
                json_free(k);
                json_free(o);
                return NULL;
            }
            s->p++;
            json_t *val = jp_value(s);
            if (!val) {
                json_free(k);
                json_free(o);
                return NULL;
            }
            jpush(o, val);
            o->keys[o->n - 1] = k->str;
            k->str = NULL;
            json_free(k);
            jp_skip_ws(s);
            if (s->p < s->end && *s->p == ',') {
                s->p++;
                continue;
            }
            if (s->p < s->end && *s->p == '}') {
                s->p++;
                return o;
            }
            s->ok = 0;
            json_free(o);
            return NULL;
        }
    }
    if (c == '[') {
        s->p++;
        json_t *a = jarr();
        jp_skip_ws(s);
        if (s->p < s->end && *s->p == ']') {
            s->p++;
            return a;
        }
        for (;;) {
            json_t *val = jp_value(s);
            if (!val) {
                json_free(a);
                return NULL;
            }
            jpush(a, val);
            jp_skip_ws(s);
            if (s->p < s->end && *s->p == ',') {
                s->p++;
                continue;
            }
            if (s->p < s->end && *s->p == ']') {
                s->p++;
                return a;
            }
            s->ok = 0;
            json_free(a);
            return NULL;
        }
    }
    if (c == '"') return jp_string(s);
    if (jp_lit(s, "true")) return jbool(1);
    if (jp_lit(s, "false")) return jbool(0);
    if (jp_lit(s, "null")) return jnull();
    return jp_number(s);
}

json_t *json_parse(const char *s, size_t len) {
    if (!s) return NULL;
    jp_t st;
    st.p = s;
    st.end = s + len;
    st.ok = 1;
    json_t *v = jp_value(&st);
    if (!st.ok) {
        json_free(v);
        return NULL;
    }
    return v;
}

json_t *json_parse_cstr(const char *s) { return s ? json_parse(s, strlen(s)) : NULL; }

/* ------------------------------------------------------------- serializer */

static void json_write_str(buf_t *out, const char *s) {
    buf_appendc(out, '"');
    for (const unsigned char *p = (const unsigned char *)s; p && *p; p++) {
        unsigned char c = *p;
        switch (c) {
            case '"': buf_appendstr(out, "\\\""); break;
            case '\\': buf_appendstr(out, "\\\\"); break;
            case '\n': buf_appendstr(out, "\\n"); break;
            case '\r': buf_appendstr(out, "\\r"); break;
            case '\t': buf_appendstr(out, "\\t"); break;
            case '\b': buf_appendstr(out, "\\b"); break;
            case '\f': buf_appendstr(out, "\\f"); break;
            default:
                if (c < 0x20)
                    buf_appendf(out, "\\u%04x", c);
                else
                    buf_appendc(out, (char)c);
        }
    }
    buf_appendc(out, '"');
}

void json_write(json_t *v, buf_t *out) {
    if (!v) {
        buf_appendstr(out, "null");
        return;
    }
    switch (v->type) {
        case J_NULL: buf_appendstr(out, "null"); break;
        case J_BOOL: buf_appendstr(out, v->bval ? "true" : "false"); break;
        case J_NUM: {
            if (v->num == (double)(long long)v->num && fabs(v->num) < 9.0e15)
                buf_appendf(out, "%lld", (long long)v->num);
            else
                buf_appendf(out, "%.17g", v->num);
            break;
        }
        case J_STR: json_write_str(out, v->str); break;
        case J_ARR:
            buf_appendc(out, '[');
            for (size_t i = 0; i < v->n; i++) {
                if (i) buf_appendc(out, ',');
                json_write(v->items[i], out);
            }
            buf_appendc(out, ']');
            break;
        case J_OBJ:
            buf_appendc(out, '{');
            for (size_t i = 0; i < v->n; i++) {
                if (i) buf_appendc(out, ',');
                json_write_str(out, v->keys[i] ? v->keys[i] : "");
                buf_appendc(out, ':');
                json_write(v->items[i], out);
            }
            buf_appendc(out, '}');
            break;
    }
}

char *json_stringify(json_t *v) {
    buf_t b;
    buf_init(&b);
    json_write(v, &b);
    char *s = b.data ? (char *)b.data : astr_dup("null");
    if (!b.data) buf_init(&b);
    return s;
}
