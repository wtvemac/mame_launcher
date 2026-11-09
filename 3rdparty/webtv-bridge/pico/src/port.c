// The port engine on core 1 (port.h; WEBTV_LPT_BRIDGE.md §4.1, §10.6, §10.7). One pass
// of port_step() reads the cable once and answers it in the current mode:
//
//   PORT_IDLE    status lines idle, the bus released
//   PORT_DONGLE  D0-D3 mirrored onto the status lines, as the loopback dongle wires them
//                (D0 -> nAck, D1 -> Busy, D2 -> PError, D3 -> nFault and Select), and the boot
//                ROM diagnostics' serial console at 19200 8N1: the box sends on nSelectIn, we
//                send on D7. The box turns its data bus in while it sits in the console, so we
//                drive the bus only for the ~520 us of each char we send (D7 the bit, D0-D6 low,
//                as the emulator's diagnostics host does) and listen the rest of the time.
//   PORT_PARMOD   parmodhost.js's engine: three channels (modem, console, commands) on codes; the
//                bridge's loader and the patched OS both speak it
//
// With the probe watch on, the boot ROM's dongle probe after a reset (the four control lines
// high, the box driving 0xF1, 0xF2, 0xF4 or 0xF8, us not driving) switches to PORT_DONGLE in
// the same pass, in time for the ROM's STAT read.
//
// Core 0 asks for a mode or the watch; core 1 applies it at the top of its next pass. Events
// are counters core 1 bumps and core 0 compares, so neither side writes the other's word.
#include "port.h"
#include "link.h"
#include "link_hw.h"
#include "ring.h"
#include "bridge_config.h"

#define CTRL4 (M_STROBE | M_AUTOFD | M_INIT | M_SELIN)

static volatile int want_mode = -1;       // core 0 -> core 1: a port_mode_t, -1 none
static volatile int want_watch = -1;      // core 0 -> core 1: 0 / 1, -1 none
static volatile port_mode_t mode;         // core 1's
static volatile bool watching;
static volatile uint32_t probes, entries, reentries;    // core 1 bumps
static uint32_t probes_seen, entries_seen, reentries_seen; // core 0's
static uint32_t last_in;
static port_stats_t S;

// ---------------------------------------------------------------- the dongle and the diagnostics serial

#define BAUD 19200u
#define QUIET_US 1500u                    // neither line moved for this long: the box listens

static uint8_t diag_rx_s[1024], diag_tx_s[1024];
static ring_t diag_rx = RING_INIT(diag_rx_s);   // core 1 -> core 0
static ring_t diag_tx = RING_INIT(diag_tx_s);   // core 0 -> core 1

static struct {
    bool rx_busy, tx_busy;
    int rx_n, tx_n;
    uint8_t rx_byte, tx_byte;
    uint32_t rx_t0, tx_t0, quiet_since;
} D;

ALWAYS_INLINE uint32_t half_bits_us(uint32_t h) { return (h * 1000000u + BAUD) / (2u * BAUD); }

ALWAYS_INLINE uint32_t mirror(uint32_t in) {
    uint8_t d = pins_to_byte(in);
    return (d & 1 ? M_ACK : 0) | (d & 2 ? M_BUSY : 0) | (d & 4 ? M_PERROR : 0) | (d & 8 ? M_FAULT | M_SELECT : 0);
}

static void LINK_RAM(dongle_enter)(void) {
    hw_data_release();
    D.rx_busy = D.tx_busy = false;               // (no memset: it lives in flash, and this is core 1)
    D.rx_n = D.tx_n = 0;
    D.quiet_since = hw_now_us();
    hw_status(mirror(hw_in()));
    hw_mirror_on();                              // the PIO answers the probe within nanoseconds
}

