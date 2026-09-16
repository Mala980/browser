/* astra/http.c */
#include "http.h"

#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

const char *http_resp_header(const http_resp_t *r, const char *name) {
    if (!r) return NULL;
    for (size_t i = 0; i < r->nh; i++)
        if (strcasecmp(r->names[i], name) == 0) return r->values[i];
    return NULL;
}

void http_resp_free(http_resp_t *r) {
    if (!r) return;
    free(r->body);
    for (size_t i = 0; i < r->nh; i++) {
        free(r->names[i]);
        free(r->values[i]);
    }
    free(r->names);
    free(r->values);
    memset(r, 0, sizeof(*r));
}

static void add_header(http_resp_t *r, const char *name, size_t nl, const char *val, size_t vl) {
    r->names = (char **)realloc(r->names, (r->nh + 1) * sizeof(char *));
    r->values = (char **)realloc(r->values, (r->nh + 1) * sizeof(char *));
    r->names[r->nh] = astr_dupn(name, nl);
    r->values[r->nh] = astr_dupn(val, vl);
    r->nh++;
}

static int connect_timeout(const char *host, int port, int timeout_ms) {
    char portstr[16];
    snprintf(portstr, sizeof(portstr), "%d", port);
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *ai = NULL;
    if (getaddrinfo(host, portstr, &hints, &ai) != 0 || !ai) return -1;
    int fd = -1;
    for (struct addrinfo *p = ai; p; p = p->ai_next) {
        fd = socket(p->ai_family, p->ai_socktype, 0);
        if (fd < 0) continue;
        int one = 1;
        setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
        /* non blocking connect */
        int fl = fcntl(fd, F_GETFL, 0);
        fcntl(fd, F_SETFL, fl | O_NONBLOCK);
        if (connect(fd, p->ai_addr, p->ai_addrlen) == 0) {
            fcntl(fd, F_SETFL, fl);
            freeaddrinfo(ai);
            return fd;
        }
        if (errno != EINPROGRESS) {
            close(fd);
            fd = -1;
            continue;
        }
        struct pollfd pfd;
        pfd.fd = fd;
        pfd.events = POLLOUT;
        int rc = poll(&pfd, 1, timeout_ms);
        if (rc > 0) {
            int err = 0;
            socklen_t elen = sizeof(err);
            if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &elen) == 0 && err == 0) {
                fcntl(fd, F_SETFL, fl);
                freeaddrinfo(ai);
                return fd;
            }
        }
        close(fd);
        fd = -1;
    }
    freeaddrinfo(ai);
    return -1;
}

int http_request(const char *method, const char *host, int port, const char *path,
                 const char *body, int timeout_ms, http_resp_t *out) {
    memset(out, 0, sizeof(*out));
    int fd = connect_timeout(host, port, timeout_ms > 0 ? timeout_ms : 5000);
    if (fd < 0) return -1;
    struct timeval tv;
    tv.tv_sec = (timeout_ms > 0 ? timeout_ms : 5000) / 1000;
    tv.tv_usec = ((timeout_ms > 0 ? timeout_ms : 5000) % 1000) * 1000;
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof(tv));
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof(tv));

    buf_t req;
    buf_init(&req);
    buf_appendf(&req,
                "%s %s HTTP/1.1\r\nHost: %s:%d\r\nConnection: close\r\nUser-Agent: Astra/0.1\r\n"
                "Accept: */*\r\n",
                method ? method : "GET", path, host, port);
    if (body)
        buf_appendf(&req, "Content-Length: %zu\r\n", strlen(body));
    buf_appendstr(&req, "\r\n");
    if (body) buf_appendstr(&req, body);
    size_t off = 0;
    while (off < req.len) {
        ssize_t w = send(fd, req.data + off, req.len - off, 0);
        if (w <= 0) {
            if (errno == EINTR) continue;
            close(fd);
            buf_free(&req);
            return -1;
        }
        off += (size_t)w;
    }
    buf_free(&req);

    buf_t raw;
    buf_init(&raw);
    uint8_t chunk[8192];
    long long content_len = -1;
    int headers_done = 0;
    size_t body_start = 0;
    uint64_t deadline = now_ms() + (uint64_t)(timeout_ms > 0 ? timeout_ms : 5000) + 2000;
    for (;;) {
        ssize_t r = recv(fd, chunk, sizeof(chunk), 0);
        if (r > 0) {
            buf_append(&raw, chunk, (size_t) r);
            if (!headers_done) {
                const char *hend = NULL;
                /* find CRLFCRLF */
                for (size_t i = 0; i + 3 < raw.len; i++) {
                    if (raw.data[i] == '\r' && raw.data[i + 1] == '\n' && raw.data[i + 2] == '\r' &&
                        raw.data[i + 3] == '\n') {
                        hend = (const char *)raw.data + i;
                        body_start = i + 4;
                        break;
                    }
                }
                if (hend) {
                    headers_done = 1;
                    /* status line */
                    const char *p = (const char *)raw.data;
                    if (strncmp(p, "HTTP/1.", 7) == 0) {
                        p += 7;
                        while (*p && *p != ' ') p++;
                        out->status = atoi(p);
                    }
                    /* headers */
                    const char *line = (const char *)raw.data;
                    const char *endh = hend;
                    line = memchr(line, '\n', (size_t)(endh - line));
                    if (line) line++;
                    while (line && line < endh) {
                        const char *eol = memchr(line, '\n', (size_t)(endh - line));
                        size_t ll = eol ? (size_t)(eol - line) : (size_t)(endh - line);
                        while (ll && (line[ll - 1] == '\r')) ll--;
                        const char *colon = memchr(line, ':', ll);
                        if (colon) {
                            size_t nl = (size_t)(colon - line);
                            const char *v = colon + 1;
                            while (v < line + ll && (*v == ' ' || *v == '\t')) v++;
                            add_header(out, line, nl, v, (size_t)(line + ll - v));
                        }
                        if (!eol) break;
                        line = eol + 1;
                    }
                    const char *cl = http_resp_header(out, "Content-Length");
                    if (cl) content_len = atoll(cl);
                }
            }
            if (headers_done && content_len >= 0 && (long long)(raw.len - body_start) >= content_len)
                break;
        } else if (r == 0) {
            break;
        } else {
            if (errno == EINTR) continue;
            if (errno == EAGAIN || errno == EWOULDBLOCK) {
                if (now_ms() > deadline) break;
                sleep_ms(5);
                continue;
            }
            break;
        }
        if (now_ms() > deadline) break;
    }
    close(fd);

    if (!headers_done && raw.len == 0) {
        buf_free(&raw);
        return -1;
    }
    size_t blen = raw.len - (headers_done ? body_start : 0);
    if (content_len >= 0 && (long long)blen > content_len) blen = (size_t)content_len;
    out->body = (char *)malloc(blen + 1);
    if (!out->body) {
        buf_free(&raw);
        return -1;
    }
    if (blen) memcpy(out->body, raw.data + (headers_done ? body_start : 0), blen);
    out->body[blen] = 0;
    out->body_len = blen;
    buf_free(&raw);
    return 0;
}

