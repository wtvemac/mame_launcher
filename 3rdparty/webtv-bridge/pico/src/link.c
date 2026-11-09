// The parallel port's pins on the parallel2usb board: which board this is, core 1's start (it
// runs the port engine, port.c), and the bring-up tests (link.h). Core 1 stays in RAM: it must
// keep answering the box while core 0 writes flash.
#include <string.h>
#include "link.h"
#include "link_hw.h"
#include "port.h"
#include "bridge_config.h"

link_board_t link_board;
link_pins_t link_pins;

void link_select_board(const link_board_t *b) {
    link_board = *b;
    link_pins_t *p = &link_pins;
    p->d0 = b->d0;
    p->reversed = b->reversed;
    p->data = b->reversed ? 0xffu << (b->d0 - 7) : 0xffu << b->d0;
    p->strobe = 1u << b->strobe; p->autofd = 1u << b->autofd; p->init = 1u << b->init; p->selin = 1u << b->selin;
    p->ack = 1u << b->ack; p->busy = 1u << b->busy; p->perror = 1u << b->perror;
    p->select = 1u << b->select; p->fault = 1u << b->fault;
    p->status = p->ack | p->busy | p->perror | p->select | p->fault;
    p->dir = 1u << b->dir; p->hd = 1u << b->hd;
    p->dbg0 = 1u << b->dbg0; p->dbg1 = 1u << b->dbg1;
}

#ifndef LINK_HOST_SIM
#include "pico/multicore.h"
#include "hardware/gpio.h"
#include "hardware/pio.h"
#include "hardware/structs/io_bank0.h"
#include "hardware/timer.h"
#include "hardware/clocks.h"
#include "hardware/structs/systick.h"

static volatile bool pause_req, paused;

// ---------------------------------------------------------------- the dongle's mirror in PIO

// One state machine per status line, each running a single `mov pins, pins` forever: the level
// of its data pin (IN base) copied to its status pin (OUT base, one pin), a few clock cycles
// after it changes. Five lines, so they are spread over the PIO blocks with a free SM.
static struct { PIO pio; uint8_t sm, pin, func; } mir[5];
static int mir_n;

static void mirror_setup(void) {
    const link_board_t *b = &link_board;
    static const uint16_t mov_pins_pins = 0xa000;   // mov pins, pins
    static const pio_program_t prog = { .instructions = &mov_pins_pins, .length = 1, .origin = -1 };
    const int dpin[4] = { b->reversed ? b->d0 : b->d0, b->reversed ? b->d0 - 1 : b->d0 + 1,
                          b->reversed ? b->d0 - 2 : b->d0 + 2, b->reversed ? b->d0 - 3 : b->d0 + 3 };
    const uint8_t from[5] = { dpin[0], dpin[1], dpin[2], dpin[3], dpin[3] };
    const uint8_t to[5] = { b->ack, b->busy, b->perror, b->fault, b->select };
    PIO pios[] = { pio2, pio1, pio0 };
    int offset[3] = { -1, -1, -1 };
    for (int i = 0; i < 5; i++) {
        for (int k = 0; k < 3; k++) {
            PIO p = pios[k];
            int sm = pio_claim_unused_sm(p, false);
            if (sm < 0) continue;
            if (offset[k] < 0) {
                if (!pio_can_add_program(p, &prog)) { pio_sm_unclaim(p, (uint)sm); continue; }
                offset[k] = (int)pio_add_program(p, &prog);
            }
            pio_sm_config c = pio_get_default_sm_config();
            sm_config_set_wrap(&c, (uint)offset[k], (uint)offset[k]);
            sm_config_set_in_pins(&c, from[i]);
            sm_config_set_out_pins(&c, to[i], 1);
            pio_sm_init(p, (uint)sm, (uint)offset[k], &c);
            pio_sm_set_pindirs_with_mask(p, (uint)sm, 1u << to[i], 1u << to[i]);
            mir[mir_n].pio = p; mir[mir_n].sm = (uint8_t)sm; mir[mir_n].pin = to[i];
            mir[mir_n].func = (uint8_t)pio_get_funcsel(p);
            mir_n++;
            break;
        }
    }
}

