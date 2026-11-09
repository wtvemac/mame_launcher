// PPP server (the "ISP" end of the dial-up link): async-HDLC framing (RFC 1662), LCP
// (RFC 1661), PAP from the box (RFC 1334; its login carries WiFi settings) and IPCP
// (RFC 1332/1877).
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

void ppp_start(void);                 // carrier up: begin LCP shortly
void ppp_stop(void);                  // carrier down
void ppp_rx_byte(uint8_t b);          // byte from the box (data mode)
bool ppp_send_ip(const uint8_t *pkt, size_t len);   // false = no room towards the box
bool ppp_ip_up(void);
size_t ppp_frame_room(void);          // largest IP packet that fits towards the box right now
void ppp_poll(uint32_t now_ms);
void ppp_status(void (*print)(const char *fmt, ...));

// provided by modem.c
void modem_ppp_down(const char *why);
