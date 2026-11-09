// The OS into the box's RAM, patched there, and started by the boot ROM (WEBTV_LPT_BRIDGE.md
// §10.7), over the loader's disk_read / disk_write / sum_words / boot_ram
// (CALL) and its READ / WRITE. One operation at a time; each step's reply starts the next.
#include <string.h>
#include "sb.h"
#include "../settings.h"
#include "../log.h"
#include "../gen/sb_assets.h"

#define SELECT_LBA   33795u                 // first byte: 0 boots "Browser 0", else "Browser 1"
#define SLOT0        1027u
#define SLOT1        17411u
#define SLOT_SECTORS 16384u
#define BOOT_STRUCT  0xa000059cu            // the boot ROM's OS base and size (0xa0000400 + 0x19c / + 0x1a0)
#define CHUNK        128u                   // sectors a disk_read into RAM (64 KB)
#define SAVE_CHUNK   64u                    // sectors through SCRATCH a time (32 KB)
#define MIN_BASE     0x803c0000u
#define RAM_END      0x80800000u

void (*sb_rb_progress)(int permille);

static uint32_t kseg1(uint32_t a) { return (a & 0x1fffffffu) | 0xa0000000u; }
static uint32_t be32(const uint8_t *p) { return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3]; }

static struct {
    sb_os_t *os;
    sb_rb_done_fn done;
    uint32_t slot, base, size, at, total, run;
    const uint8_t *runs;
    uint8_t hdr[64], trailer[8], byte;
    bool after;                     // check(): after patching
    sb_rb_chunk_fn chunk;
    void (*then)(void);             // check()'s continuation
    bool held;                  // Save: the sink still has the last chunk
} B;

static uint8_t big[SAVE_CHUNK * 512];   // Save's chunk, Install's staging

static void progress(uint32_t done, uint32_t total) { if (sb_rb_progress && total) sb_rb_progress((int)(done * 1000ull / total)); }
static void fail(const char *err) { LOGI("[ramboot] %s\n", err); sb_rb_done_fn d = B.done; B.done = NULL; if (d) d(err); }
static void ok(void) { sb_rb_done_fn d = B.done; B.done = NULL; if (d) d(NULL); }
static bool call(uint32_t fn, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, sb_done_fn cb) { return sb_mem_call(fn, a0, a1, a2, a3, cb, NULL); }

// ---------------------------------------------------------------- the identity, checked as the boot ROM will

static void check_fs(void *ctx, const char *err, uint32_t sum) {
    (void)ctx;
    if (err) { fail(err); return; }
    if (sum != B.os->id.fs_sum) { fail("ROM filesystem sum doesn't match its trailer"); return; }
    B.then();
}
static void check_code(void *ctx, const char *err, uint32_t sum) {
    (void)ctx;
    if (err) { fail(err); return; }
    if (sum != B.os->id.code) { fail("code checksum doesn't match the header"); return; }
    if (!B.os->id.has_fs) { B.then(); return; }
    call(SB_SUM_WORDS, kseg1(B.base) + B.os->id.fs_start, B.os->id.fs_words, 0xffffffffu, 0, check_fs);
}
static void check_trailer(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    if (!sb_identity(B.hdr, be32(B.trailer), be32(B.trailer + 4), &B.os->id)) { fail("not an OS image"); return; }
    call(SB_SUM_WORDS, kseg1(B.base), B.os->id.codewords, 2, 0, check_code);
}
static void check_header(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    sb_mem_read(kseg1(B.base) + B.size - 8, 4, 2, B.trailer, check_trailer, NULL);
}
static void check(void (*then)(void)) {
    B.then = then;
    sb_mem_read(kseg1(B.base), 4, 16, B.hdr, check_header, NULL);
}

// ---------------------------------------------------------------- load

static void select_read(void *ctx, const char *err, uint32_t v0);

static void loaded(void) {
    B.os->slot = B.slot; B.os->base = B.base; B.os->size = B.size;
    B.os->has_patch = false; B.os->patched = false;
    LOGI("[ramboot] slot %u: base 0x%08x, %u bytes, checked\n", (unsigned)B.slot, (unsigned)B.base, (unsigned)B.size);
    ok();
}

static void read_more(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("disk read failed"); return; }
    uint32_t sectors = (B.size + 511) / 512;
    if (B.at) progress(B.at, sectors);
    if (B.at >= sectors) { check(loaded); return; }
    uint32_t n = sectors - B.at < CHUNK ? sectors - B.at : CHUNK;
    uint32_t lba = B.slot + B.at, dest = kseg1(B.base + B.at * 512);
    B.at += n;
    call(SB_DISK_READ, lba, n, dest, 0, read_more);
}

static void header_got(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    B.base = be32(B.hdr + 0x30); B.size = be32(B.hdr + 0x0c) * 4;
    uint32_t codewords = be32(B.hdr + 0x10);
    if ((be32(B.hdr) >> 16) != 0x1000) { fail("no OS in this slot"); return; }
    if (B.base < MIN_BASE || B.base + B.size > RAM_END || B.size > SLOT_SECTORS * 512 || codewords * 4 > B.size) {
        fail("the OS header makes no sense"); return;
    }
    B.at = 0;
    read_more(NULL, NULL, 0);
}

