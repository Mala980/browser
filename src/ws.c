/* astra/ws.c */
#include "ws.h"

#include <stdlib.h>

static const char *ci_find(const char *hay, size_t haylen, const char *needle) {
    size_t n = strlen(needle);
    if (n > haylen) return NULL;
    for (size_t i = 0; i + n <= haylen; i++)
        if (strncasecmp(hay + i, needle, n) == 0) return hay + i;
    return NULL;
}

#define WS_GUID "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

void ws_parser_init(ws_parser_t *p, ws_msg_fn cb, void *ud) {
    memset(p, 0, sizeof(*p));
    p->cb = cb;
    p->ud = ud;
    p->max_message = 256u * 1024u * 1024u; /* 256 MiB hard cap */
    buf_init(&p->payload);
}

void ws_parser_free(ws_parser_t *p) { buf_free(&p->payload); }

size_t ws_parser_pending(ws_parser_t *p) {
    if (p->state == 0) return 2;
    return (size_t)(p->need - p->got);
}

static void ws_deliver(ws_parser_t *p, int opcode, const uint8_t *data, size_t len) {
    if (p->cb) p->cb(p->ud, opcode, data, len);
}

int ws_parser_feed(ws_parser_t *p, const uint8_t *data, size_t len) {
    size_t i = 0;
    while (i < len && !p->failed) {
        switch (p->state) {
            case 0: { /* 2 byte header */
                while (i < len && p->got < 2) p->hdr[p->got++] = data[i++];
                if (p->got < 2) return 0;
                p->fin = (p->hdr[0] & 0x80) != 0;
                int opcode = p->hdr[0] & 0x0F;
                p->masked = (p->hdr[1] & 0x80) != 0;
                uint64_t l7 = (uint64_t)(p->hdr[1] & 0x7F);
                if (opcode != WS_CONT) {
                    p->frag_opcode = opcode;
                    buf_clear(&p->payload);
                }
                p->opcode = opcode;
                if (l7 < 126) {
                    p->need = l7;
                    p->state = p->masked ? 3 : 4;
                    p->got = 0;
                    p->mask_off = 0;
                } else if (l7 == 126) {
                    p->need = 0;
                    p->state = 1;
                    p->got = 0;
                } else {
                    p->need = 0;
                    p->state = 2;
                    p->got = 0;
                }
                break;
            }
            case 1: {
                while (i < len && p->got < 2) {
                    p->need = (p->need << 8) | data[i++];
                    p->got++;
                }
                if (p->got < 2) return 0;
                p->state = p->masked ? 3 : 4;
                p->got = 0;
                p->mask_off = 0;
                break;
            }
            case 2: {
                while (i < len && p->got < 8) {
                    p->need = (p->need << 8) | data[i++];
                    p->got++;
                }
                if (p->got < 8) return 0;
                p->state = p->masked ? 3 : 4;
                p->got = 0;
                p->mask_off = 0;
                break;
            }
            case 3: {
                while (i < len && p->got < 4) p->mask[p->got++] = data[i++];
                if (p->got < 4) return 0;
                p->state = 4;
                p->got = 0;
                p->mask_off = 0;
                break;
            }
            case 4: {
                uint64_t remain = p->need - p->got;
                size_t avail = len - i;
                size_t take = (size_t)(remain < (uint64_t)avail ? remain : (uint64_t)avail);
                if (take) {
                    size_t base = p->payload.len;
                    if (base + take > p->max_message) {
                        p->failed = 1;
                        return -1;
                    }
                    buf_append(&p->payload, data + i, take);
                    if (p->masked) {
                        for (size_t k = 0; k < take; k++)
                            p->payload.data[base + k] ^= p->mask[(p->mask_off + k) & 3];
                        p->mask_off += take;
                    }
                    i += take;
                    p->got += take;
                }
                if (p->got < p->need) return 0;
                /* complete frame */
                int op = p->opcode == WS_CONT ? p->frag_opcode : p->opcode;
                if (op == WS_CLOSE) {
                    p->closed = 1;
                    ws_deliver(p, WS_CLOSE, p->payload.data, p->payload.len);
                } else if (op == WS_PING) {
                    ws_deliver(p, WS_PING, p->payload.data, p->payload.len);
                } else if (op == WS_PONG) {
                    ws_deliver(p, WS_PONG, p->payload.data, p->payload.len);
                } else {
                    if (p->fin) {
                        ws_deliver(p, op, p->payload.data, p->payload.len);
                        buf_clear(&p->payload);
                    }
                    /* continuation frames accumulate in p->payload */
                }
                p->state = 0;
                p->got = 0;
                p->need = 0;
                break;
            }
            default: p->failed = 1; return -1;
        }
    }
    return p->failed ? -1 : 0;
}

