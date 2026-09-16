/* astra/cdp.c - CDP proxy: HTTP discovery endpoints + WebSocket session routing */
#include "cdp.h"

#include "http.h"
#include "stats.h"
#include "ws.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <netdb.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <signal.h>
#include <sys/socket.h>
#include <unistd.h>

#ifndef MSG_NOSIGNAL
#define MSG_NOSIGNAL 0 /* macOS / BSD: SIGPIPE is ignored instead (install_signals) */
#endif

#define MAX_CHUNK (256 * 1024)

typedef enum { CONN_CLIENT = 0, CONN_ENGINE = 1 } conn_kind_t;
typedef enum { CS_HTTP = 0, CS_WS = 1 } conn_state_t;

typedef struct conn {
    int fd;
    conn_kind_t kind;
    conn_state_t state;
    buf_t in, out;
    ws_parser_t ws;
    int want_close;
    int discover;
    int auto_attach;
    struct conn *next;
} conn_t;

typedef struct session {
    char our_id[64];
    char engine_id[160];
    char target_id[160];
    conn_t *client;
    struct session *next;
} session_t;

typedef struct pending {
    int id;
    int orig_id;
    conn_t *client;
    char session[160];
    char method[64];
    struct pending *next;
} pending_t;

typedef struct attach_req {
    char target_id[160];
    conn_t *client;
    struct attach_req *next;
} attach_req_t;

static struct {
    int listen_fd;
    int serving;
    conn_t *clients;
    conn_t *engine;
    session_t *sessions;
    pending_t *pendings;
    attach_req_t *attach_reqs;
    int next_id;
    int next_session;
    optimizer_t *opt;
    engine_t *eng;
    astra_config cfg_store;
    astra_config *cfg;
    int stop;
    int engine_dead;
    char browser_id[64];
    uint64_t last_activity;
    char page_session[160];

    struct {
        int active;
        int id;
        cdp_result_t *out;
        int got;
    } wait_resp;
    struct {
        int active;
        const char *method;
        json_t **params_out;
        int got;
    } wait_event;
} S;

/* ---------------------------------------------------------------- plumbing */

static void set_nonblocking(int fd) {
    int fl = fcntl(fd, F_GETFL, 0);
    if (fl >= 0) fcntl(fd, F_SETFL, fl | O_NONBLOCK);
}

static void on_ws_msg(void *ud, int opcode, const uint8_t *data, size_t len);

static conn_t *conn_new(int fd, conn_kind_t kind) {
    conn_t *c = (conn_t *)calloc(1, sizeof(conn_t));
    c->fd = fd;
    c->kind = kind;
    c->state = CS_HTTP;
    buf_init(&c->in);
    buf_init(&c->out);
    ws_parser_init(&c->ws, on_ws_msg, c);
    set_nonblocking(fd);
    if (kind == CONN_CLIENT) {
        c->next = S.clients;
        S.clients = c;
    }
    return c;
}

static void conn_close(conn_t *c) {
    if (!c) return;
    if (c->kind == CONN_CLIENT) {
        conn_t **pp = &S.clients;
        while (*pp) {
            if (*pp == c) {
                *pp = c->next;
                break;
            }
            pp = &(*pp)->next;
        }
        /* drop sessions owned by this client */
        session_t **sp = &S.sessions;
        while (*sp) {
            if ((*sp)->client == c) {
                session_t *d = *sp;
                *sp = d->next;
                free(d);
            } else {
                sp = &(*sp)->next;
            }
        }
    }
    if (c->fd >= 0) close(c->fd);
    c->fd = -1;
    buf_free(&c->in);
    buf_free(&c->out);
    ws_parser_free(&c->ws);
    free(c);
}

static void send_json_to(conn_t *c, json_t *msg) {
    if (!c || c->fd < 0 || !msg) return;
    char *s = json_stringify(msg);
    if (!s) return;
    buf_t frame;
    buf_init(&frame);
    ws_frame(&frame, WS_TEXT, s, strlen(s), c->kind == CONN_ENGINE ? 1 : 0);
    buf_append(&c->out, frame.data, frame.len);
    buf_free(&frame);
    free(s);
}

static void send_error(conn_t *c, int id, const char *session, const char *text) {
    json_t *m = jobj();
    jset(m, "id", jnum((double)id));
    json_t *e = jobj();
    jset(e, "code", jnum(-32000));
    jset(e, "message", jstr(text));
    jset(m, "error", e);
    if (session) jset(m, "sessionId", jstr(session));
    send_json_to(c, m);
    json_free(m);
}

static void send_ok(conn_t *c, int id, const char *session, json_t *result) {
    json_t *m = jobj();
    jset(m, "id", jnum((double)id));
    jset(m, "result", result ? json_clone(result) : jobj());
    if (session) jset(m, "sessionId", jstr(session));
    send_json_to(c, m);
    json_free(m);
}

/* ---------------------------------------------------------------- sessions */

static session_t *session_by_our(const char *id) {
    for (session_t *s = S.sessions; s; s = s->next)
        if (!strcmp(s->our_id, id)) return s;
    return NULL;
}

static session_t *session_by_engine(const char *id) {
    if (!id) return NULL;
    for (session_t *s = S.sessions; s; s = s->next)
        if (!strcmp(s->engine_id, id)) return s;
    return NULL;
}