static void LINK_RAM(set_funcsel)(uint pin, uint func) {
    hw_write_masked(&io_bank0_hw->io[pin].ctrl, func << IO_BANK0_GPIO0_CTRL_FUNCSEL_LSB, IO_BANK0_GPIO0_CTRL_FUNCSEL_BITS);
}

void LINK_RAM(hw_mirror_on)(void) {
    if (mir_n < 5) return;           // not all five set up: the software mirror carries on alone
    for (int i = 0; i < mir_n; i++) hw_set_bits(&mir[i].pio->ctrl, 1u << mir[i].sm);
    for (int i = 0; i < mir_n; i++) set_funcsel(mir[i].pin, mir[i].func);
}

void LINK_RAM(hw_mirror_off)(void) {
    for (int i = 0; i < mir_n; i++) set_funcsel(mir[i].pin, GPIO_FUNC_SIO);
    for (int i = 0; i < mir_n; i++) hw_clear_bits(&mir[i].pio->ctrl, 1u << mir[i].sm);
}

static void LINK_RAM(core1_main)(void) {
    for (;;) {
        if (pause_req) {
            hw_mirror_off();         // the pin tests drive the status lines through SIO
            paused = true;
            while (pause_req) { }
            port_resume();
            paused = false;
        }
        port_step();
    }
}

static const char *const names_in[] = { "nStrobe", "nAutoFd", "nInit", "nSelectIn" };
static const char *const names_out[] = { "nAck", "Busy", "PError", "Select", "nFault" };

void link_print_pins(link_print_fn print) {
    const link_board_t *b = &link_board;
    uint32_t in = gpio_get_all();
    const uint8_t gin[] = { b->strobe, b->autofd, b->init, b->selin }, gout[] = { b->ack, b->busy, b->perror, b->select, b->fault };
    print("parallel2usb rev %d%s, RP2350 A%d\nfrom the box:", b->rev, BRIDGE_BOARD ? " (forced)" : " (GP28 strap)",
          rp2350_rom_version());
    for (int i = 0; i < 4; i++) print(" %s=%d", names_in[i], (int)(in >> gin[i]) & 1);
    print("\nto the box:  ");
    for (int i = 0; i < 5; i++) print(" %s=%d", names_out[i], (int)(in >> gout[i]) & 1);
    print("\ndata 0x%02x, DIR=%d (%s), HD=%d\n", pins_to_byte(in), (int)(in >> b->dir) & 1,
          (in >> b->dir) & 1 ? "bridge drives" : "box drives", (int)(in >> b->hd) & 1);
}

// ---------------------------------------------------------------- the sniffer

#define SNIFF_MAX 4096
static struct { uint32_t t, in; } sniff_buf[SNIFF_MAX];

static void sniff_line(link_print_fn print, uint32_t t, uint32_t in) {
    const link_board_t *b = &link_board;
    print("%9lu us  strb=%d afd=%d init=%d selin=%d  data=%02x%s  ack=%d busy=%d pe=%d sel=%d flt=%d\n",
          (unsigned long)t, (int)(in >> b->strobe) & 1, (int)(in >> b->autofd) & 1, (int)(in >> b->init) & 1,
          (int)(in >> b->selin) & 1, pins_to_byte(in), (in >> b->dir) & 1 ? " (ours)" : "       ",
          (int)(in >> b->ack) & 1, (int)(in >> b->busy) & 1, (int)(in >> b->perror) & 1,
          (int)(in >> b->select) & 1, (int)(in >> b->fault) & 1);
}

