// The bridge's start-up with a stock box (WEBTV_LPT_BRIDGE.md §10.7; tested by tools/bridgesim.mjs):
// at every start, the OS on the disk is read into the box's RAM, patched there, and started by
// the boot ROM. The disk is only written by Install another OS.
//
//   armed      the dongle; a reset lands in the diagnostics and the session installs the loader
//   wifi       no WiFi saved: WiFi setup on the TV
//   loading    "Preparing your WebTV": the OS into RAM, checked
//   patching   the build's patch from the table, checked again
//   countdown  what was found and a menu (Start now, Save this OS, Install another OS,
//              Settings); starts by itself after COUNTDOWN_S unless a key was pressed
//   settings   WiFi network, WTVP server (settings.service), Remotes (Sony, Philips: settings.box_off),
//              powering on by itself; the OS gets the last two as Start writes them (sb_rb_options)
//   saving / installing
//   os         the patched OS runs from RAM. The boot ROM's dongle probe (any reset) brings the
//              bridge back to armed, from the loader's start on, except for PROBE_AFTER_MS after
//              boot_ram's own reset. After a REBOOT from the OS, the next start skips the countdown.
#include <stdio.h>
#include <string.h>
#include "sb.h"
#include "../port.h"
#include "../net.h"
#include "../settings.h"
#include "../log.h"
#include "../bridge.h"

#define COUNTDOWN_S 10
#define PROBE_AFTER_MS 2000
#define RELEASE_MS 50
#define RELEASE_WAIT_MS 5000   // longest to wait for the loader to take boot_ram's CALL
#define NO_TIER3_RETRY_MS 30000
#define MENU_Y 228
#define MENU_ROW 24
#define BAR_Y 200
#define BAR_H 16

static struct {
    sb_state_t state;
    sb_os_t os;
    char patch_error[64];
    bool rebooting, held, have_os;
    bool web_busy;              // the web page is reading or writing the box (sb_startup_web_hold)
    bool web_said;              // ... and the screen says so (not for a screenshot: it would be in it)
    int left;                   // countdown seconds, -1 = none
    uint32_t second_at, release_at, probe_at, boot_at;
    int nitems, sel, menu_y;
    int items[6];               // ACT_*
    bool in_settings, in_remotes;
    char last[128];
} T;

enum { ACT_START, ACT_SAVE, ACT_INSTALL, ACT_SETTINGS, ACT_WIFI, ACT_WTVP, ACT_REMOTES, ACT_POWER, ACT_DONE,
       ACT_SONY, ACT_PHILIPS, ACT_REMOTES_DONE };

sb_rb_chunk_fn sb_save_sink;
void (*sb_install_source)(void);

extern uint32_t bridge_now_ms(void);
static void note(const char *what) { snprintf(T.last, sizeof T.last, "%s", what); LOGI("[bridge] %.2f s %s\n", bridge_now_ms() / 1000.0, what); }

sb_state_t sb_startup_state(void) { return T.state; }
bool sb_startup_saving(void) { return T.state == ST_SAVING; }
bool sb_startup_owns_link(void) {
    sb_session_phase_t p = sb_session_phase();
    return T.state != ST_OS && T.state != ST_OFF && T.state != ST_NO_TIER3 && (p == SS_STAGE0 || p == SS_TIER3 || p == SS_LOADER);
}

// ---------------------------------------------------------------- screens

static void line(int n, const char *s, uint32_t fg) { sb_print(SB_FONT_BODY, SB_X, 100 + n * SB_LINE, SB_W, SB_LINE, fg, C_BG, s); }

