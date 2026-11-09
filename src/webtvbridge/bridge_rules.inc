// license:BSD-3-Clause
// Appended to a private build copy of upstream port.c. All PARMOD knowledge
// stays in this external adapter; MAME receives only generic electrical rules.
#include <string.h>
extern int bridge_reboot_pending(void);
static uint32_t bridge_generation;
static unsigned br_pos, br_count;
static uint8_t *br_out;
static void br_rule(unsigned state, unsigned next, unsigned control_mask, unsigned control,
                    unsigned data_mask, unsigned data, unsigned drive_mask, unsigned drive,
                    unsigned rose, unsigned fell, unsigned nonempty, unsigned empty,
                    unsigned flags, const uint8_t *actions, unsigned count) {
    uint8_t *r = br_out + br_pos;
    memset(r, 0, 48);
    r[0] = state; r[1] = next; r[2] = control_mask; r[3] = control;
    r[4] = data_mask; r[5] = data; r[6] = drive_mask; r[7] = drive;
    r[8] = rose; r[9] = fell; r[10] = nonempty; r[11] = empty;
    r[12] = count; r[13] = flags;
    if (count) memcpy(r + 16, actions, count * 4);
    br_pos += 48; br_count++;
}
static void br_fifo(ring_t *ring, unsigned tokens) {
    unsigned n = ring ? ring_used(ring) : tokens;
    unsigned capacity = ring ? sizeof pmm_to_s : tokens;
    br_out[br_pos++] = n >> 8; br_out[br_pos++] = n;
    br_out[br_pos++] = capacity >> 8; br_out[br_pos++] = capacity;
    for (unsigned i = 0; i < n; i++) br_out[br_pos++] = ring ? ring_peek(ring, i) : 0;
}
unsigned bridge_engine_config(uint8_t *out, unsigned capacity) {
    if (capacity < 49152) return 0;
    br_out = out; br_pos = 8; br_count = 0;
    memset(out, 0, 8); out[0] = 1;
    uint32_t generation = bridge_generation + 1;
    bridge_generation = generation;
    out[4] = generation >> 24; out[5] = generation >> 16; out[6] = generation >> 8; out[7] = generation;
    bool active = !bridge_reboot_pending() && mode == PORT_PARMOD && want_mode < 0 && T.session && !T.pending && !T.full && !T.pk_more;
    if (active) {
        // Reset/probe patterns always go back to the real firmware immediately.
        static const uint8_t probes[] = {0xf1, 0xf2, 0xf4, 0xf8};
        for (unsigned i = 0; i < sizeof probes; i++)
            br_rule(0, 0, 15, 15, 255, probes[i], 255, 255, 0, 0, 0, 0, 1, NULL, 0);
        static const uint8_t real_probes[] = {0x00, 0x51, 0x52, 0x54, 0x58};
        for (unsigned i = 0; i < sizeof real_probes; i++)
            br_rule(0, 0, 15, 3, 255, real_probes[i], 255, 255, 0, 0, 0, 0, 1, NULL, 0);
        // Codes 2 and 6: present the next modem/console byte, latch Select,
        // then raise Busy. Pop locally at the falling edge; no other transaction
        // can inspect the queue until the matching rise releases the bus.
        for (unsigned channel = 0; channel < 3; channel++) {
            unsigned fifo = channel == 2 ? 3 : channel;
            unsigned code = channel == 0 ? C_MODEM_IN : channel == 1 ? C_CON_IN : C_CTRL_IN;
            if (channel == 2 && (!ring_empty(&pmpk_kind) || T.pk_given)) continue;
            unsigned control = (code & 4 ? 8 : 0) | (code & 2 ? 4 : 0) | (code & 1 ? 2 : 0);
            uint8_t actions[] = {3, fifo, 255, 0, 4, fifo, 0, 0, 2, 8, 8, 0, 2, 2, 2, 0};
            br_rule(0, 1, 15, control, 0, 0, 255, 0, 0, 1, 1 << fifo, 0, 0, actions, 4);
            // An empty channel supplies the dynamic status of all three queues.
            // This polling path must be local too; otherwise an idle modem
            // continues to pay a network round trip for every empty read.
            for (unsigned bits = 0; bits < 8; bits++) {
                unsigned occupied = (bits & 3) | (bits & 4 ? 8 : 0);
                if (occupied & (1u << fifo)) continue;
                unsigned status = bits | (T.carrier ? ST_CARRIER : 0);
                uint8_t empty_actions[] = {1, status, 255, 0, 2, 8, 0, 0, 2, 2, 2, 0};
                br_rule(0, 1, 15, control, 0, 0, 255, 0, 0, 1, occupied, 11 ^ occupied, 0, empty_actions, 3);
            }
        }
        // Codes 3 and 1: accept a byte. Modem output is credit-limited by
        // the actual firmware ring's free space; console output may be dropped
        // by the firmware exactly as in raw mode when its ring is full.
        uint8_t modem[] = {4, 2, 0, 0, 2, 8, 8, 0, 2, 2, 2, 0};
        uint8_t console[] = {2, 8, 8, 0, 2, 2, 2, 0};
        br_rule(0, 2, 15, 6, 0, 0, 255, 255, 0, 1, 4, 0, 0, modem, 3);
        br_rule(0, 2, 15, 2, 0, 0, 255, 255, 0, 1, 0, 0, 0, console, 2);
        // Read completion: preserve each release transition, and recompute
        // nFault after taking the last byte from the currently queued streams.
        bool extra = !ring_empty(&pmpk_kind) || T.pk_given;
        uint8_t finish[] = {1, 255, 255, 0, 1, 255, 0, 0, 2, 26, 0, 0};
        finish[10] = extra ? 0 : 16;
        br_rule(1, 0, 1, 1, 0, 0, 0, 0, 1, 0, 0, 11, 0, finish, 3);
        finish[10] = 0;
        br_rule(1, 0, 1, 1, 0, 0, 0, 0, 1, 0, 1, 0, 0, finish, 3);
        br_rule(1, 0, 1, 1, 0, 0, 0, 0, 1, 0, 2, 0, 0, finish, 3);
        br_rule(1, 0, 1, 1, 0, 0, 0, 0, 1, 0, 8, 0, 0, finish, 3);
        uint8_t finish_write[] = {2, 10, 0, 0};
        br_rule(2, 0, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, finish_write, 1);
        // Data/direction setup with nStrobe high is independent of a reply.
        br_rule(0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, NULL, 0);
    }
    if (!bridge_reboot_pending() && mode == PORT_DONGLE && want_mode < 0 && port_diag_tx_idle()) {
        // The physical dongle's combinational mirror is also an electrical
        // rule. nSelectIn edges still fall back for the firmware UART decoder.
        unsigned select = last_in & M_SELIN ? 8 : 0;
        for (unsigned data = 0; data < 16; data++) {
            uint8_t action[] = {2, 31, (data & 7) | (data & 8 ? 24 : 0), 0};
            br_rule(0, 0, 8, select, 15, data, 255, 255, 0, 0, 0, 0, 0, action, 1);
        }
        uint8_t released[] = {2, 31, 31, 0};
        br_rule(0, 0, 8, select, 0, 0, 255, 0, 0, 0, 0, 0, 0, released, 1);
    }
    out[2] = br_count >> 8; out[3] = br_count;
    br_fifo(active ? &pmm_to : NULL, 0);
    br_fifo(active ? &pmc_to : NULL, 0);
    br_fifo(NULL, active ? ring_free(&pmm_from) : 0);
    br_fifo(active ? &pmrep : NULL, 0);
    for (unsigned i = 4; i < 8; i++) br_fifo(NULL, 0);
    return br_pos;
}
uint32_t bridge_engine_generation(void) { return bridge_generation; }