static void header_read(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("can't read the OS header"); return; }
    sb_mem_read(SB_SCRATCH, 4, 16, B.hdr, header_got, NULL);
}

static void select_got(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    B.slot = B.byte ? SLOT1 : SLOT0;
    call(SB_DISK_READ, B.slot, 1, SB_SCRATCH, 0, header_read);
}

static void select_read(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("can't read the select block"); return; }
    sb_mem_read(SB_SCRATCH, 1, 1, &B.byte, select_got, NULL);
}

void sb_rb_load(sb_os_t *os, sb_rb_done_fn done) {
    memset(&B, 0, sizeof B);
    B.os = os; B.done = done;
    call(SB_DISK_READ, SELECT_LBA, 1, SB_SCRATCH, 0, select_read);
}

// ---------------------------------------------------------------- patch

static void patched(void) {
    const sb_patch_entry_t *e = &B.os->patch;
    if (B.os->id.code != e->after_code || (e->flags & 1 && B.os->id.fs_sum != e->after_fs)) { fail("after patching the build doesn't read as expected"); return; }
    B.os->patched = true;
    LOGI("[ramboot] patched (%s)\n", sb_patch_name(e));
    ok();
}

static void write_run(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    if (B.run == B.total) { check(patched); return; }
    uint32_t off = be32(B.runs), n = be32(B.runs + 4);
    const uint8_t *words = B.runs + 8;
    B.runs = words + n * 4;
    B.run++;
    sb_mem_write(kseg1(B.base) + off, 4, n, words, write_run, NULL);
}

void sb_rb_patch(sb_os_t *os, sb_rb_done_fn done) {
    sb_os_t *keep = os;
    memset(&B, 0, sizeof B);
    B.os = keep; B.done = done; B.base = os->base; B.size = os->size; B.slot = os->slot;
    const sb_patch_entry_t *e = sb_patch_find(&os->id);
    if (!e) { B.done = NULL; done("unknown"); return; }
    os->patch = *e; os->has_patch = true;
    B.runs = sb_patches + e->runs + 4;
    B.total = be32(sb_patches + e->runs);
    write_run(NULL, NULL, 0);
}

// ---------------------------------------------------------------- the settings that change the OS

// the words as the patch left them (the table), changed as settings.box_off asks; written every
// start, so a setting turned back on goes back
static struct { uint8_t words[3][4]; uint32_t offs[3], sum; int n, at; } O;

static void put_be(uint8_t *p, uint32_t v) { p[0] = (uint8_t)(v >> 24); p[1] = (uint8_t)(v >> 16); p[2] = (uint8_t)(v >> 8); p[3] = (uint8_t)v; }

static void options_checked(void) {
    if (B.os->id.code != O.sum) { fail("after the settings the build doesn't read as expected"); return; }
    ok();
}

static void write_option(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    if (O.at == O.n) { check(options_checked); return; }
    int i = O.at++;
    sb_mem_write(kseg1(B.base) + O.offs[i], 4, 1, O.words[i], write_option, NULL);
}

void sb_rb_options(sb_os_t *os, sb_rb_done_fn done) {
    sb_os_t *keep = os;
    memset(&B, 0, sizeof B);
    memset(&O, 0, sizeof O);
    B.os = keep; B.done = done; B.base = os->base; B.size = os->size; B.slot = os->slot;
    const sb_patch_entry_t *e = &os->patch;
    if (!os->patched) { ok(); return; }
    O.sum = e->after_code;
    uint32_t ir_mask = (settings.box_off & BOX_OFF_SONY ? 0 : 1) | (settings.box_off & BOX_OFF_PHILIPS ? 0 : 2);
    if (e->ir_off) {
        uint32_t w = e->ir_word | ir_mask;            // ori v0, v0, 4 | Sony 1 | Philips 2
        O.sum += w - e->ir_word;
        O.offs[O.n] = e->ir_off; put_be(O.words[O.n++], w);
    }
    if (e->power_off) {
        uint32_t w = settings.box_off & BOX_OFF_AUTOPOWER ? e->power_word : 0;   // the kPowerOnReboot test, or a nop
        O.sum += w - e->power_word;
        O.offs[O.n] = e->power_off; put_be(O.words[O.n++], w);
    }
    if (!O.n) { ok(); return; }
    O.offs[O.n] = 8; put_be(O.words[O.n++], O.sum);  // the code checksum (header word 2)
    LOGI("[ramboot] settings: remotes WebTV%s%s, %s\n", ir_mask & 1 ? " Sony" : "", ir_mask & 2 ? " Philips" : "",
         e->power_off ? (settings.box_off & BOX_OFF_AUTOPOWER ? "waits for the power key" : "powers on by itself") : "no power-on site");
    write_option(NULL, NULL, 0);
}

// ---------------------------------------------------------------- boot (no reply: the box resets)

static void never(void *ctx, const char *err, uint32_t v0) { (void)ctx; (void)err; (void)v0; }
void sb_rb_boot(const sb_os_t *os) { call(SB_BOOT_RAM, os->base, os->size, BOOT_STRUCT, 0, never); }

