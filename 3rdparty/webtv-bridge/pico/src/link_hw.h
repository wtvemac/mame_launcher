// Pin access for the link engine. On the Pico: SIO registers, with the 161284 between the
// pins and the cable (bridge_config.h). Built for the host (LINK_HOST_SIM), the pins are words
// shared with a simulated box (host/link_test.c), laid out like whichever board is selected.
#pragma once
#include <stdint.h>
#include <stdbool.h>
#include "bridge_config.h"

// GPIO numbers of one board (BOARD_REV1 / BOARD_REV2)
typedef struct {
    uint8_t rev, d0;
    bool reversed;            // D0..D7 on descending GPIOs (rev 1)
    uint8_t strobe, autofd, init, selin, fault, ack, busy, perror, select, hd, dir;
    uint8_t uart, uart_tx, uart_rx, dbg0, dbg1;
} link_board_t;

// the same as masks, for the engine
typedef struct {
    uint32_t data, strobe, autofd, init, selin, ack, busy, perror, select, fault, dir, hd, status;
    uint32_t dbg0, dbg1;
    uint8_t d0;
    bool reversed;
} link_pins_t;

extern link_board_t link_board;
extern link_pins_t link_pins;
void link_select_board(const link_board_t *b);

#define M_DATA    (link_pins.data)
#define M_STROBE  (link_pins.strobe)
#define M_AUTOFD  (link_pins.autofd)
#define M_INIT    (link_pins.init)
#define M_SELIN   (link_pins.selin)
#define M_ACK     (link_pins.ack)
#define M_BUSY    (link_pins.busy)
#define M_DIR     (link_pins.dir)
#define M_PERROR  (link_pins.perror)
#define M_SELECT  (link_pins.select)
#define M_FAULT   (link_pins.fault)
#define M_HD      (link_pins.hd)
#define M_STATUS  (link_pins.status)

#define ALWAYS_INLINE static inline __attribute__((always_inline))

ALWAYS_INLINE uint32_t bitrev32(uint32_t x) {
#ifndef LINK_HOST_SIM
    __asm__("rbit %0, %1" : "=r"(x) : "r"(x));
    return x;
#else
    uint32_t r = 0;
    for (int i = 0; i < 32; i++, x >>= 1) r = (r << 1) | (x & 1);
    return r;
#endif
}

// D0-D7 between a byte and the GPIO word
ALWAYS_INLINE uint8_t pins_to_byte(uint32_t in) {
    return link_pins.reversed ? (uint8_t)(bitrev32(in) >> (31 - link_pins.d0)) : (uint8_t)(in >> link_pins.d0);
}
ALWAYS_INLINE uint32_t byte_to_pins(uint8_t b) {
    return link_pins.reversed ? bitrev32((uint32_t)b << (31 - link_pins.d0)) : (uint32_t)b << link_pins.d0;
}

#ifndef LINK_HOST_SIM
#include "hardware/structs/sio.h"
#include "hardware/timer.h"
#include "hardware/sync.h"
#include "pico/platform.h"

#define LINK_RAM __not_in_flash_func

ALWAYS_INLINE uint32_t hw_in(void) { return sio_hw->gpio_in; }
// drive the status lines (all five at once)
ALWAYS_INLINE void hw_status(uint32_t levels) {
    sio_hw->gpio_togl = (sio_hw->gpio_out ^ levels) & M_STATUS;
}
ALWAYS_INLINE void hw_data_drive(uint8_t b) {
    sio_hw->gpio_togl = (sio_hw->gpio_out ^ byte_to_pins(b)) & M_DATA;
    sio_hw->gpio_oe_set = M_DATA;      // pins first, then turn the transceiver around
    sio_hw->gpio_set = M_DIR;
}
ALWAYS_INLINE void hw_data_release(void) {
    sio_hw->gpio_clr = M_DIR;          // transceiver back to cable -> pins first
    sio_hw->gpio_oe_clr = M_DATA;
}
ALWAYS_INLINE void hw_delay(uint32_t cycles) {
    for (volatile uint32_t i = 0; i < cycles / 4; i++) { }
}
ALWAYS_INLINE uint32_t hw_now_us(void) { return time_us_32(); }
// The dongle's mirror (D0 -> nAck, D1 -> Busy, D2 -> PError, D3 -> nFault and Select) in PIO: the
// boot ROM reads the status lines about a microsecond after it drives each probe byte, too soon
// for a software loop to be sure of. On: the five status pins are handed to PIO state machines
// that copy them continuously; off: back to SIO (whose levels dongle_step keeps current, so the
// hand-back doesn't glitch). link.c; core 1 calls them, so they run from RAM.
void hw_mirror_on(void);
void hw_mirror_off(void);
ALWAYS_INLINE void hw_dbg_toggle(uint32_t mask) { if (LINK_DEBUG_PINS) sio_hw->gpio_togl = mask; }
ALWAYS_INLINE void hw_dbg_put(uint32_t mask, bool on) {
    if (LINK_DEBUG_PINS) { if (on) sio_hw->gpio_set = mask; else sio_hw->gpio_clr = mask; }
}
#else
#include <stdatomic.h>
#define LINK_RAM(f) f
extern _Atomic uint32_t sim_box_pins;      // nStrobe, nAutoFd, nInit, nSelectIn, and D0-D7 when the box drives
extern _Atomic uint32_t sim_bridge_pins;   // status lines, and D0-D7 + DIR when the bridge drives
extern _Atomic int sim_box_drives_data;
void sim_trace(void);                      // link_test.c: record the cable state after a change
uint32_t sim_now_us(void);                 // link_test.c: the clock core 1 times the serial line by

ALWAYS_INLINE uint32_t hw_in(void) {
    uint32_t box = atomic_load(&sim_box_pins), br = atomic_load(&sim_bridge_pins);
    uint32_t data = (br & M_DIR) ? (br & M_DATA) : atomic_load(&sim_box_drives_data) ? (box & M_DATA) : M_DATA;
    return (box & (M_STROBE | M_AUTOFD | M_INIT | M_SELIN)) | data | (br & (M_STATUS | M_DIR));
}
ALWAYS_INLINE void hw_status(uint32_t levels) {
    uint32_t v = atomic_load(&sim_bridge_pins);
    if (((v ^ levels) & M_STATUS) == 0) return;
    atomic_store(&sim_bridge_pins, (v & ~M_STATUS) | (levels & M_STATUS));
    sim_trace();
}
ALWAYS_INLINE void hw_data_drive(uint8_t b) {
    uint32_t v = atomic_load(&sim_bridge_pins);
    atomic_store(&sim_bridge_pins, (v & ~M_DATA) | byte_to_pins(b) | M_DIR);
    sim_trace();
}
ALWAYS_INLINE void hw_data_release(void) {
    uint32_t v = atomic_load(&sim_bridge_pins);
    atomic_store(&sim_bridge_pins, v & ~(M_DIR | M_DATA));
    sim_trace();
}
ALWAYS_INLINE void hw_delay(uint32_t cycles) { (void)cycles; }
ALWAYS_INLINE uint32_t hw_now_us(void) { return sim_now_us(); }
ALWAYS_INLINE void hw_mirror_on(void) { }     // the sim's mirror is dongle_step's, in software
ALWAYS_INLINE void hw_mirror_off(void) { }
ALWAYS_INLINE void hw_dbg_toggle(uint32_t mask) { (void)mask; }
ALWAYS_INLINE void hw_dbg_put(uint32_t mask, bool on) { (void)mask; (void)on; }
#endif
