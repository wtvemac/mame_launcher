// The bridge's own web page (port 80 on the Pico) and the box's console (port 2323), on net.h's
// listening ports; the Pico and its emulator build (sim/) share it.
//
//   GET  /          the page (pico/web/, built into gen/web_page.c by tools/webgen.mjs, gzipped):
//                   tabs for the box's console, info, server settings, its OS, a screenshot and
//                   its disk
//   GET  /status    the bridge's state as text (console_status; the page polls it)
//   GET  /console?from=N   the box's console output from byte N on (up to 4 KB; X-Next says
//                   where to go on from, X-Session whether the OS is in a PARMOD session)
//   POST /console   the body typed on the box's console
//   GET  /dns, POST /dns   the DNS server setting: a dotted quad, or "auto" (the router's, or
//                   8.8.8.8 when it gives none); a POST saves it
//   GET  /service, POST /service   the WTVP server (settings.service): a name or address, or
//                   blank for where the box asks; a POST saves it and the gateway uses it at once
//
//   GET  /mem?addr=A&len=N   box memory, words, streamed (big-endian). On the WebTV Bridge
//                   screen through the loader (RAM, the boot ROM, SOLO registers); while the
//                   patched OS runs, read by the OS itself for the Bridge on PARMOD's control
//                   channel (port_pm_peek; the OS patch's pm_ch_peek): RAM, 64 bytes a peek, or
//                   the SOLO's registers, a word a peek. X-Mem on /status: 1 the one, 2 the
//                   other, 0 neither. The page's Screenshot reads the video registers, then the
//                   framebuffer, or the compositor's y-map, cels and textures (web/boxscreen.js).
//
// The disk, through the loader, only while the TV shows the WebTV Bridge screen (X-Box on
// /status): the first of these holds the screen (no countdown, no Start) until it is done.
//   GET  /disk?lba=L&count=N   disk sectors, as the CPU sees them (the image file's byte pairs
//                   swapped), streamed 64 sectors at a time
//   PUT  /disk?lba=L   the body (whole sectors) written from L on, 64 sectors at a time; TCP
//                   is held while each is written. The page's Box Filesystem tab reads the
//                   partition table and the FAT filesystems with these and writes files back.
//   GET  /os.o      Save this OS: starts it from the bridge's menu on the TV and streams the image
//                   from the box's disk, as it is there (sb_rb_save), 32 KB at a time; the save
//                   waits whenever TCP can't take more
//   POST /install   Install another OS: the body is the image file. It goes into the box's RAM
//                   through the loader 32 KB at a time (TCP is held while each chunk is written),
//                   then onto the slot that isn't booting (sb_rb_install_*)
//
// Port 2323 is a raw TCP console for an OS speaking PARMOD: its output to every client, what
// they type to the box.
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <stdarg.h>
#include "web.h"
#include "net.h"
#include "port.h"
#include "settings.h"
#include "sb/sb.h"
#include "console.h"
#include "log.h"
#include "link_hw.h"
#include "gw.h"
#include "gen/sb_assets.h"
#include "gen/web_page.h"
#ifndef LINK_HOST_SIM
#include "pico/platform.h"
#endif

#define MAX_WEB 6
#define REQ_MAX 1536
#define OUT_MAX 8192
#define STAGE 32768                       // one install chunk (sb_rb_install_chunk)

typedef struct {
    bool used, closing;
    net_conn_t *c;
    char req[REQ_MAX];
    size_t req_len;
    bool have_headers;
    uint32_t body_len, body_got;
    enum { W_REQ, W_REPLY, W_SAVE, W_INSTALL, W_BODY, W_JOB, W_DISKW } mode;
    char body[256];                       // a small request body (POST /console, /dns, /service)
    enum { B_CONSOLE, B_DNS, B_SERVICE } body_for;
    const char *page;                     // a reply body sent from where it is (the page)
    size_t page_left;
    char out[OUT_MAX];                    // reply bytes not yet taken by TCP
    size_t out_len, out_off;
    // Save: the chunk the save is waiting on (sb_rb_chunk_fn)
    const uint8_t *chunk;
    size_t chunk_len, chunk_off;
    bool ended;                           // our side is closed: close when the browser does
} web_t;

static web_t webs[MAX_WEB];
static web_t *saver, *installer;          // the one Save, the one Install
static sb_rb_chunk_fn saved_sink;          // what sb_save_sink was before a web Save
static void (*saved_source)(void);        // and sb_install_source before a web Install
static uint8_t stage[STAGE];
extern uint32_t bridge_now_ms(void);
static size_t stage_len;
static bool chunk_busy, install_started;
static bool save_running;

// ---------------------------------------------------------------- replies

static void put(web_t *w, const char *d, size_t n) {
    if (w->out_len + n > sizeof w->out) n = sizeof w->out - w->out_len;
    memcpy(w->out + w->out_len, d, n);
    w->out_len += n;
}

static void putf(web_t *w, const char *fmt, ...) {
    char b[512];
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(b, sizeof b, fmt, ap);
    va_end(ap);
    if (n > 0) put(w, b, (size_t)n < sizeof b ? (size_t)n : sizeof b - 1);
}

static void flush(web_t *w) {
    while (w->out_off < w->out_len) {
        size_t k = net_tcp_send(w->c, (const uint8_t *)w->out + w->out_off, w->out_len - w->out_off);
        if (!k) return;
        w->out_off += k;
    }
    w->out_len = w->out_off = 0;
}

