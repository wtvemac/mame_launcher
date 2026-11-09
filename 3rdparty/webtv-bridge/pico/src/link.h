// The parallel port's pins (parallel2usb, link_hw.h): which board, core 1's start, and the
// bring-up tests below. The protocol engine itself is port.c.
#pragma once
#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

void link_init(void);                  // pins to idle; on the Pico also starts core 1 (port_step)

// bring-up (Pico only; the emulator build says so)
typedef void (*link_print_fn)(const char *fmt, ...);
void link_print_pins(link_print_fn print);   // board revision and every line's level right now
bool link_walk(void);                        // the walk pattern below, for tools/saleae/lptscope.py
int link_loop(int mode, link_print_fn print); // loopback test below; problems found, -1 = not run
// a logic-analyzer look at the cable from the Pico's side: wait up to wait_s for the first change,
// then record every change of the lines (both directions) for record_s, and print them
void link_sniff(int wait_s, int record_s, link_print_fn print);

// The walk: the engine pauses and core 0, with interrupts off, pulses the bridge's outputs one
// after another: nAck, Busy, PError, Select, nFault, then D0..D7. Line k (0-based) gets k + 1
// pulses, each 2 us away from its idle level and 2 us back, with 100 us between lines. For the
// data lines DIR goes to 1 with D0-D7 = 0 first, and each pulse is a 1. Afterwards the engine
// resets and carries on. Idle: nAck 1, Busy 0, PError 0, Select 1 (no carrier), nFault 1.
//
// The loopback test, with jumpers on J2 and no box: each of the five drivers (nAck, Busy,
// PError, Select, nFault) is jumpered to one receiver. The receivers, in order, are nStrobe,
// nAutoFd, nInit, nSelectIn, D0-D7; loop 1 takes the first five, loop 2 the next five and
// loop 3 the last two (nAck-D6, Busy-D7). The engine pauses, DIR goes to 0 so the data lines
// are receivers, and with HD = 0 (open drain) and then HD = 1 (push-pull) the test reads every
// receiver with all drivers high, then with each driver low in turn. Each receiver must follow
// its own driver and nothing else, so an open joint or a short between any two lines shows up.
// Loop 4 and 5 turn the data bus around: four data lines (D0-D3 for loop 4, D4-D7 for loop 5)
// are jumpered to nStrobe, nAutoFd, nInit and nSelectIn, and nAck, Busy, PError and Select to
// the other four. With DIR = 1 the bridge drives a zero across the four and reads them back on
// the control lines; with DIR = 0 it releases the bus and the status lines drive the other four.
// Then 1000 round trips (drive, release, receive) with changing patterns, timing each step. All
// open drain, then push-pull, which is only tried when open drain found the jumpers right (the
// wrong jumpers would have data outputs fighting status outputs).