// ---------------------------------------------------------------- Save this OS: from the disk, as it is there

static void save_more(void *ctx, const char *err, uint32_t v0);
static void save_got(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    uint32_t bytes = B.size - B.at * 512 < SAVE_CHUNK * 512 ? B.size - B.at * 512 : SAVE_CHUNK * 512;
    bool taken = B.chunk(big, bytes, B.at * 512, B.size);
    B.at += SAVE_CHUNK;
    progress(B.at * 512 < B.size ? B.at * 512 : B.size, B.size);
    if (taken) save_more(NULL, NULL, 0);
    else B.held = true;             // the sink sends `big` on; sb_rb_save_resume() when it's done
}
void sb_rb_save_resume(void) {
    if (!B.held) return;
    B.held = false;
    save_more(NULL, NULL, 0);
}
static void save_read(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("disk read failed"); return; }
    uint32_t bytes = B.size - B.at * 512 < SAVE_CHUNK * 512 ? B.size - B.at * 512 : SAVE_CHUNK * 512;
    sb_mem_read(SB_SCRATCH, 4, bytes / 4, big, save_got, NULL);
}
static void save_more(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)err; (void)v0;
    if (B.at * 512 >= B.size) { ok(); return; }
    uint32_t sectors = (B.size + 511) / 512, n = sectors - B.at < SAVE_CHUNK ? sectors - B.at : SAVE_CHUNK;
    call(SB_DISK_READ, B.slot + B.at, n, SB_SCRATCH, 0, save_read);
}

void sb_rb_save(const sb_os_t *os, sb_rb_chunk_fn chunk, sb_rb_done_fn done) {
    memset(&B, 0, sizeof B);
    B.done = done; B.chunk = chunk; B.slot = os->slot; B.size = os->size;
    save_more(NULL, NULL, 0);
}

// ---------------------------------------------------------------- Install another OS, streamed

const char *sb_rb_install_begin(sb_os_t *os, const uint8_t hdr[64], uint32_t total) {
    memset(&B, 0, sizeof B);
    B.os = os;
    if ((be32(hdr) >> 16) != 0x1000) return "not an OS image";
    B.base = be32(hdr + 0x30); B.size = be32(hdr + 0x0c) * 4;
    if (B.base < MIN_BASE || B.base + B.size > RAM_END || B.size > SLOT_SECTORS * 512) return "this image doesn't fit";
    if (total < B.size) return "the image is shorter than its header says";
    B.total = B.size;               // only the build's bytes go in (files can be longer)
    return NULL;
}

static void chunk_written(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) fail(err); else ok();
}

void sb_rb_install_chunk(const uint8_t *d, size_t n, sb_rb_done_fn done) {
    B.done = done;
    if (B.at >= B.total) { ok(); return; }
    if (n > sizeof big) n = sizeof big;
    if (n > B.total - B.at) n = B.total - B.at;
    memcpy(big, d, n);
    while (n % 4) big[n++] = 0;
    uint32_t at = B.at;
    B.at += (uint32_t)n;
    progress(B.at, B.total);
    sb_mem_write(kseg1(B.base) + at, 4, (uint32_t)n / 4, big, chunk_written, NULL);
}

static void flipped(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("can't write the select block"); return; }
    LOGI("[ramboot] installed in slot %u; it boots from now on\n", (unsigned)B.slot);
    loaded();
}
static void flip_write(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    call(SB_DISK_WRITE, SELECT_LBA, 1, SB_SCRATCH, 0, flipped);
}
static void flip_read(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("can't read the select block"); return; }
    B.byte = B.slot == SLOT1 ? 1 : 0;
    sb_mem_write(SB_SCRATCH, 1, 1, &B.byte, flip_write, NULL);
}
static void write_more(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("disk write failed"); return; }
    uint32_t sectors = (B.size + 511) / 512;
    if (B.at) progress(B.at, sectors);
    if (B.at >= sectors) { call(SB_DISK_READ, SELECT_LBA, 1, SB_SCRATCH, 0, flip_read); return; }
    uint32_t n = sectors - B.at < CHUNK ? sectors - B.at : CHUNK;
    uint32_t lba = B.slot + B.at, src = kseg1(B.base + B.at * 512);
    B.at += n;
    call(SB_DISK_WRITE, lba, n, src, 0, write_more);
}
static void install_checked(void) {
    B.at = 0;
    write_more(NULL, NULL, 0);
}
static void install_select_got(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { fail(err); return; }
    B.slot = B.byte ? SLOT0 : SLOT1;      // the one that isn't booting
    check(install_checked);
}
static void install_select(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { fail("can't read the select block"); return; }
    sb_mem_read(SB_SCRATCH, 1, 1, &B.byte, install_select_got, NULL);
}

void sb_rb_install_end(sb_rb_done_fn done) {
    B.done = done;
    if (B.at < B.total) { fail("the image ended early"); return; }
    call(SB_DISK_READ, SELECT_LBA, 1, SB_SCRATCH, 0, install_select);
}
