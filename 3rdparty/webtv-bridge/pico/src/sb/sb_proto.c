// The loader protocol (src/stockbox/protocol.js): commands to the loader, the parser for what it
// sends back. Bytes go over PARMOD's modem channel (port.h); a buffer here takes what the
// channel's ring can't yet (a WRITE of 1 KB and a screen of text can come at once).
#include <string.h>
#include "sb.h"
#include "../port.h"
#include "../ring.h"
#include "../log.h"

enum { CMD_HELLO = 0x01, CMD_FILL = 0x02, CMD_TEXT = 0x03, CMD_SYNC = 0x04, CMD_RESET = 0x05, CMD_READ = 0x06,
       CMD_WRITE = 0x07, CMD_CALL = 0x08 , CMD_TEXT2 = 0x09, CMD_VFILT = 0x0a, CMD_TARGET = 0x0b, CMD_COPY = 0x0c };
enum { MSG_HELLO = 0x81, MSG_SYNC = 0x84, MSG_IR = 0x90, MSG_ERR = 0xee, MSG_LOADED = 0x8a, MSG_BADSUM = 0x8b,
       MSG_DATA = 0x92, MSG_DONE = 0x93, MSG_RET = 0x94, MSG_FAULT = 0x9f };

// RGB (0-255) -> a Y Cb Y Cr word, ITU-R BT.601 studio range
uint32_t sb_yuyv(int r, int g, int b) {
    int y = (int)(16 + (65.481 * r + 128.553 * g + 24.966 * b) / 255 + 0.5);
    int cb = (int)(128 + (-37.797 * r - 74.203 * g + 112 * b) / 255 + 0.5);
    int cr = (int)(128 + (112 * r - 93.786 * g - 18.214 * b) / 255 + 0.5);
    return ((uint32_t)y << 24) | ((uint32_t)cb << 16) | ((uint32_t)y << 8) | (uint32_t)cr;
}

static uint8_t out_storage[16384];
static ring_t out = RING_INIT(out_storage);

void sb_send_flush(void) {
    uint8_t buf[256];
    size_t room;
    while (!ring_empty(&out) && (room = port_pm_modem_free()) > 0) {
        size_t n = ring_read(&out, buf, room < sizeof buf ? room : sizeof buf);
        port_pm_modem_write(buf, n);
    }
}

void sb_send(const uint8_t *d, size_t n) {
    while (n) {
        size_t k = ring_write(&out, d, n);
        d += k; n -= k;
        sb_send_flush();
        if (n && !k) break;       // full: what can't go is dropped (the screen loses a draw)
    }
    sb_send_flush();
}

size_t sb_send_pending(void) { return ring_used(&out); }

static uint8_t *u16(uint8_t *p, uint32_t v) { *p++ = (uint8_t)(v >> 8); *p++ = (uint8_t)v; return p; }
static uint8_t *u32(uint8_t *p, uint32_t v) { *p++ = (uint8_t)(v >> 24); *p++ = (uint8_t)(v >> 16); *p++ = (uint8_t)(v >> 8); *p++ = (uint8_t)v; return p; }

void sb_hello(void) { uint8_t c = CMD_HELLO; sb_send(&c, 1); }

void sb_fill(int x, int y, int w, int h, uint32_t colour) {
    uint8_t b[13], *p = b;
    *p++ = CMD_FILL;
    p = u16(p, (uint32_t)x); p = u16(p, (uint32_t)y); p = u16(p, (uint32_t)w); p = u16(p, (uint32_t)h); p = u32(p, colour);
    sb_send(b, (size_t)(p - b));
}

void sb_text(int x, int y, int scale, uint32_t fg, uint32_t bg, const char *s) {
    uint8_t b[15 + 255], *p = b;
    size_t n = strlen(s);
    if (n > 255) n = 255;
    *p++ = CMD_TEXT;
    p = u16(p, (uint32_t)x); p = u16(p, (uint32_t)y); *p++ = (uint8_t)scale; p = u32(p, fg); p = u32(p, bg);
    *p++ = (uint8_t)n;
    memcpy(p, s, n); p += n;
    sb_send(b, (size_t)(p - b));
}

