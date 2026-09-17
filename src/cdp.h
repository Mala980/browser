/* astra/cdp.h - CDP control plane: HTTP discovery endpoints + WebSocket proxy */
#ifndef ASTRA_CDP_H
#define ASTRA_CDP_H

#include "config.h"
#include "engine.h"
#include "json.h"
#include "optimizer.h"

typedef struct {
    json_t *result; /* owned by caller */
    char error[256];
} cdp_result_t;

/* Long running server: owns the event loop until SIGINT/SIGTERM or idle timeout. */
int cdp_serve(const astra_config *cfg, engine_t *eng);

/* One-shot mode: connect to the engine, let callers issue commands, then detach. */
int cdp_attach(const astra_config *cfg, engine_t *eng);
void cdp_detach(void);

/* Synchronous helpers (only valid while attached / serving). */
int cdp_cmd(const char *session, const char *method, json_t *params, int timeout_ms,
            cdp_result_t *out);
int cdp_cmd_simple(const char *session, const char *method, json_t *params, int timeout_ms);
int cdp_wait_event(const char *method, int timeout_ms, json_t **params_out);

/* Arm before issuing a command so that an event fired while the command is still
 * in flight is not missed (avoids the navigate/load race). */
void cdp_arm_event(const char *method);
int cdp_wait_armed(int timeout_ms);
void cdp_release_result(cdp_result_t *r);

/* Pump the event loop until `done(ud)` returns non zero, or timeout (ms). */
int cdp_run_until(int (*done)(void *ud), void *ud, int timeout_ms);

/* Currently attached page session (set by cdp_attach_to_first_page) */
const char *cdp_page_session(void);

/* Toggle lite mode at runtime (used by `astra bench`). */
int cdp_set_lite(int on);
void cdp_set_config(const astra_config *cfg);
int cdp_attach_to_first_page(int timeout_ms);
void cdp_net_rx_reset(void);
uint64_t cdp_net_rx(void);

#endif /* ASTRA_CDP_H */
