/* astra/ws.h - RFC 6455 WebSocket framing, handshake, incremental parser */
#ifndef ASTRA_WS_H
#define ASTRA_WS_H

#include "util.h"

#define WS_CONT 0x0
#define WS_TEXT 0x1
#define WS_BIN 0x2
#define WS_CLOSE 0x8
#define WS_PING 0x9
#define WS_PONG 0xA

typedef void (*ws_msg_fn)(void *ud, int opcode, const uint8_t *data, size_t len);

typedef struct {
    int state; /* 0=header 1=len16 2=len64 3=mask 4=payload */
    uint8_t hdr[2];
    int fin;
    int opcode;
    int masked;
    uint64_t need;
    uint64_t got;
    uint8_t mask[4];
    uint64_t mask_off;
    buf_t payload;
    int frag_opcode;
    int closed;
    int failed;
    ws_msg_fn cb;
    void *ud;
    size_t max_message; /* guard against unbounded memory use */
} ws_parser_t;

void ws_parser_init(ws_parser_t *p, ws_msg_fn cb, void *ud);
void ws_parser_free(ws_parser_t *p);
/* returns 0 on success, -1 on protocol error / too large */
int ws_parser_feed(ws_parser_t *p, const uint8_t *data, size_t len);
size_t ws_parser_pending(ws_parser_t *p); /* bytes still needed for a partial frame */

/* framing (mask=1 for messages sent by a client) */
void ws_frame(buf_t *out, int opcode, const void *payload, size_t len, int mask);
void ws_frame_ex(buf_t *out, int opcode, const void *payload, size_t len, int mask, int fin);
void ws_text(buf_t *out, const char *s, int mask);

/* handshake helpers */
char *ws_client_request(const char *host, int port, const char *path); /* malloc'd request */
int ws_client_check_response(const char *resp, size_t len, const char *sec_key);
int ws_server_handshake(const char *request, size_t len, buf_t *response);

#endif /* ASTRA_WS_H */