static void LINK_RAM(dongle_step)(uint32_t in, uint32_t fell, uint32_t rose) {
    hw_status(mirror(in));
    uint32_t now = hw_now_us();
    if ((fell | rose) & M_SELIN) D.quiet_since = now;

    // box -> us on nSelectIn, sampled mid-bit as a UART does
    if (!D.rx_busy && (fell & M_SELIN)) { D.rx_busy = true; D.rx_t0 = now; D.rx_n = 0; D.rx_byte = 0; }
    if (D.rx_busy && now - D.rx_t0 >= half_bits_us(2u * (uint32_t)D.rx_n + 1)) {
        int v = (in & M_SELIN) ? 1 : 0;
        if (D.rx_n == 0 && v) D.rx_busy = false;                  // a glitch, not a start bit
        else if (D.rx_n >= 1 && D.rx_n <= 8) D.rx_byte |= (uint8_t)(v << (D.rx_n - 1));
        if (D.rx_n == 9) {
            D.rx_busy = false;
            D.quiet_since = now;
            if (!v) S.diag_framing++;
            else if (ring_put(&diag_rx, D.rx_byte)) S.diag_rx++;
        }
        D.rx_n++;
    }

    // us -> box on D7: a char only once the box has been quiet long enough to be listening
    if (!D.tx_busy) {
        if (D.rx_busy || ring_empty(&diag_tx) || now - D.quiet_since < QUIET_US) return;
        D.tx_byte = (uint8_t)ring_get(&diag_tx);
        D.tx_busy = true;
        D.tx_t0 = now;
        D.tx_n = 0;
        hw_data_drive(0);                                         // start bit
        return;
    }
    if (now - D.tx_t0 < half_bits_us(2u * (uint32_t)(D.tx_n + 1))) return;
    D.tx_n++;
    if (D.tx_n <= 8) hw_data_drive((D.tx_byte >> (D.tx_n - 1)) & 1 ? 0x80 : 0);
    else if (D.tx_n == 9) hw_data_drive(0x80);                    // stop bit
    else {
        hw_data_release();                                        // the pull-ups hold D7 high
        D.tx_busy = false;
        D.quiet_since = now;
        S.diag_tx++;
    }
}

// ---------------------------------------------------------------- PARMOD (src/machine/parmodhost.js)

enum { C_CON_OUT = 1, C_MODEM_IN = 2, C_MODEM_OUT = 3, C_CTRL_IN = 4, C_CTRL_OUT = 5, C_CON_IN = 6, C_IDLE = 7 };
enum { ST_MODEM = 1, ST_CONIN = 2, ST_REPLY = 4, ST_CARRIER = 8, ST_MFULL = 16, ST_PEEK = 64, ST_KEY = 128 };
#define PARMOD_VERSION 2
static const uint8_t enter_sig[4] = { 0x4c, 0x50, 0x54, 0x32 };   // 'L' 'P' 'T' '2'

static uint8_t pmm_to_s[4096], pmm_from_s[4096], pmc_to_s[256], pmc_from_s[4096], pmcmd_s[64], pmrep_s[16];
static ring_t pmm_to = RING_INIT(pmm_to_s);       // core 0 -> box: the modem's output
static ring_t pmm_from = RING_INIT(pmm_from_s);   // box -> core 0: what the box sends its modem
static ring_t pmc_to = RING_INIT(pmc_to_s);       // core 0 -> box: typed at its console
static ring_t pmc_from = RING_INIT(pmc_from_s);   // box -> core 0: its console output (never refused: dropped when full)
static ring_t pmcmd = RING_INIT(pmcmd_s);         // box -> core 0: (command, argument) pairs
static ring_t pmrep = RING_INIT(pmrep_s);         // core 1 only: replies on CTRL_IN
// Peeks (the web page's screenshot while the OS runs): core 0 queues 4-byte addresses; each goes
// to the box as four replies on CTRL_IN and then a status byte with ST_PEEK, and the box's
// answer (PM_PEEK, a count of words, the words) comes back on CTRL_OUT into pmpk.
// Keys (the web page's Remote tab) go the same way, with ST_KEY: the four bytes are the first word
// of an entry in the OS's IR input ring, and nothing comes back. pmpk_kind says which each
// four bytes are (0 a peek, 1 a key); core 0 writes it after the bytes, so a kind means its four
// are all there.
static uint8_t pmpk_req_s[64], pmpk_s[4096], pmpk_kind_s[16];
static ring_t pmpk_req = RING_INIT(pmpk_req_s);   // core 0 -> box: peek addresses, keys
static ring_t pmpk = RING_INIT(pmpk_s);           // box -> core 0: the words peeked
static ring_t pmpk_kind = RING_INIT(pmpk_kind_s); // core 0 -> box: what each four in pmpk_req are