static void head(web_t *w, int code, const char *type, size_t len, const char *extra) {
    const char *what = code == 200 ? "OK" : code == 404 ? "Not Found" : code == 409 ? "Conflict" : code == 500 ? "Internal Server Error" : "Bad Request";
    putf(w, "HTTP/1.1 %d %s\r\nContent-Type: %s\r\nContent-Length: %u\r\nCache-Control: no-store\r\n%sConnection: close\r\n\r\n",
         code, what, type, (unsigned)len, extra ? extra : "");
    w->mode = W_REPLY;
    w->closing = true;
}

static void reply(web_t *w, int code, const char *type, const char *body) {
    head(w, code, type, strlen(body), NULL);
    put(w, body, strlen(body));
}

// sb_startup_status() and friends print through a function: into a buffer
static char *pbuf;
static size_t plen, pmax;
static void pprint(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vsnprintf(pbuf + plen, pmax - plen, fmt, ap);
    va_end(ap);
    if (n > 0) plen += (size_t)n < pmax - plen ? (size_t)n : pmax - plen - 1;
}

static void status_text(char *b, size_t max) {
    pbuf = b; plen = 0; pmax = max; b[0] = 0;
    char ip[20];
    net_address(ip, sizeof ip);
    pprint("WiFi %s%s%s; WTVP server %s\n", settings.ssid[0] ? settings.ssid : "(not set up)", ip[0] ? ", address " : "", ip,
           settings.service[0] ? settings.service : "(where the box asks)");
    console_status(pprint);
    const char *n = sb_startup_build_name();
    if (n) pprint("  build %s\n", n);
}

// GET /info: what the page shows under its title, "#Group" lines then "label\tvalue" lines: this
// firmware, the box (what the Bridge has seen of it), and the settings
static void info_text(char *b, size_t max) {
    pbuf = b; plen = 0; pmax = max; b[0] = 0;
    pprint("#Firmware\nBuild\tlptbridge, built %s %s\n", __DATE__, __TIME__);
    pprint("Patch table\tversion %d, %u builds\n", SB_PATCH_TABLE_VERSION, (unsigned)sb_patch_count());
#ifdef LINK_HOST_SIM
    pprint("Board\tparallel2usb rev %d (the simulator)\n", link_board.rev);
#else
    pprint("Board\tparallel2usb rev %d, RP2350 A%d\n", link_board.rev, rp2350_rom_version());
#endif
    const sb_rom_info_t *r = sb_session_rom();
    const sb_os_t *os = sb_startup_os();
    bool have = sb_startup_have_os();
    if (r->seen || have) pprint("#Box\n");
    if (r->seen) {
        pprint("Boot ROM\t%s (%s)", r->version[0] ? r->version : "?", r->debug ? "debug" : "production");
        if (r->built_on[0]) pprint(", built %s", r->built_on);
        if (r->built_by[0]) pprint(" by %s", r->built_by);
        pprint("\nStarted\t%s\n", sb_session_via_tier3() ? "through the disk's tier-3 area (test 19)" : "through the debug monitor (test 18)");
    }
    if (have) {
        const char *n = sb_startup_build_name();
        if (n) pprint("OS\t%s, %s\n", n, os->patched ? "patched for WiFi" : "not patched");
        else pprint("OS\tcode %08x, a build the patch table doesn't know\n", (unsigned)os->id.code);
        pprint("OS slot\t%u, %u bytes at 0x%08x\n", (unsigned)os->slot, (unsigned)os->size, (unsigned)os->base);
    }
    pprint("#Settings\n");
    pprint("WiFi\t%s\n", settings.ssid[0] ? settings.ssid : "not set up");
    pprint("WTVP server\t%s\n", settings.service[0] ? settings.service : "where the box asks");
    pprint("DNS server\t%s\n", settings.dns[0] ? settings.dns : "automatic");
    pprint("Remotes\tWebTV remote and keyboard%s%s\n", settings.box_off & BOX_OFF_SONY ? "" : ", Sony WebTV remote",
           settings.box_off & BOX_OFF_PHILIPS ? "" : ", Philips (RC-5)");
    pprint("Turn on by itself\t%s\n", settings.box_off & BOX_OFF_AUTOPOWER ? "no (waits for the power key)" : "yes");
    pprint("Entry\t%s\n", settings.box_off & BOX_OFF_TEST18 ? "tier3 (as a production boot ROM must)" : "auto");
    pprint("Start-up\t%s\n", settings.no_startup ? "off" : "on");
}

// ---------------------------------------------------------------- the box's console, kept for the page

#define CON_LOG 16384                     // power of two
static char con_log[CON_LOG];
static uint32_t con_seq;                  // bytes ever seen

static void con_keep(const uint8_t *d, size_t n) {
    for (size_t i = 0; i < n; i++) con_log[con_seq++ & (CON_LOG - 1)] = (char)d[i];
}

static void console_out(web_t *w, uint32_t from) {
    uint32_t first = con_seq > CON_LOG ? con_seq - CON_LOG : 0;
    if (from < first || from > con_seq) from = first;
    uint32_t n = con_seq - from;
    if (n > 4096) n = 4096;
    char extra[64];
    snprintf(extra, sizeof extra, "X-Next: %lu\r\nX-Session: %d\r\n", (unsigned long)(from + n), port_pm_session() ? 1 : 0);
    head(w, 200, "text/plain; charset=utf-8", n, extra);
    for (uint32_t i = 0; i < n; i++) put(w, &con_log[(from + i) & (CON_LOG - 1)], 1);
}

