/* astra/http.h - tiny blocking HTTP client + request/response helpers */
#ifndef ASTRA_HTTP_H
#define ASTRA_HTTP_H

#include "json.h"
#include "util.h"

typedef struct {
    int status;
    char *body;
    size_t body_len;
    char **names;
    char **values;
    size_t nh;
} http_resp_t;

int http_get(const char *host, int port, const char *path, int timeout_ms, http_resp_t *out);
int http_request(const char *method, const char *host, int port, const char *path,
                 const char *body, int timeout_ms, http_resp_t *out);
const char *http_resp_header(const http_resp_t *r, const char *name);
void http_resp_free(http_resp_t *r);

/* server side */
typedef struct {
    char method[16];
    char target[1024];
    char **names;
    char **values;
    size_t nh;
    char *body;
    size_t body_len;
    size_t consumed;
} http_req_t;

/* 1 = complete request parsed, 0 = need more data, -1 = malformed */
int http_req_parse(const char *data, size_t len, http_req_t *out);
const char *http_req_header(const http_req_t *r, const char *name);
void http_req_free(http_req_t *r);
void http_write_response(buf_t *out, int status, const char *ctype, const char *body,
                         size_t body_len);

#endif /* ASTRA_HTTP_H */