void ws_frame_ex(buf_t *out, int opcode, const void *payload, size_t len, int mask, int fin) {
    uint8_t hdr[14];
    size_t hn = 0;
    hdr[0] = (uint8_t)((fin ? 0x80 : 0x00) | (opcode & 0x0F));
    uint8_t maskbit = mask ? 0x80 : 0;
    if (len < 126) {
        hdr[1] = (uint8_t)(maskbit | len);
        hn = 2;
    } else if (len < 65536) {
        hdr[1] = (uint8_t)(maskbit | 126);
        hdr[2] = (uint8_t)(len >> 8);
        hdr[3] = (uint8_t)(len & 0xFF);
        hn = 4;
    } else {
        hdr[1] = (uint8_t)(maskbit | 127);
        for (int k = 0; k < 8; k++) hdr[2 + k] = (uint8_t)(((uint64_t)len >> (56 - k * 8)) & 0xFF);
        hn = 10;
    }
    uint8_t mk[4];
    if (mask) {
        random_bytes(mk, 4);
        memcpy(hdr + hn, mk, 4);
        hn += 4;
    }
    buf_append(out, hdr, hn);
    if (len) {
        if (mask) {
            size_t base = out->len;
            buf_append(out, payload, len);
            for (size_t k = 0; k < len; k++) out->data[base + k] ^= mk[k & 3];
        } else {
            buf_append(out, payload, len);
        }
    }
}

void ws_frame(buf_t *out, int opcode, const void *payload, size_t len, int mask) {
    ws_frame_ex(out, opcode, payload, len, mask, 1);
}

void ws_text(buf_t *out, const char *s, int mask) { ws_frame(out, WS_TEXT, s, strlen(s), mask); }

char *ws_client_request(const char *host, int port, const char *path) {
    uint8_t raw[16];
    random_bytes(raw, 16);
    char *key = b64_encode(raw, 16);
    buf_t b;
    buf_init(&b);
    buf_appendf(&b,
                "GET %s HTTP/1.1\r\n"
                "Host: %s:%d\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                "Sec-WebSocket-Key: %s\r\n"
                "Sec-WebSocket-Version: 13\r\n"
                "User-Agent: Astra/0.1\r\n"
                "\r\n",
                path, host, port, key);
    free(key);
    char *out = b.data ? (char *)b.data : NULL;
    if (!b.data) buf_init(&b);
    return out;
}

int ws_client_check_response(const char *resp, size_t len, const char *sec_key) {
    (void)len;
    if (!resp || strncmp(resp, "HTTP/1.1 101", 12) != 0) return -1;
    if (!sec_key) return 0;
    buf_t b;
    buf_init(&b);
    buf_appendstr(&b, sec_key);
    buf_appendstr(&b, WS_GUID);
    uint8_t d[20];
    astra_sha1(b.data, b.len, d);
    buf_free(&b);
    char *expect = b64_encode(d, 20);
    const char *found = ci_find(resp, strlen(resp), "Sec-WebSocket-Accept:");
    int ok = 0;
    if (found) {
        found += strlen("Sec-WebSocket-Accept:");
        while (*found == ' ') found++;
        ok = strncmp(found, expect, strlen(expect)) == 0;
    }
    free(expect);
    return ok ? 0 : -1;
}

static const char *hdr_find(const char *req, size_t len, const char *name) {
    size_t nl = strlen(name);
    const char *p = req;
    const char *end = req + len;
    while (p < end) {
        const char *eol = memchr(p, '\n', (size_t)(end - p));
        size_t line = eol ? (size_t)(eol - p) : (size_t)(end - p);
        if (line >= nl && strncasecmp(p, name, nl) == 0) {
            const char *v = p + nl;
            while (v < p + line && (*v == ' ' || *v == '\t' || *v == ':')) v++;
            return v; /* caller must bound by line end */
        }
        if (!eol) break;
        p = eol + 1;
        if (p < end && p[-1] == '\n' && (p - req) >= 2 && p[-2] == '\n') break;
    }
    return NULL;
}

int ws_server_handshake(const char *request, size_t len, buf_t *response) {
    if (!request || len < 20) return -1;
    if (strncmp(request, "GET ", 4) != 0) return -1;
    const char *kw = ci_find(request, len, "Sec-WebSocket-Key:");
    if (!kw) return -1;
    kw += strlen("Sec-WebSocket-Key:");
    while (*kw == ' ') kw++;
    const char *ke = kw;
    while (*ke && *ke != '\r' && *ke != '\n') ke++;
    size_t klen = (size_t)(ke - kw);
    if (klen == 0 || klen > 128) return -1;
    buf_t b;
    buf_init(&b);
    buf_append(&b, kw, klen);
    buf_appendstr(&b, WS_GUID);
    uint8_t d[20];
    astra_sha1(b.data, b.len, d);
    buf_free(&b);
    char *accept = b64_encode(d, 20);
    buf_appendstr(response, "HTTP/1.1 101 Switching Protocols\r\n");
    buf_appendstr(response, "Upgrade: websocket\r\n");
    buf_appendstr(response, "Connection: Upgrade\r\n");
    buf_appendf(response, "Sec-WebSocket-Accept: %s\r\n", accept);
    buf_appendstr(response, "\r\n");
    free(accept);
    (void)hdr_find;
    return 0;
}