static void console_in(web_t *w) {
    size_t n = w->body_got;
    if (!port_pm_session()) { reply(w, 409, "text/plain", "The box isn't in a PARMOD session.\n"); return; }
    size_t k = port_pm_console_write((const uint8_t *)w->body, n);
    reply(w, k == n ? 200 : 409, "text/plain", k == n ? "sent\n" : "the box's console is busy\n");
}

static void dns_in(web_t *w) {
    char v[32];
    size_t n = w->body_got < sizeof v - 1 ? w->body_got : sizeof v - 1;
    memcpy(v, w->body, n); v[n] = 0;
    while (n && (v[n - 1] == '\n' || v[n - 1] == '\r' || v[n - 1] == ' ')) v[--n] = 0;
    unsigned a, b, c, d;
    char extra;
    if (!n || !strcmp(v, "auto")) settings.dns[0] = 0;
    else if (sscanf(v, "%u.%u.%u.%u%c", &a, &b, &c, &d, &extra) == 4 && a < 256 && b < 256 && c < 256 && d < 256)
        snprintf(settings.dns, sizeof settings.dns, "%u.%u.%u.%u", a, b, c, d);
    else { reply(w, 400, "text/plain", "An address like 192.168.1.1, or blank for automatic.\n"); return; }
    bool saved = settings_save();
    LOGI("[web] DNS server %s%s\n", settings.dns[0] ? settings.dns : "auto", saved ? ", saved" : "; couldn't save it");
    reply(w, saved ? 200 : 500, "text/plain", saved ? "saved\n" : "couldn't save it\n");
}

static void service_in(web_t *w) {
    char v[64];
    size_t n = w->body_got < sizeof v - 1 ? w->body_got : sizeof v - 1;
    memcpy(v, w->body, n); v[n] = 0;
    while (n && (v[n - 1] == '\n' || v[n - 1] == '\r' || v[n - 1] == ' ')) v[--n] = 0;
    for (size_t i = 0; i < n; i++) {
        char c = v[i];
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '.' || c == '-' || c == ':' || c == '_')) {
            reply(w, 400, "text/plain", "A name like wtv.example.com or an address like 192.168.1.10; blank for where the box asks.\n");
            return;
        }
    }
    snprintf(settings.service, sizeof settings.service, "%s", v);
    gw_set_service(settings.service);
    bool saved = settings_save();
    sb_startup_refresh();
    LOGI("[web] WTVP server %s%s\n", settings.service[0] ? settings.service : "(where the box asks)", saved ? ", saved" : "; couldn't save it");
    reply(w, saved ? 200 : 500, "text/plain", saved ? "saved\n" : "couldn't save it\n");
}

// ---------------------------------------------------------------- the box: memory and the disk (/mem, /disk)
// One job at a time, sharing `stage` with Install (which can't run meanwhile). Each step's reply
// starts the next; a box reset (the loader gone) ends the job, and `gen` makes any late answer
// to an old job harmless.

#define DISK_CHUNK (STAGE / 512)          // sectors a step: SB_SCRATCH holds them

static struct {
    web_t *w;                             // NULL: the browser went away (a read stops; a write ends its step)
    enum { J_NONE, J_MEM, J_DISK_READ, J_DISK_WRITE, J_PEEK_MEM } kind;
    uint32_t gen;
    uint32_t addr, lba;                   // where it starts
    uint32_t total, at;                   // bytes
    size_t have, sent;                    // a read: bytes in stage, bytes of them sent
    bool step;                            // a box request in flight
    // J_PEEK_MEM (the OS running): the patched OS reads its memory for us (port_pm_peek)
    bool regs;                            // SOLO registers: a word a peek
    uint32_t asked, got, fill;            // bytes asked for, bytes come back, bytes in stage
    uint32_t heard;                       // when the box last answered (ms)
} J;
#define PEEK_BYTES 64                     // a peek: 16 words
#define PEEK_AHEAD 2048                   // asked for and not yet back, at most (port.h: under 4 KB)
#define PEEK_QUIET_MS 3000                // no answer this long: the OS isn't answering peeks

#define JOB_CTX ((void *)(uintptr_t)J.gen)
#define JOB_OLD(ctx) ((uint32_t)(uintptr_t)(ctx) != J.gen || J.kind == J_NONE)


static bool job_busy(void) { return J.kind != J_NONE; }

static void peek_forget(void);

static void job_end(void) {
    peek_forget();
    J.kind = J_NONE;
    J.gen++;
    J.step = false;
    sb_startup_web_hold(false, false);
}

// a failure: a reply if nothing has been sent yet, else the connection just ends (the browser sees it short)
static void job_fail(const char *err) {
    LOGI("[web] box %s: %s\n", J.kind == J_DISK_WRITE ? "write" : "read", err);
    web_t *w = J.w;
    bool headed = w && w->mode == W_JOB;
    job_end();
    if (!w) return;
    if (headed) { w->closing = true; return; }
    char b[160];
    snprintf(b, sizeof b, "The box: %s\n", err);
    w->out_len = w->out_off = 0;
    reply(w, 500, "text/plain", b);
}