static session_t *session_new(conn_t *client, const char *engine_id, const char *target_id) {
    session_t *s = session_by_engine(engine_id);
    if (s) {
        if (client) s->client = client;
        return s;
    }
    s = (session_t *)calloc(1, sizeof(session_t));
    snprintf(s->our_id, sizeof(s->our_id), "astra-session-%d", ++S.next_session);
    snprintf(s->engine_id, sizeof(s->engine_id), "%s", engine_id ? engine_id : "");
    snprintf(s->target_id, sizeof(s->target_id), "%s", target_id ? target_id : "");
    s->client = client;
    s->next = S.sessions;
    S.sessions = s;
    LOGD("session %s -> engine %s (target %s)", s->our_id, s->engine_id, s->target_id);
    return s;
}

/* enable what astra itself needs on a freshly attached session */
static void session_init(const char *engine_session) {
    if (!engine_session || !S.engine) return;
    if (S.cfg->lite) {
        json_t *p = jobj();
        json_t *patterns = jarr();
        json_t *pat = jobj();
        jset(pat, "urlPattern", jstr("*"));
        jset(pat, "requestStage", jstr("Request"));
        jpush(patterns, pat);
        jset(p, "patterns", patterns);
        int id = ++S.next_id;
        json_t *m = jobj();
        jset(m, "id", jnum((double)id));
        jset(m, "method", jstr("Fetch.enable"));
        jset(m, "params", p);
        jset(m, "sessionId", jstr(engine_session));
        send_json_to(S.engine, m);
        json_free(m);
    }
    /* keep network events flowing (harmless if the client also enables it) */
    int id = ++S.next_id;
    json_t *m = jobj();
    jset(m, "id", jnum((double)id));
    jset(m, "method", jstr("Network.enable"));
    jset(m, "sessionId", jstr(engine_session));
    send_json_to(S.engine, m);
    json_free(m);
}

/* ----------------------------------------------------------------- pending */

static void pending_add(int id, int orig_id, conn_t *client, const char *session,
                        const char *method) {
    pending_t *p = (pending_t *)calloc(1, sizeof(pending_t));
    p->id = id;
    p->orig_id = orig_id;
    p->client = client;
    snprintf(p->session, sizeof(p->session), "%s", session ? session : "");
    snprintf(p->method, sizeof(p->method), "%s", method ? method : "");
    p->next = S.pendings;
    S.pendings = p;
}

static pending_t *pending_find(int id) {
    for (pending_t *p = S.pendings; p; p = p->next)
        if (p->id == id) return p;
    return NULL;
}

static void pending_del(int id) {
    pending_t **pp = &S.pendings;
    while (*pp) {
        if ((*pp)->id == id) {
            pending_t *d = *pp;
            *pp = d->next;
            free(d);
            return;
        }
        pp = &(*pp)->next;
    }
}

static void attach_req_add(const char *target_id, conn_t *client) {
    attach_req_t *a = (attach_req_t *)calloc(1, sizeof(attach_req_t));
    snprintf(a->target_id, sizeof(a->target_id), "%s", target_id);
    a->client = client;
    a->next = S.attach_reqs;
    S.attach_reqs = a;
}

static conn_t *attach_req_take(const char *target_id) {
    attach_req_t **pp = &S.attach_reqs;
    while (*pp) {
        if (!strcmp((*pp)->target_id, target_id)) {
            attach_req_t *d = *pp;
            conn_t *c = d->client;
            *pp = d->next;
            free(d);
            return c;
        }
        pp = &(*pp)->next;
    }
    return NULL;
}

/* ------------------------------------------------------- optimizer callback */

static void optimizer_send(void *ud, const char *session_id, json_t *msg) {
    (void)ud;
    send_json_to(S.engine, msg);
    if (session_id && S.engine) { /* flushed by the main loop */ }
}

/* ------------------------------------------------------------- HTTP surface */

static void patch_engine_port(char *s, size_t len, int from, int to) {
    if (from == to || from <= 0) return;
    char frompat[32], topat[32];
    snprintf(frompat, sizeof(frompat), ":%d", from);
    snprintf(topat, sizeof(topat), ":%d", to);
    size_t fl = strlen(frompat);
    size_t pos = 0;
    while (pos + fl + 1 <= len) {
        char *hit = (char *)memmem(s + pos, len - pos, frompat, fl);
        if (!hit) break;
        size_t off = (size_t)(hit - s);
        char nextc = s[off + fl];
        if (nextc == '/' || nextc == '"' || nextc == '\'' || nextc == '\\') {
            if (strlen(topat) == fl) {
                memcpy(hit, topat, fl);
            } else {
                size_t tl = strlen(topat);
                if (tl > fl) {
                    memmove(s + off + tl, s + off + fl, len - off - fl + 1);
                    len += tl - fl;
                } else {
                    memmove(s + off + tl, s + off + fl, len - off - fl + 1);
                    len -= fl - tl;
                }
                memcpy(s + off, topat, tl);
            }
            pos = off + strlen(topat);
        } else {
            pos = off + 1;
        }
    }
}

static void http_json(conn_t *c, int status, const char *body) {
    buf_t out;
    buf_init(&out);
    http_write_response(&out, status, "application/json; charset=UTF-8", body, strlen(body));
    buf_append(&c->out, out.data, out.len);
    buf_free(&out);
    c->want_close = 1;
}