// Core 0 polls the pins as fast as it can (well under 1 us a pass) while core 1 goes on running
// the port engine, so what is recorded is what the box does and how the bridge answers.
void LINK_RAM(link_sniff)(int wait_s, int record_s, link_print_fn print) {
    const uint32_t mask = M_DATA | M_STROBE | M_AUTOFD | M_INIT | M_SELIN | M_STATUS | M_DIR;
    uint32_t last = hw_in() & mask, t0 = time_us_32(), n = 0;
    print("sniff: waiting up to %d s for a change on the cable; now:\n", wait_s);
    sniff_line(print, 0, last);
    // The tight loops read only the pins; the timer is read when something changes, and for the
    // timeouts every 4096 passes, so a change of well under a microsecond is still caught (the
    // boot ROM's dongle probe drives each byte for about 1 us).
    for (uint32_t k = 0; (hw_in() & mask) == last; k++)
        if (!(k & 4095) && time_us_32() - t0 > (uint32_t)wait_s * 1000000u) { print("sniff: nothing changed\n"); return; }
    uint32_t start = time_us_32(), limit = (uint32_t)record_s * 1000000u;
    for (uint32_t k = 0; n < SNIFF_MAX; k++) {
        uint32_t in = hw_in() & mask;
        if (in != last) {
            sniff_buf[n].t = time_us_32() - start; sniff_buf[n].in = in; n++;
            last = in;
        } else if (!(k & 4095) && time_us_32() - start > limit) break;
    }
    print("sniff: %lu changes in %lu ms%s\n", (unsigned long)n, (unsigned long)((time_us_32() - start) / 1000),
          n == SNIFF_MAX ? " (buffer full)" : "");
    for (uint32_t i = 0; i < n; i++) sniff_line(print, sniff_buf[i].t, sniff_buf[i].in);
    print("sniff: done\n");
}

static void walk_pulses(uint32_t mask, int n, bool idle_high) {
    for (int i = 0; i < n; i++) {
        if (idle_high) sio_hw->gpio_clr = mask; else sio_hw->gpio_set = mask;
        busy_wait_us_32(2);
        if (idle_high) sio_hw->gpio_set = mask; else sio_hw->gpio_clr = mask;
        busy_wait_us_32(2);
    }
    busy_wait_us_32(100);
}

bool link_walk(void) {
    pause_req = true;
    while (!paused) { }
    uint32_t irq = save_and_disable_interrupts();
    // idle levels first
    sio_hw->gpio_clr = M_DIR;
    sio_hw->gpio_oe_clr = M_DATA;
    hw_status(M_ACK | M_SELECT | M_FAULT);
    busy_wait_us_32(200);
    const uint32_t out[] = { M_ACK, M_BUSY, M_PERROR, M_SELECT, M_FAULT };
    const bool idle_high[] = { true, false, false, true, true };
    int k = 0;
    for (int i = 0; i < 5; i++, k++) walk_pulses(out[i], k + 1, idle_high[i]);
    hw_data_drive(0);
    busy_wait_us_32(100);
    for (int i = 0; i < 8; i++, k++) walk_pulses(byte_to_pins((uint8_t)(1u << i)), k + 1, false);
    hw_data_release();
    restore_interrupts(irq);
    pause_req = false;
    while (paused) { }
    return true;
}

// The loopback test (link.h). Drivers are names_out[]; receivers are the four control lines
// then D0-D7. Mode m jumpers driver i to receiver 5 * (m - 1) + i, where there is one.
static const char *const names_rx[] = { "nStrobe", "nAutoFd", "nInit", "nSelectIn", "D0", "D1", "D2", "D3",
                                        "D4", "D5", "D6", "D7" };
static const uint8_t j2_out[] = { 6, 5, 4, 3, 16 }, j2_rx[] = { 19, 18, 14, 12, 17, 15, 13, 11, 10, 9, 8, 7 };
#define LOOP_RX 12

static int loop_rx(int mode, int driver) {
    int r = 5 * (mode - 1) + driver;
    return r < LOOP_RX ? r : -1;
}

