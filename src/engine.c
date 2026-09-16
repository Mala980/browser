/* astra/engine.c */
#include "engine.h"

#include "http.h"
#include "json.h"

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static const char *CANDIDATES[] = {
    "chromium",           "chromium-browser",  "google-chrome",  "google-chrome-stable",
    "google-chrome-beta", "chrome",            "chrome-headless-shell", "chromium-headless-shell",
    "headless_shell",     "brave-browser",     "brave",          "microsoft-edge",
    "msedge",             "vivaldi",           "opera",          NULL};

static int is_exec(const char *p) {
    return p && access(p, X_OK) == 0 && file_exists(p);
}

int engine_autodetect(char *out, size_t n) {
    const char *env = getenv("ASTRA_ENGINE");
    if (env && is_exec(env)) {
        snprintf(out, n, "%s", env);
        return 0;
    }
    /* Termux / Android */
    const char *prefix = getenv("PREFIX");
    if (prefix) {
        char p[1024];
        snprintf(p, sizeof(p), "%s/bin/chromium", prefix);
        if (is_exec(p)) {
            snprintf(out, n, "%s", p);
            return 0;
        }
        snprintf(p, sizeof(p), "%s/bin/chrome-headless-shell", prefix);
        if (is_exec(p)) {
            snprintf(out, n, "%s", p);
            return 0;
        }
    }
    static const char *abs[] = {"/data/data/com.termux/files/usr/bin/chromium",
                                "/usr/bin/chromium",
                                "/usr/bin/chromium-browser",
                                "/usr/bin/google-chrome",
                                "/usr/bin/google-chrome-stable",
                                "/opt/google/chrome/chrome",
                                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                                NULL};
    for (int i = 0; abs[i]; i++)
        if (is_exec(abs[i])) {
            snprintf(out, n, "%s", abs[i]);
            return 0;
        }
    const char *path = getenv("PATH");
    if (path) {
        char *copy = astr_dup(path);
        char *save = NULL;
        for (char *dir = strtok_r(copy, ":", &save); dir; dir = strtok_r(NULL, ":", &save)) {
            for (int i = 0; CANDIDATES[i]; i++) {
                char p[2048];
                snprintf(p, sizeof(p), "%s/%s", dir, CANDIDATES[i]);
                if (is_exec(p)) {
                    snprintf(out, n, "%s", p);
                    free(copy);
                    return 0;
                }
            }
        }
        free(copy);
    }
    return -1;
}

int engine_probe(const char *host, int port, engine_t *eng, int timeout_ms) {
    http_resp_t r;
    if (http_get(host, port, "/json/version", timeout_ms, &r) != 0) {
        http_resp_free(&r);
        return -1;
    }
    json_t *v = json_parse(r.body, r.body_len);
    if (!v) {
        http_resp_free(&r);
        return -1;
    }
    const char *ws = json_get_str(v, "webSocketDebuggerUrl", NULL);
    if (!ws) {
        json_free(v);
        http_resp_free(&r);
        return -1;
    }
    snprintf(eng->ws_url, sizeof(eng->ws_url), "%s", ws);
    snprintf(eng->browser, sizeof(eng->browser), "%s", json_get_str(v, "Browser", "unknown"));
    snprintf(eng->protocol, sizeof(eng->protocol), "%s",
             json_get_str(v, "Protocol-Version", "1.3"));
    eng->port = port;
    json_free(v);
    http_resp_free(&r);
    return 0;
}

static int push(char **argv, int *n, int cap, const char *fmt, ...) {
    if (*n >= cap - 1) return -1;
    char buf[2048];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    argv[(*n)++] = astr_dup(buf);
    return 0;
}

