// license:BSD-3-Clause
// The platform under webtv-bridge's simulation build (pico/sim): the functions sim_lib.js supplies
// in the emulator, here backed by one host callback into the launcher (webtvbridge.rs, which has
// the op numbers). Plain C, no OS calls: sockets, files and the clock all belong to the host.
//
// The firmware's pin output is recorded here as it happens (js_pins_changed), so the host can read
// a whole exchange's worth of pin states in one call.
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include "net.h"
#include "settings.h"

typedef intptr_t (*host_callback)(int op, intptr_t a, intptr_t b, intptr_t c);

enum {
    HOST_SETTINGS_READ = 2, HOST_SETTINGS_WRITE = 3, HOST_REBOOT = 4,
    HOST_TCP_OPEN = 5, HOST_TCP_STATE = 6, HOST_TCP_RECV = 7, HOST_TCP_SEND = 8, HOST_TCP_END = 9, HOST_TCP_CLOSE = 10,
    HOST_RESOLVE = 11, HOST_RESOLVE_RESULT = 12, HOST_LISTEN = 13, HOST_ACCEPT = 14,
    HOST_NET_UP = 15, HOST_SCAN_START = 16, HOST_SCAN_RESULTS = 17, HOST_ADDRESS = 18,
    HOST_JOIN = 19, HOST_JOIN_STATUS = 20, HOST_RECONFIGURE = 21,
    HOST_LOG = 30
};

#define MAX_EVENTS 4095

static host_callback host;
static uint32_t now_us;
static uint8_t events[MAX_EVENTS][4];
static uint8_t last_state[4];
static unsigned event_count;
static bool have_state, event_overflow;
static bool reboot_pending;
extern int sim_status(void);
extern int sim_data(void);

void bridge_set_host(host_callback callback) { host = callback; }

// ---------------------------------------------------------------- time and pins

void bridge_set_time(uint64_t ns) { now_us = (uint32_t)(ns / 1000); }
uint32_t js_now_us(void) { return now_us; }

// The bridge's lines as a PERIPHERAL_PIN_STATE payload: data, status, drive, reserved
void js_pins_changed(void) {
    int s = sim_status(), d = sim_data();
    uint8_t state[4] = {
        d >= 0 ? (uint8_t)d : 255,
        (uint8_t)(((s >> 1) & 1) | ((s & 1) << 1) | ((s & 16) >> 2) | (s & 8) | ((s & 4) << 2)),
        d >= 0 ? 255 : 0, 0
    };
    if (have_state && !memcmp(state, last_state, sizeof state)) return;
    memcpy(last_state, state, sizeof state);
    have_state = true;
    if (event_count == MAX_EVENTS) { event_overflow = true; return; }
    memcpy(events[event_count++], state, sizeof state);
}

// The pin states since the last read (out holds MAX_EVENTS * 4 bytes), or -1 after an overflow.
int bridge_read_events(void *out) {
    js_pins_changed();
    if (event_overflow) return -1;
    unsigned n = event_count;
    memcpy(out, events, n * 4);
    event_count = 0;
    return (int)n;
}
unsigned bridge_event_count(void) { return event_count; }
int bridge_seed_events(const void *in, unsigned n) {
    if (n > MAX_EVENTS) return -1;
    memcpy(events, in, n * 4);
    event_count = n;
    event_overflow = false;
    return 0;
}
void bridge_get_state(void *out) { memcpy(out, last_state, sizeof last_state); }
int bridge_reboot_pending(void) { return reboot_pending; }

// ---------------------------------------------------------------- settings and reboot

int js_settings_read(void *p, uint32_t n) { return (int)host(HOST_SETTINGS_READ, (intptr_t)p, n, 0); }
int js_settings_write(const void *p, uint32_t n) { return (int)host(HOST_SETTINGS_WRITE, (intptr_t)p, n, 0); }
void js_reboot(void) { reboot_pending = true; host(HOST_REBOOT, 0, 0, 0); }

// ---------------------------------------------------------------- TCP and DNS

int js_tcp_open(const char *name, int port) { return (int)host(HOST_TCP_OPEN, (intptr_t)name, port, 0); }
int js_tcp_state(int id) { return (int)host(HOST_TCP_STATE, id, 0, 0); }
int js_tcp_recv(int id, void *p, int n) { return (int)host(HOST_TCP_RECV, id, (intptr_t)p, n); }
int js_tcp_send(int id, const void *p, int n) { return (int)host(HOST_TCP_SEND, id, (intptr_t)p, n); }
void js_tcp_end(int id) { host(HOST_TCP_END, id, 0, 0); }
void js_tcp_close(int id) { host(HOST_TCP_CLOSE, id, 0, 0); }
const char *js_tcp_error(int id) { (void)id; return "host socket error (see the launcher console)"; }
int js_resolve(const char *name) { return (int)host(HOST_RESOLVE, (intptr_t)name, (intptr_t)settings.dns, 0); }
int js_resolve_result(int id, uint8_t *ip) { return (int)host(HOST_RESOLVE_RESULT, id, (intptr_t)ip, 0); }
int js_listen(int port) { return (int)host(HOST_LISTEN, port, 0, 0); }
int js_accept(int id) { return (int)host(HOST_ACCEPT, id, 0, 0); }

// ---------------------------------------------------------------- the "radio"

// The host's network stands in for WiFi: the list is the host's (or just its uplink), and joining
// only picks a name. Traffic always uses the host's own network.
bool net_up(void) { return settings.ssid[0] && host(HOST_NET_UP, 0, 0, 0); }
void net_scan_start(void) { host(HOST_SCAN_START, 0, 0, 0); }
int net_scan_results(net_ap_t *p, int n) { return (int)host(HOST_SCAN_RESULTS, (intptr_t)p, n, 0); }
void net_address(char *p, size_t n) { if (!n) return; if (!net_up()) { p[0] = 0; return; } host(HOST_ADDRESS, (intptr_t)p, (intptr_t)n, 0); }
void net_join(const char *ssid, const char *password) { host(HOST_JOIN, (intptr_t)ssid, (intptr_t)password, 0); }
int net_join_status(void) { return (int)host(HOST_JOIN_STATUS, 0, 0, 0); }
void net_reconfigure(void) { host(HOST_RECONFIGURE, 0, 0, 0); }
void sim_scan_crowd(int n) { (void)n; }

// ---------------------------------------------------------------- log output (bridge_stdio.h)

int bridge_vprintf(const char *fmt, va_list ap) {
    char buffer[2048];
    int n = vsnprintf(buffer, sizeof buffer, fmt, ap);
    if (n <= 0) return n;
    unsigned length = n < (int)sizeof buffer ? (unsigned)n : (unsigned)sizeof buffer - 1;
    if (host) host(HOST_LOG, (intptr_t)buffer, length, 0);
    return n;
}

int bridge_printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = bridge_vprintf(fmt, ap);
    va_end(ap);
    return n;
}

int bridge_putchar(int c) {
    char byte = (char)c;
    if (host) host(HOST_LOG, (intptr_t)&byte, 1, 0);
    return (unsigned char)c;
}
