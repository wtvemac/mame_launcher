// net.h on the Pico 2 W: CYW43 WiFi (station) + lwIP raw TCP and DNS, polled from the main
// loop (pico_cyw43_arch_lwip_poll). lwIP callbacks only record what happened; net_poll()
// delivers it to the gateway, so gateway code never runs inside lwIP and vice versa.
#include <string.h>
#include <stdio.h>
#include "pico/cyw43_arch.h"
#include "lwip/tcp.h"
#include "lwip/dns.h"
#include "lwip/ip_addr.h"
#include "net.h"
#include "settings.h"
#include "console.h"
#include "log.h"

#define MAX_CONNS 16          // the gateway's 8, the web page's and the console's
#define MAX_RESOLVE 8

enum { S_FREE, S_RESOLVING, S_CONNECTING, S_OPEN };

struct net_conn {
    int state;
    struct tcp_pcb *pcb;
    const net_handlers_t *h;
    void *ctx;
    char host[64];
    uint16_t port;
    ip_addr_t addr;
    bool resolved, resolve_failed;
    bool open_pending, eof, eof_delivered, error_pending;
    net_accept_fn accept_pending;   // accepted by a listener: ask it for handlers
    err_t err;
    struct pbuf *rx;      // received, not yet taken by the gateway
    uint16_t rx_off;
};

static struct net_conn conns[MAX_CONNS];

static struct {
    bool used, done, ok;
    char name[256];
    ip_addr_t addr;
    net_resolve_cb cb;
    void *ctx;
} resq[MAX_RESOLVE];

static struct {
    bool connecting;
    uint32_t retry_at;
    int last_status;
    int join;              // NET_JOIN_*: a join the box asked for (net_join)
    uint32_t join_at, join_deadline;
} wifi;

// ---------------------------------------------------------------- scan

#define MAX_SCAN 32
static struct { bool running; int n; net_ap_t aps[MAX_SCAN]; } scan;

static int scan_result(void *env, const cyw43_ev_scan_result_t *r) {
    (void)env;
    if (r) scan_list_add(scan.aps, &scan.n, MAX_SCAN, r->ssid, r->ssid_len, r->rssi, r->auth_mode != 0);
    return 0;
}

void net_scan_start(void) {
    cyw43_wifi_scan_options_t opts = { 0 };
    scan.n = 0;
    scan.running = cyw43_wifi_scan(&cyw43_state, &opts, NULL, scan_result) == 0;
}

int net_scan_results(net_ap_t *out, int max) {
    if (scan.running && cyw43_wifi_scan_active(&cyw43_state)) return -1;
    scan.running = false;
    scan_list_sort(scan.aps, scan.n);
    int n = scan.n < max ? scan.n : max;
    memcpy(out, scan.aps, (size_t)n * sizeof *out);
    return n;
}

void net_address(char *out, size_t size) {
    if (!net_up()) { if (size) out[0] = 0; return; }
    snprintf(out, size, "%s", ip4addr_ntoa(netif_ip4_addr(&cyw43_state.netif[CYW43_ITF_STA])));
}

bool net_up(void) {
    return cyw43_tcpip_link_status(&cyw43_state, CYW43_ITF_STA) == CYW43_LINK_UP;
}

// ---------------------------------------------------------------- WiFi

static void wifi_connect(void) {
    if (!settings.ssid[0]) return;
    uint32_t auth = settings.password[0] ? CYW43_AUTH_WPA2_MIXED_PSK : CYW43_AUTH_OPEN;
    LOGI("[wifi] connecting to \"%s\"\n", settings.ssid);
    if (cyw43_arch_wifi_connect_async(settings.ssid, settings.password[0] ? settings.password : NULL, auth) == 0) wifi.connecting = true;
    wifi.retry_at = to_ms_since_boot(get_absolute_time()) + 20000;
}

