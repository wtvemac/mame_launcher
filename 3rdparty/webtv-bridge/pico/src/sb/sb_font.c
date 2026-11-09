// The boot ROM's own fonts for the Bridge's screens (sb.h): the strikes are found in the box's
// boot ROM filesystem and their widths read through the loader (READ); the loader draws with
// them in place (TEXT2) and applies the OS's flicker filter (VFILT). The formats are in
// src/stockbox/strike.js (the strike) and src/machine/romfs.js (the filesystem: 0x38-byte
// entries, next @0 child @8 data @0xc name @0x1c, the root 0x40 below the ROM's end).
#include <string.h>
#include "sb.h"
#include "../log.h"

#define ROM_HDR 0x9fc00000u          // the boot ROM: header word 3 its size in words, 0x30 its base
#define B 0x3c                      // a strike's tables start here
#define CHARS 224                   // character entries, from the space

static const struct { const char *name; } FACES[SB_FONTS] = {
    [SB_FONT_TITLE] = { "Bold_22" }, [SB_FONT_BODY] = { "Roman_16" }, [SB_FONT_ITEM] = { "Roman_18" },
    [SB_FONT_ITEM_SEL] = { "Bold_18" }, [SB_FONT_SMALL] = { "Roman_14" },
};

typedef struct {
    uint32_t addr;                  // the strike in the boot ROM (0: not found)
    int ascent, descent;
    int16_t adv[CHARS];             // advance, 1/16 px (0: no glyph)
} font_t;

static font_t F[SB_FONTS];
static bool ready;

static struct {
    void (*done)(bool ok);
    uint8_t ent[0x38];              // a filesystem entry
    uint8_t hdr[0x40];
    uint8_t tab[0x708];             // a strike's character tables and counts (B+0xe0 .. B+0x7e8)
    uint8_t met[256 * 4];           // its metrics
    uint32_t at;                    // the entry being read
    bool in_fonts;                  // walking Fonts/'s children
    int k;                          // the strike being read
} L;

static uint32_t be32(const uint8_t *p) { return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3]; }
static uint32_t be16(const uint8_t *p) { return ((uint32_t)p[0] << 8) | p[1]; }

bool sb_font_ready(void) { return ready; }
void sb_font_reset(void) { memset(F, 0, sizeof F); ready = false; }

static void finish(bool ok) {
    int found = 0;
    for (int i = 0; i < SB_FONTS; i++) found += F[i].addr != 0;
    ready = ok && found == SB_FONTS;
    LOGI("[font] %s (%d of %d strikes in the boot ROM)\n", ready ? "the boot ROM's fonts" : "the 5x7 font", found, SB_FONTS);
    void (*d)(bool) = L.done;
    L.done = NULL;
    if (d) d(ready);
}

// ---------------------------------------------------------------- the strikes' widths

static void strike_next(void);

static void got_metrics(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    font_t *f = &F[L.k];
    unsigned mcount = be16(L.tab + 0x700);
    for (int c = 0; c < CHARS; c++) {
        unsigned mi = L.tab[c * 4], gi = L.tab[c * 4 + 1];
        f->adv[c] = gi == 0xff || mi >= mcount ? 0 : (int16_t)be16(L.met + mi * 4);
    }
    L.k++;
    strike_next();
}

static void got_tables(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    unsigned mcount = be16(L.tab + 0x700);       // B+0x7e0
    if (mcount > 256) { finish(false); return; }
    sb_mem_read(F[L.k].addr + B + 0x7e8, 1, mcount * 4, L.met, got_metrics, NULL);
}

static void got_header(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    // version/flags with the bit 3 layout, 4-bit depth (strike.js)
    if (!(be16(L.hdr) & 8) || L.hdr[0x2c] != 4) { LOGI("[font] %s: not a strike this knows\n", FACES[L.k].name); finish(false); return; }
    F[L.k].ascent = (int)be16(L.hdr + 0x34);
    F[L.k].descent = (int)be16(L.hdr + 0x36);
    sb_mem_read(F[L.k].addr + B + 0xe0, 1, sizeof L.tab, L.tab, got_tables, NULL);
}

static void strike_next(void) {
    if (L.k >= SB_FONTS) { finish(true); return; }
    if (!F[L.k].addr) { finish(false); return; }
    sb_mem_read(F[L.k].addr, 1, sizeof L.hdr, L.hdr, got_header, NULL);
}

