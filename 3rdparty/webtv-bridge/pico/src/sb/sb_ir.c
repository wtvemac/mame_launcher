// IR decoding for the stock-box loader: a port of src/stockbox/irdecode.js, itself a port of the
// WebTV OS's own IR stack (16406): the Sejin keyboard decoder, the WebTV remote's (EchoStar style),
// Sony's WebTV remote (20-bit SIRC) and the frames -> keys tables. See irdecode.js for the details; this follows it line for line.
#include <string.h>
#include "sb.h"

#define IR_US_PER_TICK 15

void (*sb_on_key)(int key);
static void key_out(int k) { if (sb_on_key) sb_on_key(k); }

// ---------------------------------------------------------------- keyboard (Sejin)

enum { BIT_OFF_MIN = 715, BIT_OFF_MAX = 1300, BIT_ON_MIN = 555, BIT_ON_MAX = 810, BITCELL = 771, HALF_BITCELL = 340 };

static struct { int state; uint32_t header, data; int count, parity, bit10; } S = { .state = 1 };

static void sj_clear(void) { S.header = 0; S.data = 0; S.count = 0; S.parity = 0; S.bit10 = 0; }

static void sj_test(int level, int us, int want, int mult, int next) {
    int min = want ? BIT_ON_MIN : BIT_OFF_MIN, max = want ? BIT_ON_MAX : BIT_OFF_MAX;
    S.state = level == want && us >= mult * min && us <= mult * max ? next : 1;
}

static void sj_get_bit(int bit) {
    S.data = (S.data >> 1) | (bit ? 0x80000000u : 0);
    if (bit) S.parity = (S.parity + 1) & 0xff;
    S.count = (S.count + 1) & 0xff;
    if (S.count == 10) S.bit10 = bit;
}

static void kb_frame(uint32_t header, uint32_t code);

static void sj_validate(void) {
    uint32_t code = (S.data >> 23) & 0xff;
    if (S.bit10) S.parity = (S.parity - 1) & 0xff;
    int odd = S.parity & 1;
    if ((odd && !S.bit10) || (!odd && S.bit10)) kb_frame(S.header, code);
}

static void sj_feed(int level, int us) {
    switch (S.state) {
    case 1: sj_clear(); sj_test(level, us, 0, 3, 3); break;
    case 2: sj_test(level, us, 0, 3, 3); break;
    case 3: sj_test(level, us, 1, 1, 4); break;
    case 4: sj_test(level, us, 0, 1, 5); break;
    case 5: sj_test(level, us, 1, 1, 6); break;
    case 6: sj_test(level, us, 0, 2, 7); if (S.state == 1) sj_test(level, us, 0, 1, 10); break;
    case 7: sj_test(level, us, 1, 1, 8); break;
    case 8: sj_test(level, us, 0, 1, 9); break;
    case 9: sj_test(level, us, 1, 1, 11); if (S.state != 1) S.header = 0x94; break;
    case 10: sj_test(level, us, 1, 4, 11); if (S.state != 1) S.header = 0xd4; break;
    case 11: case 12:
        if (level != 0) { S.state = 1; break; }
        us -= HALF_BITCELL;
        if (us >= BITCELL && S.count < 9) {
            for (;;) {
                us -= BITCELL;
                S.count++;
                if (us < BITCELL || S.count >= 9) break;
            }
        }
        if (S.count == 8) sj_get_bit(1);
        if (S.count == 9) { sj_validate(); S.state = 1; } else S.state = 13;
        break;
    case 13:
        sj_get_bit(level);
        us -= HALF_BITCELL;
        if (us >= BITCELL) {
            while (S.count < 9) {
                sj_get_bit(level);
                us -= BITCELL;
                if (us < BITCELL) break;
            }
        }
        if (S.count == 8) sj_get_bit(1);
        if (S.count == 9) { sj_validate(); S.state = 1; }
        break;
    default: S.state = 1;
    }
}

// ---------------------------------------------------------------- the WebTV remote

enum { OFF_MIN = 400, OFF_MAX = 705, GAP_MIN = 5700, GAP_MAX = 6150, ZERO_MIN = 2400, ZERO_MAX = 2830, ONE_MIN = 1340, ONE_MAX = 1630 };

static struct { int state, count; uint32_t bits; } R;

