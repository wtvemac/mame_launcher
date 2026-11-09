// From the boot ROM's diagnostics to the loader (src/stockbox/session.js):
//
//   debug boot ROM    `18` (EnterDebugger) -> `app>`; `sl` black into the video's blank colour
//                     (the diagnostics leave the TV magenta); `dr a8` (why it reset); stage 0 typed in
//                     with `sl`, `epc`, `g`; at the end of the `g` line the port goes to PARMOD
//                     and the loader goes over it; stage 0 answers LOADED
//   production ROM    its banner says "non-debug" (no monitor): `19` boots the tier-3 image on
//                     the disk, which is the loader; HELLO until it answers, or after 10 s give
//                     up (no tier-3 image: the box boots its own OS, unpatched)
#include <stdio.h>
#include <string.h>
#include <stddef.h>
#include "sb.h"
#include "../port.h"
#include "../log.h"
#include "../bridge_config.h"
#include "../gen/sb_assets.h"
#include "../settings.h"

#define WORDS_PER_LINE 32
#define TIER3_MS 10000

void (*sb_on_phase)(sb_session_phase_t p);

static struct {
    sb_session_phase_t phase;
    char line[512];
    size_t len;
    int cause;
    bool debug_rom;
    bool via_tier3;             // this start went through test 19 (the tier-3 area)
    bool rom_seen;              // this session's banner has started (rom_banner)
    uint32_t next_line;         // stage 0's `sl` lines sent
    uint32_t hello_at, deadline;
} Z;

static void set_phase(sb_session_phase_t p) {
    static const char *const names[] = { "off", "mirroring", "debugger", "black", "cause", "upload", "epc", "go", "stage0",
                                         "tier3-boot", "tier3", "loader", "no-tier3" };
    Z.phase = p;
    LOGI("[stockbox] %s\n", names[p]);
    if (sb_on_phase) sb_on_phase(p);
}

// the boot ROM's own banner in its diagnostics ("### Boot ROM version  : {2046}", "built on", "built by"),
// kept across sessions for the web page: the box last seen
static sb_rom_info_t rom;
const sb_rom_info_t *sb_session_rom(void) { return &rom; }

static void rom_banner(const char *l) {
    static const struct { const char *key; size_t off, size; } F[] = {
        { "### Boot ROM version", offsetof(sb_rom_info_t, version), sizeof rom.version },
        { "### Boot ROM built on", offsetof(sb_rom_info_t, built_on), sizeof rom.built_on },
        { "### Boot ROM built by", offsetof(sb_rom_info_t, built_by), sizeof rom.built_by },
    };
    for (size_t i = 0; i < sizeof F / sizeof F[0]; i++) {
        if (strncmp(l, F[i].key, strlen(F[i].key))) continue;
        const char *a = strchr(l, '{'), *b = a ? strchr(a, '}') : NULL;
        if (!b) return;
        if (!Z.rom_seen) { memset(&rom, 0, sizeof rom); Z.rom_seen = true; }   // this box's banner, from its first line
        snprintf((char *)&rom + F[i].off, F[i].size, "%.*s", (int)(b - a - 1), a + 1);
        for (char *c = rom.built_on; *c; c++) if (*c == ',') *c = ' ';   // "12/19/97,00:01:43"
        rom.seen = true;
        rom.debug = Z.debug_rom;                // "non-debug" may come before or after (diag_char)
    }
}

sb_session_phase_t sb_session_phase(void) { return Z.phase; }
bool sb_session_debug_rom(void) { return Z.debug_rom; }
bool sb_session_via_tier3(void) { return Z.via_tier3; }

static void type(const char *s) { port_diag_write((const uint8_t *)s, strlen(s)); }

static uint32_t be32(const uint8_t *p) { return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3]; }

static uint32_t stage0_lines(void) { return (sizeof sb_stage0 / 4 + WORDS_PER_LINE - 1) / WORDS_PER_LINE; }

static void upload(void) {
    if (Z.next_line < stage0_lines()) {
        char buf[16 + WORDS_PER_LINE * 9 + 2];
        uint32_t w0 = Z.next_line * WORDS_PER_LINE, nw = sizeof sb_stage0 / 4 - w0;
        if (nw > WORDS_PER_LINE) nw = WORDS_PER_LINE;
        int n = snprintf(buf, sizeof buf, "sl %x", (unsigned)(SB_STAGE0_BASE + w0 * 4));
        for (uint32_t i = 0; i < nw; i++) n += snprintf(buf + n, sizeof buf - (size_t)n, " %x", (unsigned)be32(sb_stage0 + (w0 + i) * 4));
        snprintf(buf + n, sizeof buf - (size_t)n, "\r");
        Z.next_line++;
        type(buf);
        return;
    }
    char buf[32];
    snprintf(buf, sizeof buf, "epc %x\r", (unsigned)SB_STAGE0_BASE);
    type(buf);
    set_phase(SS_EPC);
}

// the loader image for stage 0: length, the words, their sum
static void send_image(void) {
    uint8_t h[4] = { (uint8_t)(sizeof sb_loader >> 24), (uint8_t)(sizeof sb_loader >> 16), (uint8_t)(sizeof sb_loader >> 8), (uint8_t)sizeof sb_loader };
    uint32_t sum = 0;
    for (size_t i = 0; i < sizeof sb_loader; i += 4) sum += be32(sb_loader + i);
    uint8_t t[4] = { (uint8_t)(sum >> 24), (uint8_t)(sum >> 16), (uint8_t)(sum >> 8), (uint8_t)sum };
    sb_send(h, 4);
    sb_send(sb_loader, sizeof sb_loader);
    sb_send(t, 4);
}

