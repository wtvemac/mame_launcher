// The bridge's screens on the TV, drawn by the loader:
// the frame they share, a line of text typed on the IR keyboard or the on-screen keyboard, and
// WiFi setup (scan, pick, password, join).
#include <stdio.h>
#include <string.h>
#include "sb.h"
#include "../net.h"
#include "../settings.h"
#include "../log.h"

// yuyv() of the JS screens' colours
const uint32_t C_BG = 0x2e9e2e74u, C_PANEL = 0x42a8426fu, C_BAR = 0xb73db799u, C_TEXT = 0xda80da80u, C_DIM = 0x9d929d77u,
               C_BARTEXT = 0x258c257bu, C_BOX = 0xda80da80u, C_BOXTEXT = 0x258c257bu, C_GOOD = 0x9c669c52u, C_BAD = 0x8e648ebbu;

// a line of body text across the column, at y
static void body(int y, const char *s, uint32_t fg) { sb_print(SB_FONT_BODY, SB_X, y, SB_W, SB_LINE, fg, C_BG, s); }

// s cut to fit w pixels in font f, ending in "..." when cut
static const char *fit(int f, const char *s, int w, char *out, size_t size) {
    if (sb_font_width(f, s) <= w) return s;
    size_t n = strlen(s);
    if (n >= size - 3) n = size - 4;
    for (; n; n--) {
        snprintf(out, size, "%.*s...", (int)n, s);
        if (sb_font_width(f, out) <= w) return out;
    }
    return "...";
}

void sb_frame(const char *title, const char *prompt, const char *const *help, int nhelp) {
    sb_fill(0, 0, SCREEN_W, SCREEN_H, C_BG);
    sb_print(SB_FONT_TITLE, SB_X, 40, SB_W, 30, C_TEXT, C_BG, title);
    sb_fill(SB_X, 82, SB_W, 2, C_DIM);
    if (prompt) body(94, prompt, C_TEXT);
    for (int i = 0; i < nhelp; i++) body(340 + i * SB_LINE, help[i], C_DIM);
}

// ---------------------------------------------------------------- a line of text

#define BOX_Y 124
#define BOX_H 28
#define KEYBTN_W 108
#define OSK_BOX_Y 52
#define OSK_Y 100
#define OSK_KW 42
#define OSK_KH 30
#define OSK_GAP 4
#define OSK_X (SB_X + (SB_W - (10 * OSK_KW + 9 * OSK_GAP)) / 2)

static const char *const pages[2][4] = {
    { "1234567890", "qwertyuiop", "asdfghjkl-", "zxcvbnm.@_" },
    { "!@#$%^&*()", "-_=+[]{}\\|", ";:'\",.<>/?", "`~" },
};
enum { ACT_NONE, ACT_SHIFT, ACT_PAGE, ACT_SPACE, ACT_DEL, ACT_DONE };
static const struct { const char *label; int span, act; } special[] = {
    { "Shift", 2, ACT_SHIFT }, { "#+=", 2, ACT_PAGE }, { "Space", 3, ACT_SPACE }, { "Del", 1, ACT_DEL }, { "Done", 2, ACT_DONE },
};

static struct {
    bool active;
    int screen;                 // 0 box, 1 osk
    const char *title, *prompt;
    const char *const *help;
    int nhelp, max;
    bool empty, focus_button;
    char value[128];
    bool fresh;                 // the value given, shown selected: the first key typed replaces it
    int page, row, idx, col;
    bool shift;
    sb_entry_fn done;
} E;

typedef struct { char ch; int c0, span, act; const char *label; char buf[2]; } okey_t;

static int row_len(int r) { return r == 4 ? 5 : (int)strlen(pages[E.page][r]); }
static okey_t key_at(int r, int i) {
    okey_t k = { 0 };
    if (r == 4) {
        int c0 = 0;
        for (int j = 0; j < i; j++) c0 += special[j].span;
        k.c0 = c0; k.span = special[i].span; k.act = special[i].act;
        k.label = special[i].act == ACT_PAGE && E.page == 1 ? "abc" : special[i].label;
        return k;
    }
    k.ch = pages[E.page][r][i];
    if (E.shift && E.page == 0 && k.ch >= 'a' && k.ch <= 'z') k.ch = (char)(k.ch - 32);
    k.c0 = i; k.span = 1; k.buf[0] = k.ch; k.label = k.buf;
    return k;
}