static void serve_http(conn_t *c, http_req_t *r) {
    const char *target = r->target;
    LOGD("http %s %s", r->method, target);

    if (!strncmp(target, "/json/version", 13)) {
        http_resp_t er;
        json_t *v = NULL;
        if (S.eng && S.eng->port > 0 &&
            http_get("127.0.0.1", S.eng->port, "/json/version", 3000, &er) == 0) {
            v = json_parse(er.body, er.body_len);
            http_resp_free(&er);
        }
        if (!v) v = jobj();
        char browser[256];
        snprintf(browser, sizeof(browser), "Astra/%s", ASTRA_VERSION);
        if (S.eng && S.eng->browser[0]) {
            char tmp[512];
            snprintf(tmp, sizeof(tmp), "%s (%s)", browser, S.eng->browser);
            snprintf(browser, sizeof(browser), "%s", tmp);
        }
        jset(v, "Browser", jstr(browser));
        jset(v, "Protocol-Version", jstr("1.3"));
        jset(v, "User-Agent", jstr(browser));
        jset(v, "Astra-Version", jstr(ASTRA_VERSION));
        char ws[512];
        snprintf(ws, sizeof(ws), "ws://%s:%d/devtools/browser/%s",
                 S.cfg->bind_addr[0] ? S.cfg->bind_addr : "127.0.0.1", S.cfg->port, S.browser_id);
        jset(v, "webSocketDebuggerUrl", jstr(ws));
        char *s = json_stringify(v);
        json_free(v);
        if (s) {
            http_json(c, 200, s);
            free(s);
        }
        return;
    }
    if (!strncmp(target, "/json/list", 10) || !strncmp(target, "/json", 5)) {
        http_resp_t er;
        if (S.eng && S.eng->port > 0 &&
            http_get("127.0.0.1", S.eng->port, "/json/list", 3000, &er) == 0) {
            patch_engine_port(er.body, er.body_len, S.eng->port, S.cfg->port);
            http_json(c, 200, er.body);
            http_resp_free(&er);
            return;
        }
        http_resp_free(&er);
        http_json(c, 200, "[]");
        return;
    }
    if (!strncmp(target, "/json/new", 9)) {
        http_resp_t er;
        if (S.eng && S.eng->port > 0 &&
            http_request("PUT", "127.0.0.1", S.eng->port, target, NULL, 5000, &er) == 0) {
            patch_engine_port(er.body, er.body_len, S.eng->port, S.cfg->port);
            http_json(c, 200, er.body);
            http_resp_free(&er);
            return;
        }
        http_resp_free(&er);
        http_json(c, 500, "{\"error\":\"cannot create target\"}");
        return;
    }
    if (!strncmp(target, "/json/protocol", 14)) {
        http_resp_t er;
        if (S.eng && S.eng->port > 0 &&
            http_get("127.0.0.1", S.eng->port, "/json/protocol", 8000, &er) == 0 && er.body) {
            buf_t out;
            buf_init(&out);
            http_write_response(&out, 200, "application/json; charset=UTF-8", er.body,
                                er.body_len);
            buf_append(&c->out, out.data, out.len);
            buf_free(&out);
            c->want_close = 1;
            http_resp_free(&er);
            return;
        }
        http_resp_free(&er);
        http_json(c, 404, "{\"error\":\"protocol not available\"}");
        return;
    }
    if (!strncmp(target, "/stats", 6) || !strncmp(target, "/json/stats", 11)) {
        char *s = stats_json_text();
        if (s) {
            http_json(c, 200, s);
            free(s);
        }
        return;
    }
    if (!strncmp(target, "/healthz", 8)) {
        http_json(c, 200, "{\"ok\":true}");
        return;
    }
    const char *html =
        "<html><head><title>Astra</title></head><body><h1>Astra browser control plane</h1>"
        "<ul><li><a href='/json/version'>/json/version</a></li>"
        "<li><a href='/json/list'>/json/list</a></li>"
        "<li><a href='/json/protocol'>/json/protocol</a></li>"
        "<li><a href='/stats'>/stats</a></li></ul></body></html>";
    buf_t out;
    buf_init(&out);
    http_write_response(&out, 200, "text/html; charset=UTF-8", html, strlen(html));
    buf_append(&c->out, out.data, out.len);
    buf_free(&out);
    c->want_close = 1;
}

/* ------------------------------------------------------- client -> engine */

static void forward_to_engine(conn_t *client, json_t *m) {
    if (!S.engine || S.engine->fd < 0) {
        LOGE("engine not connected, dropping %s", json_get_str(m, "method", "?"));
        return;
    }
    int newid = ++S.next_id;
    int orig_id = (int)json_get_num(m, "id", 0);
    json_t *out = jobj();
    jset(out, "id", jnum((double)newid));
    jset(out, "method", jstr(json_get_str(m, "method", "")));
    json_t *params = json_get(m, "params");
    if (params) jset(out, "params", json_clone(params));
    const char *sid = json_get_str(m, "sessionId", NULL);
    if (sid) {
        session_t *s = session_by_our(sid);
        if (s) jset(out, "sessionId", jstr(s->engine_id));
        else {
            LOGD("forwarding with unknown session %s", sid);
            jset(out, "sessionId", jstr(sid));
        }
    }
    pending_add(newid, orig_id, client, sid, json_get_str(m, "method", NULL));
    send_json_to(S.engine, out);
    json_free(out);
}