static void remote_frame(int code);

static int rc_update(int bit) {
    R.count++;
    R.bits = (R.bits << 1) | (uint32_t)bit;
    if (R.count == 13) { R.count = 0; return 5; }
    return 3;
}

static void rc_post(void) {
    int key = (int)((R.bits >> 7) & 0x3f), addr = (int)((R.bits >> 2) & 0x1f), type = (int)(R.bits & 3);
    if (addr == 0 && type == 0) remote_frame(key);   // type 3: the key's release, absorbed by the hold rule
    else if (addr == 0 && type == 3) remote_frame(key);
}

static void rc_feed(int level, int us) {
    bool off = level == 0 && us >= OFF_MIN && us <= OFF_MAX;
    bool gap = level == 1 && us >= GAP_MIN && us <= GAP_MAX;
    switch (R.state) {
    case 0: R.count = 0; R.bits = 0; /* fall through */
    case 1: if (off) R.state = 2; break;
    case 2: R.state = gap ? 3 : 0; break;
    case 3: R.state = off ? 4 : 0; break;
    case 4:
        if (level == 1 && us >= ZERO_MIN && us <= ZERO_MAX) R.state = rc_update(0);
        else if (level == 1 && us >= ONE_MIN && us <= ONE_MAX) R.state = rc_update(1);
        else R.state = 0;
        break;
    case 5: R.state = off ? 6 : 0; break;
    case 6: if (gap) { rc_post(); R.state = 3; } else R.state = 0; break;
    default: R.state = 0;
    }
}

// ---------------------------------------------------------------- Sony's WebTV remote (DecodeSony)
// 20-bit SIRC, least significant bit first: a ~2.4 ms mark, then per bit a ~600 us space and a mark
// of ~600 us (0) or ~1200 us (1); command:7 device:5 extended:8. A frame counts only when the one
// after it is the same (ValidateSonyData), so a press posts once per two frames. Level 0 is a mark.

enum { SN_GUIDE_MIN = 2000, SN_GUIDE_MAX = 2780, SN_OFF_MIN = 320, SN_OFF_MAX = 750, SN_ZERO_MIN = 450, SN_ZERO_MAX = 880,
       SN_ONE_MIN = 1050, SN_ONE_MAX = 1480, SN_GAP_MIN = 8000, SN_GAP_MAX = 26000 };   // kTGuide.. kTInter20

static struct { int state, bits, frame; uint32_t data[2]; } N;

static void sony_frame(int cmd);

static void sn_bit(int bit) {   // SonyBitUpdate
    N.data[N.frame] = (N.data[N.frame] >> 1) | (bit ? 0x80000000u : 0);
    if (++N.bits == 20) { N.bits = 0; N.state = 4; N.frame++; } else N.state = 2;
}

static void sn_feed(int level, int us) {
    switch (N.state) {
    case 0: N.bits = 0; N.frame = 0; N.data[0] = N.data[1] = 0; /* fall through */
    case 1: if (level == 0 && us >= SN_GUIDE_MIN && us <= SN_GUIDE_MAX) N.state = 2; break;   // (stays put otherwise)
    case 2: N.state = level == 1 && us >= SN_OFF_MIN && us <= SN_OFF_MAX ? 3 : 0; break;
    case 3:
        if (level == 0 && us >= SN_ZERO_MIN && us <= SN_ZERO_MAX) sn_bit(0);
        else if (level == 0 && us >= SN_ONE_MIN && us <= SN_ONE_MAX) sn_bit(1);
        else N.state = 0;
        break;
    case 4:
        if (level != 1 || us < SN_GAP_MIN || us > SN_GAP_MAX) { N.state = 0; break; }
        if (N.frame < 2) { N.state = 1; break; }
        if (N.data[0] == N.data[1]) sony_frame((int)((N.data[0] >> 12) & 0x7f));
        N.state = 0;
        break;
    default: N.state = 0;
    }
}

// ---------------------------------------------------------------- frames -> keys