static void frame_rect(int x, int y, int w, int h, uint32_t c) {
    sb_fill(x, y, w, 4, c); sb_fill(x, y + h - 4, w, 4, c);
    sb_fill(x, y, 4, h, c); sb_fill(x + w - 4, y, 4, h, c);
}

static void draw_box(void) {
    bool osk = E.screen == 1;
    int y = osk ? OSK_BOX_Y : BOX_Y, w = osk ? SB_W : SB_W - KEYBTN_W - 8;
    if (!osk) frame_rect(SB_X - 4, y - 4, w + 8, BOX_H + 8, E.focus_button ? C_BG : C_BAR);
    const char *shown = E.value;         // the end stays in view
    while (*shown && sb_font_width(SB_FONT_ITEM, shown) > w - 40) shown++;
    int tw = sb_font_width(SB_FONT_ITEM, shown);
    sb_back_begin(SB_X, y, w, BOX_H);    // put up whole: typing doesn't blink the box
    sb_fill(SB_X, y, w, BOX_H, C_BOX);
    if (E.fresh && *shown) {             // selected, as a text field shows a value it will replace
        sb_fill(SB_X + 10, y + 3, tw + 6, BOX_H - 6, C_BAR);
        sb_print(SB_FONT_ITEM, SB_X + 12, y + 4, tw + 2, BOX_H - 8, C_BARTEXT, C_BAR, shown);
    } else if (*shown) sb_print(SB_FONT_ITEM, SB_X + 12, y + 4, tw + 2, BOX_H - 8, C_BOXTEXT, C_BOX, shown);
    sb_fill(SB_X + 12 + tw + 4, y + 5, 2, BOX_H - 10, C_BOXTEXT);   // cursor
    sb_back_end();
}

// a label centred in a box (already filled with bg)
static void centred(int f, int x, int y, int w, int h, const char *s, uint32_t fg, uint32_t bg) {
    int tw = sb_font_width(f, s);
    sb_print(f, x + (w - tw) / 2, y + 3, tw + 2, h - 6, fg, bg, s);
}

static void draw_button(void) {
    bool on = E.focus_button;
    int x = SB_X + SB_W - KEYBTN_W;
    sb_back_begin(x, BOX_Y, KEYBTN_W, BOX_H);
    sb_fill(x, BOX_Y, KEYBTN_W, BOX_H, on ? C_BAR : C_PANEL);
    centred(on ? SB_FONT_ITEM_SEL : SB_FONT_ITEM, x, BOX_Y, KEYBTN_W, BOX_H, "Keyboard", on ? C_BARTEXT : C_TEXT, on ? C_BAR : C_PANEL);
    sb_back_end();
}

static void entry_show(void) {
    E.screen = 0;
    sb_frame(E.title, E.prompt, E.help, E.nhelp);
    draw_box();
    draw_button();
}

void sb_entry_status(const char *s, uint32_t fg, int row) {
    body(176 + row * SB_LINE, s, fg);
}

void sb_entry_show(const char *title, const char *prompt, const char *const *help, int nhelp,
                   const char *value, int max, bool empty, sb_entry_fn done) {
    memset(&E, 0, sizeof E);
    E.active = true; E.title = title; E.prompt = prompt; E.help = help; E.nhelp = nhelp;
    E.max = max < (int)sizeof E.value - 1 ? max : (int)sizeof E.value - 1;
    E.empty = empty; E.done = done; E.row = 1;
    snprintf(E.value, sizeof E.value, "%s", value ? value : "");
    E.fresh = E.value[0] != 0;
    entry_show();
}

static void set_focus(bool button) {
    if (button == E.focus_button) return;
    E.focus_button = button;
    draw_box();
    draw_button();
}