static void handle_astra(conn_t *c, json_t *m) {
    const char *method = json_get_str(m, "method", "");
    int id = (int)json_get_num(m, "id", 0);
    if (!strcmp(method, "Astra.getStats")) {
        json_t *r = jobj();
        stats_json(r);
        send_ok(c, id, NULL, r);
        json_free(r);
        return;
    }
    if (!strcmp(method, "Astra.resetStats")) {
        stats_reset();
        send_ok(c, id, NULL, jobj());
        return;
    }
    if (!strcmp(method, "Astra.getConfig")) {
        json_t *r = jobj();
        jset(r, "version", jstr(ASTRA_VERSION));
        jset(r, "lite", jbool(S.cfg->lite));
        jset(r, "blockAds", jbool(S.cfg->block_ads));
        jset(r, "optimizeImages", jbool(S.cfg->optimize_images));
        jset(r, "maxImageWidth", jnum((double)S.cfg->max_image_width));
        jset(r, "imageQuality", jnum((double)S.cfg->image_quality));
        jset(r, "cache", jbool(S.cfg->cache_enabled));
        jset(r, "rules", jnum((double)optimizer_rule_count(S.opt)));
        send_ok(c, id, NULL, r);
        json_free(r);
        return;
    }
    if (!strcmp(method, "Astra.getVersion")) {
        json_t *r = jobj();
        jset(r, "version", jstr(ASTRA_VERSION));
        jset(r, "engine", jstr(S.eng && S.eng->bin[0] ? S.eng->bin : "attached"));
        jset(r, "browser", jstr(S.eng && S.eng->browser[0] ? S.eng->browser : "unknown"));
        send_ok(c, id, NULL, r);
        json_free(r);
        return;
    }
    send_error(c, id, NULL, "Unknown Astra domain method");
}

static void handle_target(conn_t *c, json_t *m) {
    const char *method = json_get_str(m, "method", "");
    json_t *params = json_get(m, "params");

    if (!strcmp(method, "Target.setDiscoverTargets")) {
        c->discover = json_get_bool(params, "discover", 0);
        forward_to_engine(c, m);
        return;
    }
    if (!strcmp(method, "Target.setAutoAttach")) {
        c->auto_attach = json_get_bool(params, "autoAttach", 0);
        forward_to_engine(c, m);
        return;
    }
    if (!strcmp(method, "Target.attachToTarget")) {
        const char *tid = json_get_str(params, "targetId", NULL);
        if (tid) attach_req_add(tid, c);
        /* always flatten so that all sessions share one websocket */
        json_t *copy = json_clone(m);
        if (json_get(copy, "params")) jset(json_get(copy, "params"), "flatten", jbool(1));
        forward_to_engine(c, copy);
        json_free(copy);
        return;
    }
    if (!strcmp(method, "Target.getBrowserContexts")) {
        /* Chromium supports it; unknown engines may not - answer locally as a fallback */
        forward_to_engine(c, m);
        return;
    }
    forward_to_engine(c, m);
}

static void client_on_msg(conn_t *c, const uint8_t *data, size_t len) {
    json_t *m = json_parse((const char *)data, len);
    if (!m) {
        LOGW("client sent invalid JSON (%zu bytes)", len);
        return;
    }
    S.last_activity = now_ms();
    const char *method = json_get_str(m, "method", NULL);
    if (!method) {
        json_free(m);
        return; /* responses from clients are not expected */
    }
    const char *sid = json_get_str(m, "sessionId", NULL);
    int id = (int)json_get_num(m, "id", 0);
    LOGT("client method=%s id=%d sid=%s", method, id, sid ? sid : "-");

    if (!sid) {
        if (!strncmp(method, "Target.", 7))
            handle_target(c, m);
        else if (!strncmp(method, "Astra.", 6))
            handle_astra(c, m);
        else
            forward_to_engine(c, m);
        json_free(m);
        return;
    }

    session_t *s = session_by_our(sid);
    if (!s) {
        send_error(c, id, sid, "No such session");
        json_free(m);
        return;
    }
    if (S.cfg->lite && !strncmp(method, "Fetch.", 6)) {
        /* Astra owns Fetch interception while lite mode is on */
        if (!strcmp(method, "Fetch.enable") || !strcmp(method, "Fetch.disable")) {
            send_ok(c, id, sid, jobj());
            json_free(m);
            return;
        }
    }
    forward_to_engine(c, m);
    json_free(m);
}

/* ------------------------------------------------------- engine -> clients */

static void fanout_event(json_t *m) {
    for (conn_t *c = S.clients; c; c = c->next) {
        if (c->state != CS_WS) continue;
        send_json_to(c, m);
    }
}