// ---------------------------------------------------------------- the filesystem walk

static void walk(void);

static void got_entry(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    char name[0x1d];
    memcpy(name, L.ent + 0x1c, 0x1c); name[0x1c] = 0;
    uint32_t next = be32(L.ent), child = be32(L.ent + 8), data = be32(L.ent + 12);
    if (!L.in_fonts) {
        if (!strcmp(name, "Fonts") && !data) { L.in_fonts = true; L.at = child; walk(); return; }
    } else {
        for (int i = 0; i < SB_FONTS; i++) if (!strcmp(name, FACES[i].name)) F[i].addr = data;
    }
    L.at = next;
    walk();
}

static void walk(void) {
    // the entries live in the ROM; anything else ends the walk
    if (L.at < 0x9fc00000u || L.at >= 0x9fe00000u) {
        if (!L.in_fonts) { LOGI("[font] no Fonts/ in the boot ROM\n"); finish(false); return; }
        L.k = 0;
        strike_next();
        return;
    }
    sb_mem_read(L.at, 4, 0x38 / 4, L.ent, got_entry, NULL);
}

static void got_root(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    char name[8];
    memcpy(name, L.ent + 0x1c, 7); name[7] = 0;
    if (strcmp(name, "ROM")) { LOGI("[font] no ROM filesystem root in the boot ROM\n"); finish(false); return; }
    L.in_fonts = false;
    L.at = be32(L.ent + 8);
    walk();
}

static void got_rom_header(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(false); return; }
    uint32_t size = be32(L.hdr + 0x0c) * 4, base = be32(L.hdr + 0x30);
    if (base != ROM_HDR || size < 0x1000 || size > 0x200000) { LOGI("[font] the boot ROM's header makes no sense\n"); finish(false); return; }
    sb_mem_read(base + size - 0x40, 4, 0x38 / 4, L.ent, got_root, NULL);
}

void sb_font_load(void (*done)(bool ok)) {
    sb_font_reset();
    memset(&L, 0, sizeof L);
    L.done = done;
    sb_mem_read(ROM_HDR, 4, 0x40 / 4, L.hdr, got_rom_header, NULL);
}

// ---------------------------------------------------------------- measuring and printing

int sb_font_width(int f, const char *s) {
    if (!ready) return (int)strlen(s) * CELL_W;
    int pen = 0;
    for (; *s; s++) {
        unsigned c = (uint8_t)*s;
        if (c >= 0x20) pen += F[f].adv[c - 0x20];
    }
    return (pen + 8) >> 4;
}

int sb_font_height(int f) { return ready ? F[f].ascent + F[f].descent : CELL_H; }

// Drawing off the screen (sb.h): the box, with a row above and below (the flicker filter reads
// them), from the screen into the loader's back buffer; drawing goes there until the matching
// end, which copies the box back in one go. Nested pairs join the outermost one.
static struct { int depth, x, y, w, h; } BK;

void sb_back_begin(int x, int y, int w, int h) {
    if (BK.depth++) return;
    BK.x = x; BK.y = y; BK.w = w; BK.h = h;
    int top = y > 0 ? y - 1 : 0;
    sb_copy(SB_TO_BACK, x, top, w, y + h + 1 - top);
    sb_target(1);
}

void sb_back_end(void) {
    if (!BK.depth || --BK.depth) return;
    sb_target(0);
    sb_copy(SB_TO_SCREEN, BK.x, BK.y, BK.w, BK.h);
}

// The box filled, the text blended on and filtered, all off the screen, then copied to it in one
// go: drawn on the screen itself, the erase showed for a moment first (the countdown blinked
// every second).
void sb_print(int f, int x, int y, int w, int h, uint32_t fg, uint32_t bg, const char *s) {
    if (!w) w = sb_font_width(f, s);
    sb_back_begin(x, y, w, h);
    sb_fill(x, y, w, h, bg);
    if (!ready) sb_text(x, y + (h - CELL_H) / 2, 1, fg, bg, s);
    else {
        int base = y + (h - (F[f].ascent + F[f].descent)) / 2 + F[f].ascent;
        sb_text2(F[f].addr, x, base, fg, s);
        sb_vfilt(x, y, w, h);
    }
    sb_back_end();
}

void sb_print_right(int f, int right, int y, int h, uint32_t fg, uint32_t bg, const char *s) {
    int w = sb_font_width(f, s);
    sb_print(f, right - w, y, w, h, fg, bg, s);
}