static void type(int k) {
    if (E.fresh) { E.fresh = false; E.value[0] = 0; }   // replaces (or with Backspace, clears) the value given
    size_t n = strlen(E.value);
    if (k == '\b') { if (n) E.value[n - 1] = 0; }
    else if ((int)n < E.max) { E.value[n] = (char)k; E.value[n + 1] = 0; }
    draw_box();
}

static void submit(void) { if (E.value[0] || E.empty) { E.active = false; E.done(E.value); } }

static void draw_key(int r, int i) {
    okey_t k = key_at(r, i);
    bool on = r == E.row && i == E.idx, lit = k.act == ACT_SHIFT && E.shift;
    int x = OSK_X + k.c0 * (OSK_KW + OSK_GAP), y = OSK_Y + r * (OSK_KH + OSK_GAP);
    int w = k.span * OSK_KW + (k.span - 1) * OSK_GAP;
    uint32_t bg = on ? C_BAR : lit ? C_GOOD : C_PANEL, fg = on || lit ? C_BARTEXT : C_TEXT;
    sb_back_begin(x, y, w, OSK_KH);
    sb_fill(x, y, w, OSK_KH, bg);
    centred(on ? SB_FONT_ITEM_SEL : SB_FONT_ITEM, x, y, w, OSK_KH, k.label, fg, bg);
    sb_back_end();
}

static void draw_keys(void) { for (int r = 0; r < 5; r++) for (int i = 0; i < row_len(r); i++) draw_key(r, i); }

static void show_osk(void) {
    E.screen = 1;
    sb_fill(0, 0, SCREEN_W, SCREEN_H, C_BG);
    body(22, E.prompt, C_TEXT);
    draw_box();
    draw_keys();
    body(300, "Arrows move, Select presses a key.", C_DIM);
    body(300 + SB_LINE, "Back: close the keyboard.", C_DIM);
}

// left/right along the row; up/down to the key under the column last moved to
static void osk_move(int dr, int dc) {
    int wr = E.row, wi = E.idx;
    if (dc) {
        E.idx += dc;
        if (E.idx < 0) E.idx = 0;
        if (E.idx > row_len(E.row) - 1) E.idx = row_len(E.row) - 1;
        E.col = key_at(E.row, E.idx).c0;
    } else {
        int row = E.row + dr;
        if (row < 0 || row > 4) return;
        int idx = -1;
        for (int i = 0; i < row_len(row); i++) { okey_t k = key_at(row, i); if (E.col >= k.c0 && E.col < k.c0 + k.span) { idx = i; break; } }
        if (idx < 0) idx = row_len(row) - 1;
        E.row = row; E.idx = idx;
    }
    if (E.row != wr || E.idx != wi) { draw_key(wr, wi); draw_key(E.row, E.idx); }
}

static void osk_press(void) {
    okey_t k = key_at(E.row, E.idx);
    if (k.ch) type(k.ch);
    else if (k.act == ACT_SPACE) type(' ');
    else if (k.act == ACT_DEL) type('\b');
    else if (k.act == ACT_SHIFT) { E.shift = !E.shift; draw_keys(); }
    else if (k.act == ACT_PAGE) {
        E.page ^= 1;
        sb_fill(OSK_X, OSK_Y, 10 * OSK_KW + 9 * OSK_GAP, 4 * (OSK_KH + OSK_GAP), C_BG);
        draw_keys();
    } else if (k.act == ACT_DONE && (E.value[0] || E.empty)) { entry_show(); submit(); }
}

bool sb_entry_key(int k) {
    if (!E.active) return false;
    bool printable = k == '\b' || (k >= ' ' && k <= '~');
    if (E.screen == 0) {
        if (k == '\r') { if (E.focus_button) show_osk(); else submit(); }
        else if (k == KEY_BACK) { E.active = false; E.done(NULL); }
        else if (k == KEY_RIGHT) set_focus(true);
        else if (k == KEY_LEFT) set_focus(false);
        else if (printable) type(k);
        else return false;
        return true;
    }
    if (k == KEY_UP) osk_move(-1, 0);
    else if (k == KEY_DOWN) osk_move(1, 0);
    else if (k == KEY_LEFT) osk_move(0, -1);
    else if (k == KEY_RIGHT) osk_move(0, 1);
    else if (k == '\r') osk_press();
    else if (k == KEY_BACK) entry_show();
    else if (printable) type(k);
    else return false;
    return true;
}

