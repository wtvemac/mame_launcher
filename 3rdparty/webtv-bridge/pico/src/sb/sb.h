// The bridge's start-up with a stock box (WEBTV_LPT_BRIDGE.md §10.6, §10.7), in C. It began as a
// port of JS in ../../src/stockbox/; the start-up itself (the TV flow, OS into RAM, the screens)
// now lives only here, tested in the emulator with tools/bridgesim.mjs (run.mjs --pico).
// session.js, protocol.js, boxmem.js and irdecode.js remain as the emulator's own JS take on
// the loader session (tools/boxshell.mjs). Platform-independent: it
// talks to the box through port.h and link.h, to WiFi through net.h.
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

// ---------------------------------------------------------------- keys (irdecode.js)
// What the screens get: printable ASCII, '\b', '\r', 0x1b, or one of these (the OS's values)
enum {
    KEY_UP = 0x8000, KEY_LEFT = 0x8001, KEY_RIGHT = 0x8002, KEY_DOWN = 0x8003, KEY_OPTIONS = 0x8004,
    KEY_POWER = 0x8006, KEY_BACK = 0x8008, KEY_SCROLLUP = 0x8009, KEY_SCROLLDOWN = 0x800a,
    KEY_RECENT = 0x800b, KEY_HOME = 0x800c,
};
void sb_ir_reset(void);
void sb_ir_entry(uint16_t e, uint32_t now_ms);          // a raw IR receiver run from the loader
extern void (*sb_on_key)(int key);                      // set by whoever has the screen
void sb_ir_inject(uint32_t entry, uint32_t now_ms);      // a key from the web page (an IR input entry's first word)

// ---------------------------------------------------------------- loader protocol (protocol.js)
#define SCREEN_W 560
#define SCREEN_H 420
#define CELL_W 12
#define CELL_H 16
uint32_t sb_yuyv(int r, int g, int b);
void sb_send(const uint8_t *d, size_t n);               // to the loader (queued for the link)
void sb_send_flush(void);                               // move what fits into the link
size_t sb_send_pending(void);
void sb_hello(void);
void sb_fill(int x, int y, int w, int h, uint32_t colour);
void sb_text(int x, int y, int scale, uint32_t fg, uint32_t bg, const char *s);
void sb_text2(uint32_t strike, int x, int y, uint32_t fg, const char *s);   // a boot ROM font strike (loader 4+)
void sb_vfilt(int x, int y, int w, int h);                                // the OS's flicker filter (loader 4+)
void sb_target(int which);                                                // draw on the screen (0) or the back buffer (1) (loader 5+)
enum { SB_TO_SCREEN = 0, SB_TO_BACK = 1 };
void sb_copy(int dir, int x, int y, int w, int h);                        // a rectangle between them (loader 5+)
// Draw a box off the screen and put it up whole (a row's fill and its label, say), so nothing
// shows erased first: between these, drawing goes to the back buffer (sb_font.c). Nests.
void sb_back_begin(int x, int y, int w, int h);
void sb_back_end(void);
void sb_parser_reset(void);
void sb_parser_feed(uint8_t b);                         // bytes from the loader
void sb_parser_width(int width);                        // DATA items' width (the reader knows it)
enum { SBM_HELLO = 1, SBM_SYNC, SBM_IR, SBM_ERR, SBM_LOADED, SBM_BADSUM, SBM_DATA, SBM_DONE, SBM_RET, SBM_FAULT, SBM_JUNK };
typedef struct {
    int type;
    uint32_t v0;                // RET
    uint8_t byte;               // SYNC n, ERR byte, FAULT why, HELLO version, JUNK byte
    const uint8_t *data;        // DATA's items (big-endian), IR entries (pairs)
    size_t len;
} sb_msg_t;
extern void (*sb_on_message)(const sb_msg_t *m);       // set by the session

// ---------------------------------------------------------------- the box's memory (boxmem.js)
// One request on the link at a time. err: NULL, or why. READ fills dst with count*width bytes,
// big-endian; WRITE takes them from src. Long transfers go in 1 KB requests.
typedef void (*sb_done_fn)(void *ctx, const char *err, uint32_t v0);
bool sb_mem_read(uint32_t addr, int width, uint32_t count, uint8_t *dst, sb_done_fn done, void *ctx);
bool sb_mem_write(uint32_t addr, int width, uint32_t count, const uint8_t *src, sb_done_fn done, void *ctx);
bool sb_mem_call(uint32_t addr, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, sb_done_fn done, void *ctx);
void sb_mem_reset(void);
bool sb_mem_idle(void);
void sb_mem_poll(uint32_t now);                         // warns of a request the loader never answers
void sb_mem_status(void (*print)(const char *fmt, ...));
bool sb_mem_message(const sb_msg_t *m);                 // the session hands it the replies