static void on_message(const sb_msg_t *m) {
    if (m->type == SBM_IR) {
        extern uint32_t bridge_now_ms(void);
        for (size_t i = 0; i + 1 < m->len; i += 2) sb_ir_entry((uint16_t)((m->data[i] << 8) | m->data[i + 1]), bridge_now_ms());
        return;
    }
    if (m->type == SBM_LOADED) { set_phase(SS_LOADER); return; }
    if (m->type == SBM_HELLO && Z.phase == SS_TIER3) { set_phase(SS_LOADER); return; }
    if (m->type == SBM_BADSUM) { LOGI("[stockbox] stage 0: bad sum, sending the loader again\n"); send_image(); return; }
    if (sb_mem_message(m)) return;
    if (m->type == SBM_JUNK) LOGD("[stockbox] junk 0x%02x from the loader\n", m->byte);
}

// the port is the loader's now: PARMOD, our protocol on its modem channel
static void to_link(void) {
    uint8_t junk[64];
    port_set_mode(PORT_PARMOD);
    while (port_pm_modem_read(junk, sizeof junk)) { }   // anything from before
    sb_parser_reset();
    sb_mem_reset();
}

static void line_done(void) {
    if (Z.len) LOGD("[diag] %s\n", Z.line);
    rom_banner(Z.line);
    if (Z.phase == SS_CAUSE) {
        unsigned v;
        if (sscanf(Z.line, "0xa40000a8: 0x%x", &v) == 1) Z.cause = (int)v;
    }
    Z.len = 0; Z.line[0] = 0;
    if (Z.phase == SS_GO) {      // the `g` line's CR is out: the loader's turn
        to_link();
        set_phase(SS_STAGE0);
        send_image();
    }
}

static bool at(const char *s) {
    size_t n = strlen(s);
    return Z.len >= n && !memcmp(Z.line + Z.len - n, s, n);
}

static void diag_char(uint8_t c) {
    if (c == 10) { line_done(); return; }
    if (c == 13) return;
    if (Z.len + 1 < sizeof Z.line) { Z.line[Z.len++] = (char)c; Z.line[Z.len] = 0; }
    if (at("non-debug")) Z.debug_rom = rom.debug = false;
    switch (Z.phase) {
    case SS_MIRRORING:
        if (!at("Parallel in>")) break;
        // a debug ROM's monitor, unless the settings say to start as a production ROM must
        if (Z.debug_rom && !(settings.box_off & BOX_OFF_TEST18)) { Z.via_tier3 = false; type("18\r"); set_phase(SS_DEBUGGER); }
        else { Z.via_tier3 = true; type("19\r"); set_phase(SS_TIER3_BOOT); }
        break;
    case SS_TIER3_BOOT:
        if (at("Now booting to Tier 3")) {
            extern uint32_t bridge_now_ms(void);
            to_link();
            set_phase(SS_TIER3);
            Z.hello_at = bridge_now_ms() + 500;
            Z.deadline = bridge_now_ms() + TIER3_MS;
        }
        break;
    case SS_DEBUGGER:
        if (at("app>")) { type("sl a4009088 108080\r"); set_phase(SS_BLACK); }   // POT blank colour: black
        break;
    case SS_BLACK:
        if (at("app>")) { type("dr a8\r"); set_phase(SS_CAUSE); }
        break;
    case SS_CAUSE:
        if (at("app>")) {
            LOGI("[stockbox] reset cause %d (%s)\n", Z.cause, Z.cause & 4 ? "software" : Z.cause & 2 ? "watchdog" : "power applied");
            Z.next_line = 0;
            set_phase(SS_UPLOAD);
            upload();
        }
        break;
    case SS_UPLOAD:
        if (at("app>")) upload();
        break;
    case SS_EPC:
        if (at("app>")) { type("g\r"); set_phase(SS_GO); }
        break;
    default: break;
    }
}

void sb_session_start(void) {
    memset(&Z, 0, sizeof Z);
    Z.debug_rom = true;
    Z.cause = -1;
    sb_on_message = on_message;
    sb_parser_reset();
    sb_mem_reset();
    port_set_mode(PORT_DONGLE);
    set_phase(SS_MIRRORING);
}

void sb_session_end(void) { if (Z.phase != SS_OFF) set_phase(SS_OFF); }

void sb_session_poll(uint32_t now) {
    uint8_t buf[256];
    size_t n;
    if ((Z.phase >= SS_MIRRORING && Z.phase <= SS_GO) || Z.phase == SS_TIER3_BOOT) {   // the diagnostics console
        while ((n = port_diag_read(buf, sizeof buf)) > 0) for (size_t i = 0; i < n; i++) diag_char(buf[i]);
        return;
    }
    if (Z.phase == SS_STAGE0 || Z.phase == SS_TIER3 || Z.phase == SS_LOADER) {
        sb_send_flush();
        while ((n = port_pm_modem_read(buf, sizeof buf)) > 0) for (size_t i = 0; i < n; i++) sb_parser_feed(buf[i]);
        sb_mem_poll(now);
    }
    if (Z.phase == SS_TIER3) {
        if ((int32_t)(now - Z.deadline) >= 0) { port_set_mode(PORT_IDLE); set_phase(SS_NO_TIER3); return; }
        if ((int32_t)(now - Z.hello_at) >= 0) {
            Z.hello_at = now + 200;
            if (!sb_send_pending() && !port_pm_modem_queued()) sb_hello();   // one at a time
        }
    }
}