static void engine_on_msg(conn_t *c, const uint8_t *data, size_t len) {
    (void)c;
    json_t *m = json_parse((const char *)data, len);
    if (!m) {
        LOGW("engine sent invalid JSON");
        return;
    }
    S.last_activity = now_ms();
    LOGT("engine msg id=%d method=%s", (int)json_get_num(m, "id", 0), json_get_str(m, "method", "-"));
    json_t *error = json_get(m, "error");
    json_t *result = json_get(m, "result");

    if (json_get(m, "id")) {
        int id = (int)json_get_num(m, "id", 0);
        if (id >= ASTRA_OPTIMIZER_ID_BASE) {
            const char *sid = json_get_str(m, "sessionId", NULL);
            optimizer_handle_response(S.opt, sid, id, result, error);
            json_free(m);
            return;
        }
        if (S.wait_resp.active && S.wait_resp.id == id) {
            cdp_result_t *out = S.wait_resp.out;
            if (out) {
                out->result = result ? json_clone(result) : NULL;
                if (error) {
                    const char *msg = json_get_str(error, "message", "error");
                    snprintf(out->error, sizeof(out->error), "%s", msg);
                } else {
                    out->error[0] = 0;
                }
            }
            S.wait_resp.got = 1;
            json_free(m);
            return;
        }
        pending_t *p = pending_find(id);
        if (!p || !p->client) {
            json_free(m);
            return;
        }
        jset(m, "id", jnum((double)p->orig_id));
        const char *sid = json_get_str(m, "sessionId", NULL);
        if (sid) {
            session_t *s = session_by_engine(sid);
            if (s) jset(m, "sessionId", jstr(s->our_id));
        }
        /* Target.attachToTarget also returns the session id inside the result */
        if (!strcmp(p->method, "Target.attachToTarget")) {
            json_t *res = json_get(m, "result");
            const char *esid = res ? json_get_str(res, "sessionId", NULL) : NULL;
            if (esid) {
                session_t *s = session_by_engine(esid);
                if (s) jset(res, "sessionId", jstr(s->our_id));
            }
        }
        send_json_to(p->client, m);
        pending_del(id);
        json_free(m);
        return;
    }

    const char *method = json_get_str(m, "method", NULL);
    const char *sid = json_get_str(m, "sessionId", NULL);
    json_t *params = json_get(m, "params");
    if (!method) {
        json_free(m);
        return;
    }

    if (S.wait_event.active && !strcmp(method, S.wait_event.method)) {
        if (S.wait_event.params_out) *S.wait_event.params_out = params ? json_clone(params) : NULL;
        S.wait_event.got = 1;
    }

    /* page host tracking for third-party detection */
    if (!strcmp(method, "Page.frameNavigated") && params) {
        json_t *frame = json_get(params, "frame");
        const char *url = frame ? json_get_str(frame, "url", NULL) : NULL;
        if (url && *url) optimizer_set_page_host(S.opt, sid, url);
    }

    if (S.cfg->lite && !strcmp(method, "Fetch.requestPaused")) {
        optimizer_handle_paused(S.opt, sid, params);
        json_free(m);
        return;
    }
    if (!strcmp(method, "Target.attachedToTarget") && params) {
        const char *engine_sid = json_get_str(params, "sessionId", NULL);
        json_t *ti = json_get(params, "targetInfo");
        const char *tid = ti ? json_get_str(ti, "targetId", NULL) : NULL;
        conn_t *owner = tid ? attach_req_take(tid) : NULL;
        if (!owner) {
            for (conn_t *cc = S.clients; cc; cc = cc->next)
                if (cc->auto_attach || cc->discover) {
                    owner = cc;
                    break;
                }
        }
        session_t *s = session_new(owner, engine_sid, tid);
        session_init(s->engine_id);
        json_t *copy = json_clone(m);
        if (json_get(copy, "params")) jset(json_get(copy, "params"), "sessionId", jstr(s->our_id));
        if (owner) {
            send_json_to(owner, copy);
        } else {
            fanout_event(copy);
        }
        json_free(copy);
        json_free(m);
        return;
    }
    if (!strcmp(method, "Target.detachedFromTarget") && params) {
        const char *engine_sid = json_get_str(params, "sessionId", NULL);
        json_t *copy = json_clone(m);
        session_t *s = engine_sid ? session_by_engine(engine_sid) : NULL;
        if (s && json_get(copy, "params")) jset(json_get(copy, "params"), "sessionId", jstr(s->our_id));
        fanout_event(copy);
        json_free(copy);
        json_free(m);
        return;
    }
    if (!strncmp(method, "Target.", 7)) {
        /* targetCreated / targetDestroyed / targetInfoChanged / targetCrashed */
        if (!strcmp(method, "Target.targetDestroyed") && params) {
            const char *tid = json_get_str(params, "targetId", NULL);
            session_t **sp = &S.sessions;
            while (*sp) {
                if (tid && !strcmp((*sp)->target_id, tid)) {
                    session_t *d = *sp;
                    *sp = d->next;
                    free(d);
                } else {
                    sp = &(*sp)->next;
                }
            }
        }
        for (conn_t *cc = S.clients; cc; cc = cc->next)
            if (cc->state == CS_WS && (cc->discover || cc->auto_attach)) send_json_to(cc, m);
        json_free(m);
        return;
    }
    if (sid) {
        session_t *s = session_by_engine(sid);
        if (s && s->client) {
            json_t *copy = json_clone(m);
            jset(copy, "sessionId", jstr(s->our_id));
            send_json_to(s->client, copy);
            json_free(copy);
            json_free(m);
            return;
        }
    }
    fanout_event(m);
    json_free(m);
}

static void on_ws_msg(void *ud, int opcode, const uint8_t *data, size_t len) {
    conn_t *c = (conn_t *)ud;
    if (opcode == WS_CLOSE) {
        c->want_close = 1;
        return;
    }
    if (opcode == WS_PING) {
        buf_t f;
        buf_init(&f);
        ws_frame(&f, WS_PONG, data, len, c->kind == CONN_ENGINE ? 1 : 0);
        buf_append(&c->out, f.data, f.len);
        buf_free(&f);
        return;
    }
    if (opcode != WS_TEXT) return;
    if (c->kind == CONN_ENGINE)
        engine_on_msg(c, data, len);
    else
        client_on_msg(c, data, len);
}

/* --------------------------------------------------------------- poll loop */

