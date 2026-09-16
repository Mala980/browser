/* astra/optimizer.h - request-level bandwidth optimizer (adblock, cache, images, minify) */
#ifndef ASTRA_OPTIMIZER_H
#define ASTRA_OPTIMIZER_H

#include "cache.h"
#include "config.h"
#include "filters.h"
#include "json.h"

/* Callback used by the optimizer to emit CDP commands toward the engine.
 * The optimizer keeps ownership of `msg`; the callee serializes it immediately. */
typedef void (*cdp_send_fn)(void *ud, const char *session_id, json_t *msg);

typedef struct optimizer optimizer_t;

optimizer_t *optimizer_new(const astra_config *cfg, cdp_send_fn send, void *send_ud);
void optimizer_free(optimizer_t *o);

/* Fetch.requestPaused event (both Request and Response stage) */
void optimizer_handle_paused(optimizer_t *o, const char *engine_session, json_t *params);

/* Reply to a Fetch.getResponseBody issued by the optimizer */
void optimizer_handle_response(optimizer_t *o, const char *engine_session, int id, json_t *result,
                               json_t *error);

/* Called when a page navigation happens so third-party detection has a base host */
void optimizer_set_page_host(optimizer_t *o, const char *engine_session, const char *url);

size_t optimizer_rule_count(const optimizer_t *o);
size_t optimizer_session_count(const optimizer_t *o);

#endif /* ASTRA_OPTIMIZER_H */
