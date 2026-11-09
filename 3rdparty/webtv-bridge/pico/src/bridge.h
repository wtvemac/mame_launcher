// The bridge's service side, independent of the hardware: a Hayes modem (modem.c) whose
// "line" is a PPP server (ppp.c) feeding an IP gateway (gw.c) that splices the box's TCP
// connections onto outbound ones (net.h). The link layer (link.c on the Pico, a socket on
// the host) moves bytes between the box and this module.
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

void bridge_init(void);
void bridge_poll(uint32_t now_ms);          // timers, deferred work; call often

// link side
void bridge_from_box(const uint8_t *d, size_t n);
size_t bridge_to_box(uint8_t *d, size_t max);   // take bytes queued for the box
size_t bridge_to_box_pending(void);
void bridge_set_dtr(bool on);
void bridge_link_reset(void);               // nInit: hang up, flush, back to command mode
bool bridge_carrier(void);

// status for the console
void bridge_status(void (*print)(const char *fmt, ...));

// modem.c
void modem_init(void);
void modem_reset(void);
void modem_from_host(uint8_t b);
void modem_poll(uint32_t now_ms);
void modem_set_dtr(bool on);
void modem_hangup(const char *why);
bool modem_online(void);
bool modem_dcd(void);
size_t modem_out_free(void);
size_t modem_out_used(void);
size_t modem_out_read(uint8_t *d, size_t max);
bool modem_out_put(const uint8_t *d, size_t n);   // all or nothing (PPP frames)
void modem_status(void (*print)(const char *fmt, ...));

uint32_t bridge_now_ms(void);               // provided by the platform
