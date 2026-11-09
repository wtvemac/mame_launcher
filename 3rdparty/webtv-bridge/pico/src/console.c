// Line-oriented setup console (USB CDC / UART on the Pico, stdin on the host).
//
//   status                 the port, WiFi, modem, PPP, connections and the start-up
//   scan                   the WiFi networks in range
//   wifi <ssid> [password] set the WiFi network ("quotes" for spaces)
//   service <host>|none    WebTV service for ports 1600-1699
//   dns <a.b.c.d>|auto     DNS server; auto = the router's (DHCP), or 8.8.8.8 when it gives none
//   country <XX>           WiFi country code
//   log <0-3>              errors, info, debug, trace
//   save                   store the settings in flash
//   reboot                 restart the bridge
//   pins                   board revision and the level of every parallel-port line
//   sniff [wait] [record]  wait up to wait s (60) for a change on the cable, then record every change
//                          for record s (20) and print them: a logic analyzer from the Pico's side
//   walk                   pulse each output line in turn, for a logic analyzer (link.h)
//   loop [1-5]             loopback test with jumpers on J2 (link.h); no number lists the jumpers
//   startup on|off         take a stock box through its start-up at every reset (§10.7); off: only
//                          an OS patched on its disk gets online (save + reboot to apply)
//   type <text>            type on the box's PARMOD console channel ("quotes" for spaces; CR added)
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <stdarg.h>
#include "console.h"
#include "settings.h"
#include "bridge.h"
#include "link.h"
#include "port.h"
#include "sb/sb.h"
#include "gw.h"
#include "net.h"
#include "log.h"

static char line[160];
static int len;
static bool scanning;

static void print(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
}

// split into words; "double quotes" group
static int words(char *s, char **w, int max) {
    int n = 0;
    while (*s && n < max) {
        while (*s == ' ') s++;
        if (!*s) break;
        if (*s == '"') {
            w[n++] = ++s;
            while (*s && *s != '"') s++;
        } else {
            w[n++] = s;
            while (*s && *s != ' ') s++;
        }
        if (*s) *s++ = 0;
    }
    return n;
}

static void show_settings(void) {
    print("wifi \"%s\" (%s), country %s, service %s, dns %s, log %d, start-up %s, remotes WebTV%s%s, autopower %s, entry %s\n", settings.ssid,
          settings.password[0] ? "password set" : "no password",
          settings.country, settings.service[0] ? settings.service : "(where the box asks)", settings.dns[0] ? settings.dns : "auto",
          settings.log_level, settings.no_startup ? "off" : "on", settings.box_off & BOX_OFF_SONY ? "" : " sony",
          settings.box_off & BOX_OFF_PHILIPS ? "" : " philips", settings.box_off & BOX_OFF_AUTOPOWER ? "off" : "on",
          settings.box_off & BOX_OFF_TEST18 ? "tier3" : "auto");
}