// receiver levels expected with nothing (step 0) or driver step - 1 pulled low
static uint16_t loop_expect(int mode, int step) {
    int r = step ? loop_rx(mode, step - 1) : -1;
    return (uint16_t)(((1u << LOOP_RX) - 1) & ~(r >= 0 ? 1u << r : 0));
}

static void loop_jumpers(int mode, link_print_fn print) {
    print("loop %d jumpers on J2:", mode);
    for (int i = 0; i < 5; i++) {
        int r = loop_rx(mode, i);
        if (r >= 0) print(" %s-%s (%d-%d)", names_out[i], names_rx[r], j2_out[i], j2_rx[r]);
    }
    print("\n");
}

static void loop_rx_pins(uint32_t *rx) {
    rx[0] = M_STROBE; rx[1] = M_AUTOFD; rx[2] = M_INIT; rx[3] = M_SELIN;
    for (int i = 0; i < 8; i++) rx[4 + i] = byte_to_pins((uint8_t)(1u << i));
}

// receiver levels (bit r = names_rx[r]) as GPIO levels
static uint32_t loop_gpio(const uint32_t *rx, uint16_t v) {
    uint32_t g = 0;
    for (int r = 0; r < LOOP_RX; r++) if (v & (1u << r)) g |= rx[r];
    return g;
}

static uint16_t loop_read(const uint32_t *rx) {
    uint32_t in = hw_in();
    uint16_t v = 0;
    for (int r = 0; r < LOOP_RX; r++) if (in & rx[r]) v |= 1u << r;
    return v;
}

// Loop 4 and 5: turning the data bus around. Four data lines (D0-D3 for loop 4, D4-D7 for
// loop 5) drive nStrobe, nAutoFd, nInit and nSelectIn while DIR = 1; nAck, Busy, PError and
// Select drive the other four while DIR = 0.
#define TURN_TRIPS   1000
#define TURN_TIMEOUT 3000       // cycles to wait for the lines to settle
#define TURN_STEPS   12

static int turn_out(int mode) { return mode == 4 ? 0 : 4; }   // the first data line that drives

static void turn_jumpers(int mode, link_print_fn print) {
    int o = turn_out(mode), i = 4 - o;
    print("loop %d jumpers on J2:", mode);
    for (int k = 0; k < 4; k++) print(" %s-%s (%d-%d)", names_rx[4 + o + k], names_rx[k], j2_rx[4 + o + k], j2_rx[k]);
    for (int k = 0; k < 4; k++) print(" %s-%s (%d-%d)", names_out[k], names_rx[4 + i + k], j2_out[k], j2_rx[4 + i + k]);
    print("\n");
}

// DIR = 1: drive the outgoing data lines with out (bit k = line k). The incoming ones are driven
// high, like the status lines on the other ends of their jumpers, so push-pull doesn't fight.
// DIR = 0: release the bus and pull status line k low for each bit k of low.
static void LINK_RAM(turn_set)(int mode, bool dir, unsigned out, unsigned low) {
    int o = turn_out(mode), i = 4 - o;
    if (dir) {
        hw_status(M_STATUS);
        hw_data_drive((uint8_t)((out & 15u) << o | 15u << i));
    } else {
        const uint32_t drv[] = { M_ACK, M_BUSY, M_PERROR, M_SELECT };
        hw_data_release();
        uint32_t s = M_STATUS;
        for (int k = 0; k < 4; k++) if (low >> k & 1) s &= ~drv[k];
        hw_status(s);
    }
}

// the receivers that tell, and their levels; with DIR = 1 the Pico only reads its own data outputs
static void turn_expect(int mode, bool dir, unsigned out, unsigned low, uint16_t *want, uint16_t *mask) {
    int i = 4 - turn_out(mode);
    if (dir) {
        *mask = 15;
        *want = (uint16_t)(out & 15u);
    } else {
        *mask = 0xfff;
        *want = (uint16_t)(0xfffu & ~((low & 15u) << (4 + i)));
    }
}