// ---------------------------------------------------------------- WiFi setup

#define LIST_Y 124
#define LIST_ROW 24
#define LIST_ROWS 8
#define MAX_NETS 24

static struct {
    int screen;                 // 0 off, 1 scanning, 2 list, 3 password, 4 joining, 5 done
    net_ap_t nets[MAX_NETS];
    int n, sel, top;
    bool ok;
    char ip[20], password[65];
    void (*done)(const char *ssid, const char *ip);
} W;

// signal strength: four bars rising to the right, the ones lit by rssi; bottom at y, right edge at x
#define BARS_W 22
static void bars(int x, int y, int rssi, uint32_t on, uint32_t off) {
    int n = rssi >= -55 ? 4 : rssi >= -67 ? 3 : rssi >= -75 ? 2 : 1;
    for (int i = 0; i < 4; i++) sb_fill(x - BARS_W + i * 6, y - 4 * (i + 1), 4, 4 * (i + 1), i < n ? on : off);
}
static int items(void) { return W.n + 1; }   // + "Scan again"

static void draw_row(int i) {
    int r = i - W.top;
    if (r < 0 || r >= LIST_ROWS) return;
    int y = LIST_Y + r * LIST_ROW;
    bool on = i == W.sel;
    uint32_t bg = on ? C_BAR : C_PANEL, fg = on ? C_BARTEXT : C_TEXT, quiet = on ? C_BARTEXT : C_DIM;
    sb_back_begin(SB_X, y, SB_W, LIST_ROW);   // the row put up whole: no blink as the bar moves
    sb_fill(SB_X, y, SB_W, LIST_ROW, bg);
    int f = on ? SB_FONT_ITEM_SEL : SB_FONT_ITEM, right = SB_X + SB_W - 12;
    char b[48];
    if (i < W.n) {
        sb_print(f, SB_X + 12, y + 2, SB_W - 24 - BARS_W - 60, LIST_ROW - 4, fg, bg, fit(f, W.nets[i].ssid, SB_W - 24 - BARS_W - 64, b, sizeof b));
        sb_print_right(SB_FONT_SMALL, right - BARS_W - 10, y + 2, LIST_ROW - 4, quiet, bg, W.nets[i].secure ? "WPA2" : "open");
        bars(right, y + LIST_ROW - 4, W.nets[i].rssi, fg, on ? C_PANEL : C_BG);
    } else if (i == W.n) sb_print(f, SB_X + 12, y + 2, 0, LIST_ROW - 4, fg, bg, "Scan again");
    sb_back_end();
}

static void draw_rows(void) {
    for (int r = 0; r < LIST_ROWS; r++) draw_row(W.top + r);
    int y = LIST_Y + LIST_ROWS * LIST_ROW + 6;
    sb_fill(SB_X + SB_W - 80, y, 80, 16, C_BG);
    if (W.top + LIST_ROWS < items()) sb_print_right(SB_FONT_SMALL, SB_X + SB_W, y, 16, C_DIM, C_BG, "more");
}

static void show_list(void) {
    static const char *const help[] = { "Up/Down to choose, then Select." };
    W.screen = 2;
    sb_frame("WiFi setup", W.n ? "Choose your network:" : "No networks found.", help, 1);
    sb_fill(SB_X, LIST_Y - 4, SB_W, LIST_ROWS * LIST_ROW + 8, C_PANEL);
    draw_rows();
}

static void scan(void) {
    W.screen = 1;
    sb_frame("WiFi setup", "Looking for networks...", NULL, 0);
    net_scan_start();
}

