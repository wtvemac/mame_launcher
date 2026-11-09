// Build-time configuration for the LPT modem bridge (WEBTV_LPT_BRIDGE.md).
#pragma once

// ---------------------------------------------------------------- boards
// Tom Verbeure's parallel2usb boards (github.com/tomverbeure/fake_parallel_printer), with a
// Pico 2 W in place of the Pico. link_init() tells the revisions apart the way his firmware
// does: GP28 with its pull-up reads 1 on rev 1 (floating) and 0 on rev 2 (tied to GND).
// Pico GPIO -> 161284 A side; B/Y/C side -> DB-25. The 161284 doesn't invert; DIR = 1 drives
// A1-A8 -> B1-B8 (the bridge drives the cable), DIR = 0 is B -> A. A9-A13 -> Y9-Y13 (the five
// status lines) and C14-C17 -> A14-A17 (the four control lines) don't depend on DIR. HD = 1
// makes the cable-side outputs totem-pole; HD = 0 is open drain with the 1.4k pull-ups.
// Both boards pull DIR and HD down (4.7k), so before the firmware runs the data lines are
// inputs on the Pico and the status lines are open drain.
//
//   signal (DB-25 pin)   rev 2   rev 1
//   D0-D7  (2-9)         GP3-10  GP11-4 (reversed)
//   nStrobe (1)          GP11    GP15     in
//   nAutoFd (14)         GP0     GP14     in
//   nInit   (16)         GP1     GP13     in
//   nSelectIn (17)       GP2     GP12     in
//   nFault  (15)         GP12    GP22     out
//   nAck    (10)         GP13    GP21     out
//   Busy    (11)         GP14    GP20     out
//   PError  (12)         GP15    GP19     out
//   Select  (13)         GP18    GP18     out
//   161284 HD            GP16    GP17     out
//   161284 DIR           GP17    GP16     out
//   console UART         UART1 GP20 TX / GP21 RX    UART0 GP0 TX / GP1 RX
//   debug strobes        GP26, GP27 (both)
#define BOARD_REV2 { .rev = 2, .d0 = 3, .reversed = false, .strobe = 11, .autofd = 0, .init = 1, \
    .selin = 2, .fault = 12, .ack = 13, .busy = 14, .perror = 15, .select = 18, .hd = 16, .dir = 17, \
    .uart = 1, .uart_tx = 20, .uart_rx = 21, .dbg0 = 26, .dbg1 = 27 }
#define BOARD_REV1 { .rev = 1, .d0 = 11, .reversed = true, .strobe = 15, .autofd = 14, .init = 13, \
    .selin = 12, .fault = 22, .ack = 21, .busy = 20, .perror = 19, .select = 18, .hd = 17, .dir = 16, \
    .uart = 0, .uart_tx = 0, .uart_rx = 1, .dbg0 = 26, .dbg1 = 27 }
#define PIN_REV_STRAP  28
#define BRIDGE_BOARD   0      // 0 = detect from GP28; 1 or 2 forces a revision

// Debug strobes for a logic analyzer: dbg0 toggles each time the link engine acts on an edge
// from the box (compare with the cable to see the bridge's response time); dbg1 is high while
// the bridge holds Busy because its buffer toward core 0 is full.
#define LINK_DEBUG_PINS 1

// Timing of the bridge's own outputs, in CPU cycles at 150 MHz.
#define LINK_DATA_SETUP_CYCLES  150  // our byte (and Select) on the cable before Busy rises (1 us: the real cable needs more than 200 ns)
#define LINK_STROBE_SAMPLE_CYCLES 150 // wait after seeing nStrobe low before sampling data (1 us: the OS driver strobes right after writing DATA)
#define LINK_ACK_PULSE_CYCLES   300  // nAck low time for an attention pulse (2 us)

// ---------------------------------------------------------------- buffers
#define MODEM_OUT_SIZE      16384   // modem/PPP output waiting for the link (power of two)
#define PPP_MAX_FRAME       2048    // largest PPP frame accepted from the box
#define GW_MAX_CONNS        8       // concurrent TCP connections from the box
#define GW_CONN_BUF         8192    // per connection: remote data not yet acked by the box

// ---------------------------------------------------------------- network
#define BOX_IP      {10, 0, 0, 2}
#define GATEWAY_IP  {10, 0, 0, 1}
#define WTVP_PORT_MIN 1600          // connections to these ports go to the WebTV service
#define WTVP_PORT_MAX 1699