static const struct { bool dir; uint8_t out, low; } turn_steps[TURN_STEPS] = {
    { 0, 0, 0 }, { 0, 0, 1 }, { 0, 0, 2 }, { 0, 0, 4 }, { 0, 0, 8 },
    { 1, 15, 0 }, { 1, 14, 0 }, { 1, 13, 0 }, { 1, 11, 0 }, { 1, 7, 0 }, { 1, 0, 0 },
    { 0, 0, 0 },
};

static void turn_describe(int mode, int s, link_print_fn print) {
    int o = turn_out(mode);
    unsigned out = turn_steps[s].out, low = turn_steps[s].low;
    if (!turn_steps[s].dir) {
        if (!low) print("DIR=0, drivers high%s", s ? " (after DIR=1)" : "");
        for (int k = 0; k < 4; k++) if (low == 1u << k) print("DIR=0, %s low", names_out[k]);
    } else if (out == 15 || out == 0) {
        print("DIR=1, D%d-D%d %s", o, o + 3, out ? "high" : "low");
    } else {
        for (int k = 0; k < 4; k++) if ((~out & 15u) == 1u << k) print("DIR=1, D%d low", o + k);
    }
}

// cycles until the receivers in mask read want, or UINT32_MAX
static uint32_t LINK_RAM(turn_wait)(uint32_t mask, uint32_t want, uint32_t t0) {
    uint32_t t;
    do {
        t = (t0 - systick_hw->cvr) & 0xffffff;
        if ((sio_hw->gpio_in & mask) == want) return t;
    } while (t < TURN_TIMEOUT);
    return UINT32_MAX;
}

typedef struct { uint32_t worst[3], fails[3]; } turn_times_t;   // drive, release, receive

// TURN_TRIPS round trips with changing patterns, timing each step with SysTick (CPU cycles)
static void LINK_RAM(turn_trips)(int mode, const uint32_t *rx, turn_times_t *tt) {
    for (int n = 0; n < TURN_TRIPS; n++) {
        unsigned out = n & 15u, low = (unsigned)(n * 7 + 3) & 15u;
        uint32_t gmask[3], gwant[3], t[3];
        uint16_t want, mask;
        turn_expect(mode, 1, out, 0, &want, &mask);
        gmask[0] = loop_gpio(rx, mask); gwant[0] = loop_gpio(rx, want);
        turn_expect(mode, 0, 0, 0, &want, &mask);
        gmask[1] = loop_gpio(rx, mask); gwant[1] = loop_gpio(rx, want);
        turn_expect(mode, 0, 0, low, &want, &mask);
        gmask[2] = loop_gpio(rx, mask); gwant[2] = loop_gpio(rx, want);

        uint32_t t0 = systick_hw->cvr;
        turn_set(mode, 1, out, 0);
        t[0] = turn_wait(gmask[0], gwant[0], t0);
        t0 = systick_hw->cvr;
        hw_data_release();
        t[1] = turn_wait(gmask[1], gwant[1], t0);
        t0 = systick_hw->cvr;
        turn_set(mode, 0, 0, low);
        t[2] = turn_wait(gmask[2], gwant[2], t0);
        for (int k = 0; k < 3; k++) {
            if (t[k] == UINT32_MAX) tt->fails[k]++;
            else if (t[k] > tt->worst[k]) tt->worst[k] = t[k];
        }
    }
}