static void move(int d) {
    int was = W.sel;
    W.sel += d;
    if (W.sel < 0) W.sel = 0;
    if (W.sel > items() - 1) W.sel = items() - 1;
    if (W.sel == was) return;
    if (W.sel < W.top || W.sel >= W.top + LIST_ROWS) {
        int top = W.sel - (d > 0 ? LIST_ROWS - 1 : 0);
        if (top > items() - LIST_ROWS) top = items() - LIST_ROWS;
        W.top = top < 0 ? 0 : top;
        draw_rows();
    } else { draw_row(was); draw_row(W.sel); }
}

static void join_selected(const char *password);

static void password_done(const char *v) {
    if (!v) { show_list(); return; }
    snprintf(W.password, sizeof W.password, "%s", v);
    join_selected(W.password);
}

static void show_password(void) {
    static const char *const help[] = { "Type the password, then Enter, or", "pick Keyboard to use the remote.", "Back: choose another network." };
    static char prompt[64];
    W.screen = 3;
    snprintf(prompt, sizeof prompt, "Password for %s:", W.nets[W.sel].ssid);
    sb_entry_show("WiFi setup", prompt, help, 3, "", 63, false, password_done);
}

static void join_selected(const char *password) {
    W.screen = 4;
    char b[64];
    snprintf(b, sizeof b, "Connecting to %s...", W.nets[W.sel].ssid);
    sb_entry_status(b, C_DIM, 0);
    net_join(W.nets[W.sel].ssid, W.nets[W.sel].secure ? password : "");
}

void sb_wifi_start(void (*done)(const char *ssid, const char *ip)) {
    memset(&W, 0, sizeof W);
    W.done = done;
    scan();
}

void sb_wifi_poll(uint32_t now) {
    (void)now;
    if (W.screen == 1) {
        int n = net_scan_results(W.nets, MAX_NETS);   // one entry a name, strongest first
        if (n < 0) return;          // still scanning
        W.n = n; W.sel = 0; W.top = 0;
        show_list();
    } else if (W.screen == 4) {
        int st = net_join_status();
        if (st == NET_JOIN_PENDING || st == NET_JOIN_IDLE) return;
        W.screen = 5;
        W.ok = st == NET_JOIN_OK;
        if (W.ok) {
            static const char *const help[] = { "Select: continue." };
            char b[64];
            snprintf(settings.ssid, sizeof settings.ssid, "%s", W.nets[W.sel].ssid);
            snprintf(settings.password, sizeof settings.password, "%s", W.nets[W.sel].secure ? W.password : "");
            settings_save();
            net_address(W.ip, sizeof W.ip);
            snprintf(b, sizeof b, "Connected to %s.", W.nets[W.sel].ssid);
            sb_frame("WiFi setup", b, help, 1);
            snprintf(b, sizeof b, "Address %s", W.ip);
            body(124, b, C_GOOD);
            LOGI("[wifi] joined %s for the bridge, %s; saved\n", W.nets[W.sel].ssid, W.ip);
        } else {
            sb_entry_status(st == NET_JOIN_BADAUTH ? "Couldn't connect: wrong password" : st == NET_JOIN_NONET ? "Couldn't connect: network not found"
                            : "Couldn't connect", C_BAD, 0);
            sb_entry_status("Select to try again.", C_DIM, 1);
        }
    }
}

bool sb_wifi_key(int k) {
    switch (W.screen) {
    case 2:
        if (k == KEY_UP || k == KEY_LEFT) move(-1);
        else if (k == KEY_DOWN || k == KEY_RIGHT) move(1);
        else if (k == KEY_SCROLLUP) move(-LIST_ROWS);
        else if (k == KEY_SCROLLDOWN) move(LIST_ROWS);
        else if (k == '\r') {
            if (W.sel == W.n) scan();
            else if (W.nets[W.sel].secure) show_password();
            else join_selected("");
        }
        return true;
    case 3: return sb_entry_key(k);
    case 5:
        if (W.ok) { if (k == '\r') { W.screen = 0; W.done(W.nets[W.sel].ssid, W.ip); } }
        else if (k == '\r') show_password();
        else if (k == KEY_BACK) show_list();
        return true;
    }
    return false;
}