// Table.2492: Sony command -> key (Sony_ParseAndPost). 0x8005 is Execute, which the screens take
// as Select. (0x2e / 0x2f, discrete power on / off, aren't wanted here.)
static const uint16_t sony_keys[][2] = {
    { 0x74, 0x8000 }, { 0x75, 0x8003 }, { 0x34, 0x8001 }, { 0x33, 0x8002 }, { 0x61, 0x8004 }, { 0x65, 0x8005 },
    { 0x4c, 0x8008 }, { 0x4b, 0x800b }, { 0x58, 0x8009 }, { 0x59, 0x800a }, { 0x62, 0x800c }, { 0x15, 0x8006 },
    { 0x5c, 0x8018 }, { 0x10, 0x8019 }, { 0x11, 0x801a }, { 0x09, 0x30 }, { 0x00, 0x31 }, { 0x01, 0x32 },
    { 0x02, 0x33 }, { 0x03, 0x34 }, { 0x04, 0x35 }, { 0x05, 0x36 }, { 0x06, 0x37 }, { 0x07, 0x38 }, { 0x08, 0x39 },
    { 0x0b, 0x0d },
};
// Table.2460: Sony commands that don't repeat when held (SonyKeyCanRepeat)
static const uint8_t sony_no_repeat[] = { 0x61, 0x65, 0x4c, 0x4b, 0x62, 0x15, 0x5c, 0x0b };

// Table.2502: remote code -> key
static const uint16_t remote_keys[][2] = {
    { 0x1a, 0x8000 }, { 0x1e, 0x8003 }, { 0x1c, 0x8001 }, { 0x18, 0x8002 }, { 0x16, 0x8004 }, { 0x10, 0x8005 },
    { 0x00, 0x8008 }, { 0x12, 0x800b }, { 0x19, 0x8009 }, { 0x1d, 0x800a }, { 0x14, 0x800c }, { 0x02, 0x8006 },
    { 0x0b, 0x8018 }, { 0x20, 0x8019 }, { 0x0f, 0x801a }, { 0x30, 0x801f }, { 0x31, 0x8020 }, { 0x32, 0x8012 },
    { 0x35, 0x8021 }, { 0x04, 0x31 }, { 0x05, 0x32 }, { 0x06, 0x33 }, { 0x08, 0x34 }, { 0x09, 0x35 }, { 0x0a, 0x36 },
    { 0x0c, 0x37 }, { 0x0d, 0x38 }, { 0x0e, 0x39 }, { 0x11, 0x30 }, { 0x1b, 0x0d }, { 0x33, 0x8022 }, { 0x34, 0x8023 },
    { 0x3b, 0x8017 }, { 0x1f, 0x8024 }, { 0x22, 0x8025 }, { 0x23, 0x8026 }, { 0x07, 0x8027 }, { 0x24, 0x8028 },
    { 0x25, 0x8029 }, { 0x21, 0x8030 }, { 0x03, 0x8031 }, { 0x26, 0x8032 },
};

