// The bridge firmware built for the emulator (WebAssembly, emscripten): the same C as the Pico
// build, both cores, with only the platform underneath replaced. src/machine/picosim.js loads it
// and puts it on the emulated box's parallel port:
//
//   pins     the board's GPIO words (link_hw.h, LINK_HOST_SIM): the box's lines come in through
//            sim_box() and the firmware's levels go out through sim_status() / sim_data(); every
//            change the firmware makes is reported at once (js_pins_changed), so an nAck pulse
//            reaches the box as an edge
//   core 1   port_step(): on every change the box makes, and on ticks while the diagnostics
//            serial needs timing
//   core 0   sim_core0(): one pass of main_pico.c's loop, every 250 us of emulated time
//   time     the emulated clock (js_now_us), not the computer's
//   WiFi     net_sim.c: a simulated radio, and TCP through the emulator process's sockets
//   flash    the settings sector, kept in a file by picosim.js
#include <stdio.h>
#include <string.h>
#include <stdatomic.h>
#include <emscripten/emscripten.h>
#include "link.h"
#include "link_hw.h"
#include "port.h"
#include "bridge.h"
#include "settings.h"
#include "console.h"
#include "gw.h"
#include "web.h"
#include "net.h"
#include "log.h"
#include "sb/sb.h"

// sim_lib.js
extern uint32_t js_now_us(void);
extern void js_pins_changed(void);
extern int js_settings_read(void *buf, uint32_t len);
extern int js_settings_write(const void *buf, uint32_t len);
extern void js_reboot(void);

_Atomic uint32_t sim_box_pins;
_Atomic uint32_t sim_bridge_pins;
_Atomic int sim_box_drives_data;

void sim_trace(void) { js_pins_changed(); }
uint32_t sim_now_us(void) { return js_now_us(); }
uint32_t bridge_now_ms(void) { return js_now_us() / 1000u; }

bool settings_storage_read(void *buf, uint32_t len) { return js_settings_read(buf, len) != 0; }
bool settings_storage_write(const void *buf, uint32_t len) { return js_settings_write(buf, len) != 0; }
void platform_reboot(void) { js_reboot(); }

// SOLO CTRL bits (src/lptmodem/link.js) and STAT bits, as the box sees its lines
enum { C_STB_N = 1, C_INIT_N = 2, C_AUTOFD_N = 4, C_SELIN_N = 8 };
enum { S_BUSY = 1, S_ACK_N = 2, S_FAULT_N = 4, S_SEL = 8, S_PE = 16 };

EMSCRIPTEN_KEEPALIVE void sim_init(int log_level, const char *service) {
    static const link_board_t rev2 = BOARD_REV2;
    link_select_board(&rev2);
    atomic_store(&sim_box_pins, M_STROBE | M_AUTOFD | M_INIT | M_SELIN | M_DATA);
    atomic_store(&sim_box_drives_data, 0);
    link_init();
    port_resume();
    settings_load();
    if (log_level >= 0) settings.log_level = (uint8_t)log_level;
    if (service && service[0]) snprintf(settings.service, sizeof settings.service, "%s", service);
    log_level = settings.log_level;
    gw_set_service(settings.service);
    bridge_init();
    web_port = 8080;
    LOGI("[pico] WebTV LPT modem bridge (simulated Pico 2 W, parallel2usb rev 2), start-up %s\n",
         settings.no_startup ? "off" : "on");
    port_service_start();
}

// the box's lines: ctrl = SOLO CTRL levels, data = the byte on D0-D7 when the box drives them
EMSCRIPTEN_KEEPALIVE void sim_box(int ctrl, int data, int drives) {
    uint32_t v = (ctrl & C_STB_N ? M_STROBE : 0) | (ctrl & C_INIT_N ? M_INIT : 0) | (ctrl & C_AUTOFD_N ? M_AUTOFD : 0)
                 | (ctrl & C_SELIN_N ? M_SELIN : 0) | byte_to_pins((uint8_t)data);
    atomic_store(&sim_box_pins, v);
    atomic_store(&sim_box_drives_data, drives);
    port_step();
}

EMSCRIPTEN_KEEPALIVE void sim_tick(void) { port_step(); }

EMSCRIPTEN_KEEPALIVE int sim_status(void) {
    uint32_t b = atomic_load(&sim_bridge_pins);
    return (b & M_BUSY ? S_BUSY : 0) | (b & M_ACK ? S_ACK_N : 0) | (b & M_FAULT ? S_FAULT_N : 0) | (b & M_SELECT ? S_SEL : 0)
           | (b & M_PERROR ? S_PE : 0);
}

// the byte the bridge drives, or -1 (its 161284 turned towards the Pico)
EMSCRIPTEN_KEEPALIVE int sim_data(void) {
    uint32_t b = atomic_load(&sim_bridge_pins);
    return b & M_DIR ? pins_to_byte(b) : -1;
}

EMSCRIPTEN_KEEPALIVE int sim_mode(void) { return (int)port_mode(); }

// one pass of core 0's loop (main_pico.c), then core 1 sees what core 0 changed
EMSCRIPTEN_KEEPALIVE void sim_core0(void) {
    uint32_t now = bridge_now_ms();
    net_poll(now);
    port_service(now);
    bridge_poll(now);
    port_service(now);
    web_poll(now);
    console_poll();
    port_step();
}

EMSCRIPTEN_KEEPALIVE void sim_console(int c) { console_input(c); }

// the start-up's state (sb_state_t), for tests (tools/bridgesim.mjs)
EMSCRIPTEN_KEEPALIVE int sim_state(void) { return (int)sb_startup_state(); }