static void job_step(void);

static void got_chunk(void *ctx, const char *err, uint32_t v0) {
    (void)v0;
    if (JOB_OLD(ctx)) return;
    J.step = false;
    if (err) { job_fail(err); return; }
    J.have = J.total - J.at < STAGE ? J.total - J.at : STAGE;
    J.sent = 0;
    if (!J.w) job_end();                  // nobody to send it to; otherwise web_poll sends it
}

static void disk_read_done(void *ctx, const char *err, uint32_t v0) {
    if (JOB_OLD(ctx)) return;
    if (err || v0) { J.step = false; job_fail(err ? err : "the disk read failed"); return; }
    uint32_t n = J.total - J.at < STAGE ? J.total - J.at : STAGE;
    sb_mem_read(SB_SCRATCH, 4, n / 4, stage, got_chunk, JOB_CTX);
}

// a read: the next chunk into stage (web_poll sends it, then asks for the next)
static void job_step(void) {
    if (J.at >= J.total) {
        web_t *w = J.w;
        job_end();
        if (w) w->closing = true;
        return;
    }
    if (J.kind == J_PEEK_MEM) return;     // job_peek_poll fills stage
    uint32_t n = J.total - J.at < STAGE ? J.total - J.at : STAGE;
    J.step = true;
    if (J.kind == J_DISK_READ) sb_mem_call(SB_DISK_READ, J.lba + J.at / 512, n / 512, SB_SCRATCH, 0, disk_read_done, JOB_CTX);
    else sb_mem_read(J.addr + J.at, 4, n / 4, stage, got_chunk, JOB_CTX);
}

static void job_head(web_t *w, const char *name, const char *extra) {
    char x[256];
    snprintf(x, sizeof x, "Content-Disposition: attachment; filename=\"%s\"\r\n%s", name, extra ? extra : "");
    head(w, 200, "application/octet-stream", J.total, x);
    w->mode = W_JOB;                      // head() said W_REPLY: this one streams
    w->closing = false;
}

static bool job_start(web_t *w, int kind) {
    if (save_running || installer || job_busy()) { reply(w, 409, "text/plain", "The Bridge is busy with the box (another download, write, save or install).\n"); return false; }
    if (kind != J_PEEK_MEM && !sb_startup_web_hold(true, kind == J_DISK_READ || kind == J_DISK_WRITE)) {
        reply(w, 409, "text/plain", "The TV isn't showing the WebTV Bridge screen, so the Bridge can't reach the box now.\n");
        return false;
    }
    uint32_t gen = J.gen;
    memset(&J, 0, sizeof J);
    J.gen = gen + 1;
    J.w = w;
    J.kind = kind;
    return true;
}

static uint32_t qarg(const char *q, const char *name, uint32_t def) {
    size_t n = strlen(name);
    for (const char *p = q; p && *p; ) {
        if (!strncmp(p, name, n) && p[n] == '=') return (uint32_t)strtoul(p + n + 1, NULL, 0);
        p = strchr(p, '&');
        if (p) p++;
    }
    return def;
}

// the OS is running, patched, in its PARMOD session: its driver answers peeks (pm_ch_peek)
static bool os_peekable(void) { return sb_startup_state() == ST_OS && port_mode() == PORT_PARMOD && port_pm_session(); }

// peek bytes still to come for requests a job no longer wants (it ended early, or read past its
// end to a whole peek); dropped before the next job's. A new session loses them all.
static uint32_t pk_stale, pk_sessions;

static void peek_forget(void) {
    if (J.kind == J_PEEK_MEM && J.asked > J.got) pk_stale += J.asked - J.got;
}

static void start_mem(web_t *w, const char *q) {
    uint32_t addr = qarg(q, "addr", 0), len = qarg(q, "len", 0);
    if (!len || len > 0x800000u || (addr | len) & 3) { reply(w, 400, "text/plain", "addr and len, both multiples of 4.\n"); return; }
    if (!sb_startup_box_ready() && os_peekable()) {
        if (!job_start(w, J_PEEK_MEM)) return;
        J.addr = addr; J.total = len;
        J.regs = (addr >> 16) == 0xa400u;
        J.heard = bridge_now_ms();
    } else {
        if (!job_start(w, J_MEM)) return;
        J.addr = addr; J.total = len;
    }
    char name[40];
    snprintf(name, sizeof name, "mem-%08x-%x.bin", (unsigned)addr, (unsigned)len);
    job_head(w, name, NULL);
    job_step();
}

static void start_disk_read(web_t *w, const char *q) {
    uint32_t lba = qarg(q, "lba", 0), count = qarg(q, "count", 1);
    if (!count || count > 0x40000u) { reply(w, 400, "text/plain", "lba and count (up to 262144 sectors).\n"); return; }
    if (!job_start(w, J_DISK_READ)) return;
    J.lba = lba; J.total = count * 512;
    char name[40];
    snprintf(name, sizeof name, "lba-%u-%u.bin", (unsigned)lba, (unsigned)count);
    job_head(w, name, NULL);
    job_step();
}

