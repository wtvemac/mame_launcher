// Hayes/Rockwell modem as the WebTV OS expects it (port of the emulator's
// src/machine/modem.js). Command mode with echo and result codes, the AT** firmware download
// that TellyScript performs (acknowledged and discarded), dialing, +++ escape and DTR
// hang-up. "Connecting" starts the PPP server (ppp.c); in data mode bytes go straight to it.
#include <string.h>
#include <stdio.h>
#include <ctype.h>
#include "bridge.h"
#include "bridge_config.h"
#include "ppp.h"
#include "gw.h"
#include "net.h"
#include "ring.h"
#include "log.h"

#define CR 13
#define LF 10

static uint8_t out_storage[MODEM_OUT_SIZE];
static ring_t outq = RING_INIT(out_storage);

static struct {
    char cmd[260];
    int cmd_len;
    bool echo, verbose, quiet;
    bool online;      // data mode
    bool connected;   // carrier up (data mode or escaped to command mode)
    bool dialing;
    bool dtr;
    int download;     // 0 none, 1 loader, 2 firmware
    uint32_t download_count;
    char dl_line[600];
    int dl_len;
    bool dl_data;           // phase 2: the firmware's first byte has come
    uint32_t dl_idle_at;    // phase 2: done if nothing more comes by then
    uint32_t dl_end_at;     // an S-record image ended: answer once the box has gone quiet (DL_QUIET_MS)
    int plus_count;
    uint32_t last_tx_ms;
    uint32_t esc_at;          // when to check the +++ guard time (0 = not pending)
    uint32_t dial_at;         // when the pending dial step fires (0 = none)
    int dial_step;            // 1 = "OK" for ATDT...; (stay), 2 = connect, 3 = NO DIALTONE
    uint8_t sreg[256];
    char dialed[40];
} m;

// ---------------------------------------------------------------- output towards the box

size_t modem_out_free(void) { return ring_free(&outq); }
size_t modem_out_used(void) { return ring_used(&outq); }
size_t modem_out_read(uint8_t *d, size_t max) { return ring_read(&outq, d, max); }

#ifdef LINK_HOST_SIM
// The simulator logs the modem's output while it isn't online (log level 2: --pico-log 2).
static void sim_trace(const char *dir, const uint8_t *d, size_t n) {
    char t[160];
    size_t k = 0;
    for (size_t i = 0; i < n && k + 6 < sizeof t; i++) {
        uint8_t c = d[i];
        if (c >= 32 && c < 127) t[k++] = (char)c;
        else k += (size_t)snprintf(t + k, sizeof t - k, "<%02x>", c);
    }
    t[k] = 0;
    LOGD("[modem %s] %s\n", dir, t);
}
#endif

bool modem_out_put(const uint8_t *d, size_t n) {
#ifdef LINK_HOST_SIM
    if (!m.online) sim_trace("to box", d, n);
#endif
    if (ring_free(&outq) < n) return false;
    ring_write(&outq, d, n);
    return true;
}

static void put_str(const char *s) { modem_out_put((const uint8_t *)s, strlen(s)); }

static void result(const char *text) {
    if (m.quiet) return;
    char buf[64];
    if (m.verbose) {
        snprintf(buf, sizeof buf, "\r\n%s\r\n", text);
    } else {
        // Rockwell numeric result codes (CONNECT <DTE rate> with X4)
        static const struct { const char *t; int c; } codes[] = {
            {"OK", 0}, {"CONNECT", 1}, {"RING", 2}, {"NO CARRIER", 3}, {"ERROR", 4}, {"NO DIALTONE", 6},
            {"BUSY", 7}, {"NO ANSWER", 8}, {"CONNECT 9600", 12}, {"CONNECT 19200", 16}, {"CONNECT 38400", 17},
            {"CONNECT 57600", 18}, {"CONNECT 115200", 19},
        };
        int code = 1;
        for (size_t i = 0; i < sizeof codes / sizeof codes[0]; i++) if (!strcmp(codes[i].t, text)) code = codes[i].c;
        snprintf(buf, sizeof buf, "%d\r", code);
    }
    put_str(buf);
}

// ---------------------------------------------------------------- state

void modem_reset(void) {
    if (m.connected || m.dialing) { ppp_stop(); gw_reset(); }
    bool dtr = m.dtr;
    memset(&m, 0, sizeof m);
    m.dtr = dtr;
    m.echo = true; m.verbose = true;
    m.sreg[2] = 43; m.sreg[3] = 13; m.sreg[4] = 10; m.sreg[5] = 8; m.sreg[12] = 50;
    ring_flush(&outq);
}

void modem_init(void) { memset(&m, 0, sizeof m); modem_reset(); }

bool modem_online(void) { return m.online; }
bool modem_dcd(void) { return m.connected; }