void net_reconfigure(void) {
    cyw43_wifi_leave(&cyw43_state, CYW43_ITF_STA);
    wifi.connecting = false;
    wifi.join = NET_JOIN_IDLE;
    wifi_connect();
}

void net_join(const char *ssid, const char *password) {
    cyw43_wifi_leave(&cyw43_state, CYW43_ITF_STA);
    uint32_t now = to_ms_since_boot(get_absolute_time());
    LOGI("[wifi] joining \"%s\" for the box\n", ssid);
    wifi.join = NET_JOIN_PENDING;
    wifi.join_at = now;
    wifi.join_deadline = now + 20000;
    wifi.connecting = cyw43_arch_wifi_connect_async(ssid, password[0] ? password : NULL,
                                                    password[0] ? CYW43_AUTH_WPA2_MIXED_PSK : CYW43_AUTH_OPEN) == 0;
    if (!wifi.connecting) wifi.join = NET_JOIN_FAIL;
}

int net_join_status(void) { return wifi.join; }

bool net_init(void) {
    uint32_t country = CYW43_COUNTRY(settings.country[0] ? settings.country[0] : 'X', settings.country[1] ? settings.country[1] : 'X', 0);
    if (cyw43_arch_init_with_country(country)) { LOGE("[wifi] CYW43 init failed\n"); return false; }
    cyw43_arch_enable_sta_mode();
    cyw43_wifi_pm(&cyw43_state, CYW43_NONE_PM);   // no power saving: latency matters more
    wifi_connect();
    return true;
}

static void wifi_poll(uint32_t now) {
    int st = cyw43_tcpip_link_status(&cyw43_state, CYW43_ITF_STA);
    if (st != wifi.last_status) {
        wifi.last_status = st;
        if (st == CYW43_LINK_UP) {
            LOGI("[wifi] connected, address %s\n", ip4addr_ntoa(netif_ip4_addr(netif_default)));
            wifi.connecting = false;
        } else if (st == CYW43_LINK_FAIL || st == CYW43_LINK_NONET || st == CYW43_LINK_BADAUTH) {
            LOGI("[wifi] %s\n", st == CYW43_LINK_BADAUTH ? "wrong password" : st == CYW43_LINK_NONET ? "network not found" : "connection failed");
            wifi.connecting = false;
        } else if (st == CYW43_LINK_DOWN) LOGI("[wifi] down\n");
    }
    if (wifi.join == NET_JOIN_PENDING) {
        // a failure status left over from before the join is ignored for a moment
        if (st == CYW43_LINK_UP) wifi.join = NET_JOIN_OK;
        else if (st < 0 && (int32_t)(now - wifi.join_at) >= 1000)
            wifi.join = st == CYW43_LINK_BADAUTH ? NET_JOIN_BADAUTH : st == CYW43_LINK_NONET ? NET_JOIN_NONET : NET_JOIN_FAIL;
        else if ((int32_t)(now - wifi.join_deadline) >= 0) wifi.join = NET_JOIN_FAIL;
        return;   // no retries on the saved network meanwhile
    }
    if (st != CYW43_LINK_UP && settings.ssid[0] && (int32_t)(now - wifi.retry_at) >= 0) {
        cyw43_wifi_leave(&cyw43_state, CYW43_ITF_STA);
        wifi_connect();
    }
}

// ---------------------------------------------------------------- TCP

static err_t on_recv(void *arg, struct tcp_pcb *pcb, struct pbuf *p, err_t err) {
    struct net_conn *c = arg;
    if (!c) { if (p) pbuf_free(p); return ERR_OK; }
    if (!p) { c->eof = true; return ERR_OK; }
    if (c->rx) pbuf_cat(c->rx, p); else { c->rx = p; c->rx_off = 0; }
    return ERR_OK;
}

static void on_err(void *arg, err_t err) {
    struct net_conn *c = arg;
    if (!c) return;
    c->pcb = NULL;   // lwIP has freed it
    c->error_pending = true;
    c->err = err;
}

