// What core 0 does with the port each time round its loop, on the Pico and in the emulator alike:
// the modem (modem.c -> ppp.c -> gw.c) on PARMOD's modem channel, its commands and console, and
// the start-up's turn (sb/). The modem channel is the loader's while the start-up runs it
// (sb_startup_owns_link).
#include <string.h>
#include "port.h"
#include "bridge.h"
#include "settings.h"
#include "sb/sb.h"
#include "log.h"

// the box's console channel, line by line to the log; also to `port_console_sink` (a TCP
// console, when there is one)
void (*port_console_sink)(const uint8_t *d, size_t n);
static char con[160];
static size_t con_len;

static void console_bytes(const uint8_t *d, size_t n) {
    if (port_console_sink) port_console_sink(d, n);
    for (size_t i = 0; i < n; i++) {
        uint8_t c = d[i];
        if (c == '\n') { con[con_len] = 0; LOGI("[pmcon] %s\n", con); con_len = 0; }
        else if (c != '\r' && con_len < sizeof con - 1) con[con_len++] = (char)c;
    }
}

// PARMOD's commands, read every pass whatever the mode: the OS sends REBOOT just before it
// resets, and by the time core 0 looks, core 1 may already have seen the boot ROM's probe
// and left PARMOD. The start-up must hear REBOOT before it handles that probe.
static void pm_commands(void) {
    uint8_t arg;
    int c;
    while ((c = port_pm_command(&arg)) >= 0) {
        switch (c) {
        case PM_MODEM_RESET: LOGI("[parmod] modem reset\n"); bridge_link_reset(); break;
        case PM_DTR_ON: bridge_set_dtr(true); break;
        case PM_DTR_OFF: bridge_set_dtr(false); break;
        case PM_REBOOT: LOGI("[parmod] the OS is rebooting\n"); sb_startup_reboot_notice(); break;
        case PM_LEGACY_ECP: LOGI("[parmod] LEGACY_ECP 0x%02x: no 1997 ECP transfers on this bridge yet\n", arg); break;
        case PM_EXIT: LOGI("[parmod] the box left its session\n"); break;
        }
    }
}

static void parmod(void) {
    uint8_t buf[512];
    size_t n, room;
    while ((n = port_pm_modem_read(buf, sizeof buf)) > 0) bridge_from_box(buf, n);
    while ((room = port_pm_modem_free()) > 0 && bridge_to_box_pending()) {
        n = bridge_to_box(buf, room < sizeof buf ? room : sizeof buf);
        port_pm_modem_write(buf, n);
    }
    port_pm_carrier(bridge_carrier());
    while ((n = port_pm_console_read(buf, sizeof buf)) > 0) console_bytes(buf, n);
}

void port_service(uint32_t now) {
    uint32_t ev = port_take_events(PORT_EV_PARMOD | PORT_EV_REENTRY);
    if (ev & PORT_EV_REENTRY) {
        // The box entered again inside a session: its OS started over without our seeing the
        // reset (out of its debugger, or a reset the probe watch missed). What the modem, PPP and
        // the connections held belongs to the OS that's gone: hang up, as its MODEM_RESET would,
        // so the new one finds the modem in command mode for its download and dial. (The loader
        // and stage 0 own the link while the start-up runs them; they keep no modem state.)
        if (settings.no_startup || !sb_startup_owns_link()) {
            LOGI("[port] the box entered PARMOD again: its OS started over; the modem hangs up\n");
            bridge_link_reset();
        }
    } else if (ev & PORT_EV_PARMOD) LOGI("[port] a PARMOD session began\n");
    pm_commands();
    if (!settings.no_startup) sb_startup_poll(now);
    switch (port_mode()) {
    case PORT_PARMOD: if (settings.no_startup || !sb_startup_owns_link()) parmod(); break;
    default: break;
    }
}

// at boot: the start-up takes the port (the dongle), or PARMOD waits for a patched OS
void port_service_start(void) {
    if (settings.no_startup) port_set_mode(PORT_PARMOD);
    else sb_startup_start();
}
