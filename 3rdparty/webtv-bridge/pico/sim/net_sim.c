// net.h for the firmware in the emulator: a simulated radio (scans show a few networks, a join
// takes 3 s of emulated time and fails for a password starting with "bad" or a network starting
// with "nonet"), and TCP and DNS through the emulator process's own sockets (sim_lib.js). The
// connections themselves work like net_posix.c's: data is pulled in net_poll() and offered to
// the handler, and whatever it doesn't take waits, so the remote is flow-controlled.
#include <stdio.h>
#include <string.h>
#include "net.h"
#include "settings.h"
#include "log.h"

// sim_lib.js; ids are the JS side's
extern int js_tcp_open(const char *host, int port);
extern int js_tcp_state(int id);                       // 0 connecting, 1 open, -1 failed
extern int js_tcp_recv(int id, uint8_t *buf, int max); // bytes, 0 at the end, -1 nothing yet, -2 failed
extern int js_tcp_send(int id, const uint8_t *buf, int n);
extern void js_tcp_end(int id);
extern void js_tcp_close(int id);
extern const char *js_tcp_error(int id);
extern int js_resolve(const char *name);
extern int js_resolve_result(int id, uint8_t *ip);    // -1 pending, 0 not found, 1 found
extern int js_listen(int port);
extern int js_accept(int lid);
uint32_t bridge_now_ms(void);

#define MAX_CONNS 16

struct net_conn {
    bool used, open, remote_eof;
    int id;
    const net_handlers_t *h;
    void *ctx;
    uint8_t pend[8192];
    size_t pend_off, pend_len;
};
static struct net_conn conns[MAX_CONNS];

#define MAX_RESOLVE 8
static struct { bool used; int id; net_resolve_cb cb; void *ctx; } resq[MAX_RESOLVE];

// ---------------------------------------------------------------- the radio

static struct { int status, result; uint32_t done_at; } join;
static int scan_left;

bool net_up(void) { return settings.ssid[0] != 0; }

// sim_scan_crowd(n): later scans hear n reports the way a crowded building does: the four
// networks above plus many weak neighbours, each heard several times, and the strongest
// network reported last
static int crowd;
void sim_scan_crowd(int n) { crowd = n; }

void net_scan_start(void) { scan_left = 3; }
int net_scan_results(net_ap_t *out, int max) {
    static const struct { const char *ssid; int8_t rssi; bool secure; } nets[] = {
        { "NETGEAR42", -48, true }, { "xfinitywifi", -60, false }, { "Artemis WiFi", -63, true },
    };
    if (scan_left > 0) { scan_left--; return -1; }
    net_ap_t list[32];
    int n = 0;
    for (int i = 0; i < crowd; i++) {   // neighbours, -70 to -91 dBm, each heard three times
        char name[33];
        snprintf(name, sizeof name, "Apt %d%02d", 2 + i / 3 % 4, i / 3 + 1);
        scan_list_add(list, &n, 32, (const uint8_t *)name, strlen(name), -70 - (i * 7) % 22, i % 5 != 0);
    }
    for (size_t i = sizeof nets / sizeof nets[0]; i-- > 0;)   // the strongest come last
        scan_list_add(list, &n, 32, (const uint8_t *)nets[i].ssid, strlen(nets[i].ssid), nets[i].rssi, nets[i].secure);
    scan_list_sort(list, n);
    if (n > max) n = max;
    memcpy(out, list, (size_t)n * sizeof *out);
    return n;
}

void net_address(char *out, size_t size) { snprintf(out, size, "%s", net_up() ? "192.168.1.42" : ""); }

void net_join(const char *ssid, const char *password) {
    LOGI("[wifi] joining \"%s\" (simulated radio)\n", ssid);
    join.status = NET_JOIN_PENDING;
    join.result = !strncmp(password, "bad", 3) ? NET_JOIN_BADAUTH : !strncmp(ssid, "nonet", 5) ? NET_JOIN_NONET : NET_JOIN_OK;
    join.done_at = bridge_now_ms() + 3000;
}

int net_join_status(void) {
    if (join.status == NET_JOIN_PENDING && (int32_t)(bridge_now_ms() - join.done_at) >= 0) join.status = join.result;
    return join.status;
}

void net_reconfigure(void) { join.status = NET_JOIN_IDLE; }

// ---------------------------------------------------------------- TCP