static int loop_turn(int mode, link_print_fn print) {
    uint32_t rx[LOOP_RX];
    loop_rx_pins(rx);
    uint16_t got[2][TURN_STEPS];
    turn_times_t tt[2] = { 0 };
    int bad[2] = { 0 }, passes = 0;
    bool tripped[2] = { false };

    pause_req = true;
    while (!paused) { }
    uint32_t csr = systick_hw->csr, rvr = systick_hw->rvr;
    systick_hw->rvr = 0xffffff;
    systick_hw->cvr = 0;
    systick_hw->csr = 5;                 // CPU clock, no interrupt
    // open drain first; push-pull only if that found the jumpers right, since the wrong
    // jumpers would have the bridge's data outputs fighting its status outputs
    for (int hd = 0; hd < 2 && !(hd && bad[0]); hd++, passes++) {
        if (hd) sio_hw->gpio_set = M_HD; else sio_hw->gpio_clr = M_HD;
        for (int s = 0; s < TURN_STEPS; s++) {
            turn_set(mode, turn_steps[s].dir, turn_steps[s].out, turn_steps[s].low);
            busy_wait_us_32(20);
            uint16_t want, mask;
            turn_expect(mode, turn_steps[s].dir, turn_steps[s].out, turn_steps[s].low, &want, &mask);
            got[hd][s] = loop_read(rx);
            if ((got[hd][s] ^ want) & mask) bad[hd]++;
        }
        if (bad[hd]) continue;
        tripped[hd] = true;
        uint32_t irq = save_and_disable_interrupts();
        turn_trips(mode, rx, &tt[hd]);
        restore_interrupts(irq);
        for (int k = 0; k < 3; k++) bad[hd] += tt[hd].fails[k] ? 1 : 0;
    }
    turn_set(mode, 0, 0, 0);
    sio_hw->gpio_set = M_HD;             // as link_init left it; port_resume() restores the status lines
    systick_hw->csr = csr;
    systick_hw->rvr = rvr;
    pause_req = false;
    while (paused) { }

    print("parallel2usb rev %d, ", link_board.rev);
    turn_jumpers(mode, print);
    const uint32_t mhz = clock_get_hz(clk_sys) / 1000000;
    static const char *const what[] = { "drive (DIR=1, then the control lines follow)", "release (DIR=0, then the bus floats high)",
                                        "receive (a status line, then the data line follows)" };
    for (int hd = 0; hd < passes; hd++) {
        print("HD=%d (%s):\n", hd, hd ? "push-pull" : "open drain");
        for (int s = 0; s < TURN_STEPS; s++) {
            uint16_t want, mask;
            turn_expect(mode, turn_steps[s].dir, turn_steps[s].out, turn_steps[s].low, &want, &mask);
            uint16_t diff = (got[hd][s] ^ want) & mask;
            if (!diff) continue;
            print("  ");
            turn_describe(mode, s, print);
            print(":");
            for (int r = 0; r < LOOP_RX; r++)
                if (diff & (1u << r)) print(" %s (J2-%d) %s", names_rx[r], j2_rx[r], want & (1u << r) ? "low" : "high");
            print("\n");
        }
        if (!tripped[hd]) {
            print("  (round trips skipped)\n");
            continue;
        }
        for (int k = 0; k < 3; k++) {
            if (tt[hd].fails[k]) print("  %s: %lu of %d timed out\n", what[k], (unsigned long)tt[hd].fails[k], TURN_TRIPS);
            else print("  %s: worst %lu ns\n", what[k], (unsigned long)(tt[hd].worst[k] * 1000 / mhz));
        }
    }
    if (bad[0]) print("(push-pull not tried; check the jumpers: loop with no number lists them)\n");
    if (!bad[0] && !bad[1]) {
        print("loop %d passed, %d round trips each way, open drain and push-pull\n", mode, TURN_TRIPS);
        return 0;
    }
    print("loop %d failed\n", mode);
    return bad[0] + bad[1];
}