// The bar's frame, then the green part and the rest side by side: every pixel goes straight to its
// colour (filling the whole bar first and the green over it made the bar blink at each step).
// The green width is even: FILL works in pixel pairs, so the two never share one.
static void progress(int permille) {
    int inner = SB_W - 8;
    int w = (int)(inner * (long)(permille > 1000 ? 1000 : permille) / 1000) & ~1;
    sb_fill(SB_X, BAR_Y, SB_W, 4, C_PANEL);                        // the frame: top, bottom, sides
    sb_fill(SB_X, BAR_Y + BAR_H - 4, SB_W, 4, C_PANEL);
    sb_fill(SB_X, BAR_Y + 4, 4, BAR_H - 8, C_PANEL);
    sb_fill(SB_X + SB_W - 4, BAR_Y + 4, 4, BAR_H - 8, C_PANEL);
    if (w) sb_fill(SB_X + 4, BAR_Y + 4, w, BAR_H - 8, C_GOOD);
    if (w < inner) sb_fill(SB_X + 4 + w, BAR_Y + 4, inner - w, BAR_H - 8, C_PANEL);
}

static const char *item_label(int act) {
    switch (act) {
    case ACT_START: return T.os.patched ? "Start now" : "Start without WiFi (uses the modem)";
    case ACT_SAVE: return "Save this OS";
    case ACT_INSTALL: return "Install another OS";
    case ACT_SETTINGS: return "Settings";
    case ACT_WIFI: return "WiFi network";
    case ACT_WTVP: return "WTVP server";
    case ACT_REMOTES: return "Remotes";
    case ACT_POWER: return settings.box_off & BOX_OFF_AUTOPOWER ? "Turn on by itself: no (power key)" : "Turn on by itself: yes";
    case ACT_SONY: return settings.box_off & BOX_OFF_SONY ? "[ ] Sony WebTV remote" : "[x] Sony WebTV remote";
    case ACT_PHILIPS: return settings.box_off & BOX_OFF_PHILIPS ? "[ ] Philips remotes (RC-5)" : "[x] Philips remotes (RC-5)";
    default: return "Done";
    }
}

static void draw_item(int i) {
    bool on = i == T.sel;
    int y = T.menu_y + i * MENU_ROW;
    sb_back_begin(SB_X, y, SB_W, MENU_ROW - 2);      // the row put up whole: no blink as the bar moves
    sb_fill(SB_X, y, SB_W, MENU_ROW - 2, on ? C_BAR : C_PANEL);
    sb_print(on ? SB_FONT_ITEM_SEL : SB_FONT_ITEM, SB_X + 12, y + 1, SB_W - 24, MENU_ROW - 4, on ? C_BARTEXT : C_TEXT,
             on ? C_BAR : C_PANEL, item_label(T.items[i]));
    sb_back_end();
}
static void draw_menu(void) { for (int i = 0; i < T.nitems; i++) draw_item(i); }

static void draw_left(void) {
    char b[64] = "";
    if (T.left >= 0) snprintf(b, sizeof b, "Starting in %d s. Any key stops it.", T.left);
    line(5, b, C_TEXT);
}

static const char *server(void) { return settings.service[0] ? settings.service : "the software's own"; }

// ---------------------------------------------------------------- the flow

static void on_key(int k);
static void prepare(void);
static void wifi_done(const char *ssid, const char *ip);
static void countdown(void);
static void boot(void);
static void settings_menu(void);
static void choose(int act);
static void loader_ready(void);
static void t3_progress(void);
static void t3_checked(const char *err, bool updated);

static void on_phase(sb_session_phase_t p) {
    if (p == SS_DEBUGGER) note("box reset: diagnostics");
    else if (p == SS_TIER3_BOOT) note(sb_session_debug_rom() ? "box reset: diagnostics (entry tier3: test 19, the tier-3 loader)"
                                                              : "box reset: diagnostics (no debug monitor: test 19, the tier-3 loader)");
    else if (p == SS_NO_TIER3) {
        note("no loader in the tier-3 area: the box boots its own OS, unpatched (install it with lptdisk.mjs --tier3)");
        T.state = ST_NO_TIER3;
        T.probe_at = bridge_now_ms() + NO_TIER3_RETRY_MS;
    } else if (p == SS_LOADER) {
        note("loader running");
        port_watch_probe(true);   // a reset from now on (a crash, a power-off) is caught
        sb_ir_reset();
        // a tier-3 start: the loader came from the disk; keep it this bridge's (sb_tier3.c)
        if (sb_session_via_tier3()) { T.state = ST_LOADING; sb_t3_progress = t3_progress; sb_t3_check(t3_checked); }
        else loader_ready();
    }
}