static struct {
    volatile bool session;       // read by core 0
    bool busy, sel, pending, driving, full, fault_low, carrier;
    int sig, arg_for;            // arg_for: a command waiting for its argument byte, -1 none
    ring_t *pop;                 // the _IN byte given, taken off its ring when nStrobe rises
    int pk_given;                // a peek's address bytes the box has taken (4: the status says PEEK next)
    bool pk_status;              // the status byte given carries ST_PEEK
    bool pk_more;                // a peek was taken and more are queued: pulse nAck so the box pumps again
    int pk_state, pk_left;       // the box's answer on CTRL_OUT: 1 the count next, 2 the data (pk_left bytes)
} T;
static volatile bool pm_carrier_want;  // core 0

static inline bool pm_waiting(void) {
    return !ring_empty(&pmm_to) || !ring_empty(&pmc_to) || !ring_empty(&pmrep) || !ring_empty(&pmpk_kind) || T.pk_given;
}

static void LINK_RAM(pm_status)(void) {
    bool waiting = pm_waiting();
    hw_status((T.busy ? M_BUSY : 0) | M_ACK | (T.session && waiting ? 0 : M_FAULT) | (T.sel ? M_SELECT : 0)
              | (T.session ? 0 : M_PERROR));
}

// nAck low then high: the box's port interrupt
static void LINK_RAM(pm_attention)(void) {
    bool waiting = pm_waiting();
    uint32_t st = (T.busy ? M_BUSY : 0) | (T.session && waiting ? 0 : M_FAULT) | (T.sel ? M_SELECT : 0) | (T.session ? 0 : M_PERROR);
    hw_status(st);
    hw_delay(LINK_ACK_PULSE_CYCLES);
    hw_status(st | M_ACK);
    S.pm_attentions++;
}

static void LINK_RAM(pm_enter)(void) {
    if (T.driving) hw_data_release();
    T.session = T.busy = T.sel = T.pending = T.driving = T.full = T.fault_low = T.carrier = false;
    T.sig = 0;
    T.arg_for = -1;
    T.pop = NULL;
    T.pk_given = T.pk_state = T.pk_left = 0;
    T.pk_status = T.pk_more = false;
    // anything left for the box is from before (we are these rings' consumer)
    ring_flush(&pmm_to); ring_flush(&pmc_to); ring_flush(&pmrep); ring_flush(&pmpk_req); ring_flush(&pmpk_kind);
    pm_status();
}

static void LINK_RAM(pm_cmd)(uint8_t c, uint8_t arg) {
    if (ring_free(&pmcmd) < 2) { S.pm_cmd_dropped++; return; }
    ring_put(&pmcmd, c);
    ring_put(&pmcmd, arg);
}

// CTRL_OUT: the entry signature (any time), else a command -> true if taken
static bool LINK_RAM(pm_command)(uint8_t b) {
    if (T.session && T.pk_state) {         // a peek's answer: never a command, whatever its bytes
        if (T.pk_state == 1) { T.pk_left = b * 4; T.pk_state = b ? 2 : 0; }
        else {
            if (!ring_put(&pmpk, b)) S.pm_peek_dropped++;
            if (--T.pk_left <= 0) T.pk_state = 0;
        }
        return true;
    }
    if (T.session && b == PM_PEEK) { T.pk_state = 1; S.pm_peeks++; return true; }
    if (T.session && T.arg_for >= 0) { pm_cmd((uint8_t)T.arg_for, b); T.arg_for = -1; return true; }
    if (b == enter_sig[T.sig]) {
        if (++T.sig == 4) {
            T.sig = 0;
            // inside a session, the box's side lost it: an OS that started over (the debugger, a
            // reset the probe watch missed). Nothing of the old session's peeks is coming.
            if (T.session) { S.pm_reentries++; reentries++; T.pk_state = 0; T.arg_for = -1; }
            S.pm_sessions++; entries++;
            T.session = true;
            ring_put(&pmrep, PARMOD_VERSION);
        }
        return true;
    }
    T.sig = b == enter_sig[0] ? 1 : 0;
    if (!T.session) return T.sig == 1;     // outside a session only the signature counts
    if (b == PM_LEGACY_ECP) { T.arg_for = b; return true; }
    if (b == PM_EXIT) { T.session = false; ring_flush(&pmrep); }
    pm_cmd(b, 0);
    return true;
}