// /mem while the OS runs: peeks kept PEEK_AHEAD ahead (a word each for registers, 16 for RAM);
// web_poll sends each full stage as it does for the loader's reads
static void job_peek_poll(uint32_t now) {
    port_stats_t st;
    port_get_stats(&st);
    if (st.pm_sessions != pk_sessions) { pk_sessions = st.pm_sessions; pk_stale = 0; }   // a new session: nothing old to come
    while (pk_stale) {                    // what earlier jobs asked for and didn't wait for
        uint8_t junk[64];
        size_t n = port_pm_peek_read(junk, pk_stale < sizeof junk ? pk_stale : sizeof junk);
        if (!n) break;
        pk_stale -= (uint32_t)n;
    }
    if (J.kind != J_PEEK_MEM) return;
    if (!os_peekable()) { job_fail("the box left its PARMOD session"); return; }
    if ((int32_t)(now - J.heard) > PEEK_QUIET_MS) { job_fail("the OS didn't answer (is it patched by this Bridge?)"); return; }
    if (pk_stale) return;
    uint32_t step = J.regs ? 4 : PEEK_BYTES;
    while (J.asked < J.total && J.asked - J.got < PEEK_AHEAD && port_pm_peek(J.addr + J.asked, J.regs)) J.asked += step;
    if (J.have) return;                   // stage is still going out
    uint32_t want = J.total - J.at < STAGE ? J.total - J.at : STAGE;
    size_t n = port_pm_peek_read(stage + J.fill, want - J.fill);
    if (n) J.heard = now;
    J.fill += (uint32_t)n;
    J.got += (uint32_t)n;
    if (J.fill == want) { J.have = want; J.sent = 0; J.fill = 0; }
}

// ---- PUT /disk: the body into stage, a chunk at a time, then SB_SCRATCH, then the disk

static void diskw_feed(void);

static void diskw_written(void *ctx, const char *err, uint32_t v0) {
    if (JOB_OLD(ctx)) return;
    J.step = false;
    if (err || v0) { job_fail(err ? err : "the disk write failed"); return; }
    J.at += (uint32_t)stage_len;
    stage_len = 0;
    if (J.at >= J.total) {
        web_t *w = J.w;
        LOGI("[web] wrote %u sectors from LBA %u\n", (unsigned)(J.total / 512), (unsigned)J.lba);
        job_end();
        if (w) reply(w, 200, "text/plain", "written\n");
        return;
    }
    if (!J.w) { job_fail("the browser went away"); return; }
    net_tcp_kick(J.w->c);
    diskw_feed();
}

static void diskw_copied(void *ctx, const char *err, uint32_t v0) {
    (void)v0;
    if (JOB_OLD(ctx)) return;
    if (err) { J.step = false; job_fail(err); return; }
    sb_mem_call(SB_DISK_WRITE, J.lba + J.at / 512, (uint32_t)stage_len / 512, SB_SCRATCH, 0, diskw_written, JOB_CTX);
}

static void diskw_feed(void) {
    if (J.kind != J_DISK_WRITE || J.step || !J.w) return;
    bool last = J.at + stage_len >= J.total;
    if (stage_len == STAGE || (last && stage_len)) {
        J.step = true;
        sb_mem_write(SB_SCRATCH, 4, (uint32_t)stage_len / 4, stage, diskw_copied, JOB_CTX);
    }
}

static void start_disk_write(web_t *w, const char *q) {
    uint32_t lba = qarg(q, "lba", 0xffffffffu);
    if (lba == 0xffffffffu || !w->body_len || w->body_len % 512) { reply(w, 400, "text/plain", "lba, and a body of whole sectors.\n"); return; }
    if (!job_start(w, J_DISK_WRITE)) return;
    J.lba = lba; J.total = w->body_len;
    stage_len = 0;
    w->mode = W_DISKW;
    LOGI("[web] writing %u sectors from LBA %u\n", (unsigned)(J.total / 512), (unsigned)lba);
}


// ---------------------------------------------------------------- Save (GET /os.o)

// stays the sink until the save ends, even if the browser goes away meanwhile
static bool web_save_sink(const uint8_t *d, size_t n, uint32_t at, uint32_t total) {
    (void)at; (void)total;
    if (!d) {
        sb_save_sink = saved_sink;
        save_running = false;
        if (saver) saver->closing = true;
        return true;
    }
    if (!saver) return true;                      // the browser went away: the save runs out
    saver->chunk = d; saver->chunk_len = n; saver->chunk_off = 0;
    return false;                                  // web_poll sends it, then resumes the save
}

static void start_save(web_t *w) {
    if (save_running || installer || job_busy()) { reply(w, 409, "text/plain", "The Bridge is already saving or installing.\n"); return; }
    saved_sink = sb_save_sink;
    sb_save_sink = web_save_sink;
    saver = w;
    save_running = true;
    if (!sb_startup_web_action(SB_WEB_SAVE)) {
        sb_save_sink = saved_sink; saver = NULL; save_running = false;
        reply(w, 409, "text/plain", "The TV isn't showing the WebTV Bridge screen, so the Bridge can't read the OS now.\n");
        return;
    }
    const char *n = sb_startup_build_name(), *base = n ? strrchr(n, '/') : NULL;
    putf(w, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: %u\r\n"
            "Content-Disposition: attachment; filename=\"%s.o\"\r\nConnection: close\r\n\r\n",
         (unsigned)sb_startup_os()->size, base ? base + 1 : n ? n : "os");
    w->mode = W_SAVE;
    LOGI("[web] Save this OS: %u bytes to the browser\n", (unsigned)sb_startup_os()->size);
}