// the boot ROM's fonts first (sb_font.c): every screen after uses them (or the 5x7 font, if not found)
static void fonts_read(bool ok) {
    (void)ok;
    sb_on_key = on_key;
    if (settings.ssid[0] && net_up()) prepare();
    else { T.state = ST_WIFI; sb_wifi_start(wifi_done); }
}

static void loader_ready(void) {
    if (!sb_font_ready()) { sb_font_load(fonts_read); return; }
    sb_on_key = on_key;
    if (settings.ssid[0] && net_up()) prepare();
    else { T.state = ST_WIFI; sb_wifi_start(wifi_done); }
}

static void t3_progress(void) {
    sb_frame("Updating the Bridge", NULL, NULL, 0);
    line(0, "Writing its new loader to the disk...", C_TEXT);
    line(1, "(the tier-3 area; the OS isn't touched)", C_DIM);
}

// a reset the way the OS's own Reboot does it: 7 to BUS_RESETCAUSE (a plain WRITE, so whatever
// loader runs)
static void reset_written(void *ctx, const char *err, uint32_t v0) { (void)ctx; (void)err; (void)v0; }
static void reset_box(void) {
    static const uint8_t seven[4] = { 0, 0, 0, 7 };
    sb_mem_write(0xa40000a8u, 4, 1, seven, reset_written, NULL);
}

// The loader running came from the disk. Not this bridge's (rewritten now, or not): its routines
// may sit elsewhere, so the start-up can't go on with it.
static void t3_checked(const char *err, bool updated) {
    if (err) {
        char b[128];
        snprintf(b, sizeof b, "tier-3 area: %s; the box starts its own OS, unpatched", err);
        note(b);
        T.state = ST_NO_TIER3;
        T.probe_at = bridge_now_ms() + NO_TIER3_RETRY_MS;
        port_watch_probe(false);                // its reset goes past: the ROM boots the disk's OS
        reset_box();
        return;
    }
    if (updated) {
        note("tier-3 area updated: restarting the box with the new loader");
        reset_box();                            // the probe watch is on: the bridge takes it from the probe
        return;
    }
    loader_ready();
}

static void arm(void) {
    T.state = ST_ARMED;
    T.second_at = T.release_at = T.probe_at = 0;
    port_watch_probe(false);
    note(T.rebooting ? "armed (the dongle): the OS is rebooting" : "armed (the dongle)");
    // the box reset: its modem would have lost DTR. Hang up (PPP and the gateway's connections
    // go with the call), or the next OS starts with carrier up and takes itself to be online.
    bridge_link_reset();
    sb_font_reset();            // read again from this box's boot ROM once its loader runs
    sb_on_phase = on_phase;
    sb_session_start();
}

void sb_startup_start(void) { memset(&T, 0, sizeof T); T.left = -1; arm(); }

void sb_startup_reboot_notice(void) {
    if (T.state != ST_OS) return;
    note("the OS says it is rebooting");
    T.rebooting = true;
}

static void loaded(const char *err);
static void patched_cb(const char *err);

static void prepare(void) {
    static const char *const help[] = { "This takes a few seconds." };
    char b[64];
    T.state = ST_LOADING;
    T.have_os = false; T.patch_error[0] = 0; T.held = false; T.web_busy = false;
    sb_frame("Preparing your WebTV", NULL, help, 1);
    snprintf(b, sizeof b, "WiFi: %.40s", settings.ssid);
    line(0, b, C_DIM);
    line(1, "Reading the software from the disk...", C_TEXT);
    progress(0);
    sb_rb_progress = progress;
    sb_rb_load(&T.os, loaded);
}