static int engine_launch_once(const astra_config *cfg, engine_t *eng, int force_no_sandbox,
                              int force_disable_gpu) {
    memset(eng, 0, sizeof(*eng));
    eng->pid = -1;

    /* attach to an already running browser? */
    if (cfg->engine_url[0]) {
        url_t u;
        if (url_parse(cfg->engine_url, &u) != 0) {
            LOGE("bad --engine-url: %s", cfg->engine_url);
            return -1;
        }
        snprintf(eng->ws_url, sizeof(eng->ws_url), "%s", cfg->engine_url);
        eng->port = u.port;
        char host[256];
        snprintf(host, sizeof(host), "%s", u.host);
        if (engine_probe(host, u.port, eng, 5000) == 0) {
            /* keep the ws url that the browser advertises (path may differ) */
            LOGI("attached to engine at %s (%s)", eng->ws_url, eng->browser);
            return 0;
        }
        LOGW("probe failed on %s:%d, using provided url as-is", host, u.port);
        return 0;
    }

    char bin[1024];
    if (cfg->engine_path[0]) {
        snprintf(bin, sizeof(bin), "%s", cfg->engine_path);
    } else if (engine_autodetect(bin, sizeof(bin)) != 0) {
        LOGE("no Chromium-family engine found. Install chromium/chrome or pass --engine /path");
        LOGE("on Termux: pkg install chromium   (or: pkg install x11-repo && pkg install chromium)");
        return -1;
    }
    snprintf(eng->bin, sizeof(eng->bin), "%s", bin);

    /* profile dir */
    if (cfg->profile_dir[0]) {
        snprintf(eng->profile, sizeof(eng->profile), "%s", cfg->profile_dir);
    } else {
        char tmpl[] = "/tmp/astra-profile-XXXXXX";
        const char *tmp = getenv("TMPDIR");
        char buf[512];
        if (tmp)
            snprintf(buf, sizeof(buf), "%s/astra-profile-XXXXXX", tmp);
        else
            snprintf(buf, sizeof(buf), "%s", tmpl);
        char *d = mkdtemp(buf);
        if (!d) {
            LOGE("mkdtemp failed: %s", strerror(errno));
            return -1;
        }
        snprintf(eng->profile, sizeof(eng->profile), "%s", d);
    }
    mkdir_p(eng->profile, 0700);

    int is_root = (geteuid() == 0);
    int no_sandbox = cfg->no_sandbox >= 0 ? cfg->no_sandbox : (is_root ? 1 : 0);
    const char *prefix = getenv("PREFIX");
    int on_android = prefix || access("/system/bin/app_process", F_OK) == 0;
    if (on_android) no_sandbox = 1;
    if (force_no_sandbox) no_sandbox = 1;
    int use_gpu = cfg->gpu && !force_disable_gpu;

    char *argv[96];
    int n = 0;
    push(argv, &n, 96, "%s", bin);
    if (cfg->mode == MODE_HEADLESS) push(argv, &n, 96, "--headless=new");
    push(argv, &n, 96, "--remote-debugging-port=0");
    push(argv, &n, 96, "--user-data-dir=%s", eng->profile);
    push(argv, &n, 96, "--window-size=%d,%d", cfg->width, cfg->height);
    push(argv, &n, 96, "--remote-allow-origins=*");
    push(argv, &n, 96, "--no-first-run");
    push(argv, &n, 96, "--no-default-browser-check");
    push(argv, &n, 96, "--disable-background-networking");
    push(argv, &n, 96, "--disable-component-update");
    push(argv, &n, 96, "--disable-sync");
    push(argv, &n, 96, "--disable-domain-reliability");
    push(argv, &n, 96, "--disable-client-side-phishing-detection");
    push(argv, &n, 96, "--disable-breakpad");
    push(argv, &n, 96, "--metrics-recording-only");
    push(argv, &n, 96, "--disable-features=Translate,OptimizationHints,MediaRouter,DialMediaRouteProvider,CalculateNativeWinOcclusion");
    push(argv, &n, 96, "--disable-hang-monitor");
    push(argv, &n, 96, "--disable-ipc-flooding-protection");
    push(argv, &n, 96, "--disable-renderer-backgrounding");
    push(argv, &n, 96, "--disable-backgrounding-occluded-windows");
    push(argv, &n, 96, "--disable-background-timer-throttling");
    push(argv, &n, 96, "--disable-dev-shm-usage");
    push(argv, &n, 96, "--autoplay-policy=no-user-gesture-required");
    push(argv, &n, 96, "--force-device-scale-factor=1");
    push(argv, &n, 96, "--hide-scrollbars");
    push(argv, &n, 96, "--mute-audio");
    if (use_gpu) {
        /* compositor tuning for smoother scrolling / animation */
        push(argv, &n, 96, "--enable-gpu-rasterization");
        push(argv, &n, 96, "--ignore-gpu-blocklist");
        push(argv, &n, 96, "--enable-zero-copy");
        push(argv, &n, 96, "--num-raster-threads=2");
        push(argv, &n, 96, "--disable-checker-imaging");
        push(argv, &n, 96, "--disable-partial-raster");
    } else {
        push(argv, &n, 96, "--disable-gpu");
    }
    if (no_sandbox) push(argv, &n, 96, "--no-sandbox");
    if (cfg->single_process) push(argv, &n, 96, "--single-process");
    if (cfg->user_agent[0]) push(argv, &n, 96, "--user-agent=%s", cfg->user_agent);
    if (cfg->extra_flags[0]) {
        char *copy = astr_dup(cfg->extra_flags);
        char *save = NULL;
        for (char *tok = strtok_r(copy, " ", &save); tok; tok = strtok_r(NULL, " ", &save))
            push(argv, &n, 96, "%s", tok);
        free(copy);
    }
    argv[n] = NULL;

    LOGI("launching engine: %s (%s)", bin, cfg->mode == MODE_HEADLESS ? "headless" : "windowed");
    pid_t pid = fork();
    if (pid < 0) {
        LOGE("fork failed: %s", strerror(errno));
        return -1;
    }
    if (pid == 0) {
        int devnull = open("/dev/null", O_RDWR);
        if (devnull >= 0) {
            dup2(devnull, STDIN_FILENO);
            const char *log = getenv("ASTRA_ENGINE_LOG");
            if (log) {
                int lf = open(log, O_WRONLY | O_CREAT | O_APPEND, 0644);
                dup2(lf >= 0 ? lf : devnull, STDOUT_FILENO);
                dup2(lf >= 0 ? lf : devnull, STDERR_FILENO);
            } else {
                dup2(devnull, STDOUT_FILENO);
                dup2(devnull, STDERR_FILENO);
            }
        }
        setpgid(0, 0);
        execvp(bin, argv);
        _exit(127);
    }
    for (int i = 0; i < n; i++) free(argv[i]);
    eng->pid = pid;
    eng->owned = 1;

    /* wait for the DevTools port file */
    char portfile[1200];
    path_join(portfile, sizeof(portfile), eng->profile, "DevToolsActivePort");
    uint64_t deadline = now_ms() + 25000;
    int port = 0;
    char wspath[512] = {0};
    while (now_ms() < deadline) {
        uint8_t *data = NULL;
        size_t len = 0;
        if (file_read(portfile, &data, &len) == 0 && len > 0) {
            char *s = (char *)data;
            char *nl = strchr(s, '\n');
            if (nl) {
                *nl = 0;
                port = atoi(s);
                char *p2 = nl + 1;
                char *nl2 = strchr(p2, '\n');
                if (nl2) *nl2 = 0;
                snprintf(wspath, sizeof(wspath), "%s", p2);
            }
            free(data);
            if (port > 0) break;
        }
        /* did the child die? */
        int status = 0;
        pid_t w = waitpid(pid, &status, WNOHANG);
        if (w == pid) {
            LOGE("engine exited during startup (status %d). Try --no-sandbox or check ASTRA_ENGINE_LOG",
                 status);
            eng->owned = 0;
            return -1;
        }
        sleep_ms(50);
    }
    if (port <= 0) {
        LOGE("timed out waiting for DevTools port file %s", portfile);
        engine_stop(eng);
        return -1;
    }
    eng->port = port;
    if (wspath[0])
        snprintf(eng->ws_url, sizeof(eng->ws_url), "ws://127.0.0.1:%d%s", port, wspath);
    else
        snprintf(eng->ws_url, sizeof(eng->ws_url), "ws://127.0.0.1:%d/devtools/browser/astra",
                 port);

    if (engine_probe("127.0.0.1", port, eng, 8000) != 0) {
        LOGW("engine /json/version not reachable yet, using %s", eng->ws_url);
    } else {
        LOGI("engine ready: %s on port %d (%s)", eng->browser, port, eng->ws_url);
    }
    return 0;
}

int engine_launch(const astra_config *cfg, engine_t *eng) {
    int rc = engine_launch_once(cfg, eng, 0, 0);
    if (rc != 0 && !cfg->engine_url[0]) {
        /* Containers, Android and locked down kernels often cannot use the
         * Chromium sandbox - retry instead of failing the whole session. */
        LOGW("engine failed to start, retrying with --no-sandbox");
        rc = engine_launch_once(cfg, eng, 1, 0);
    }
    if (rc != 0 && !cfg->engine_url[0] && cfg->gpu) {
        LOGW("still failing, retrying with --no-sandbox --disable-gpu");
        rc = engine_launch_once(cfg, eng, 1, 1);
    }
    return rc;
}

void engine_stop(engine_t *eng) {
    if (!eng || !eng->owned || eng->pid <= 0) return;
    kill(-(eng->pid), SIGTERM);
    kill(eng->pid, SIGTERM);
    for (int i = 0; i < 30; i++) {
        int st = 0;
        pid_t w = waitpid(eng->pid, &st, WNOHANG);
        if (w == eng->pid) break;
        sleep_ms(50);
    }
    kill(-(eng->pid), SIGKILL);
    kill(eng->pid, SIGKILL);
    eng->pid = -1;
    eng->owned = 0;
}