static void LINK_RAM(pm_give)(ring_t *r, int *drive, bool *sel) {
    int b = ring_peek(r, 0);
    if (b >= 0) { *drive = b; *sel = true; T.pop = r; return; }
    *drive = (!ring_empty(&pmm_to) ? ST_MODEM : 0) | (!ring_empty(&pmc_to) ? ST_CONIN : 0) | (!ring_empty(&pmrep) ? ST_REPLY : 0)
             | (T.carrier ? ST_CARRIER : 0) | (T.full ? ST_MFULL : 0);
    *sel = false;
}

// CTRL_IN: replies first, then a peek's or a key's four bytes (only once all four are queued),
// then the status byte, with ST_PEEK or ST_KEY when those four were just taken
static void LINK_RAM(pm_give_ctrl)(int *drive, bool *sel) {
    if (!ring_empty(&pmrep) || (T.pk_given == 0 && ring_empty(&pmpk_kind))) {
        pm_give(&pmrep, drive, sel);
        if (!*sel && T.pk_given == 4) { *drive |= ring_peek(&pmpk_kind, 0) == 1 ? ST_KEY : ST_PEEK; T.pk_status = true; }
        return;
    }
    if (T.pk_given < 4) { *drive = ring_peek(&pmpk_req, 0); *sel = true; T.pop = &pmpk_req; return; }
    pm_give(&pmrep, drive, sel);
    *drive |= ring_peek(&pmpk_kind, 0) == 1 ? ST_KEY : ST_PEEK;
    T.pk_status = true;
}

static void LINK_RAM(pm_step)(uint32_t in, uint32_t fell, uint32_t rose) {
    if ((fell & M_STROBE) && !T.pending) {
        int code = (in & M_SELIN ? 4 : 0) | (in & M_INIT ? 2 : 0) | (in & M_AUTOFD ? 1 : 0);
        hw_delay(LINK_STROBE_SAMPLE_CYCLES);
        uint32_t now = hw_in();
        // nStrobe back high already: ringing on the cable, not a transaction (taking it would
        // give away or take in a byte the box never sees)
        if (now & M_STROBE) { S.pm_glitches++; last_in = now; pm_status(); return; }
        uint8_t data = pins_to_byte(now);
        bool sel = true, answer = true;
        int drive = -1;
        T.pop = NULL;
        if (!T.session) {
            if (code == C_CTRL_OUT) sel = pm_command(data);      // only the signature answered
            else answer = false;
        } else {
            switch (code) {
            case C_CTRL_OUT: pm_command(data); break;
            case C_CTRL_IN: pm_give_ctrl(&drive, &sel); break;
            case C_MODEM_IN: pm_give(&pmm_to, &drive, &sel); break;
            case C_CON_IN: pm_give(&pmc_to, &drive, &sel); break;
            case C_MODEM_OUT:
                sel = ring_put(&pmm_from, data);
                if (!sel) { T.full = true; S.pm_refused++; }
                break;
            case C_CON_OUT: if (!ring_put(&pmc_from, data)) S.pm_con_dropped++; break;
            default: answer = false;                              // IDLE and 000 never strobe
            }
        }
        if (answer) {
            S.pm_xfers++;
            // the data and Select first, Busy after them: the box reads STAT the moment it sees
            // Busy, and Select must already be right there (Select low = no byte, but the byte
            // still leaves the ring when nStrobe rises)
            if (drive >= 0) { hw_data_drive((uint8_t)drive); T.driving = true; }
            T.sel = sel;
            pm_status();
            hw_delay(LINK_DATA_SETUP_CYCLES);
            T.busy = true;
            T.pending = true;
        }
    } else if ((rose & M_STROBE) && T.pending) {
        hw_delay(LINK_STROBE_SAMPLE_CYCLES);
        uint32_t now = hw_in();
        if (!(now & M_STROBE)) { S.pm_glitches++; last_in = now; pm_status(); return; }   // ringing
        // nStrobe high: release the bus, then drop Busy. The bus goes to all ones first, so it only
        // floats up from there: the four lines high never show beside a dongle probe byte.
        if (T.driving) { hw_data_drive(0xff); hw_delay(LINK_DATA_SETUP_CYCLES); hw_data_release(); T.driving = false; }
        if (T.pop) { ring_get(T.pop); if (T.pop == &pmpk_req) T.pk_given++; T.pop = NULL; }
        // the box has the PEEK status: it answers next. With more queued, nAck now: its interrupt
        // clears ACKDONE before pumping, so this pump is followed by another
        if (T.pk_status) {
            if (ring_get(&pmpk_kind) == 1) S.pm_keys++;
            T.pk_status = false; T.pk_given = 0; T.pk_more = !ring_empty(&pmpk_kind);
        }
        T.busy = T.sel = T.pending = false;
    }

    bool attention = false;
    if (!T.pending) {
        bool waiting = pm_waiting();
        bool low = T.session && waiting;
        if (low && !T.fault_low) attention = true;               // something new for the box
        T.fault_low = low;
        if (T.full && ring_free(&pmm_from) >= 256) { T.full = false; attention |= T.session; }
        if (T.carrier != pm_carrier_want) { T.carrier = pm_carrier_want; attention |= T.session; }
        if (T.pk_more) { T.pk_more = false; attention |= T.session; }
    }
    if (attention) pm_attention();
    else pm_status();
}