static void loaded(const char *err) {
    if (err) {
        snprintf(T.patch_error, sizeof T.patch_error, "Can't read the software: %s", err);
        note(T.patch_error);
        countdown();
        return;
    }
    T.have_os = true;
    char b[96];
    snprintf(b, sizeof b, "OS read from slot %u: %08x-%08x", (unsigned)T.os.slot, (unsigned)T.os.id.code, (unsigned)T.os.size);
    note(b);
    T.state = ST_PATCHING;
    line(1, "Patching it for WiFi...", C_TEXT);
    sb_rb_patch(&T.os, patched_cb);
}

static void patched_cb(const char *err) {
    if (err && !strcmp(err, "unknown")) { snprintf(T.patch_error, sizeof T.patch_error, "This software version can't be patched for WiFi."); note("no patch for this build"); }
    else if (err) { snprintf(T.patch_error, sizeof T.patch_error, "Patching failed: %s", err); note(T.patch_error); }
    else { char b[96]; snprintf(b, sizeof b, "patched: %s", sb_patch_name(&T.os.patch)); note(b); }
    if (T.rebooting && !err) { T.rebooting = false; boot(); return; }
    T.rebooting = false;
    countdown();
}

static void countdown(void) {
    // Save and Install are on the Bridge's web page (web.c): it can start them while this screen is up
    static char web[64];
    const char *help[3] = { "Up/Down to choose, then Select." };
    int nhelp = 1;
    char b[64], ip[20];
    net_address(ip, sizeof ip);
    if (T.have_os) {
        help[nhelp++] = "While this screen is up, the Bridge's web page can";
        if (ip[0]) snprintf(web, sizeof web, "save this OS or install another: http://%s/", ip);
        else snprintf(web, sizeof web, "save this OS or install another.");
        help[nhelp++] = web;
    }
    bool patched = T.have_os && T.os.patched;
    T.state = ST_COUNTDOWN;
    T.in_settings = T.in_remotes = false;
    T.menu_y = MENU_Y;
    T.nitems = 0;
    if (T.have_os) T.items[T.nitems++] = ACT_START;
    T.items[T.nitems++] = ACT_SETTINGS;
    T.sel = 0;
    sb_frame("WebTV Bridge", NULL, help, nhelp);
    snprintf(b, sizeof b, "WiFi: %.40s", settings.ssid); line(0, b, C_DIM);
    snprintf(b, sizeof b, "Server: %.40s", server()); line(1, b, C_DIM);
    if (T.have_os && T.os.has_patch) {
        const char *n = sb_patch_name(&T.os.patch), *s = strrchr(n, '/');
        snprintf(b, sizeof b, "Software: %s", s ? s + 1 : n);
    } else if (T.have_os) snprintf(b, sizeof b, "Software: %08x-%08x", (unsigned)T.os.id.code, (unsigned)T.os.size);
    else snprintf(b, sizeof b, "Software: none");
    line(2, b, C_DIM);
    line(3, patched ? "Patched for WiFi." : T.patch_error, patched ? C_GOOD : C_BAD);
    draw_menu();
    T.left = patched && !T.held ? COUNTDOWN_S : -1;
    draw_left();
    if (T.left >= 0) T.second_at = bridge_now_ms() + 1000;
}

static void menu_move(int k) {
    int was = T.sel;
    T.sel += k == KEY_UP ? -1 : 1;
    if (T.sel < 0) T.sel = 0;
    if (T.sel > T.nitems - 1) T.sel = T.nitems - 1;
    if (was != T.sel) { draw_item(was); draw_item(T.sel); }
}

// Save and Install go through the bridge's web page (web.c), which starts them from there
static void web_hint(const char *what) {
    char ip[20], b[64];
    net_address(ip, sizeof ip);
    if (ip[0]) snprintf(b, sizeof b, "Open http://%s/ to %s.", ip, what);
    else snprintf(b, sizeof b, "Use the Bridge's web page to %s.", what);
    line(4, b, C_DIM);
}