// ---------------------------------------------------------------- Install (POST /install)

static void install_reply(const char *err) {
    if (!installer) return;
    web_t *w = installer;
    installer = NULL;
    sb_install_source = saved_source;
    char b[160];
    if (err) snprintf(b, sizeof b, "Install failed: %s\n", err);
    else snprintf(b, sizeof b, "Installed in the other slot: the box boots it from now on. Start it from the menu on the TV.\n");
    w->out_len = w->out_off = 0;
    reply(w, err ? 400 : 200, "text/plain", b);
}

static void installed(const char *err) {
    sb_startup_installed(err);
    install_reply(err);
}

static void chunk_done(const char *err);

// the stage is full, or the body is all here: write it into the box
static void install_feed(void) {
    if (!installer || chunk_busy || !install_started) return;
    bool last = installer->body_got >= installer->body_len;
    if (stage_len == STAGE || (last && stage_len)) {
        chunk_busy = true;
        sb_rb_install_chunk(stage, stage_len, chunk_done);
    } else if (last && !stage_len) {
        install_started = false;
        sb_rb_install_end(installed);
    }
}

static void chunk_done(const char *err) {
    chunk_busy = false;
    if (err) { install_started = false; sb_startup_installed(err); install_reply(err); return; }
    stage_len = 0;
    if (installer) net_tcp_kick(installer->c);
    install_feed();
}

// sb_install_source: the start-up asks for the image once its screen is up
static void web_install_source(void) {
    if (!installer || stage_len < 64) { sb_startup_installed("no upload"); return; }
    const char *e = sb_rb_install_begin(sb_startup_os(), stage, installer->body_len);
    if (e) { sb_startup_installed(e); install_reply(e); return; }
    install_started = true;
    install_feed();
}

static void start_install(web_t *w) {
    if (save_running || installer || job_busy()) { reply(w, 409, "text/plain", "The Bridge is already saving or installing.\n"); return; }
    if (w->body_len < 64) { reply(w, 400, "text/plain", "That isn't an OS image.\n"); return; }
    installer = w;
    stage_len = 0;
    chunk_busy = install_started = false;
    w->mode = W_INSTALL;
    LOGI("[web] Install another OS: %u bytes coming\n", (unsigned)w->body_len);
}

// the first 64 bytes are here: start it from the menu
static void install_go(web_t *w) {
    saved_source = sb_install_source;
    sb_install_source = web_install_source;
    if (!sb_startup_web_action(SB_WEB_INSTALL)) {
        sb_install_source = saved_source;
        installer = NULL;
        reply(w, 409, "text/plain", "The TV isn't showing the WebTV Bridge screen, so the Bridge can't install now.\n");
    }
}

// ---------------------------------------------------------------- keys (the Remote tab)
// POST /key?e=94100000,d4100000: each, an IR input entry's first word in hex (port.h,
// port_pm_key): to the WebTV Bridge screen while it is up, else to the running OS when its
// patch takes keys. 200 with how many went; 409 when nothing can take them; 503 when the queue
// to the box is full (what went before stays sent).

// 1: the Bridge's own screens take keys, 2: the running OS does, 0: neither
static int keys_to(void) {
    if (sb_startup_state() != ST_OS) return sb_startup_owns_link() && sb_on_key ? 1 : 0;
    return os_peekable() && sb_startup_os_takes_keys() ? 2 : 0;
}

static void key_in(web_t *w, char *q) {
    int to = keys_to();
    if (!to) {
        reply(w, 409, "text/plain", sb_startup_state() == ST_OS && os_peekable()
              ? "This OS build doesn't take keys from the Bridge.\n" : "The box can't take keys right now.\n");
        return;
    }
    if (!q || strncmp(q, "e=", 2)) { reply(w, 400, "text/plain", "e=<hex>[,<hex>...]\n"); return; }
    int n = 0;
    for (char *p = q + 2; *p;) {
        char *end;
        uint32_t e = (uint32_t)strtoul(p, &end, 16);
        if (end == p) { reply(w, 400, "text/plain", "e=<hex>[,<hex>...]\n"); return; }
        bool ok = to == 1 ? sb_startup_key(e) : port_pm_key(e);
        if (!ok) { char b[48]; snprintf(b, sizeof b, "%d sent; the queue is full\n", n); reply(w, 503, "text/plain", b); return; }
        n++;
        p = *end == ',' ? end + 1 : end;
    }
    char b[24];
    snprintf(b, sizeof b, "%d sent\n", n);
    reply(w, 200, "text/plain", b);
}

// ---------------------------------------------------------------- requests