// ---------------------------------------------------------------- diagnostics -> loader (session.js)
typedef enum {
    SS_OFF, SS_MIRRORING, SS_DEBUGGER, SS_BLACK, SS_CAUSE, SS_UPLOAD, SS_EPC, SS_GO, SS_STAGE0,
    SS_TIER3_BOOT, SS_TIER3, SS_LOADER, SS_NO_TIER3,
} sb_session_phase_t;
void sb_session_start(void);                            // the dongle on: any reset lands in the diagnostics
void sb_session_end(void);                              // the port is the OS's now
void sb_session_poll(uint32_t now);
sb_session_phase_t sb_session_phase(void);
bool sb_session_debug_rom(void);
typedef struct { bool seen, debug; char version[16], built_on[24], built_by[24]; } sb_rom_info_t;
const sb_rom_info_t *sb_session_rom(void);           // the boot ROM's banner, as last seen (seen = false: none yet)
bool sb_session_via_tier3(void);                        // this start came through test 19 (the tier-3 area)
extern void (*sb_on_phase)(sb_session_phase_t p);

// ---------------------------------------------------------------- the patch table (patchtable.js)
typedef struct { uint32_t code, codewords, size, fs_start, fs_words, fs_sum; bool has_fs; } sb_ident_t;
bool sb_identity(const uint8_t hdr[64], uint32_t trailer_words, uint32_t trailer_sum, sb_ident_t *id);
#define SB_PATCH_TABLE_VERSION 2
#define SB_PATCH_ENTRY 48
typedef struct {
    uint32_t code, size, fs, flags, after_code, after_fs, runs, name;
    uint32_t ir_off, ir_word, power_off, power_word;   // what the settings change (offset 0: none)
} sb_patch_entry_t;
const sb_patch_entry_t *sb_patch_find(const sb_ident_t *id);
const char *sb_patch_name(const sb_patch_entry_t *e);
uint32_t sb_patch_count(void);

// ---------------------------------------------------------------- OS into RAM, patch, boot (sb_ramboot.c)
typedef struct {
    uint32_t slot, base, size;
    sb_ident_t id;
    sb_patch_entry_t patch;     // valid when has_patch
    bool has_patch, patched;
} sb_os_t;
typedef void (*sb_rb_done_fn)(const char *err);   // NULL = done; "unknown" = no patch for it
extern void (*sb_rb_progress)(int permille);
void sb_rb_load(sb_os_t *os, sb_rb_done_fn done);
void sb_rb_patch(sb_os_t *os, sb_rb_done_fn done);
void sb_rb_boot(const sb_os_t *os);
// The settings that change the patched OS (settings.box_off: the remotes it decodes, powering on
// by itself), written into it with the code checksum fixed, and checked as the boot ROM will
void sb_rb_options(sb_os_t *os, sb_rb_done_fn done);
// Save this OS: the image as it is on the disk, a chunk at a time
// a chunk of Save's image; false: not taken yet, so the save waits for sb_rb_save_resume()
// (d stays valid until then). The last call has d = NULL.
typedef bool (*sb_rb_chunk_fn)(const uint8_t *d, size_t n, uint32_t at, uint32_t total);
void sb_rb_save(const sb_os_t *os, sb_rb_chunk_fn chunk, sb_rb_done_fn done);
void sb_rb_save_resume(void);
// Install another OS, streamed: begin with its first 64 bytes, then chunks in order, then end:
// the image goes to RAM at its base, is checked, written to the slot that isn't booting, and the
// select block flipped. os describes the new one when done.
const char *sb_rb_install_begin(sb_os_t *os, const uint8_t hdr[64], uint32_t total);
void sb_rb_install_chunk(const uint8_t *d, size_t n, sb_rb_done_fn done);
void sb_rb_install_end(sb_rb_done_fn done);

// ---------------------------------------------------------------- screens (sb_ui.c)
extern const uint32_t C_BG, C_PANEL, C_BAR, C_TEXT, C_DIM, C_BARTEXT, C_BOX, C_BOXTEXT, C_GOOD, C_BAD;
#define SB_X 48
#define SB_W (SCREEN_W - 2 * SB_X)
#define SB_LINE (CELL_H + 4)
void sb_frame(const char *title, const char *prompt, const char *const *help, int nhelp);
typedef struct sb_entry sb_entry_t;
typedef void (*sb_entry_fn)(const char *value);        // submitted (NULL: Back)
void sb_entry_show(const char *title, const char *prompt, const char *const *help, int nhelp,
                   const char *value, int max, bool empty, sb_entry_fn done);