// Save this OS, to whatever takes it (the web UI)
static bool save_chunk(const uint8_t *d, size_t n, uint32_t at, uint32_t total) { return sb_save_sink ? sb_save_sink(d, n, at, total) : true; }
static void saved(const char *err) {
    if (sb_save_sink) sb_save_sink(NULL, 0, T.os.size, T.os.size);
    char b[64];
    if (err) snprintf(b, sizeof b, "save failed: %s", err);
    else snprintf(b, sizeof b, "saved %u bytes", (unsigned)T.os.size);
    note(b);
    countdown();
    line(4, err ? "Save failed." : "Saved: see the Bridge's web page.", err ? C_BAD : C_GOOD);
}
static void save(void) {
    if (!sb_save_sink) { web_hint("save it"); return; }
    T.state = ST_SAVING;
    sb_frame("Save this OS", NULL, NULL, 0);
    line(0, "Reading it from the disk...", C_TEXT);
    progress(0);
    sb_rb_progress = progress;
    sb_rb_save(&T.os, save_chunk, saved);
}

// Install another OS: sb_install_source() feeds sb_rb_install_*; sb_startup_installed() when done
static void install(void) {
    if (!sb_install_source) { web_hint("install one"); return; }
    static const char *const help[] = { "Keep the box on until this is done." };
    T.state = ST_INSTALLING;
    sb_frame("Install another OS", NULL, help, 1);
    line(0, "Writing it to the disk...", C_TEXT);
    progress(0);
    sb_rb_progress = progress;
    sb_install_source();
}

void sb_startup_installed(const char *err) {
    if (err) { char b[64]; snprintf(b, sizeof b, "install failed: %s", err); note(b); countdown(); line(4, "Install failed.", C_BAD); return; }
    note("installed in the other slot; it boots from now on");
    T.have_os = true; T.patch_error[0] = 0;
    T.state = ST_PATCHING;
    sb_rb_patch(&T.os, patched_cb);
}
sb_os_t *sb_startup_os(void) { return &T.os; }

bool sb_startup_have_os(void) { return T.have_os; }
bool sb_startup_web_ready(void) { return T.state == ST_COUNTDOWN && T.have_os; }

bool sb_startup_web_action(int what) {
    if (!sb_startup_web_ready()) return false;
    if (T.left >= 0) { T.left = -1; T.second_at = 0; draw_left(); note("countdown stopped (web page)"); }
    T.held = true;
    choose(what == SB_WEB_SAVE ? ACT_SAVE : ACT_INSTALL);
    return T.state == (what == SB_WEB_SAVE ? ST_SAVING : ST_INSTALLING);
}

bool sb_startup_box_ready(void) { return T.state == ST_COUNTDOWN && sb_startup_owns_link(); }

bool sb_startup_key(uint32_t entry) {
    if (T.state == ST_OS || !sb_on_key || !sb_startup_owns_link()) return false;
    sb_ir_inject(entry, bridge_now_ms());
    return true;
}

// the patched OS is running and its patch puts keys in its IR input ring (the table's flag)
bool sb_startup_os_takes_keys(void) { return T.state == ST_OS && T.os.patched && (T.os.patch.flags & 2); }

bool sb_startup_web_hold(bool busy, bool show) {
    if (busy && !T.web_busy) {
        if (!sb_startup_box_ready()) return false;
        if (T.left >= 0) { T.left = -1; T.second_at = 0; draw_left(); note("countdown stopped (web page)"); }
        T.held = true;
        if (show) line(4, "The Bridge's web page is using the box's disk.", C_DIM);
        T.web_said = show;
    } else if (!busy && T.web_busy && T.web_said && T.state == ST_COUNTDOWN) line(4, "", C_DIM);
    T.web_busy = busy;
    return true;
}

void sb_startup_refresh(void) {
    if (T.state != ST_COUNTDOWN) return;
    char b[64];
    snprintf(b, sizeof b, "Server: %.40s", server()); line(1, b, C_DIM);
}

const char *sb_startup_build_name(void) { return T.have_os && T.os.has_patch ? sb_patch_name(&T.os.patch) : NULL; }

static void wtvp_done(const char *v) {
    if (v) {
        snprintf(settings.service, sizeof settings.service, "%s", v);
        settings_save();
        extern void gw_set_service(const char *host);
        gw_set_service(settings.service);
        char b[96]; snprintf(b, sizeof b, "WTVP server: %s", server()); note(b);
    }
    settings_menu();
}