static void route(web_t *w) {
    char method[8] = "", path[192] = "";
    sscanf(w->req, "%7s %191s", method, path);
    const char *cl = strstr(w->req, "Content-Length:");
    if (!cl) cl = strstr(w->req, "content-length:");
    w->body_len = cl ? (uint32_t)strtoul(cl + 15, NULL, 10) : 0;
    LOGD("[web] %s %s\n", method, path);
    char *q = strchr(path, '?');
    if (q) *q++ = 0;
    if (!strcmp(method, "GET") && !strcmp(path, "/")) {
        head(w, 200, "text/html; charset=utf-8", web_page_gz_len, "Content-Encoding: gzip\r\n");
        w->page = (const char *)web_page_gz; w->page_left = web_page_gz_len;   // sent from flash as it is
    } else if (!strcmp(method, "GET") && !strcmp(path, "/status")) {
        static char b[2048];
        status_text(b, sizeof b);
        // X-Ready: the WebTV Bridge screen is on the TV, so Save and Install can start (the page greys them out otherwise)
        // X-Box: the loader is free for the page's own reads and writes (Screenshot, Box Filesystem)
        // X-Mem: box memory can be read (Screenshot): 1 through the loader, 2 from the running OS (PARMOD)
        // X-Keys: keys can be sent (Remote): 1 to the WebTV Bridge screen, 2 to the running OS
        char extra[80];
        snprintf(extra, sizeof extra, "X-Ready: %d\r\nX-Box: %d\r\nX-Mem: %d\r\nX-Keys: %d\r\n", sb_startup_web_ready() ? 1 : 0,
                 sb_startup_box_ready() ? 1 : 0, sb_startup_box_ready() ? 1 : os_peekable() ? 2 : 0, keys_to());
        head(w, 200, "text/plain; charset=utf-8", strlen(b), extra);
        put(w, b, strlen(b));
    } else if (!strcmp(method, "GET") && !strcmp(path, "/info")) {
        static char b[1536];
        info_text(b, sizeof b);
        reply(w, 200, "text/plain; charset=utf-8", b);
    } else if (!strcmp(method, "GET") && !strcmp(path, "/console")) {
        console_out(w, q && !strncmp(q, "from=", 5) ? (uint32_t)strtoul(q + 5, NULL, 10) : 0);
    } else if (!strcmp(method, "POST") && !strcmp(path, "/console")) {
        if (w->body_len > sizeof w->body) reply(w, 400, "text/plain", "Too long.\n");
        else if (!w->body_len) reply(w, 400, "text/plain", "Nothing to send.\n");
        else w->mode = W_BODY;
    } else if (!strcmp(method, "GET") && !strcmp(path, "/dns")) {
        char b[24];
        snprintf(b, sizeof b, "%s\n", settings.dns[0] ? settings.dns : "auto");
        reply(w, 200, "text/plain", b);
    } else if (!strcmp(method, "POST") && !strcmp(path, "/dns")) {
        if (w->body_len > sizeof w->body) reply(w, 400, "text/plain", "Too long.\n");
        else if (!w->body_len) { w->body_got = 0; dns_in(w); }
        else { w->body_for = B_DNS; w->mode = W_BODY; }
    } else if (!strcmp(method, "GET") && !strcmp(path, "/service")) {
        char b[80];
        snprintf(b, sizeof b, "%s\n", settings.service);
        reply(w, 200, "text/plain", b);
    } else if (!strcmp(method, "POST") && !strcmp(path, "/service")) {
        if (w->body_len > sizeof w->body) reply(w, 400, "text/plain", "Too long.\n");
        else if (!w->body_len) { w->body_got = 0; service_in(w); }
        else { w->body_for = B_SERVICE; w->mode = W_BODY; }
    } else if (!strcmp(method, "GET") && !strcmp(path, "/mem")) start_mem(w, q);
    else if (!strcmp(method, "GET") && !strcmp(path, "/disk")) start_disk_read(w, q);
    else if (!strcmp(method, "PUT") && !strcmp(path, "/disk")) start_disk_write(w, q);
    else if (!strcmp(method, "GET") && !strcmp(path, "/os.o")) start_save(w);
    else if (!strcmp(method, "POST") && !strcmp(path, "/install")) start_install(w);
    else if (!strcmp(method, "POST") && !strcmp(path, "/key")) key_in(w, q);
    else reply(w, 404, "text/plain", "Not here.\n");
}

static size_t web_data(void *ctx, const uint8_t *d, size_t n) {
    web_t *w = ctx;
    if (!w->have_headers) {
        size_t took = 0;
        while (took < n && !w->have_headers) {
            if (w->req_len < sizeof w->req - 1) w->req[w->req_len++] = (char)d[took];
            took++;
            w->req[w->req_len] = 0;
            if (w->req_len >= 4 && !memcmp(w->req + w->req_len - 4, "\r\n\r\n", 4)) w->have_headers = true;
        }
        if (w->have_headers) route(w);
        if (w->mode != W_INSTALL && w->mode != W_BODY && w->mode != W_DISKW) return n;   // anything else in the body is ignored
        return took;                              // the rest comes back as body
    }
    if (w->mode == W_BODY) {
        size_t k = w->body_len - w->body_got < n ? w->body_len - w->body_got : n;
        memcpy(w->body + w->body_got, d, k);
        w->body_got += (uint32_t)k;
        if (w->body_got == w->body_len) {
            if (w->body_for == B_DNS) dns_in(w);
            else if (w->body_for == B_SERVICE) service_in(w);
            else console_in(w);
        }
        return n;
    }
    if (w->mode == W_DISKW) {
        if (J.kind != J_DISK_WRITE || J.w != w || w->body_got >= w->body_len) return n;
        if (J.step || stage_len == STAGE) return 0;   // TCP waits while the box writes
        size_t room = STAGE - stage_len, k = n < room ? n : room;
        if (w->body_got + k > w->body_len) k = w->body_len - w->body_got;
        memcpy(stage + stage_len, d, k);
        stage_len += k;
        w->body_got += (uint32_t)k;
        diskw_feed();
        return k;
    }
    if (w->mode != W_INSTALL || w != installer || w->body_got >= w->body_len) return n;   // past the body: ignored
    if (chunk_busy || stage_len == STAGE) return 0;   // TCP waits while the box writes
    size_t room = STAGE - stage_len, k = n < room ? n : room;
    if (w->body_got + k > w->body_len) k = w->body_len - w->body_got;
    memcpy(stage + stage_len, d, k);
    stage_len += k;
    w->body_got += (uint32_t)k;
    if (!install_started && stage_len >= 64 && sb_install_source != web_install_source) install_go(w);
    else install_feed();
    return k;
}

