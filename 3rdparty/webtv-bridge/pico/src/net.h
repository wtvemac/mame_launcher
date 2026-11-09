// Network transport: outbound TCP and name resolution for the gateway, listening ports for the
// bridge's own web page and console (web.c). net_lwip.c implements it on the Pico (CYW43 WiFi
// + lwIP), host/net_posix.c with BSD sockets for testing on a computer.
//
// Callbacks never run inside a net_* call; they come from net_poll() (or the platform's
// network polling), so the caller's state is always consistent when they arrive.
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

typedef struct net_conn net_conn_t;

typedef struct {
    void (*on_open)(void *ctx);
    // remote data; return how many bytes were taken. The rest is offered again after
    // net_tcp_kick() (and the remote is flow-controlled meanwhile).
    size_t (*on_data)(void *ctx, const uint8_t *d, size_t n);
    void (*on_end)(void *ctx);                    // remote closed its side
    void (*on_close)(void *ctx, const char *err); // gone (err = NULL for a normal close)
} net_handlers_t;

bool net_up(void);                                // WiFi connected with an address
net_conn_t *net_tcp_open(const char *host, uint16_t port, const net_handlers_t *h, void *ctx);
size_t net_tcp_send(net_conn_t *c, const uint8_t *d, size_t n);   // bytes accepted
void net_tcp_end(net_conn_t *c);                  // half-close (FIN after queued data)
void net_tcp_close(net_conn_t *c);                // drop it; no more callbacks
void net_tcp_kick(net_conn_t *c);                 // the consumer has room again

// Inbound: accept() gives each new connection its handlers and context (on_open isn't called:
// it is open already); NULL refuses it. The emulator build listens on 127.0.0.1 only.
typedef void *(*net_accept_fn)(net_conn_t *c, const net_handlers_t **h);
bool net_tcp_listen(uint16_t port, net_accept_fn accept);

typedef void (*net_resolve_cb)(void *ctx, bool ok, const uint8_t ip[4]);
void net_resolve(const char *name, net_resolve_cb cb, void *ctx);

// Join a WiFi network the box asked for (its PAP login), without touching the saved settings.
// net_join_status() says NET_JOIN_PENDING until that gets an address (NET_JOIN_OK) or fails.
// net_reconfigure() goes back to the network in the settings.
enum { NET_JOIN_IDLE, NET_JOIN_PENDING, NET_JOIN_OK, NET_JOIN_BADAUTH, NET_JOIN_NONET, NET_JOIN_FAIL };
void net_join(const char *ssid, const char *password);
int net_join_status(void);
void net_reconfigure(void);

// A scan for networks (the bridge's WiFi setup on the TV): net_scan_results() is -1 while it
// runs, then the number found (up to max).
typedef struct { char ssid[33]; int8_t rssi; bool secure; } net_ap_t;
void net_scan_start(void);
int net_scan_results(net_ap_t *out, int max);   // one entry a name, strongest first
// scanlist.c: merging the radio's reports into that list (by name, the strongest kept)
void scan_list_add(net_ap_t *list, int *n, int max, const uint8_t *ssid, size_t len, int rssi, bool secure);
void scan_list_sort(net_ap_t *list, int n);
void net_address(char *out, size_t size);         // "a.b.c.d", or "" without one

void net_poll(uint32_t now_ms);
void net_status(void (*print)(const char *fmt, ...));