static void wifi_done(const char *ssid, const char *ip) { (void)ssid; (void)ip; if (T.in_settings) settings_menu(); else prepare(); }

static void settings_menu(void) {
    static const char *const help[] = { "Up/Down to choose, then Select." };
    char b[64];
    T.state = ST_SETTINGS;
    T.in_settings = true;
    T.in_remotes = false;
    T.menu_y = MENU_Y - 3 * MENU_ROW;
    T.nitems = 5; T.items[0] = ACT_WIFI; T.items[1] = ACT_WTVP; T.items[2] = ACT_REMOTES; T.items[3] = ACT_POWER; T.items[4] = ACT_DONE;
    T.sel = 0;
    sb_frame("Settings", NULL, help, 1);
    snprintf(b, sizeof b, "WiFi: %.40s", settings.ssid); line(0, b, C_DIM);
    snprintf(b, sizeof b, "Server: %.40s", server()); line(1, b, C_DIM);
    draw_menu();
}

// which remotes the OS decodes besides its own (the WebTV remote and keyboard always work)
static void remotes_menu(void) {
    static const char *const help[] = { "Select turns one on or off.", "The WebTV remote and keyboard always", "work. Back or Done when finished." };
    T.state = ST_SETTINGS;
    T.in_remotes = true;
    T.menu_y = MENU_Y - 3 * MENU_ROW;
    T.nitems = 3; T.items[0] = ACT_SONY; T.items[1] = ACT_PHILIPS; T.items[2] = ACT_REMOTES_DONE;
    T.sel = 0;
    sb_frame("Remotes", NULL, help, 3);
    line(0, "Remotes the box listens to:", C_TEXT);
    draw_menu();
}

static void remotes_done(void) {
    bool saved = settings_save();
    char b[64];
    snprintf(b, sizeof b, "remotes: WebTV%s%s%s", settings.box_off & BOX_OFF_SONY ? "" : " Sony",
             settings.box_off & BOX_OFF_PHILIPS ? "" : " Philips", saved ? "" : " (couldn't save)");
    note(b);
    settings_menu();
    T.sel = 2; draw_menu();
}

static void choose(int act) {
    static const char *const help[] = { "Type it, then Enter, or pick Keyboard", "to use the remote. Back: no change." };
    switch (act) {
    case ACT_START:
        if (T.web_busy) { line(4, "Wait: the Bridge's web page is using the box.", C_BAD); break; }
        boot();
        break;
    case ACT_SAVE: save(); break;
    case ACT_INSTALL: install(); break;
    case ACT_SETTINGS: settings_menu(); break;
    case ACT_WIFI: T.state = ST_WIFI; sb_wifi_start(wifi_done); break;
    case ACT_WTVP:
        T.state = ST_SETTINGS;
        sb_entry_show("WTVP server", "Address or name (blank: the box's own):", help, 2, settings.service, 63, true, wtvp_done);
        break;
    case ACT_REMOTES: remotes_menu(); break;
    case ACT_POWER:
        settings.box_off ^= BOX_OFF_AUTOPOWER;
        settings_save();
        note(settings.box_off & BOX_OFF_AUTOPOWER ? "the OS waits for the power key" : "the OS turns on by itself");
        draw_item(T.sel);
        break;
    case ACT_SONY: settings.box_off ^= BOX_OFF_SONY; draw_item(T.sel); break;
    case ACT_PHILIPS: settings.box_off ^= BOX_OFF_PHILIPS; draw_item(T.sel); break;
    case ACT_REMOTES_DONE: remotes_done(); break;
    case ACT_DONE: countdown(); break;
    }
}