static err_t on_connected(void *arg, struct tcp_pcb *pcb, err_t err) {
    struct net_conn *c = arg;
    if (!c) return ERR_OK;
    c->state = S_OPEN;
    c->open_pending = true;
    return ERR_OK;
}

static void dns_found(const char *name, const ip_addr_t *addr, void *arg) {
    struct net_conn *c = arg;
    if (c->state != S_RESOLVING) return;
    if (addr) { c->addr = *addr; c->resolved = true; } else c->resolve_failed = true;
}

static void start_connect(struct net_conn *c) {
    c->pcb = tcp_new_ip_type(IPADDR_TYPE_V4);
    if (!c->pcb) { c->error_pending = true; c->err = ERR_MEM; return; }
    tcp_arg(c->pcb, c);
    tcp_recv(c->pcb, on_recv);
    tcp_err(c->pcb, on_err);
    tcp_nagle_disable(c->pcb);
    c->state = S_CONNECTING;
    err_t e = tcp_connect(c->pcb, &c->addr, c->port, on_connected);
    if (e != ERR_OK) { c->error_pending = true; c->err = e; }
}

// ---------------------------------------------------------------- listening

#define MAX_LISTEN 4
static struct { struct tcp_pcb *pcb; net_accept_fn accept; } listeners[MAX_LISTEN];

static err_t on_accept(void *arg, struct tcp_pcb *pcb, err_t err) {
    net_accept_fn accept = (net_accept_fn)arg;
    if (err != ERR_OK || !pcb) return ERR_VAL;
    for (int i = 0; i < MAX_CONNS; i++) {
        struct net_conn *c = &conns[i];
        if (c->state != S_FREE) continue;
        memset(c, 0, sizeof *c);
        c->pcb = pcb;
        c->state = S_OPEN;
        c->accept_pending = accept;
        tcp_arg(pcb, c);
        tcp_recv(pcb, on_recv);
        tcp_err(pcb, on_err);
        tcp_nagle_disable(pcb);
        return ERR_OK;
    }
    tcp_abort(pcb);
    return ERR_ABRT;
}

bool net_tcp_listen(uint16_t port, net_accept_fn accept) {
    for (int i = 0; i < MAX_LISTEN; i++) {
        if (listeners[i].pcb) continue;
        struct tcp_pcb *pcb = tcp_new_ip_type(IPADDR_TYPE_ANY);
        if (!pcb) return false;
        if (tcp_bind(pcb, IP_ANY_TYPE, port) != ERR_OK) { tcp_close(pcb); return false; }
        struct tcp_pcb *l = tcp_listen_with_backlog(pcb, 2);
        if (!l) { tcp_close(pcb); return false; }
        tcp_arg(l, (void *)accept);
        tcp_accept(l, on_accept);
        listeners[i].pcb = l;
        listeners[i].accept = accept;
        return true;
    }
    return false;
}

net_conn_t *net_tcp_open(const char *host, uint16_t port, const net_handlers_t *h, void *ctx) {
    if (!net_up()) return NULL;
    for (int i = 0; i < MAX_CONNS; i++) {
        struct net_conn *c = &conns[i];
        if (c->state != S_FREE) continue;
        memset(c, 0, sizeof *c);
        snprintf(c->host, sizeof c->host, "%s", host);
        c->port = port; c->h = h; c->ctx = ctx;
        c->state = S_RESOLVING;
        if (ipaddr_aton(host, &c->addr)) c->resolved = true;
        else {
            err_t e = dns_gethostbyname(host, &c->addr, dns_found, c);
            if (e == ERR_OK) c->resolved = true;
            else if (e != ERR_INPROGRESS) c->resolve_failed = true;
        }
        return c;
    }
    return NULL;
}