bool sb_entry_key(int key);
void sb_entry_status(const char *s, uint32_t fg, int row);
void sb_wifi_start(void (*done)(const char *ssid, const char *ip));
bool sb_wifi_key(int key);
void sb_wifi_poll(uint32_t now);

// ---------------------------------------------------------------- the boot ROM's fonts (sb_font.c)
// The OS's own typefaces, read in place from the box's boot ROM (Fonts/<Face>_<size> in its ROM
// filesystem; src/stockbox/strike.js has the format): the bridge finds the strikes and reads
// their widths through the loader, which draws with them (TEXT2). Without them (not found, or
// not loaded yet) the screens fall back to the loader's 5x7 font.
enum { SB_FONT_TITLE, SB_FONT_BODY, SB_FONT_ITEM, SB_FONT_ITEM_SEL, SB_FONT_SMALL, SB_FONTS };
void sb_font_load(void (*done)(bool ok));
void sb_font_reset(void);
bool sb_font_ready(void);
int sb_font_width(int f, const char *s);                // pixels
int sb_font_height(int f);                              // ascent + descent
// a line in a box: (x, y, w, h) filled with bg, the text from x with its baseline centred in
// the box, then the OS's flicker filter over the box. Width 0: as wide as the text.
void sb_print(int f, int x, int y, int w, int h, uint32_t fg, uint32_t bg, const char *s);
void sb_print_right(int f, int right, int y, int h, uint32_t fg, uint32_t bg, const char *s);

// ---------------------------------------------------------------- the tier-3 area (sb_tier3.c)
// After a tier-3 start, before anything else touches RAM above 0x80400000: is the loader on the
// disk this bridge's? If not, rewrite the area through the updater. done(err, updated).
typedef void (*sb_t3_done_fn)(const char *err, bool updated);
void sb_t3_check(sb_t3_done_fn done);
extern void (*sb_t3_progress)(void);                    // called when a rewrite begins

// ---------------------------------------------------------------- start-up (sb_startup.c)
typedef enum { ST_OFF, ST_ARMED, ST_WIFI, ST_LOADING, ST_PATCHING, ST_COUNTDOWN, ST_SETTINGS, ST_SAVING,
               ST_INSTALLING, ST_OS, ST_NO_TIER3 } sb_state_t;
void sb_startup_start(void);
void sb_startup_poll(uint32_t now);
void sb_startup_reboot_notice(void);                    // the OS said REBOOT (PARMOD)
sb_state_t sb_startup_state(void);
bool sb_startup_owns_link(void);                         // the loader's link: not the modem's
void sb_startup_status(void (*print)(const char *fmt, ...));
// Save / Install, for the web UI (or the host's stand-ins)
enum { SB_WEB_SAVE, SB_WEB_INSTALL };
bool sb_startup_have_os(void);                         // an OS was read from the disk (sb_startup_os())
bool sb_startup_web_ready(void);                       // the WebTV Bridge screen is up: Save and Install can start
bool sb_startup_web_action(int what);                   // from the web page: only while the menu is up
// The web page's own reads and writes of the box (memory, the disk): the loader is free while
// the WebTV Bridge screen is up, with or without an OS. Holding stops the countdown, says so on
// the TV (a disk job; not a screenshot, which would show it), and keeps Start from handing the box to the OS until it is released.
bool sb_startup_box_ready(void);
bool sb_startup_web_hold(bool busy, bool show);         // false: not now (the screen isn't up); show: say so on the TV
void sb_startup_refresh(void);                          // a setting changed: redraw what the screen shows of it
const char *sb_startup_build_name(void);               // the OS's build ("approm/.../build-16423"), or NULL
bool sb_startup_saving(void);
extern sb_rb_chunk_fn sb_save_sink;                     // where Save this OS sends the image
extern void (*sb_install_source)(void);                 // asked for an image to install: call sb_rb_install_*
sb_os_t *sb_startup_os(void);                           // the OS install feeds: sb_rb_install_begin(sb_startup_os(), ...)
// Keys from the web page (port.h, port_pm_key, has their form): to the Bridge's own screens while
// they are up (false otherwise), or whether the OS running takes them over PARMOD
bool sb_startup_key(uint32_t entry);
bool sb_startup_os_takes_keys(void);
void sb_startup_installed(const char *err);             // ...and sb_rb_install_end(sb_startup_installed)