// sej2lc / sej2uc: scancode -> key, unshifted / shifted. 0x40xx: modifiers.
static const uint16_t kb_special[][2] = {
    { 0x03, 0x4004 }, { 0x05, 0x8018 }, { 0x09, 0x4007 }, { 0x0b, 0x809b }, { 0x0c, 0x8097 }, { 0x11, 0x8099 },
    { 0x13, 0x809a }, { 0x14, 0x8098 }, { 0x18, 0x4008 }, { 0x19, 0x09 }, { 0x1b, 0x8006 }, { 0x1c, 0x4003 },
    { 0x28, 0x4009 }, { 0x29, 0x1b }, { 0x2b, 0x4005 }, { 0x2d, 0x8013 }, { 0x2f, 0x8014 }, { 0x31, 0x4001 },
    { 0x32, 0x4002 }, { 0x36, 0x801a }, { 0x37, 0x8019 }, { 0x38, 0x20 }, { 0x39, 0x08 }, { 0x3a, 0x0d },
    { 0x3b, 0x809c }, { 0x3c, 0x800c }, { 0x3d, 0x800f }, { 0x48, 0x8001 }, { 0x4a, 0x8015 }, { 0x4b, 0x8000 },
    { 0x4c, 0x8016 }, { 0x4e, 0x8008 }, { 0x50, 0x8003 }, { 0x52, 0x8012 }, { 0x53, 0x8018 }, { 0x54, 0x8004 },
    { 0x55, 0x8010 }, { 0x56, 0x8009 }, { 0x57, 0x801a }, { 0x58, 0x8002 }, { 0x5b, 0x800a }, { 0x5c, 0x800b },
    { 0x5d, 0x8011 }, { 0x5f, 0x801e }, { 0x61, 0x809d }, { 0x64, 0x800d }, { 0x70, 0x8017 },
};
static const struct { uint8_t sc; char lc, uc; } kb_chars[] = {
    { 0x04, '`', '~' }, { 0x0a, 'x', 'X' }, { 0x0d, '2', '@' }, { 0x0e, 's', 'S' }, { 0x0f, 'w', 'W' }, { 0x12, 'c', 'C' },
    { 0x15, '3', '#' }, { 0x16, 'd', 'D' }, { 0x17, 'e', 'E' }, { 0x1a, 'z', 'Z' }, { 0x1d, '1', '!' }, { 0x1e, 'a', 'A' },
    { 0x1f, 'q', 'Q' }, { 0x20, 'b', 'B' }, { 0x21, 't', 'T' }, { 0x22, 'v', 'V' }, { 0x23, 'g', 'G' }, { 0x24, '5', '%' },
    { 0x25, '4', '$' }, { 0x26, 'f', 'F' }, { 0x27, 'r', 'R' }, { 0x3e, '\\', '|' }, { 0x62, '.', '>' }, { 0x65, '9', '(' },
    { 0x66, 'l', 'L' }, { 0x67, 'o', 'O' }, { 0x68, '/', '?' }, { 0x69, '[', '{' }, { 0x6b, '\'', '"' }, { 0x6c, '-', '_' },
    { 0x6d, '0', ')' }, { 0x6e, ';', ':' }, { 0x6f, 'p', 'P' }, { 0x71, ']', '}' }, { 0x72, ',', '<' }, { 0x74, '=', '+' },
    { 0x75, '8', '*' }, { 0x76, 'k', 'K' }, { 0x77, 'i', 'I' }, { 0x78, 'n', 'N' }, { 0x79, 'y', 'Y' }, { 0x7a, 'm', 'M' },
    { 0x7b, 'h', 'H' }, { 0x7c, '6', '^' }, { 0x7d, '7', '&' }, { 0x7e, 'j', 'J' }, { 0x7f, 'u', 'U' },
};
static uint16_t kb_lc[128], kb_uc[128];

enum { MOD_SHIFT = 0x01, MOD_CAPS = 0x02, MOD_CTRL = 0x04, MOD_CMD = 0x08, MOD_ALT = 0x40 };
static int mod_of(int k) {
    switch (k) {
    case 0x4001: case 0x4002: return MOD_SHIFT;
    case 0x4003: case 0x4004: return MOD_CMD;
    case 0x4005: case 0x4006: return MOD_CTRL;
    case 0x4008: case 0x4009: return MOD_ALT;
    }
    return 0;
}
static bool repeats(int k) { return (k >= 0x8000 && k <= 0x8003) || k == 0x8009 || k == 0x800a || k == 0x08; }

static struct {
    int mods; bool caps_latch; int last_kb;
    int key; uint32_t at_ms; bool any; int dups, rate;
    int sony; uint32_t sony_at; bool sony_any; int sony_dups; bool sony_skip;
    uint32_t now_ms;
} K;

void sb_ir_reset(void) {
    memset(&S, 0, sizeof S); S.state = 1;
    memset(&R, 0, sizeof R);
    memset(&N, 0, sizeof N);
    memset(&K, 0, sizeof K); K.last_kb = -1; K.key = -1; K.sony = -1;
    static bool tables;
    if (!tables) {
        tables = true;
        for (size_t i = 0; i < sizeof kb_special / sizeof kb_special[0]; i++) kb_lc[kb_special[i][0]] = kb_uc[kb_special[i][0]] = kb_special[i][1];
        for (size_t i = 0; i < sizeof kb_chars / sizeof kb_chars[0]; i++) { kb_lc[kb_chars[i].sc] = (uint8_t)kb_chars[i].lc; kb_uc[kb_chars[i].sc] = (uint8_t)kb_chars[i].uc; }
    }
}