size_t net_tcp_send(net_conn_t *c, const uint8_t *d, size_t n) {
    if (c->state != S_OPEN || !c->pcb) return 0;
    size_t room = tcp_sndbuf(c->pcb);
    if (n > room) n = room;
    if (!n) return 0;
    if (tcp_write(c->pcb, d, (u16_t)n, TCP_WRITE_FLAG_COPY) != ERR_OK) return 0;
    tcp_output(c->pcb);
    return n;
}

void net_tcp_end(net_conn_t *c) {
    if (c->pcb && c->state == S_OPEN) tcp_shutdown(c->pcb, 0, 1);
}

void net_tcp_close(net_conn_t *c) {
    if (c->pcb) {
        tcp_arg(c->pcb, NULL);
        tcp_recv(c->pcb, NULL);
        tcp_err(c->pcb, NULL);
        if (tcp_close(c->pcb) != ERR_OK) tcp_abort(c->pcb);
        c->pcb = NULL;
    }
    if (c->rx) { pbuf_free(c->rx); c->rx = NULL; }
    c->state = S_FREE;
}

void net_tcp_kick(net_conn_t *c) { (void)c; }   // net_poll re-offers held data every pass

static const char *err_text(err_t e) {
    switch (e) {
    case ERR_RST: return "connection reset";
    case ERR_ABRT: return "connection aborted";
    case ERR_CLSD: return "connection closed";
    case ERR_TIMEOUT: return "timed out";
    case ERR_RTE: return "no route";
    case ERR_MEM: return "out of memory";
    case ERR_CONN: return "not connected";
    default: return "connection failed";
    }
}

static void conn_poll(struct net_conn *c) {
    if (c->accept_pending) {
        net_accept_fn accept = c->accept_pending;
        c->accept_pending = NULL;
        const net_handlers_t *h = NULL;
        void *ctx = accept(c, &h);
        if (!ctx || !h) { net_tcp_close(c); return; }
        c->h = h; c->ctx = ctx;
    }
    if (c->state == S_RESOLVING) {
        if (c->resolve_failed) { c->state = S_FREE; c->h->on_close(c->ctx, "host not found"); return; }
        if (c->resolved) start_connect(c);
    }
    if (c->error_pending) {
        if (c->pcb) { tcp_arg(c->pcb, NULL); tcp_err(c->pcb, NULL); tcp_recv(c->pcb, NULL); tcp_abort(c->pcb); c->pcb = NULL; }
        if (c->rx) { pbuf_free(c->rx); c->rx = NULL; }
        c->state = S_FREE;
        c->h->on_close(c->ctx, err_text(c->err));
        return;
    }
    if (c->open_pending) {
        c->open_pending = false;
        c->h->on_open(c->ctx);
        if (c->state == S_FREE) return;
    }
    // offer received data; acknowledge to the remote only what the gateway took
    while (c->rx) {
        struct pbuf *p = c->rx;
        size_t avail = p->len - c->rx_off;
        size_t took = c->h->on_data(c->ctx, (const uint8_t *)p->payload + c->rx_off, avail);
        if (c->state == S_FREE) return;
        if (took && c->pcb) tcp_recved(c->pcb, (u16_t)took);
        c->rx_off += (uint16_t)took;
        if (c->rx_off < p->len) break;
        c->rx = p->next;
        if (c->rx) pbuf_ref(c->rx);
        pbuf_free(p);
        c->rx_off = 0;
    }
    if (c->eof && !c->rx && !c->eof_delivered) {
        c->eof_delivered = true;
        c->h->on_end(c->ctx);
    }
}

// ---------------------------------------------------------------- DNS for the box

static void resolve_found(const char *name, const ip_addr_t *addr, void *arg) {
    int i = (int)(intptr_t)arg;
    if (!resq[i].used) return;
    resq[i].done = true;
    resq[i].ok = addr != NULL;
    if (addr) resq[i].addr = *addr;
}

