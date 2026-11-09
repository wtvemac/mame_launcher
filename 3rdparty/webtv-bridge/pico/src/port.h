// The parallel port, as core 0 sees it (WEBTV_LPT_BRIDGE.md §4.1, §10.6, §10.7). The port
// engine (port.c) answers the box's edges on core 1 (in the emulator: pico/sim). It runs in one
// mode at a time:
//
//   PORT_IDLE    status lines idle, nothing answered
//   PORT_DONGLE  the loopback dongle (status follows D0-D3) and the boot ROM diagnostics' serial
//                console: 19200 8N1, box -> bridge on nSelectIn, bridge -> box on D7
//   PORT_PARMOD   PARMOD (§4.1): modem, console and command channels. The bridge's own loader
//                and stage 0 speak it too, with their protocol on the modem channel. A session
//                beginning raises PORT_EV_PARMOD. The entry signature again inside a session (an
//                OS that started over without our seeing the reset) raises PORT_EV_REENTRY.
//
// With the probe watch on, the boot ROM's dongle probe after a reset (the four control lines
// high, the box driving 0xF1, 0xF2, 0xF4 or 0xF8) switches the engine to PORT_DONGLE at once,
// in time for the ROM's STAT read, and raises PORT_EV_PROBE; the watch then turns itself off.
// PARMOD never drives the bus with all four lines high, so the watch can be on whenever the
// box isn't being reset on purpose.
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

typedef enum { PORT_IDLE, PORT_DONGLE, PORT_PARMOD } port_mode_t;

void port_set_mode(port_mode_t m);
port_mode_t port_mode(void);
void port_watch_probe(bool on);

enum { PORT_EV_PROBE = 1, PORT_EV_PARMOD = 2, PORT_EV_REENTRY = 4 };
uint32_t port_take_events(uint32_t mask);   // those of `mask` that happened, cleared

// PORT_DONGLE: the diagnostics console. Writes are sent a char at a time, each once neither
// line has moved for 1.5 ms (the box only listens between its own chars).
size_t port_diag_read(uint8_t *d, size_t max);
void port_diag_write(const uint8_t *d, size_t n);
bool port_diag_tx_idle(void);              // nothing queued or on the wire

// PORT_PARMOD
enum { PM_MODEM_RESET = 0x01, PM_DTR_ON = 0x02, PM_DTR_OFF = 0x03, PM_LEGACY_ECP = 0x04, PM_EXIT = 0x05, PM_REBOOT = 0x06, PM_PEEK = 0x07 };
size_t port_pm_modem_read(uint8_t *d, size_t max);          // what the box sent its modem
size_t port_pm_modem_write(const uint8_t *d, size_t n);     // to the box's modem channel
size_t port_pm_modem_free(void);
size_t port_pm_modem_queued(void);                          // written, not yet taken by the box
size_t port_pm_console_read(uint8_t *d, size_t max);        // the box's console output
size_t port_pm_console_write(const uint8_t *d, size_t n);   // typed for the box
void port_pm_carrier(bool on);
int port_pm_command(uint8_t *arg);                          // next command from the box, -1 none
bool port_pm_session(void);
// Peeks: box memory read by the patched OS for the bridge (its pm_ch_peek), in a session. Each
// request is 16 words from a RAM address (kseg0 or kseg1; within 8 MB), or one word of the
// SOLO's registers (0xa400xxxx, one_word). Up to 16 requests queue; the words come back in
// order, big-endian, through port_pm_peek_read (4 KB held: keep no more than 64 words asked
// for and unread). A new session drops queued requests.
bool port_pm_peek(uint32_t addr, bool one_word);
size_t port_pm_peek_read(uint8_t *d, size_t max);
void port_pm_peek_flush(void);                              // drop words left from before
// A key for the OS (a patch with keys: the patch table's flag): the first word of an entry in its
// IR input ring, big-endian: 0x94 or 0xd4 (IR keyboard make / break), the scancode, 0, 0; or
// 0, 0, a remote's maker, its code. Queued with the peeks; false when the queue is full.
bool port_pm_key(uint32_t entry);

// port.c, core 1 (the Pico; the host's emulator plays the engine): one pass of the engine, and
// back to the current mode after the bring-up tests paused it
void port_step(void);
void port_resume(void);
typedef struct {
    uint32_t probes, diag_rx, diag_tx, diag_framing, pm_xfers, pm_refused, pm_attentions,
             pm_sessions, pm_reentries, pm_keys, pm_con_dropped, pm_cmd_dropped, pm_glitches, pm_peeks, pm_peek_dropped;
} port_stats_t;
void port_get_stats(port_stats_t *s);

// portglue.c: core 0's side, shared by the Pico and the host
void port_service_start(void);                // at boot: the start-up (sb/), or straight to PARMOD
void port_service(uint32_t now_ms);           // each time round the main loop
extern void (*port_console_sink)(const uint8_t *d, size_t n);   // the box's PARMOD console output