// ParseIRKBData -> IRKBMake / IRKBBreak
static void kb_frame(uint32_t header, uint32_t code) {
    if ((header & 0x30) != 0x10 || (code & 0x80)) return;
    if (header & 0x40) {   // break
        int k = kb_lc[code];
        if (mod_of(k)) K.mods &= ~mod_of(k);
        if (k == 0x4007) K.caps_latch = false;
        if ((int)code == K.last_kb) K.last_kb = -1;
        return;
    }
    if ((int)code == K.last_kb) return;
    K.last_kb = (int)code;
    int k = (K.mods & MOD_SHIFT ? kb_uc : kb_lc)[code];
    if (!k) return;
    if (mod_of(k)) { K.mods |= mod_of(k); return; }
    if (k == 0x4007) {
        if (K.mods & MOD_SHIFT) K.mods &= ~MOD_SHIFT;
        else if (!K.caps_latch) { K.mods ^= MOD_CAPS; K.caps_latch = true; }
        return;
    }
    if (K.mods & MOD_CAPS) {
        if (k >= 0x61 && k <= 0x7a) k -= 0x20;
        else if (k >= 0x41 && k <= 0x5a) k += 0x20;
    }
    if ((K.mods & MOD_CTRL) && ((k >= 0x61 && k <= 0x7a) || (k >= 0x41 && k <= 0x5a))) k &= 0x1f;
    key_out(k);
}

// HandleEchoStarIR: a new code, or the same one after a pause (7 ticks, ~117 ms), posts
static void remote_frame(int code) {
    int k = -1;
    for (size_t i = 0; i < sizeof remote_keys / sizeof remote_keys[0]; i++) if (remote_keys[i][0] == code) { k = remote_keys[i][1]; break; }
    uint32_t t = K.now_ms;
    bool fresh = !K.any || code != K.key || t - K.at_ms > 117;
    K.any = true; K.key = code; K.at_ms = t;
    if (k < 0) return;
    if (fresh) { K.dups = 0; K.rate = 0; key_out(k); return; }
    if (!repeats(k)) return;
    if (K.dups < 9) { K.dups++; return; }
    if (K.rate < 3) { K.rate++; return; }
    K.rate = 0;
    key_out(k);
}

// HandleSonyIR: a new command, or the same one after a pause (7 ticks), posts; a held one that can
// repeat posts again after 5 more, then every other
static void sony_frame(int cmd) {
    int k = -1;
    for (size_t i = 0; i < sizeof sony_keys / sizeof sony_keys[0]; i++) if (sony_keys[i][0] == cmd) { k = sony_keys[i][1]; break; }
    uint32_t t = K.now_ms;
    bool fresh = !K.sony_any || cmd != K.sony || t - K.sony_at > 117;
    K.sony_any = true; K.sony = cmd; K.sony_at = t;
    if (k < 0) return;
    if (k == 0x8005) k = '\r';
    if (fresh) { K.sony_dups = 0; K.sony_skip = false; key_out(k); return; }
    for (size_t i = 0; i < sizeof sony_no_repeat; i++) if (sony_no_repeat[i] == cmd) return;
    if (K.sony_dups < 5) { K.sony_dups++; return; }
    if (!K.sony_skip) { K.sony_skip = true; return; }
    K.sony_skip = false;
    key_out(k);
}

// A key from the web page, as the first word of an entry in the OS's IR input ring (port.h,
// port_pm_key), taken as IRIdle takes one: halfword 2 zero, the IR keyboard (ParseIRKBData: the
// make/break byte, the scancode); maker 3, the WebTV remote; else Sony's (its command).
void sb_ir_inject(uint32_t entry, uint32_t now_ms) {
    K.now_ms = now_ms;
    if (!(entry & 0xffff)) kb_frame(entry >> 24, (entry >> 16) & 0xff);
    else if (((entry >> 8) & 0xff) == 3) remote_frame((int)(entry & 0x3f));
    else sony_frame((int)(entry & 0x7f));
}

// one DEV_IRIN_TRANS_DATA entry (InterpretIRSamples)
void sb_ir_entry(uint16_t e, uint32_t now_ms) {
    if ((e & 0xf000) == 0xf000) return;   // FIFO overrun
    K.now_ms = now_ms;
    int level = (e >> 11) & 1, us = (e & 0x7ff) * IR_US_PER_TICK;
    rc_feed(level, us);
    sn_feed(level, us);
    sj_feed(level, us);
}