void net_resolve(const char *name, net_resolve_cb cb, void *ctx) {
    for (int i = 0; i < MAX_RESOLVE; i++) {
        if (resq[i].used) continue;
        resq[i].used = true; resq[i].done = false; resq[i].cb = cb; resq[i].ctx = ctx;
        snprintf(resq[i].name, sizeof resq[i].name, "%s", name);
        err_t e = net_up() ? dns_gethostbyname(name, &resq[i].addr, resolve_found, (void *)(intptr_t)i) : ERR_CONN;
        if (e == ERR_OK) { resq[i].done = true; resq[i].ok = true; }
        else if (e != ERR_INPROGRESS) { resq[i].done = true; resq[i].ok = false; }
        return;
    }
    // too many at once: report failure from net_poll's context next time round is not possible
    // without a slot, so fail now; the box retries
    cb(ctx, false, NULL);
}

// The DNS server, kept in line with settings.dns on every pass: the one set there, else the
// router's (DHCP writes it into lwIP's server 0 with every lease), else 8.8.8.8. What DHCP wrote
// is noticed as server 0 differing from what we last put there, so a renewal can't undo a set
// server.
static struct { ip_addr_t put, router; bool router_known; } dnsp;

static const char *dns_source(void) {
    ip4_addr_t a;
    if (settings.dns[0] && ip4addr_aton(settings.dns, &a)) return "set";
    return dnsp.router_known && !ip_addr_isany(&dnsp.router) ? "from the router" : "the router gave none";
}

static void dns_poll(void) {
    const ip_addr_t *cur = dns_getserver(0);
    if (!ip_addr_cmp(cur, &dnsp.put)) { dnsp.router = *cur; dnsp.router_known = true; }   // DHCP's
    ip_addr_t want;
    ip4_addr_t a;
    if (settings.dns[0] && ip4addr_aton(settings.dns, &a)) ip_addr_copy_from_ip4(want, a);
    else if (dnsp.router_known && !ip_addr_isany(&dnsp.router)) want = dnsp.router;
    else IP_ADDR4(&want, 8, 8, 8, 8);
    if (!ip_addr_cmp(cur, &want)) {
        dns_setserver(0, &want);
        LOGI("[net] DNS server %s (%s)\n", ipaddr_ntoa(&want), dns_source());
    }
    dnsp.put = want;
}

void net_poll(uint32_t now) {
    wifi_poll(now);
    dns_poll();
    for (int i = 0; i < MAX_RESOLVE; i++) {
        if (!resq[i].used || !resq[i].done) continue;
        resq[i].used = false;
        uint8_t ip[4];
        if (resq[i].ok) memcpy(ip, &ip_2_ip4(&resq[i].addr)->addr, 4);
        LOGD("[net] resolve %s: %s\n", resq[i].name, resq[i].ok ? "ok" : "not found");
        resq[i].cb(resq[i].ctx, resq[i].ok, resq[i].ok ? ip : NULL);
    }
    for (int i = 0; i < MAX_CONNS; i++) if (conns[i].state != S_FREE) conn_poll(&conns[i]);
}

void net_status(void (*print)(const char *fmt, ...)) {
    int st = cyw43_tcpip_link_status(&cyw43_state, CYW43_ITF_STA);
    int32_t rssi = 0;
    cyw43_wifi_get_rssi(&cyw43_state, &rssi);
    int n = 0;
    for (int i = 0; i < MAX_CONNS; i++) if (conns[i].state != S_FREE) n++;
    if (st == CYW43_LINK_UP)
        print("wifi: \"%s\" %s, RSSI %ld dBm; %d outbound connection%s; DNS %s (%s)\n", settings.ssid,
              ip4addr_ntoa(netif_ip4_addr(netif_default)), (long)rssi, n, n == 1 ? "" : "s",
              ipaddr_ntoa(dns_getserver(0)), dns_source());
    else
        print("wifi: %s\n", !settings.ssid[0] ? "not configured (wifi <ssid> <password>)" : wifi.connecting ? "connecting" : "down");
}