// ---------------------------------------------------------------- modes

static void LINK_RAM(enter)(port_mode_t m) {
    if (mode == PORT_DONGLE && m != PORT_DONGLE) hw_mirror_off();   // the status lines back to SIO
    if (m != PORT_PARMOD && T.driving) { hw_data_release(); T.driving = false; }
    mode = m;
    switch (m) {
    case PORT_DONGLE: dongle_enter(); break;
    case PORT_PARMOD: pm_enter(); break;
    default:
        hw_data_release();
        hw_status(M_ACK | M_FAULT | M_SELECT);
        break;
    }
}

void LINK_RAM(port_step)(void) {
    uint32_t in = hw_in();
    uint32_t fell = last_in & ~in, rose = ~last_in & in;
    last_in = in;
    if ((fell | rose) & CTRL4) hw_dbg_toggle(link_pins.dbg0);

    int w = want_watch;
    if (w >= 0) { watching = w; want_watch = -1; }
    int m = want_mode;
    if (m >= 0) { want_mode = -1; enter((port_mode_t)m); }

    // the boot ROM's dongle probe: the ROM reads STAT right after it drives the pattern
    if (watching && mode != PORT_DONGLE && (in & CTRL4) == CTRL4 && !(in & M_DIR)) {
        uint8_t d = pins_to_byte(in);
        if (d == 0xf1 || d == 0xf2 || d == 0xf4 || d == 0xf8) {
            watching = false;
            enter(PORT_DONGLE);
            S.probes++;
            probes++;
        }
    }
    // The real box's probe (captured on the cable): nStrobe and nAutoFd high, nInit and nSelectIn
    // low, the box driving 0x00, 0x51, 0x52, 0x54, 0x58 about 10 us apart with no strobe, and
    // the whole probe again ~0.4 s later when one fails. Those lines are PARMOD's CON_OUT code
    // with nStrobe up, and console bytes can be anything, but PARMOD strobes every byte it puts
    // out: with the code held and no strobe, its data changes at most once (the next byte going
    // on). Two changes or more is the probe. Caught late in one probe, the ROM's next one finds
    // the dongle. A value counts once two passes in a row read it (the probe holds each ~10 us),
    // so a byte read half-changed as it goes on isn't a change of its own.
    {
        static bool held;
        static uint8_t held_d, cand, changes;
        bool probe_lines = (in & CTRL4) == (M_STROBE | M_AUTOFD) && !(in & M_DIR);
        uint8_t d = pins_to_byte(in);
        if (!probe_lines || (fell & M_STROBE)) held = false;
        else if (!held) { held = true; held_d = cand = d; changes = 0; }
        else if (d != cand) cand = d;               // new, or back: wait for a second read
        else if (d != held_d) {                     // read twice: a change
            held_d = d;
            if (++changes >= 2 && watching && mode != PORT_DONGLE && (d == 0x51 || d == 0x52 || d == 0x54 || d == 0x58)) {
                watching = false;
                enter(PORT_DONGLE);
                S.probes++;
                probes++;
            }
        }
    }
    switch (mode) {
    case PORT_DONGLE: dongle_step(in, fell, rose); break;
    case PORT_PARMOD: pm_step(in, fell, rose); break;
    default: break;
    }
}