static void command(char *s) {
    char *w[4];
    int n = words(s, w, 4);
    if (!n) return;
    if (!strcmp(w[0], "help") || !strcmp(w[0], "?")) {
        print("status | scan | wifi <ssid> [password] | service <host>|none | dns <a.b.c.d>|auto | country <XX> | log <0-3> | startup on|off | remotes [sony] [philips] | autopower on|off | entry auto|tier3 | type <text> | save | reboot | pins | sniff [wait_s] [record_s] | walk | loop [1-5]\n");
    } else if (!strcmp(w[0], "status")) {
        show_settings();
        console_status(print);
    } else if (!strcmp(w[0], "scan")) {
        net_scan_start();
        scanning = true;
        print("scanning...\n");
    } else if (!strcmp(w[0], "startup") && n >= 2) {
        settings.no_startup = strcmp(w[1], "on") != 0;
        show_settings();
        print("(save + reboot to apply)\n");
    } else if (!strcmp(w[0], "remotes")) {
        // the remotes the OS decodes besides the WebTV remote and keyboard (always on)
        settings.box_off |= BOX_OFF_SONY | BOX_OFF_PHILIPS;
        for (int i = 1; i < n; i++) {
            if (!strcmp(w[i], "sony")) settings.box_off &= (uint8_t)~BOX_OFF_SONY;
            else if (!strcmp(w[i], "philips")) settings.box_off &= (uint8_t)~BOX_OFF_PHILIPS;
            else if (strcmp(w[i], "none")) { print("remotes [sony] [philips], or remotes none\n"); return; }
        }
        show_settings();
        print("(\"save\" to keep it; the OS gets it when it next starts)\n");
    } else if (!strcmp(w[0], "entry") && n >= 2) {
        // how the Bridge gets in on a debug boot ROM: its monitor (test 18), or test 19 and the
        // disk's tier-3 area as on a production ROM (a production ROM always takes test 19)
        if (!strcmp(w[1], "tier3")) settings.box_off |= BOX_OFF_TEST18;
        else if (!strcmp(w[1], "auto")) settings.box_off &= (uint8_t)~BOX_OFF_TEST18;
        else { print("entry auto|tier3\n"); return; }
        show_settings();
        print("(\"save\" to keep it; it applies at the box's next reset)\n");
    } else if (!strcmp(w[0], "autopower") && n >= 2) {
        if (!strcmp(w[1], "on")) settings.box_off &= (uint8_t)~BOX_OFF_AUTOPOWER; else settings.box_off |= BOX_OFF_AUTOPOWER;
        show_settings();
        print("(\"save\" to keep it; the OS gets it when it next starts)\n");
    } else if (!strcmp(w[0], "type") && n >= 2) {
        char t[200];
        snprintf(t, sizeof t, "%s\r", w[1]);   // "quotes" for spaces
        print(port_pm_session() ? "typed\n" : "no PARMOD session\n");
        port_pm_console_write((const uint8_t *)t, strlen(t));
    } else if (!strcmp(w[0], "pins")) {
        link_print_pins(print);
    } else if (!strcmp(w[0], "sniff")) {
        link_sniff(n >= 2 ? atoi(w[1]) : 60, n >= 3 ? atoi(w[2]) : 20, print);
    } else if (!strcmp(w[0], "walk")) {
        print(link_walk() ? "walk done\n" : "no pins on this build\n");
    } else if (!strcmp(w[0], "loop")) {
        link_loop(n >= 2 ? atoi(w[1]) : 0, print);
    } else if (!strcmp(w[0], "wifi") && n >= 2) {
        snprintf(settings.ssid, sizeof settings.ssid, "%s", w[1]);
        snprintf(settings.password, sizeof settings.password, "%s", n >= 3 ? w[2] : "");
        print("WiFi set to \"%s\"; reconnecting (\"save\" to keep it)\n", settings.ssid);
        net_reconfigure();
    } else if (!strcmp(w[0], "service") && n >= 2) {
        snprintf(settings.service, sizeof settings.service, "%s", strcmp(w[1], "none") ? w[1] : "");
        gw_set_service(settings.service);
        show_settings();
    } else if (!strcmp(w[0], "dns") && n >= 2) {
        unsigned a, b, c, d;
        char extra;
        if (!strcmp(w[1], "auto")) settings.dns[0] = 0;
        else if (sscanf(w[1], "%u.%u.%u.%u%c", &a, &b, &c, &d, &extra) == 4 && a < 256 && b < 256 && c < 256 && d < 256)
            snprintf(settings.dns, sizeof settings.dns, "%u.%u.%u.%u", a, b, c, d);
        else { print("dns <a.b.c.d> or dns auto\n"); return; }
        show_settings();   // net.c picks it up on its next pass; "save" keeps it
    } else if (!strcmp(w[0], "country") && n >= 2 && strlen(w[1]) == 2) {
        snprintf(settings.country, sizeof settings.country, "%s", w[1]);
        print("country %s (takes effect after save + reboot)\n", settings.country);
    } else if (!strcmp(w[0], "log") && n >= 2) {
        settings.log_level = (uint8_t)atoi(w[1]);
        log_level = settings.log_level;
        show_settings();
    } else if (!strcmp(w[0], "save")) {
        print(settings_save() ? "saved\n" : "save failed\n");
    } else if (!strcmp(w[0], "reboot")) {
        platform_reboot();
    } else {
        print("? (help for commands)\n");
    }
}

// the bridge's state, for `status` and the web page (web.c)
void console_status(void (*pr)(const char *fmt, ...)) {
    port_stats_t st;
    port_get_stats(&st);
    static const char *const modes[] = { "idle", "the dongle (diagnostics)", "PARMOD" };
    extern uint32_t bridge_now_ms(void);
    uint32_t up = bridge_now_ms() / 1000;
    pr("up %lu:%02lu:%02lu\n", (unsigned long)(up / 3600), (unsigned long)(up / 60 % 60), (unsigned long)(up % 60));
    pr("port: %s%s; PARMOD %lu sessions (%lu entered again), %lu keys, %lu transactions, %lu refused, %lu attentions, %lu strobe glitches; box resets seen %lu; "
       "diagnostics %lu chars in, %lu out\n", modes[port_mode()], port_pm_session() ? ", in a session" : "",
       (unsigned long)st.pm_sessions, (unsigned long)st.pm_reentries, (unsigned long)st.pm_keys, (unsigned long)st.pm_xfers, (unsigned long)st.pm_refused,
       (unsigned long)st.pm_attentions, (unsigned long)st.pm_glitches, (unsigned long)st.probes, (unsigned long)st.diag_rx, (unsigned long)st.diag_tx);
    bridge_status(pr);
    if (!settings.no_startup) sb_startup_status(pr);
}

// once round the main loop: a scan's results, when they come
void console_poll(void) {
    if (!scanning) return;
    net_ap_t aps[24];
    int n = net_scan_results(aps, 24);
    if (n < 0) return;
    scanning = false;
    print("\n%d network%s:\n", n, n == 1 ? "" : "s");
    for (int i = 0; i < n; i++) print("  %-32s %4d dBm  %s\n", aps[i].ssid, aps[i].rssi, aps[i].secure ? "secured" : "open");
    print("> ");
}

void console_input(int c) {
    if (c == '\r' || c == '\n') {
        if (len) { print("\n"); line[len] = 0; command(line); len = 0; }
        print("> ");
        return;
    }
    if ((c == 8 || c == 127) && len) { len--; print("\b \b"); return; }
    if (c >= 32 && c < 127 && len < (int)sizeof line - 1) { line[len++] = (char)c; putchar(c); }
}