int http_get(const char *host, int port, const char *path, int timeout_ms, http_resp_t *out) {
    return http_request("GET", host, port, path, NULL, timeout_ms, out);
}

/* ------------------------------------------------------------- server side */

const char *http_req_header(const http_req_t *r, const char *name) {
    if (!r) return NULL;
    for (size_t i = 0; i < r->nh; i++)
        if (strcasecmp(r->names[i], name) == 0) return r->values[i];
    return NULL;
}

void http_req_free(http_req_t *r) {
    if (!r) return;
    for (size_t i = 0; i < r->nh; i++) {
        free(r->names[i]);
        free(r->values[i]);
    }
    free(r->names);
    free(r->values);
    free(r->body);
    memset(r, 0, sizeof(*r));
}

int http_req_parse(const char *data, size_t len, http_req_t *out) {
    memset(out, 0, sizeof(*out));
    const char *hend = NULL;
    for (size_t i = 0; i + 3 < len; i++) {
        if (data[i] == '\r' && data[i + 1] == '\n' && data[i + 2] == '\r' && data[i + 3] == '\n') {
            hend = data + i;
            break;
        }
    }
    if (!hend) return 0;
    size_t body_start = (size_t)(hend - data) + 4;
    /* request line */
    const char *sp1 = memchr(data, ' ', (size_t)(hend - data));
    if (!sp1) return -1;
    size_t mlen = (size_t)(sp1 - data);
    if (mlen >= sizeof(out->method)) mlen = sizeof(out->method) - 1;
    memcpy(out->method, data, mlen);
    out->method[mlen] = 0;
    const char *sp2 = memchr(sp1 + 1, ' ', (size_t)(hend - (sp1 + 1)));
    const char *tend = sp2 ? sp2 : hend;
    size_t tlen = (size_t)(tend - (sp1 + 1));
    if (tlen >= sizeof(out->target)) tlen = sizeof(out->target) - 1;
    memcpy(out->target, sp1 + 1, tlen);
    out->target[tlen] = 0;

    /* headers */
    const char *line = memchr(data, '\n', (size_t)(hend - data));
    if (line) line++;
    while (line && line < hend) {
        const char *eol = memchr(line, '\n', (size_t)(hend - line));
        size_t ll = eol ? (size_t)(eol - line) : (size_t)(hend - line);
        while (ll && line[ll - 1] == '\r') ll--;
        const char *colon = memchr(line, ':', ll);
        if (colon) {
            size_t nl = (size_t)(colon - line);
            const char *v = colon + 1;
            while (v < line + ll && (*v == ' ' || *v == '\t')) v++;
            out->names = (char **)realloc(out->names, (out->nh + 1) * sizeof(char *));
            out->values = (char **)realloc(out->values, (out->nh + 1) * sizeof(char *));
            out->names[out->nh] = astr_dupn(line, nl);
            out->values[out->nh] = astr_dupn(v, (size_t)(line + ll - v));
            out->nh++;
        }
        if (!eol) break;
        line = eol + 1;
    }
    const char *cl = http_req_header(out, "Content-Length");
    size_t need = cl ? (size_t)atoll(cl) : 0;
    size_t have = len - body_start;
    if (have < need) return 0;
    if (need) {
        out->body = (char *)malloc(need + 1);
        memcpy(out->body, data + body_start, need);
        out->body[need] = 0;
    }
    out->body_len = need;
    out->consumed = body_start + need;
    return 1;
}

void http_write_response(buf_t *out, int status, const char *ctype, const char *body,
                         size_t body_len) {
    const char *reason = status == 200 ? "OK" : status == 404 ? "Not Found" : "OK";
    buf_appendf(out, "HTTP/1.1 %d %s\r\n", status, reason);
    buf_appendf(out, "Content-Type: %s\r\n", ctype ? ctype : "text/plain");
    buf_appendf(out, "Content-Length: %zu\r\n", body_len);
    buf_appendstr(out, "Cache-Control: no-store\r\n");
    buf_appendstr(out, "Connection: close\r\n\r\n");
    if (body_len) buf_append(out, body, body_len);
}