static void handle_conn_readable(conn_t *c) {
    uint8_t chunk[MAX_CHUNK];
    ssize_t r = recv(c->fd, chunk, sizeof(chunk), 0);
    if (r == 0) {
        c->want_close = 1;
        if (c->kind == CONN_ENGINE) S.engine_dead = 1;
        return;
    }
    if (r < 0) {
        if (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR) return;
        c->want_close = 1;
        if (c->kind == CONN_ENGINE) S.engine_dead = 1;
        return;
    }
    if (c->state == CS_HTTP) {
        buf_append(&c->in, chunk, (size_t)r);
        http_req_t req;
        int pr = http_req_parse((const char *)c->in.data, c->in.len, &req);
        if (pr == 0) {
            http_req_free(&req);
            return; /* need more data */
        }
        if (pr < 0) {
            http_req_free(&req);
            c->want_close = 1;
            return;
        }
        const char *up = http_req_header(&req, "Upgrade");
        if (up && !strncasecmp(up, "websocket", 9)) {
            buf_t resp;
            buf_init(&resp);
            if (ws_server_handshake((const char *)c->in.data, req.consumed, &resp) != 0) {
                buf_free(&resp);
                http_req_free(&req);
                c->want_close = 1;
                return;
            }
            buf_append(&c->out, resp.data, resp.len);
            buf_free(&resp);
            c->state = CS_WS;
            buf_consume(&c->in, req.consumed);
            http_req_free(&req);
            if (c->in.len) {
                ws_parser_feed(&c->ws, c->in.data, c->in.len);
                buf_clear(&c->in);
            }
            LOGD("websocket client connected (fd %d)", c->fd);
            return;
        }
        serve_http(c, &req);
        buf_consume(&c->in, req.consumed);
        http_req_free(&req);
        return;
    }
    if (ws_parser_feed(&c->ws, chunk, (size_t)r) != 0) c->want_close = 1;
}

static void handle_conn_writable(conn_t *c) {
    while (c->out.len) {
        ssize_t w = send(c->fd, c->out.data, c->out.len, MSG_NOSIGNAL);
        if (w > 0) {
            buf_consume(&c->out, (size_t)w);
            continue;
        }
        if (w < 0 && (errno == EAGAIN || errno == EWOULDBLOCK || errno == EINTR)) return;
        c->want_close = 1;
        return;
    }
}