// text in one of the boot ROM's font strikes (its address in the box: sb_font.c), the pen at x,
// the baseline at y, blended towards fg over what is on the screen (loader 4+)
void sb_text2(uint32_t strike, int x, int y, uint32_t fg, const char *s) {
    uint8_t b[14 + 255], *p = b;
    size_t n = strlen(s);
    if (n > 255) n = 255;
    *p++ = CMD_TEXT2;
    p = u32(p, strike); p = u16(p, (uint32_t)x); p = u16(p, (uint32_t)y); p = u32(p, fg);
    *p++ = (uint8_t)n;
    memcpy(p, s, n); p += n;
    sb_send(b, (size_t)(p - b));
}

// the OS's vertical flicker filter over a rectangle (loader 4+)
void sb_vfilt(int x, int y, int w, int h) {
    uint8_t b[9], *p = b;
    *p++ = CMD_VFILT;
    p = u16(p, (uint32_t)x); p = u16(p, (uint32_t)y); p = u16(p, (uint32_t)w); p = u16(p, (uint32_t)h);
    sb_send(b, (size_t)(p - b));
}

// FILL, TEXT, TEXT2 and VFILT draw on the screen (0) or the loader's back buffer (1) (loader 5+)
void sb_target(int which) {
    uint8_t b[2] = { CMD_TARGET, (uint8_t)(which != 0) };
    sb_send(b, 2);
}

// a rectangle from the back buffer to the screen (SB_TO_SCREEN) or the other way (loader 5+)
void sb_copy(int dir, int x, int y, int w, int h) {
    uint8_t b[10], *p = b;
    *p++ = CMD_COPY;
    *p++ = (uint8_t)dir;
    p = u16(p, (uint32_t)x); p = u16(p, (uint32_t)y); p = u16(p, (uint32_t)w); p = u16(p, (uint32_t)h);
    sb_send(b, (size_t)(p - b));
}

// ---------------------------------------------------------------- what the loader sends

void (*sb_on_message)(const sb_msg_t *m);

static struct {
    uint8_t buf[3 + 1024 * 4];
    size_t len, need;
    int width;
} P;

void sb_parser_reset(void) { P.len = 0; P.need = 0; }
void sb_parser_width(int width) { P.width = width; }

static void emit(sb_msg_t *m) {
    P.len = 0;
    if (sb_on_message) sb_on_message(m);
}

void sb_parser_feed(uint8_t b) {
    if (P.len >= sizeof P.buf) P.len = 0;   // cannot happen with the loader's lengths
    uint8_t *m = P.buf;
    m[P.len++] = b;
    size_t n = P.len;
    sb_msg_t msg = { 0 };
    switch (m[0]) {
    case MSG_LOADED: msg.type = SBM_LOADED; emit(&msg); return;
    case MSG_DONE: msg.type = SBM_DONE; emit(&msg); return;
    case MSG_BADSUM: msg.type = SBM_BADSUM; emit(&msg); return;
    case MSG_RET:
        if (n == 5) { msg.type = SBM_RET; msg.v0 = ((uint32_t)m[1] << 24) | ((uint32_t)m[2] << 16) | ((uint32_t)m[3] << 8) | m[4]; emit(&msg); }
        return;
    case MSG_FAULT: if (n == 2) { msg.type = SBM_FAULT; msg.byte = m[1]; emit(&msg); } return;
    case MSG_HELLO: if (n == 5) { msg.type = SBM_HELLO; msg.byte = m[4]; emit(&msg); } return;
    case MSG_SYNC: if (n == 2) { msg.type = SBM_SYNC; msg.byte = m[1]; emit(&msg); } return;
    case MSG_ERR: if (n == 2) { msg.type = SBM_ERR; msg.byte = m[1]; emit(&msg); } return;
    case MSG_IR:
        if (n >= 2 && n == 2 + 2 * (size_t)m[1]) { msg.type = SBM_IR; msg.data = m + 2; msg.len = n - 2; emit(&msg); }
        return;
    case MSG_DATA:
        if (n == 3) P.need = (size_t)((m[1] << 8) | m[2]) * (size_t)(P.width ? P.width : 1);
        if (n >= 3 && n == 3 + P.need) { msg.type = SBM_DATA; msg.data = m + 3; msg.len = P.need; emit(&msg); }
        return;
    default: msg.type = SBM_JUNK; msg.byte = m[0]; emit(&msg); return;
    }
}