void LINK_RAM(port_resume)(void) {
    last_in = hw_in();
    enter(mode);
}

// ---------------------------------------------------------------- core 0's side

void port_set_mode(port_mode_t m) { want_mode = (int)m; }
port_mode_t port_mode(void) { int m = want_mode; return m >= 0 ? (port_mode_t)m : mode; }
void port_watch_probe(bool on) { want_watch = on; }

uint32_t port_take_events(uint32_t mask) {
    uint32_t e = 0, p = probes, l = entries, r = reentries;
    if ((mask & PORT_EV_PROBE) && p != probes_seen) { probes_seen = p; e |= PORT_EV_PROBE; }
    if ((mask & PORT_EV_REENTRY) && r != reentries_seen) { reentries_seen = r; e |= PORT_EV_REENTRY; }
    if ((mask & PORT_EV_PARMOD) && l != entries_seen) { entries_seen = l; e |= PORT_EV_PARMOD; }
    return e;
}

size_t port_diag_read(uint8_t *d, size_t max) { return ring_read(&diag_rx, d, max); }
void port_diag_write(const uint8_t *d, size_t n) { ring_write(&diag_tx, d, n); }
bool port_diag_tx_idle(void) { return ring_empty(&diag_tx) && !D.tx_busy; }

size_t port_pm_modem_read(uint8_t *d, size_t max) { return ring_read(&pmm_from, d, max); }
// Towards the box nothing is taken while a mode change waits for core 1: entering PARMOD empties
// these rings, so what core 0 writes right after asking for it must wait until it has happened.
size_t port_pm_modem_write(const uint8_t *d, size_t n) { return want_mode >= 0 ? 0 : ring_write(&pmm_to, d, n); }
size_t port_pm_modem_free(void) { return want_mode >= 0 ? 0 : ring_free(&pmm_to); }
size_t port_pm_modem_queued(void) { return ring_used(&pmm_to); }
size_t port_pm_console_read(uint8_t *d, size_t max) { return ring_read(&pmc_from, d, max); }
size_t port_pm_console_write(const uint8_t *d, size_t n) { return want_mode >= 0 ? 0 : ring_write(&pmc_to, d, n); }
void port_pm_carrier(bool on) { pm_carrier_want = on; }
bool port_pm_session(void) { return T.session; }
int port_pm_command(uint8_t *arg) {
    if (ring_used(&pmcmd) < 2) return -1;
    int c = ring_get(&pmcmd);
    *arg = (uint8_t)ring_get(&pmcmd);
    return c;
}

static bool pm_request(uint32_t w, uint8_t kind) {
    if (want_mode >= 0 || !T.session || ring_free(&pmpk_req) < 4 || ring_free(&pmpk_kind) < 1) return false;
    uint8_t b[4] = { (uint8_t)(w >> 24), (uint8_t)(w >> 16), (uint8_t)(w >> 8), (uint8_t)w };
    if (ring_write(&pmpk_req, b, 4) != 4) return false;
    ring_put(&pmpk_kind, kind);                   // after the bytes: core 1 takes the four once it sees this
    return true;
}
bool port_pm_peek(uint32_t addr, bool one_word) { return pm_request(one_word ? addr | 1 : addr, 0); }
bool port_pm_key(uint32_t entry) { return pm_request(entry, 1); }
size_t port_pm_peek_read(uint8_t *d, size_t max) { return ring_read(&pmpk, d, max); }
void port_pm_peek_flush(void) { ring_flush(&pmpk); }

void port_get_stats(port_stats_t *s) { *s = S; }