static int do_poll(int timeout_ms) {
    struct pollfd fds[256];
    conn_t *map[256];
    int n = 0;
    if (S.listen_fd >= 0 && S.serving) {
        fds[n].fd = S.listen_fd;
        fds[n].events = POLLIN;
        map[n++] = NULL;
    }
    if (S.engine && S.engine->fd >= 0) {
        fds[n].fd = S.engine->fd;
        fds[n].events = POLLIN | (S.engine->out.len ? POLLOUT : 0);
        map[n++] = S.engine;
    }
    for (conn_t *c = S.clients; c && n < 256 - 1; c = c->next) {
        if (c->fd < 0) continue;
        fds[n].fd = c->fd;
        fds[n].events = POLLIN | (c->out.len ? POLLOUT : 0);
        map[n++] = c;
    }
    if (n == 0) {
        sleep_ms(timeout_ms > 0 ? timeout_ms : 10);
        return 0;
    }
    int rc = poll(fds, (nfds_t)n, timeout_ms);
    if (rc < 0) {
        if (errno == EINTR) return 0;
        return -1;
    }
    for (int i = 0; i < n; i++) {
        if (!map[i]) {
            if (fds[i].revents & POLLIN) {
                int afd = accept(S.listen_fd, NULL, NULL);
                if (afd >= 0) {
                    int one = 1;
                    setsockopt(afd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
                    conn_new(afd, CONN_CLIENT);
                }
            }
            continue;
        }
        conn_t *c = map[i];
        if (fds[i].revents & (POLLERR | POLLHUP | POLLNVAL)) {
            if (c->kind == CONN_ENGINE) S.engine_dead = 1;
            c->want_close = 1;
            continue;
        }
        if (fds[i].revents & POLLIN) handle_conn_readable(c);
        if (c->fd >= 0 && (fds[i].revents & POLLOUT)) handle_conn_writable(c);
    }
    /* flush queued writes before reaping, otherwise short lived HTTP
     * responses would be dropped together with the connection */
    if (S.engine && S.engine->fd >= 0 && S.engine->out.len) handle_conn_writable(S.engine);
    conn_t *c = S.clients;
    while (c) {
        if (c->fd >= 0 && c->out.len) handle_conn_writable(c);
        c = c->next;
    }
    c = S.clients;
    while (c) {
        conn_t *nx = c->next;
        if (c->want_close && !c->out.len) conn_close(c);
        c = nx;
    }
    if (S.engine && S.engine->want_close) {
        S.engine_dead = 1;
    }
    return 0;
}

int cdp_run_until(int (*done)(void *ud), void *ud, int timeout_ms) {
    uint64_t deadline = now_ms() + (uint64_t)timeout_ms;
    while (!S.stop && !S.engine_dead) {
        if (done && done(ud)) return 1;
        int t = (int)(deadline - now_ms());
        if (t < 0) t = 0;
        if (t > 100) t = 100;
        do_poll(t);
        if (now_ms() >= deadline) {
            if (done && done(ud)) return 1;
            return 0;
        }
    }
    return 0;
}

static int wait_resp_done(void *ud) {
    (void)ud;
    return S.wait_resp.got;
}

static int wait_event_done(void *ud) {
    (void)ud;
    return S.wait_event.got;
}

int cdp_cmd(const char *session, const char *method, json_t *params, int timeout_ms,
            cdp_result_t *out) {
    if (!S.engine || S.engine->fd < 0) return -1;
    if (out) {
        out->result = NULL;
        out->error[0] = 0;
    }
    int id = ++S.next_id;
    json_t *m = jobj();
    jset(m, "id", jnum((double)id));
    jset(m, "method", jstr(method));
    if (params) jset(m, "params", json_clone(params));
    if (session) jset(m, "sessionId", jstr(session));
    S.wait_resp.active = 1;
    S.wait_resp.id = id;
    S.wait_resp.out = out;
    S.wait_resp.got = 0;
    send_json_to(S.engine, m);
    json_free(m);
    int rc = cdp_run_until(wait_resp_done, NULL, timeout_ms);
    S.wait_resp.active = 0;
    S.wait_resp.out = NULL;
    if (!rc) return -2;               /* timeout */
    if (out && out->error[0]) return -3; /* CDP error */
    return 0;
}

int cdp_cmd_simple(const char *session, const char *method, json_t *params, int timeout_ms) {
    cdp_result_t r;
    int rc = cdp_cmd(session, method, params, timeout_ms, &r);
    cdp_release_result(&r);
    return rc;
}

int cdp_wait_event(const char *method, int timeout_ms, json_t **params_out) {
    if (!S.engine) return -1;
    S.wait_event.active = 1;
    S.wait_event.method = method;
    S.wait_event.params_out = params_out;
    S.wait_event.got = 0;
    int rc = cdp_run_until(wait_event_done, NULL, timeout_ms);
    S.wait_event.active = 0;
    S.wait_event.params_out = NULL;
    return rc ? 0 : -2;
}

void cdp_arm_event(const char *method) {
    S.wait_event.active = 1;
    S.wait_event.method = method;
    S.wait_event.params_out = NULL;
    S.wait_event.got = 0;
}

int cdp_wait_armed(int timeout_ms) {
    if (!S.engine) return -1;
    int rc = cdp_run_until(wait_event_done, NULL, timeout_ms);
    S.wait_event.active = 0;
    S.wait_event.params_out = NULL;
    return rc ? 0 : -2;
}

void cdp_release_result(cdp_result_t *r) {
    if (!r) return;
    if (r->result) {
        json_free(r->result);
        r->result = NULL;
    }
}

const char *cdp_page_session(void) { return S.page_session[0] ? S.page_session : NULL; }

int cdp_set_lite(int on) {
    S.cfg_store.lite = on ? 1 : 0;
    if (!S.engine) return 0;
    for (session_t *s = S.sessions; s; s = s->next) {
        json_t *m = jobj();
        jset(m, "id", jnum((double)(++S.next_id)));
        jset(m, "method", jstr(on ? "Fetch.enable" : "Fetch.disable"));
        if (on) {
            json_t *p = jobj();
            json_t *pats = jarr();
            json_t *pat = jobj();
            jset(pat, "urlPattern", jstr("*"));
            jset(pat, "requestStage", jstr("Request"));
            jpush(pats, pat);
            jset(p, "patterns", pats);
            jset(m, "params", p);
        }
        jset(m, "sessionId", jstr(s->engine_id));
        send_json_to(S.engine, m);
        json_free(m);
    }
    return 0;
}

int cdp_attach_to_first_page(int timeout_ms) {
    cdp_result_t r;
    if (cdp_cmd(NULL, "Target.getTargets", NULL, timeout_ms, &r) != 0) {
        cdp_release_result(&r);
        return -1;
    }
    json_t *infos = json_get(r.result, "targetInfos");
    const char *target = NULL;
    if (infos) {
        for (size_t i = 0; i < infos->n; i++) {
            json_t *ti = infos->items[i];
            const char *type = json_get_str(ti, "type", "");
            if (!strcmp(type, "page")) {
                target = json_get_str(ti, "targetId", NULL);
                break;
            }
        }
    }
    cdp_release_result(&r);
    if (!target) {
        /* create one */
        json_t *p = jobj();
        jset(p, "url", jstr("about:blank"));
        if (cdp_cmd(NULL, "Target.createTarget", p, timeout_ms, &r) != 0) {
            json_free(p);
            cdp_release_result(&r);
            return -1;
        }
        json_free(p);
        target = NULL;
        static char tid[160];
        snprintf(tid, sizeof(tid), "%s", json_get_str(r.result, "targetId", ""));
        cdp_release_result(&r);
        if (!tid[0]) return -1;
        target = tid;
    }
    json_t *p = jobj();
    jset(p, "targetId", jstr(target));
    jset(p, "flatten", jbool(1));
    if (cdp_cmd(NULL, "Target.attachToTarget", p, timeout_ms, &r) != 0) {
        json_free(p);
        cdp_release_result(&r);
        return -1;
    }
    json_free(p);
    snprintf(S.page_session, sizeof(S.page_session), "%s",
             json_get_str(r.result, "sessionId", ""));
    cdp_release_result(&r);
    if (!S.page_session[0]) return -1;
    session_new(NULL, S.page_session, target);
    session_init(S.page_session);
    LOGD("attached to page session %s", S.page_session);
    return 0;
}

/* -------------------------------------------------------------- lifecycle */

static void on_signal(int sig) {
    (void)sig;
    S.stop = 1;
}

static void install_signals(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_signal;
    sigaction(SIGINT, &sa, NULL);
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGHUP, &sa, NULL);
    signal(SIGPIPE, SIG_IGN);
}

static int listen_on(const char *addr, int port) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return -1;
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof(one));
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof(sa));
    sa.sin_family = AF_INET;
    sa.sin_port = htons((uint16_t)port);
    if (!addr || !strcmp(addr, "0.0.0.0"))
        sa.sin_addr.s_addr = htonl(INADDR_ANY);
    else
        inet_pton(AF_INET, addr, &sa.sin_addr);
    if (bind(fd, (struct sockaddr *)&sa, sizeof(sa)) != 0) {
        LOGE("bind %s:%d failed: %s", addr ? addr : "0.0.0.0", port, strerror(errno));
        close(fd);
        return -1;
    }
    if (listen(fd, 64) != 0) {
        close(fd);
        return -1;
    }
    set_nonblocking(fd);
    return fd;
}