static void forget(web_t *w) {
    if (w == saver) {
        saver = NULL;
        if (w->chunk) { w->chunk = NULL; sb_rb_save_resume(); }   // let the save run out
    }
    if (w == installer) { installer = NULL; sb_install_source = saved_source; }
    if (J.kind && J.w == w) {
        J.w = NULL;
        if (!J.step) job_end();               // otherwise the step in flight ends it
    }
    w->used = false;
}

// the browser closed its side: once our reply is out, so do we
static void web_end(void *ctx) {
    web_t *w = ctx;
    if (w->ended) { forget(w); net_tcp_close(w->c); }
}

static void web_close(void *ctx, const char *err) { (void)err; forget(ctx); }

static const net_handlers_t web_handlers = { NULL, web_data, web_end, web_close };

static void *web_accept(net_conn_t *c, const net_handlers_t **h) {
    for (int i = 0; i < MAX_WEB; i++) {
        web_t *w = &webs[i];
        if (w->used) continue;
        memset(w, 0, sizeof *w);
        w->used = true;
        w->c = c;
        *h = &web_handlers;
        return w;
    }
    return NULL;
}

// ---------------------------------------------------------------- the console (2323)

#define MAX_CON 3
static net_conn_t *cons[MAX_CON];

static void console_sink(const uint8_t *d, size_t n) {
    con_keep(d, n);
    for (int i = 0; i < MAX_CON; i++) if (cons[i]) net_tcp_send(cons[i], d, n);   // best effort
}

static size_t con_data(void *ctx, const uint8_t *d, size_t n) {
    (void)ctx;
    if (port_mode() != PORT_PARMOD || !port_pm_session()) return n;
    return port_pm_console_write(d, n);
}
static void con_end(void *ctx) { (void)ctx; }
static void con_close(void *ctx, const char *err) {
    (void)err;
    for (int i = 0; i < MAX_CON; i++) if (cons[i] == ctx) cons[i] = NULL;
}
static const net_handlers_t con_handlers = { NULL, con_data, con_end, con_close };

static void *con_accept(net_conn_t *c, const net_handlers_t **h) {
    for (int i = 0; i < MAX_CON; i++) if (!cons[i]) {
        cons[i] = c;
        *h = &con_handlers;
        static const char hello[] = "WebTV Bridge: the box's console (PARMOD)\r\n";
        net_tcp_send(c, (const uint8_t *)hello, sizeof hello - 1);
        return c;
    }
    return NULL;
}

// ---------------------------------------------------------------- set-up and polling

static bool listening;
uint16_t web_port = 80, web_console_port = 2323;

void web_poll(uint32_t now) {
    port_console_sink = console_sink;   // kept from the start, for the page's console
    if (!listening && net_up()) {
        bool a = net_tcp_listen(web_port, web_accept), b = net_tcp_listen(web_console_port, con_accept);
        listening = true;
        LOGI("[web] the bridge's page on port %u%s, the box's console on port %u%s\n", web_port, a ? "" : " (failed)",
             web_console_port, b ? "" : " (failed)");
    }
    // the box reset under a job: the loader is gone, and its answers with it
    if (J.kind && J.kind != J_PEEK_MEM && !sb_startup_owns_link()) job_fail("the box reset");
    job_peek_poll(now);
    for (int i = 0; i < MAX_WEB; i++) {
        web_t *w = &webs[i];
        if (!w->used) continue;
        flush(w);
        while (w->page_left && !w->out_len) {
            size_t k = net_tcp_send(w->c, (const uint8_t *)w->page, w->page_left);
            if (!k) break;
            w->page += k; w->page_left -= k;
        }
        if (w->mode == W_SAVE && w->out_off == 0 && w->out_len == 0 && w->chunk) {
            while (w->chunk_off < w->chunk_len) {
                size_t k = net_tcp_send(w->c, w->chunk + w->chunk_off, w->chunk_len - w->chunk_off);
                if (!k) break;
                w->chunk_off += k;
            }
            if (w->chunk_off == w->chunk_len) { w->chunk = NULL; sb_rb_save_resume(); }
        }
        if (w->mode == W_JOB && J.kind && J.w == w && !w->out_len && J.have) {
            while (J.sent < J.have) {
                size_t k = net_tcp_send(w->c, stage + J.sent, J.have - J.sent);
                if (!k) break;
                J.sent += k;
            }
            if (J.sent == J.have) { J.at += (uint32_t)J.have; J.have = 0; job_step(); }
        }
        if (w->closing && !w->out_len && !w->chunk && !w->page_left) {
            if (w == saver) saver = NULL;
            net_tcp_end(w->c);
            w->closing = false;
            w->ended = true;
        }
    }
}