// Earliest autonomous diagnostic output on the existing 4-us tick grid.
// RX samples still run internally. A new host edge is applied before the next
// prediction, and core 0 remains a visible deadline because it can enqueue TX.
uint64_t bridge_diag_output_deadline(uint64_t now, uint64_t tick) {
    if (mode != PORT_DONGLE || tick == UINT64_MAX) return tick;
    // Driving/releasing D0-D3 also settles the software dongle mirror on
    // the following tick. That status transition is externally observable.
    if ((atomic_load(&sim_bridge_pins) ^ mirror(hw_in())) & M_STATUS) return tick;
    uint32_t elapsed, threshold;
    if (D.tx_busy) {
        elapsed = (uint32_t)(now / 1000) - D.tx_t0;
        threshold = half_bits_us(2u * (uint32_t)(D.tx_n + 1));
    } else {
        if (ring_empty(&diag_tx)) return UINT64_MAX;
        // A receive completion/glitch can change whether queued TX may start.
        // Keep its fine deadline visible rather than predicting an RX result.
        if (D.rx_busy) return tick;
        elapsed = (uint32_t)(now / 1000) - D.quiet_since;
        threshold = QUIET_US;
    }
    if (elapsed >= threshold) return tick;
    uint64_t base = now - now % 1000;
    uint64_t delay = (uint64_t)(threshold - elapsed) * 1000;
    if (base > UINT64_MAX - delay) return tick;
    uint64_t target = base + delay;
    if (target <= tick) return tick;
    return tick + ((target - tick + 3999) / 4000) * 4000;
}