static int engine_connect_ws(const astra_config *cfg, engine_t *eng) {
    /* plain TCP connect + websocket upgrade (DevTools speaks plain ws://) */
    url_t u;
    if (url_parse(eng->ws_url, &u) != 0) return -1;
    int fd = -1;
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof(sa));
    sa.sin_family = AF_INET;
    sa.sin_port = htons((uint16_t)u.port);
    struct hostent *he = gethostbyname(u.host);
    if (he && he->h_addr_list[0])
        memcpy(&sa.sin_addr, he->h_addr_list[0], (size_t)he->h_length);
    else
        inet_pton(AF_INET, u.host, &sa.sin_addr);
    uint64_t deadline = now_ms() + 10000;
    for (;;) {
        fd = socket(AF_INET, SOCK_STREAM, 0);
        if (fd >= 0) {
            int one = 1;
            setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));
            if (connect(fd, (struct sockaddr *)&sa, sizeof(sa)) == 0) break;
            close(fd);
            fd = -1;
        }
        if (now_ms() > deadline) return -1;
        sleep_ms(50);
    }
    char *req = ws_client_request(u.host, u.port, u.path[0] ? u.path : "/");
    size_t off = 0;
    size_t rlen = strlen(req);
    while (off < rlen) {
        ssize_t w = send(fd, req + off, rlen - off, 0);
        if (w <= 0) {
            if (errno == EINTR) continue;
            free(req);
            close(fd);
            return -1;
        }
        off += (size_t)w;
    }
    free(req);
    /* read the handshake response */
    buf_t in;
    buf_init(&in);
    uint8_t chunk[4096];
    deadline = now_ms() + 10000;
    while (now_ms() < deadline) {
        ssize_t r = recv(fd, chunk, sizeof(chunk), 0);
        if (r > 0) {
            buf_append(&in, chunk, (size_t)r);
            char *hdr_end = strstr((char *)in.data, "\r\n\r\n");
            if (hdr_end) {
                size_t used = (size_t)(hdr_end - (char *)in.data) + 4;
                if (strncmp((char *)in.data, "HTTP/1.1 101", 12) != 0) {
                    LOGW("engine ws handshake failed: %.60s", (char *)in.data);
                    buf_free(&in);
                    close(fd);
                    return -1;
                }
                conn_t *c = conn_new(fd, CONN_ENGINE);
                c->state = CS_WS; /* the handshake is already done */
                S.engine = c;
                LOGD("engine websocket connected (fd %d)", fd);
                if (in.len > used) ws_parser_feed(&c->ws, in.data + used, in.len - used);
                buf_free(&in);
                (void)cfg;
                return 0;
            }
            continue;
        }
        if (r < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
            sleep_ms(20);
            continue;
        }
        break;
    }
    buf_free(&in);
    close(fd);
    return -1;
}

int cdp_attach(const astra_config *cfg, engine_t *eng) {
    memset(&S, 0, sizeof(S));
    S.cfg_store = *cfg;
    S.cfg = &S.cfg_store;
    S.eng = eng;
    S.next_id = 1;
    S.last_activity = now_ms();
    snprintf(S.browser_id, sizeof(S.browser_id), "astra-%d", (int)getpid());
    install_signals();
    stats_reset();
    S.opt = optimizer_new(cfg, optimizer_send, NULL);
    if (engine_connect_ws(cfg, eng) != 0) {
        LOGE("cannot connect to engine websocket %s", eng->ws_url);
        return -1;
    }
    return 0;
}

void cdp_detach(void) {
    if (S.engine) {
        conn_close(S.engine);
        S.engine = NULL;
    }
    conn_t *c = S.clients;
    while (c) {
        conn_t *nx = c->next;
        conn_close(c);
        c = nx;
    }
    if (S.listen_fd >= 0) {
        close(S.listen_fd);
        S.listen_fd = -1;
    }
    if (S.opt) {
        optimizer_free(S.opt);
        S.opt = NULL;
    }
    while (S.sessions) {
        session_t *d = S.sessions;
        S.sessions = d->next;
        free(d);
    }
    while (S.pendings) {
        pending_t *d = S.pendings;
        S.pendings = d->next;
        free(d);
    }
    while (S.attach_reqs) {
        attach_req_t *d = S.attach_reqs;
        S.attach_reqs = d->next;
        free(d);
    }
}

int cdp_serve(const astra_config *cfg, engine_t *eng) {
    if (cdp_attach(cfg, eng) != 0) return -1;
    S.listen_fd = listen_on(cfg->bind_addr, cfg->port);
    if (S.listen_fd < 0) {
        cdp_detach();
        return -1;
    }
    S.serving = 1;
    LOGI("astra control plane ready on http://%s:%d  (ws: /devtools/browser/%s)",
         cfg->bind_addr, cfg->port, S.browser_id);
    uint64_t last_report = now_ms();
    while (!S.stop && !S.engine_dead) {
        do_poll(100);
        if (cfg->idle_exit_ms > 0 && now_ms() - S.last_activity > (uint64_t)cfg->idle_exit_ms) {
            LOGI("idle for %d ms, shutting down", cfg->idle_exit_ms);
            break;
        }
        if (cfg->log_level >= L_INFO && now_ms() - last_report > 60000) {
            last_report = now_ms();
            char saved[64];
            snprintf(saved, sizeof(saved), "%.1f%%", stats_saving_pct());
            LOGI("saved %s so far (%llu requests, %llu blocked, %llu cached)", saved,
                 (unsigned long long)g_stats.requests, (unsigned long long)g_stats.blocked,
                 (unsigned long long)g_stats.cache_hits);
        }
    }
    S.serving = 0;
    return 0;
}