int link_loop(int mode, link_print_fn print) {
    if (mode == 4 || mode == 5) return loop_turn(mode, print);
    if (mode < 1 || mode > 3) {
        print("loop <1-5>, with jumpers on J2. 1-3: each driver to one receiver\n");
        for (int m = 1; m <= 3; m++) loop_jumpers(m, print);
        print("4, 5: turning the data bus around\n");
        for (int m = 4; m <= 5; m++) turn_jumpers(m, print);
        print("Take the jumpers off before running walk or connecting a box.\n");
        return -1;
    }
    const uint32_t out[] = { M_ACK, M_BUSY, M_PERROR, M_SELECT, M_FAULT };
    uint32_t rx[LOOP_RX];
    loop_rx_pins(rx);
    uint16_t got[2][6];

    pause_req = true;
    while (!paused) { }
    sio_hw->gpio_clr = M_DIR;            // data lines are receivers: cable -> Pico
    sio_hw->gpio_oe_clr = M_DATA;
    for (int hd = 0; hd < 2; hd++) {     // open drain with the 161284's pull-ups, then push-pull
        if (hd) sio_hw->gpio_set = M_HD; else sio_hw->gpio_clr = M_HD;
        for (int step = 0; step < 6; step++) {
            hw_status(M_STATUS & ~(step ? out[step - 1] : 0));
            busy_wait_us_32(20);
            got[hd][step] = loop_read(rx);
        }
    }
    sio_hw->gpio_set = M_HD;             // as link_init left it; port_resume() restores the status lines
    pause_req = false;
    while (paused) { }

    print("parallel2usb rev %d, ", link_board.rev);
    loop_jumpers(mode, print);
    int bad = 0;
    for (int hd = 0; hd < 2; hd++) {
        for (int step = 0; step < 6; step++) {
            uint16_t want = loop_expect(mode, step), diff = got[hd][step] ^ want;
            if (!diff) continue;
            bad++;
            print("HD=%d, %s:", hd, step ? names_out[step - 1] : "all drivers high");
            if (step) print(" low:");
            for (int r = 0; r < LOOP_RX; r++)
                if (diff & (1u << r)) print(" %s (J2-%d) %s", names_rx[r], j2_rx[r], want & (1u << r) ? "went low" : "stayed high");
            print("\n");
        }
    }
    if (!bad) {
        print("loop %d passed, open drain and push-pull\n", mode);
        return 0;
    }
    for (int m = 1; m <= 3; m++) {
        bool match = m != mode;
        for (int hd = 0; hd < 2 && match; hd++)
            for (int step = 0; step < 6; step++) if (got[hd][step] != loop_expect(m, step)) match = false;
        if (match) print("(the readings match loop %d's jumpers)\n", m);
    }
    print("loop %d: %d problem%s\n", mode, bad, bad > 1 ? "s" : "");
    return bad;
}

static const link_board_t rev1 = BOARD_REV1, rev2 = BOARD_REV2;

static int detect_board(void) {
    if (BRIDGE_BOARD) return BRIDGE_BOARD;
    gpio_init(PIN_REV_STRAP);
    gpio_pull_up(PIN_REV_STRAP);
    busy_wait_us(100);
    int rev = gpio_get(PIN_REV_STRAP) ? 1 : 2;
    gpio_disable_pulls(PIN_REV_STRAP);
    return rev;
}

void link_init(void) {
    link_select_board(detect_board() == 1 ? &rev1 : &rev2);
    const uint32_t outputs = M_STATUS | M_DIR | M_HD | (LINK_DEBUG_PINS ? link_pins.dbg0 | link_pins.dbg1 : 0);
    gpio_init_mask(M_DATA | M_STROBE | M_AUTOFD | M_INIT | M_SELIN | outputs);
    gpio_set_dir_in_masked(M_DATA | M_STROBE | M_AUTOFD | M_INIT | M_SELIN);
    gpio_put_masked(outputs, M_ACK | M_FAULT | M_SELECT | M_HD);   // idle levels; DIR = cable -> Pico
    gpio_set_dir_out_masked(outputs);
    mirror_setup();
    port_resume();
    multicore_launch_core1(core1_main);
}
#else
// host and emulator builds: the board is picked with link_select_board() first
void link_init(void) { }
void link_print_pins(link_print_fn print) { print("no pins on this build\n"); }
bool link_walk(void) { return false; }
int link_loop(int mode, link_print_fn print) { print("no pins on this build\n"); return -1; }
void link_sniff(int wait_s, int record_s, link_print_fn print) { print("no pins on this build\n"); }
#endif
