/* astra/engine.h - discovery, launch and shutdown of the rendering engine */
#ifndef ASTRA_ENGINE_H
#define ASTRA_ENGINE_H

#include "config.h"

typedef struct {
    pid_t pid;
    int owned;          /* 1 = astra started it and must stop it */
    int port;
    char ws_url[512];   /* ws://127.0.0.1:port/devtools/browser/<id> */
    char profile[1024];
    char bin[1024];
    char browser[256];  /* "Chrome/126.0.0.0" */
    char protocol[64];
} engine_t;

/* find a Chromium-family binary in PATH / Termux / well known locations */
int engine_autodetect(char *out, size_t n);

/* launch (or attach to) the engine; fills eng */
int engine_launch(const astra_config *cfg, engine_t *eng);

/* query /json/version and fill ws_url / browser / protocol */
int engine_probe(const char *host, int port, engine_t *eng, int timeout_ms);

void engine_stop(engine_t *eng);

#endif /* ASTRA_ENGINE_H */