// ---------------------------------------------------------------- requests (boxmem.js)

#define MEM_CHUNK 1024
#define MEM_QUEUE 8
#define MEM_WINDOW 8            // parts of the head request on the link at once (the loader answers in order)

static const uint32_t ranges[][2] = {
    { 0x80000000u, 0x80800000u }, { 0xa0000000u, 0xa0800000u },   // RAM, cached and uncached
    { 0x9fc00000u, 0x9fe00000u }, { 0xbfc00000u, 0xbfe00000u },   // boot ROM
    { 0xa4000000u, 0xa4010000u },                                 // SOLO registers
};
static const char *const faults[] = { "?", "out of range", "unaligned", "bad width" };

static const char *check_range(uint32_t addr, int width, uint32_t count) {
    if (width != 1 && width != 2 && width != 4) return faults[3];
    if (addr % (uint32_t)width) return faults[2];
    uint64_t end = (uint64_t)addr + (uint64_t)width * count;
    for (size_t i = 0; i < sizeof ranges / sizeof ranges[0]; i++) if (addr >= ranges[i][0] && end <= ranges[i][1]) return NULL;
    return faults[1];
}

typedef struct {
    int op;                     // CMD_READ, CMD_WRITE, CMD_CALL
    uint32_t addr, count, sent_items, done_items, args[4];
    int width;
    uint8_t *dst;
    const uint8_t *src;
    sb_done_fn done;
    void *ctx;
} req_t;
static req_t queue[MEM_QUEUE];
static int q_head, q_len;
static int inflight;            // parts of the head request on the link, answers to come
static int stale;               // answers still to come for a request that has already failed
static uint32_t heard_at;       // when the head request last moved (sent, or an answer came)
static bool stall_told;
extern uint32_t bridge_now_ms(void);

void sb_mem_reset(void) { q_head = q_len = inflight = stale = 0; }
bool sb_mem_idle(void) { return !q_len; }

static uint32_t part_items(const req_t *r, uint32_t from) {
    uint32_t per = MEM_CHUNK / (uint32_t)r->width, n = r->count - from;
    return n > per ? per : n;
}

// the head request's parts, up to MEM_WINDOW of them on the link
static void send_parts(void) {
    req_t *r = &queue[q_head];
    heard_at = bridge_now_ms();
    if (r->op == CMD_CALL) {
        if (inflight) return;
        uint8_t b[1 + 4 * 5], *p = b;
        *p++ = CMD_CALL; p = u32(p, r->addr);
        for (int i = 0; i < 4; i++) p = u32(p, r->args[i]);
        inflight = 1;
        sb_send(b, (size_t)(p - b));
        return;
    }
    sb_parser_width(r->width);
    while (inflight < MEM_WINDOW && r->sent_items < r->count) {
        uint8_t b[1 + 4 + 1 + 2 + MEM_CHUNK], *p = b;
        uint32_t n = part_items(r, r->sent_items);
        *p++ = (uint8_t)r->op; p = u32(p, r->addr + r->sent_items * (uint32_t)r->width); *p++ = (uint8_t)r->width; p = u16(p, n);
        if (r->op == CMD_WRITE) { memcpy(p, r->src + r->sent_items * (uint32_t)r->width, n * (uint32_t)r->width); p += n * (uint32_t)r->width; }
        r->sent_items += n;
        inflight++;
        sb_send(b, (size_t)(p - b));
    }
}

static bool enqueue(const req_t *r) {
    const char *bad = r->op == CMD_CALL ? check_range(r->addr, 4, 1) : check_range(r->addr, r->width, r->count);
    if (bad) { r->done(r->ctx, bad, 0); return true; }
    if (q_len >= MEM_QUEUE) return false;
    queue[(q_head + q_len) % MEM_QUEUE] = *r;
    q_len++;
    if (q_len == 1 && !stale) send_parts();
    return true;
}

