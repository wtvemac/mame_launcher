// IPv4 gateway on the PPP link: answers ICMP echo and DNS itself and terminates every TCP
// connection the box opens, splicing it onto an outbound connection (net.h).
#pragma once
#include <stdint.h>
#include <stddef.h>

void gw_input(const uint8_t *pkt, size_t len);   // a datagram from the box
void gw_reset(void);                               // hang-up: drop every connection
void gw_poll(uint32_t now_ms);
void gw_status(void (*print)(const char *fmt, ...));

// WebTV service redirect: connections to ports WTVP_PORT_MIN..MAX go to this host (name or
// dotted IP) instead of the address the box asked for. Empty = no redirect.
void gw_set_service(const char *host);