void modem_hangup(const char *why) {
    bool was = m.online || m.connected;
    if (why) LOGI("[modem] hangup (%s)\n", why);
    m.online = m.connected = m.dialing = false;
    m.dial_at = 0; m.dialed[0] = 0;
    ppp_stop();
    gw_reset();
    if (was && why) result("NO CARRIER");
}

void modem_set_dtr(bool on) {
    if (m.dtr && !on && (m.online || m.connected || m.dialing)) modem_hangup("DTR drop");
    m.dtr = on;
}

// PPP (via ppp.c) calls this when the link goes down on its side
void modem_ppp_down(const char *why) {
    if (m.connected) modem_hangup(why);
}

// ---------------------------------------------------------------- dialing

static void dial(const char *number, bool stay) {
    LOGI("[modem] dialing %s%s\n", number[0] ? number : "(none)", stay ? " (stay in command mode)" : "");
    uint32_t now = bridge_now_ms();
    if (!net_up()) { m.dial_step = 3; m.dial_at = now + 2000; return; }   // no WiFi: no dial tone
    if (stay) {
        snprintf(m.dialed, sizeof m.dialed, "%s", number);
        m.dial_step = 1; m.dial_at = now + 1000;
        return;
    }
    if (!number[0] && m.dialed[0]) number = m.dialed;
    LOGI("[modem] answering call to %s\n", number[0] ? number : "(default)");
    m.dialed[0] = 0;
    m.dialing = true;
    m.dial_step = 2; m.dial_at = now + 1500;   // a short emulated handshake
}

static void dial_fire(void) {
    int step = m.dial_step;
    m.dial_at = 0; m.dial_step = 0;
    if (step == 1) { result("OK"); return; }
    if (step == 3) { result("NO DIALTONE"); return; }
    if (!m.dialing) return;
    m.dialing = false;
    if (!net_up()) { result("NO CARRIER"); return; }
    m.connected = true;
    result("CONNECT 57600");
    m.online = true;
    ppp_start();
}

// ---------------------------------------------------------------- firmware download (AT**)

// TellyScript's Rockwell downloader: AT** -> OK; the box sends an S-record loader (or, on warm
// boots, a tiny "NULL loader" and nothing else); the loader prints its banner and "Download
// Modem Firmware .."; the box sends the firmware and waits for "Modem Firmware Successfully
// Loaded". An S-record image ends with an S7/S8/S9 record. The firmware may also come as raw
// binary (16423's "Binary Ram Loader" on a cold boot, 128 KB), which ends when the box goes
// quiet: 200 ms after its last byte.
static void download_done(void) {
    LOGI("[modem] download phase %d: %lu bytes\n", m.download, (unsigned long)m.download_count);
    bool small = m.download_count < 512;
    m.download_count = 0;
    m.dl_idle_at = 0;
    m.dl_end_at = 0;
    if (m.download == 1 && small) { m.download = 0; return; }   // NULL loader
    if (m.download == 1) {
        m.download = 2; m.dl_data = false;
        put_str("\r\nBinary Ram Loader - V0.08\r\nDownload Modem Firmware ..\r\n");
        return;
    }
    m.download = 0;
    put_str("Modem Firmware Successfully Loaded\r\n");
}

// The box's sender (SI_modem_blastdata) checks after every byte it writes whether one has come
// back, and anything but '.' fails the download (TellyScript returns 0x11, and the OS reboots).
// So the modem stays silent until the box has sent its last byte: the S-record image ends with
// CR LF, and an answer on the CR could beat the LF.
#define DL_QUIET_MS 50

static void download_byte(uint8_t b) {
    if (m.dl_end_at) { m.dl_end_at = bridge_now_ms() + DL_QUIET_MS; if (!m.dl_end_at) m.dl_end_at = 1; }
    m.download_count++;
    if (m.download == 2) {
        if (!m.dl_data && (b == CR || b == LF)) { m.download_count--; return; }   // the loader's last line end
        m.dl_data = true;
        m.dl_idle_at = bridge_now_ms() + 200;
        if (!m.dl_idle_at) m.dl_idle_at = 1;
    }
    if (b == LF || b == CR) {
        bool term = m.dl_len >= 2 && m.dl_line[0] == 'S' && m.dl_line[1] >= '7' && m.dl_line[1] <= '9';
        m.dl_len = 0;
        if (term) { m.dl_end_at = bridge_now_ms() + DL_QUIET_MS; if (!m.dl_end_at) m.dl_end_at = 1; }
        return;
    }
    if (m.dl_len < (int)sizeof m.dl_line - 1) m.dl_line[m.dl_len++] = (char)b; else m.dl_len = 0;
}

// ---------------------------------------------------------------- AT commands

static int num(const char *s, int *i) {
    int n = 0;
    while (isdigit((unsigned char)s[*i])) n = n * 10 + (s[(*i)++] - '0');
    return n;
}