bool sb_mem_read(uint32_t addr, int width, uint32_t count, uint8_t *dst, sb_done_fn done, void *ctx) {
    req_t r = { .op = CMD_READ, .addr = addr, .width = width, .count = count, .dst = dst, .done = done, .ctx = ctx };
    return enqueue(&r);
}
bool sb_mem_write(uint32_t addr, int width, uint32_t count, const uint8_t *src, sb_done_fn done, void *ctx) {
    req_t r = { .op = CMD_WRITE, .addr = addr, .width = width, .count = count, .src = src, .done = done, .ctx = ctx };
    return enqueue(&r);
}
bool sb_mem_call(uint32_t addr, uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3, sb_done_fn done, void *ctx) {
    req_t r = { .op = CMD_CALL, .addr = addr, .args = { a0, a1, a2, a3 }, .done = done, .ctx = ctx };
    return enqueue(&r);
}

static void finish(const char *err, uint32_t v0) {
    req_t r = queue[q_head];
    q_head = (q_head + 1) % MEM_QUEUE;
    q_len--;
    stale = inflight;           // on an error, the answers to its other parts are still to come
    inflight = 0;
    if (q_len && !stale) send_parts();
    r.done(r.ctx, err, v0);     // may queue more
}

bool sb_mem_message(const sb_msg_t *m) {
    if (m->type != SBM_DATA && m->type != SBM_DONE && m->type != SBM_RET && m->type != SBM_FAULT) return false;
    if (stale) {
        if (!--stale && q_len) send_parts();
        return true;
    }
    if (!inflight || !q_len) return false;
    req_t *r = &queue[q_head];
    heard_at = bridge_now_ms();
    if (stall_told) { LOGI("[stockbox] the loader answered after all\n"); stall_told = false; }
    inflight--;
    if (m->type == SBM_FAULT) { finish(m->byte < 4 ? faults[m->byte] : "fault", 0); return true; }
    if (r->op == CMD_CALL) { finish(m->type == SBM_RET ? NULL : "unexpected reply", m->v0); return true; }
    uint32_t n = part_items(r, r->done_items);
    if (r->op == CMD_READ) {
        if (m->type != SBM_DATA) { finish("unexpected reply", 0); return true; }
        memcpy(r->dst + r->done_items * (uint32_t)r->width, m->data, m->len);
    } else if (m->type != SBM_DONE) { finish("unexpected reply", 0); return true; }
    r->done_items += n;
    if (r->done_items >= r->count) finish(NULL, 0);
    else send_parts();
    return true;
}

static const char *op_name(int op) { return op == CMD_CALL ? "CALL" : op == CMD_READ ? "READ" : op == CMD_WRITE ? "WRITE" : "?"; }

// a request the loader hasn't answered in 10 s: say so (once). Nothing it runs takes that long, so
// the link has most likely lost a byte and the loader is still waiting for the rest of a command.
void sb_mem_poll(uint32_t now) {
    (void)now;
    now = bridge_now_ms();       // the clock heard_at was taken from
    if (!q_len || stall_told || now - heard_at < 10000) return;
    const req_t *r = &queue[q_head];
    stall_told = true;
    LOGI("[stockbox] no answer from the loader in 10 s: %s 0x%08x (%u of %u items sent, %u answered), "
         "%d on the link, %d stale, parser holds %u bytes\n", op_name(r->op), (unsigned)r->addr, (unsigned)r->sent_items,
         (unsigned)r->count, (unsigned)r->done_items, inflight, stale, (unsigned)P.len);
}

void sb_mem_status(void (*print)(const char *fmt, ...)) {
    if (!q_len && !stale) { print("  loader requests: none waiting; parser holds %u bytes\n", (unsigned)P.len); return; }
    const req_t *r = &queue[q_head];
    print("  loader requests: %d queued, head %s 0x%08x args %08x %08x %08x, %u/%u sent, %u answered, %d on the link, "
          "%d stale, quiet %u ms; parser holds %u bytes (first 0x%02x); %u bytes not yet on the cable\n",
          q_len, op_name(r->op), (unsigned)r->addr, (unsigned)r->args[0], (unsigned)r->args[1], (unsigned)r->args[2],
          (unsigned)r->sent_items, (unsigned)r->count, (unsigned)r->done_items, inflight, stale,
          (unsigned)(bridge_now_ms() - heard_at), (unsigned)P.len, P.len ? P.buf[0] : 0, (unsigned)sb_send_pending());
}