static struct net_conn *slot(void) {
    for (int i = 0; i < MAX_CONNS; i++) if (!conns[i].used) { memset(&conns[i], 0, sizeof conns[i]); conns[i].used = true; return &conns[i]; }
    return NULL;
}

net_conn_t *net_tcp_open(const char *host, uint16_t port, const net_handlers_t *h, void *ctx) {
    struct net_conn *c = slot();
    if (!c) return NULL;
    c->h = h; c->ctx = ctx;
    c->id = js_tcp_open(host, port);
    return c;
}

size_t net_tcp_send(net_conn_t *c, const uint8_t *d, size_t n) {
    if (!c->open) return 0;
    int k = js_tcp_send(c->id, d, (int)n);
    return k > 0 ? (size_t)k : 0;
}

void net_tcp_end(net_conn_t *c) { js_tcp_end(c->id); }
void net_tcp_close(net_conn_t *c) { js_tcp_close(c->id); c->used = false; }
void net_tcp_kick(net_conn_t *c) { (void)c; }   // net_poll offers held data every time

#define MAX_LISTEN 4
static struct { int lid; net_accept_fn accept; } listeners[MAX_LISTEN];

bool net_tcp_listen(uint16_t port, net_accept_fn accept) {
    for (int i = 0; i < MAX_LISTEN; i++) {
        if (listeners[i].accept) continue;
        int lid = js_listen(port);
        if (lid < 0) return false;
        listeners[i].lid = lid;
        listeners[i].accept = accept;
        return true;
    }
    return false;
}

void net_resolve(const char *name, net_resolve_cb cb, void *ctx) {
    for (int i = 0; i < MAX_RESOLVE; i++) if (!resq[i].used) {
        resq[i].used = true; resq[i].cb = cb; resq[i].ctx = ctx;
        resq[i].id = js_resolve(name);
        return;
    }
    cb(ctx, false, NULL);
}

static void fail(struct net_conn *c, const char *why) {
    js_tcp_close(c->id);
    c->used = false;
    c->h->on_close(c->ctx, why);
}

void net_poll(uint32_t now) {
    (void)now;
    for (int i = 0; i < MAX_RESOLVE; i++) if (resq[i].used) {
        uint8_t ip[4];
        int r = js_resolve_result(resq[i].id, ip);
        if (r < 0) continue;
        resq[i].used = false;
        resq[i].cb(resq[i].ctx, r == 1, r == 1 ? ip : NULL);
    }
    for (int i = 0; i < MAX_LISTEN; i++) {
        if (!listeners[i].accept) continue;
        int id;
        while ((id = js_accept(listeners[i].lid)) >= 0) {
            struct net_conn *c = slot();
            if (!c) { js_tcp_close(id); continue; }
            c->id = id; c->open = true;
            const net_handlers_t *h = NULL;
            void *ctx = listeners[i].accept(c, &h);
            if (!ctx || !h) { js_tcp_close(id); c->used = false; continue; }
            c->h = h; c->ctx = ctx;
        }
    }
    for (int i = 0; i < MAX_CONNS; i++) {
        struct net_conn *c = &conns[i];
        if (!c->used) continue;
        if (!c->open) {
            int st = js_tcp_state(c->id);
            if (st < 0) { fail(c, js_tcp_error(c->id)); continue; }
            if (st == 0) continue;
            c->open = true;
            c->h->on_open(c->ctx);
            if (!c->used) continue;
        }
        for (;;) {
            if (c->pend_len) {
                size_t k = c->h->on_data(c->ctx, c->pend + c->pend_off, c->pend_len);
                if (!c->used) break;
                c->pend_off += k; c->pend_len -= k;
                if (c->pend_len) break;
            }
            if (c->remote_eof) break;
            int k = js_tcp_recv(c->id, c->pend, sizeof c->pend);
            if (k > 0) { c->pend_off = 0; c->pend_len = (size_t)k; continue; }
            if (k == 0) { c->remote_eof = true; c->h->on_end(c->ctx); break; }
            if (k == -2) fail(c, js_tcp_error(c->id));
            break;
        }
    }
}

void net_status(void (*print)(const char *fmt, ...)) {
    int n = 0;
    for (int i = 0; i < MAX_CONNS; i++) if (conns[i].used) n++;
    print("wifi: %s (simulated radio); %d connection%s; DNS: the computer's resolver (setting: %s)\n",
          net_up() ? settings.ssid : "not set up", n, n == 1 ? "" : "s", settings.dns[0] ? settings.dns : "auto");
}