static void on_key(int k) {
    switch (T.state) {
    case ST_WIFI: sb_wifi_key(k); return;
    case ST_SETTINGS:
        if (sb_entry_key(k)) return;
        if (k == KEY_BACK) { if (T.in_remotes) remotes_done(); else countdown(); return; }
        // fall through: the settings menu
    case ST_COUNTDOWN:
        T.held = true;
        if (T.left >= 0) { T.left = -1; T.second_at = 0; draw_left(); note("countdown stopped"); }
        if (k == KEY_UP || k == KEY_DOWN) menu_move(k);
        else if (k == '\r') choose(T.items[T.sel]);
        return;
    default: return;
    }
}

static void go(void);

// the settings into the OS first (sb_rb_options), then boot_ram
static void options_written(const char *err) {
    if (err) {
        snprintf(T.patch_error, sizeof T.patch_error, "Settings: %.50s", err);
        note(T.patch_error);
        T.have_os = false;         // the image in RAM is no longer the checked one: read it again to start
        T.held = true;
        sb_on_key = on_key;
        countdown();
        return;
    }
    go();
}

static void boot(void) {
    T.state = ST_PATCHING;      // the loader's link still: settings go in before the hand-over
    sb_on_key = NULL;
    sb_fill(0, 0, SCREEN_W, SCREEN_H, C_BG);
    sb_print(SB_FONT_TITLE, SB_X, 196, SB_W, 28, C_TEXT, C_BG, "Starting WebTV...");
    sb_rb_options(&T.os, options_written);
}

static void go(void) {
    T.state = ST_OS;
    note(T.os.patched ? "starting the OS from RAM (patched)" : "starting the OS from RAM (unpatched)");
    port_watch_probe(false);  // boot_ram resets the box: that probe is the ROM starting our OS
    sb_rb_boot(&T.os);
    T.boot_at = bridge_now_ms();
    T.release_at = T.boot_at + RELEASE_MS;
    T.probe_at = 0;           // counted from the hand-over (below)
}

void sb_startup_poll(uint32_t now) {
    if (T.state == ST_OFF) return;
    sb_session_poll(now);
    if (T.state == ST_WIFI) sb_wifi_poll(now);
    if (T.second_at && (int32_t)(now - T.second_at) >= 0) {
        if (T.left >= 0 && --T.left <= 0) { T.second_at = 0; T.left = -1; if (!T.web_busy) boot(); }
        else { T.second_at = now + 1000; draw_left(); }
    }
    if (T.release_at && (int32_t)(now - T.release_at) >= 0) {
        // not until boot_ram's CALL has left for the box: PARMOD afresh empties the modem ring, and
        // the loader may still be drawing "Starting WebTV..." (up to RELEASE_WAIT_MS)
        if ((sb_send_pending() || port_pm_modem_queued()) && (int32_t)(now - T.boot_at) < RELEASE_WAIT_MS) {
            T.release_at = now + RELEASE_MS;
            sb_send_flush();
            return;
        }
        if (sb_send_pending() || port_pm_modem_queued()) note("the loader didn't take boot_ram: handing over anyway");
        T.release_at = 0;
        T.probe_at = now + PROBE_AFTER_MS;
        sb_send_flush();
        sb_session_end();            // the modem channel is the OS's from now on
        port_set_mode(PORT_PARMOD);   // PARMOD afresh: the OS enters a session of its own
    }
    if (T.probe_at && (int32_t)(now - T.probe_at) >= 0) {
        T.probe_at = 0;
        port_watch_probe(true);
    }
    uint32_t ev = port_take_events(PORT_EV_PROBE);
    if (ev & PORT_EV_PROBE) { note("the boot ROM is probing for the dongle: the box reset"); arm(); }
}

void sb_startup_status(void (*print)(const char *fmt, ...)) {
    static const char *const names[] = { "off", "armed", "wifi", "loading", "patching", "countdown", "settings", "saving",
                                         "installing", "os", "no-tier3" };
    print("start-up: %s; last: %s\n", names[T.state], T.last);
    sb_mem_status(print);
    if (T.have_os) print("  OS slot %u base 0x%08x size %u code %08x%s\n", (unsigned)T.os.slot, (unsigned)T.os.base,
                         (unsigned)T.os.size, (unsigned)T.os.id.code, T.os.patched ? ", patched" : "");
}