static void command(const char *s) {
    LOGI("[modem] AT%s\n", s);
    if (s[0] == '*' && s[1] == '*') {
        m.download = 1; m.download_count = 0; m.dl_len = 0;
        put_str("\r\nOK\r\n");
        return;
    }
    int i = 0;
    while (s[i]) {
        char c = s[i++];
        switch (c) {
        case ' ': break;
        case 'D': {
            // TellyScript dials "ATDT<number>;" (dial, stay in command mode -> OK), then sends a
            // bare "ATD" to go off-hook in originate mode and wait for the carrier.
            char number[40]; int n = 0;
            bool stay = strchr(s + i, ';') != NULL;
            for (; s[i]; i++) if ((isdigit((unsigned char)s[i]) || strchr("*#,WP", s[i])) && n < 39) number[n++] = s[i];
            number[n] = 0;
            dial(number, stay);
            return;
        }
        case 'A': result("NO CARRIER"); return;
        case 'H': num(s, &i); if (m.connected) modem_hangup(NULL); break;
        case 'O':
            num(s, &i);
            if (m.connected) { m.online = true; result("CONNECT"); return; }
            result("NO CARRIER"); return;
        case 'Z': num(s, &i); m.echo = true; m.verbose = true; m.quiet = false; break;
        case 'E': m.echo = num(s, &i) != 0; break;
        case 'V': m.verbose = num(s, &i) != 0; break;
        case 'Q': m.quiet = num(s, &i) != 0; break;
        case 'S': {
            int r = num(s, &i) & 255;
            if (s[i] == '=') { i++; m.sreg[r] = (uint8_t)num(s, &i); }
            else if (s[i] == '?') { i++; char b[16]; snprintf(b, sizeof b, "\r\n%03d\r\n", m.sreg[r]); put_str(b); }
            break;
        }
        case 'I': {
            int n = num(s, &i);
            put_str(n == 0 ? "\r\n56000\r\n" : "\r\nWebTV LPT bridge\r\n");
            break;
        }
        case '&': case '%': case '\\': case '+': case '-': case '#': case '*': case '$':
            // extended commands: skip letter + number (+ optional =value list)
            if (s[i]) i++;
            num(s, &i);
            if (s[i] == '=') { i++; while (s[i] && (isdigit((unsigned char)s[i]) || s[i] == ',')) i++; }
            break;
        default:
            // B, C, L, M, N, P, T, W, X, Y and friends: accept with an optional number
            num(s, &i);
        }
    }
    result("OK");
}

void modem_from_host(uint8_t b) {
    if (m.online) {
        m.last_tx_ms = bridge_now_ms();
        if (b == m.sreg[2]) { if (++m.plus_count == 3) m.esc_at = m.last_tx_ms + 1000; }
        else m.plus_count = 0;
        ppp_rx_byte(b);
        return;
    }
    if (m.download) { download_byte(b); return; }
    if (m.echo) modem_out_put(&b, 1);
    if (b == m.sreg[5]) { if (m.cmd_len) m.cmd_len--; return; }
    if (b == CR) {
        m.cmd[m.cmd_len] = 0;
        // upper-case, find the last "AT"
        for (int k = 0; k < m.cmd_len; k++) m.cmd[k] = (char)toupper((unsigned char)m.cmd[k]);
        char *at = NULL;
        for (char *p = m.cmd; (p = strstr(p, "AT")); p++) at = p;
        m.cmd_len = 0;
        if (at) {
            char line[260];
            snprintf(line, sizeof line, "%s", at + 2);
            // trim trailing spaces
            for (int k = (int)strlen(line) - 1; k >= 0 && line[k] == ' '; k--) line[k] = 0;
            command(line);
        }
        return;
    }
    if (b == LF) return;
    if (m.cmd_len >= (int)sizeof m.cmd - 1) { memmove(m.cmd, m.cmd + 1, (size_t)--m.cmd_len); }
    m.cmd[m.cmd_len++] = (char)b;
}

void modem_poll(uint32_t now) {
    if (m.download == 2 && m.dl_idle_at && (int32_t)(now - m.dl_idle_at) >= 0) download_done();
    if (m.download && m.dl_end_at && (int32_t)(now - m.dl_end_at) >= 0) download_done();
    if (m.dial_at && (int32_t)(now - m.dial_at) >= 0) dial_fire();
    if (m.esc_at && (int32_t)(now - m.esc_at) >= 0) {
        m.esc_at = 0;
        if (m.online && m.plus_count == 3 && now - m.last_tx_ms >= 900) { m.online = false; result("OK"); }
        m.plus_count = 0;
    }
}

void modem_status(void (*print)(const char *fmt, ...)) {
    print("modem: %s%s%s, dtr %s, %u bytes queued for the box\n",
          m.online ? "online" : "command mode", m.connected ? ", carrier" : "", m.dialing ? ", dialing" : "",
          m.dtr ? "on" : "off", (unsigned)ring_used(&outq));
}
