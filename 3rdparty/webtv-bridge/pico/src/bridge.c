// Glue between the link (bytes and signals to/from the box) and the modem/PPP/gateway stack.
#include "bridge.h"
#include "ppp.h"
#include "gw.h"
#include "net.h"

void bridge_init(void) { modem_init(); }

void bridge_poll(uint32_t now) {
    modem_poll(now);
    ppp_poll(now);
    gw_poll(now);
}

void bridge_from_box(const uint8_t *d, size_t n) { for (size_t i = 0; i < n; i++) modem_from_host(d[i]); }
size_t bridge_to_box(uint8_t *d, size_t max) { return modem_out_read(d, max); }
size_t bridge_to_box_pending(void) { return modem_out_used(); }
void bridge_set_dtr(bool on) { modem_set_dtr(on); }
bool bridge_carrier(void) { return modem_dcd(); }

void bridge_link_reset(void) {
    modem_hangup(NULL);
    modem_reset();
}

void bridge_status(void (*print)(const char *fmt, ...)) {
    net_status(print);
    modem_status(print);
    ppp_status(print);
    gw_status(print);
}
