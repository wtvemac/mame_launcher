// PPP server, port of the emulator's src/net/ppp.js. IPv4 datagrams from the box go to
// gw_input(); gw.c sends with ppp_send_ip(). Frames towards the box are encoded straight into
// the modem's output queue.
//
// The box is asked to log in with PAP, as its TellyScript expects; any login is accepted (the
// box's own is WPB/<serial> or wtv_<...>). WiFi is set up on the TV or the console, not here.
#include <string.h>
#include <stdio.h>
#include "ppp.h"
#include "gw.h"
#include "bridge.h"
#include "bridge_config.h"
#include "log.h"

#define PROTO_IP   0x0021
#define PROTO_LCP  0xc021
#define PROTO_PAP  0xc023
#define PROTO_CHAP 0xc223
#define PROTO_IPCP 0x8021

#define FLAG 0x7e
#define ESC  0x7d

enum { CONF_REQ = 1, CONF_ACK, CONF_NAK, CONF_REJ, TERM_REQ, TERM_ACK, CODE_REJ, PROTO_REJ, ECHO_REQ, ECHO_REP, DISCARD_REQ };
enum { ST_CLOSED, ST_STARTING, ST_REQSENT, ST_OPENED };

static const uint8_t local_ip[4] = GATEWAY_IP, peer_ip[4] = BOX_IP;

static struct {
    bool active;
    uint32_t start_at;       // when to send the first LCP request (0 = not pending)
    uint8_t rx[PPP_MAX_FRAME];
    int rx_len;
    bool rx_esc, rx_drop;
    uint32_t tx_accm, pending_accm;
    uint8_t id;
    uint32_t magic;
    int lcp_state;
    bool lcp_our_acked, lcp_their_acked;
    int lcp_retries;
    uint8_t lcp_req_id;
    uint32_t lcp_timer;
    bool ipcp_started, ipcp_our_acked, ipcp_their_acked, ipcp_no_addr;
    int ipcp_retries;
    uint8_t ipcp_req_id;
    uint32_t ipcp_timer;
    bool ip_up;
    bool want_pap, pap_ok;   // asking the box for PAP; it logged in
    uint16_t peer_mru;
    uint32_t rx_frames, tx_frames, bad_fcs, tx_dropped;
} p;

// ---------------------------------------------------------------- FCS-16 (RFC 1662 appendix C)

static uint16_t fcs_tab[256];

static void fcs_init(void) {
    for (int b = 0; b < 256; b++) {
        uint16_t v = (uint16_t)b;
        for (int i = 0; i < 8; i++) v = (v & 1) ? (uint16_t)((v >> 1) ^ 0x8408) : (uint16_t)(v >> 1);
        fcs_tab[b] = v;
    }
}

static inline uint16_t fcs_step(uint16_t fcs, uint8_t b) { return (uint16_t)((fcs >> 8) ^ fcs_tab[(fcs ^ b) & 0xff]); }

// ---------------------------------------------------------------- framing

// Encode one frame into the modem queue; drops it (and returns false) if it doesn't fit.
static bool send_frame(uint16_t proto, const uint8_t *data, size_t len) {
    static uint8_t out[2 * (PPP_MAX_FRAME + 8) + 2];
    // LCP always goes out with the default ACCM (RFC 1662 7.1)
    uint32_t accm = proto == PROTO_LCP ? 0xffffffffu : p.tx_accm;
    size_t o = 0;
    uint16_t fcs = 0xffff;
    out[o++] = FLAG;
#define PUT(b) do { uint8_t _b = (b); \
        if (_b == FLAG || _b == ESC || (_b < 0x20 && ((accm >> _b) & 1))) { out[o++] = ESC; out[o++] = _b ^ 0x20; } \
        else out[o++] = _b; } while (0)
    uint8_t hdr[4] = { 0xff, 0x03, (uint8_t)(proto >> 8), (uint8_t)proto };
    for (int i = 0; i < 4; i++) { fcs = fcs_step(fcs, hdr[i]); PUT(hdr[i]); }
    if (len > PPP_MAX_FRAME) len = PPP_MAX_FRAME;
    for (size_t i = 0; i < len; i++) { fcs = fcs_step(fcs, data[i]); PUT(data[i]); }
    fcs ^= 0xffff;
    PUT(fcs & 0xff); PUT(fcs >> 8);
#undef PUT
    out[o++] = FLAG;
    if (!modem_out_put(out, o)) { p.tx_dropped++; return false; }
    p.tx_frames++;
    return true;
}

static void send_ctl(uint16_t proto, uint8_t code, uint8_t id, const uint8_t *body, size_t blen) {
    static uint8_t buf[PPP_MAX_FRAME];
    if (blen + 4 > sizeof buf) blen = sizeof buf - 4;
    buf[0] = code; buf[1] = id; buf[2] = (uint8_t)((blen + 4) >> 8); buf[3] = (uint8_t)(blen + 4);
    if (blen) memcpy(buf + 4, body, blen);
    send_frame(proto, buf, blen + 4);
}

bool ppp_send_ip(const uint8_t *pkt, size_t len) {
    if (!p.ip_up) return false;
    return send_frame(PROTO_IP, pkt, len);
}

bool ppp_ip_up(void) { return p.ip_up; }

size_t ppp_frame_room(void) {
    size_t f = modem_out_free();
    return f < 16 ? 0 : (f - 16) / 2;   // worst case every byte escaped
}

// ---------------------------------------------------------------- state

static void reset_state(void) {
    p.lcp_state = ST_STARTING;
    p.lcp_our_acked = p.lcp_their_acked = false;
    p.lcp_retries = 0; p.lcp_timer = 0;
    p.ipcp_started = p.ipcp_our_acked = p.ipcp_their_acked = p.ipcp_no_addr = false;
    p.ipcp_retries = 0; p.ipcp_timer = 0;
    p.ip_up = false;
    p.want_pap = true; p.pap_ok = false;
    p.peer_mru = 1500;
    p.tx_accm = p.pending_accm = 0xffffffffu;
}

static void down(const char *why) {
    if (!p.active) return;
    LOGI("[ppp] down: %s\n", why);
    p.active = false;
    reset_state();
    p.lcp_state = ST_CLOSED;
    modem_ppp_down(why);
}

void ppp_start(void) {
    static bool fcs_ready;
    if (!fcs_ready) { fcs_init(); fcs_ready = true; }
    reset_state();
    p.active = true;
    p.rx_len = 0; p.rx_esc = p.rx_drop = false;
    p.magic = (bridge_now_ms() * 2654435761u) ^ 0x5eedf00du;
    if (!p.magic) p.magic = 1;
    p.start_at = bridge_now_ms() + 500;   // like a real ISP, start LCP shortly after CONNECT
}

void ppp_stop(void) {
    p.active = false;
    p.start_at = 0;
    reset_state();
    p.lcp_state = ST_CLOSED;
}

// ---------------------------------------------------------------- LCP

static size_t put_opt(uint8_t *b, uint8_t type, const uint8_t *d, uint8_t n) {
    b[0] = type; b[1] = (uint8_t)(n + 2);
    memcpy(b + 2, d, n);
    return n + 2u;
}

static void lcp_request(uint32_t now) {
    if (p.lcp_our_acked || p.lcp_state == ST_CLOSED) return;
    if (p.lcp_retries++ >= 10) { down("LCP negotiation timed out"); return; }
    p.lcp_req_id = p.id++;
    uint8_t opts[12];
    size_t n = 0;
    if (p.want_pap) {
        static const uint8_t pap[2] = { PROTO_PAP >> 8, PROTO_PAP & 0xff };
        n += put_opt(opts + n, 3, pap, 2);
    }
    if (p.magic) {
        uint8_t mg[4] = { (uint8_t)(p.magic >> 24), (uint8_t)(p.magic >> 16), (uint8_t)(p.magic >> 8), (uint8_t)p.magic };
        n += put_opt(opts + n, 5, mg, 4);
    }
    send_ctl(PROTO_LCP, CONF_REQ, p.lcp_req_id, opts, n);
    p.lcp_timer = now + 3000;
}

static void ipcp_start(uint32_t now);

static void lcp_check_open(uint32_t now) {
    if (p.lcp_state == ST_OPENED || !p.lcp_our_acked || !p.lcp_their_acked) return;
    p.lcp_state = ST_OPENED;
    p.tx_accm = p.pending_accm;
    LOGI("[ppp] LCP opened (box mru=%u accm=%08lx%s)\n", p.peer_mru, (unsigned long)p.tx_accm, p.want_pap ? ", PAP" : "");
    if (!p.want_pap) ipcp_start(now);   // else after the box logs in
}

static void lcp_packet(uint8_t code, uint8_t id, const uint8_t *body, size_t blen, const uint8_t *raw, size_t rawlen, uint32_t now) {
    switch (code) {
    case CONF_REQ: {
        if (p.lcp_state == ST_OPENED) { LOGI("[ppp] LCP renegotiation\n"); reset_state(); p.lcp_state = ST_REQSENT; lcp_request(now); }
        static uint8_t rej[PPP_MAX_FRAME]; size_t nrej = 0;
        uint32_t accm = 0xffffffffu; uint16_t mru = 1500;
        for (size_t i = 0; i + 2 <= blen;) {
            uint8_t type = body[i], len = body[i + 1];
            if (len < 2 || i + len > blen) return;
            const uint8_t *d = body + i + 2;
            switch (type) {
            case 1: mru = (uint16_t)((d[0] << 8) | d[1]); break;
            case 2: accm = ((uint32_t)d[0] << 24) | ((uint32_t)d[1] << 16) | ((uint32_t)d[2] << 8) | d[3]; break;
            case 5: case 7: case 8: break;          // magic, PFC, ACFC
            default:                                  // auth (we can't authenticate to the box) and unknowns
                memcpy(rej + nrej, body + i, len); nrej += len;
            }
            i += len;
        }
        LOGD("[ppp] LCP ConfReq from box: mru=%u accm=%08lx%s\n", mru, (unsigned long)accm, nrej ? " (rejecting some)" : "");
        if (nrej) { send_ctl(PROTO_LCP, CONF_REJ, id, rej, nrej); return; }
        send_ctl(PROTO_LCP, CONF_ACK, id, body, blen);
        p.lcp_their_acked = true;
        p.peer_mru = mru;
        p.pending_accm = accm;
        if (p.lcp_state == ST_STARTING) { p.lcp_state = ST_REQSENT; lcp_request(now); }
        lcp_check_open(now);
        return;
    }
    case CONF_ACK:
        if (id != p.lcp_req_id) return;
        p.lcp_our_acked = true;
        lcp_check_open(now);
        return;
    case CONF_NAK: case CONF_REJ:
        if (id != p.lcp_req_id) return;
        for (size_t i = 0; i + 2 <= blen; i += body[i + 1] ? body[i + 1] : 2) {
            if (code == CONF_REJ && body[i] == 5) p.magic = 0;
            if (body[i] == 3) { p.want_pap = false; LOGI("[ppp] box won't do PAP\n"); }
        }
        p.lcp_retries = 0;
        lcp_request(now);
        return;
    case TERM_REQ:
        LOGI("[ppp] LCP Terminate-Request from box\n");
        send_ctl(PROTO_LCP, TERM_ACK, id, NULL, 0);
        down("terminated by box");
        return;
    case TERM_ACK: case ECHO_REP: case DISCARD_REQ: return;
    case ECHO_REQ:
        if (p.lcp_state == ST_OPENED && blen >= 4) {
            static uint8_t r[PPP_MAX_FRAME];
            r[0] = (uint8_t)(p.magic >> 24); r[1] = (uint8_t)(p.magic >> 16); r[2] = (uint8_t)(p.magic >> 8); r[3] = (uint8_t)p.magic;
            memcpy(r + 4, body + 4, blen - 4);
            send_ctl(PROTO_LCP, ECHO_REP, id, r, blen);
        }
        return;
    case CODE_REJ: LOGI("[ppp] LCP Code-Reject from box\n"); return;
    case PROTO_REJ: if (blen >= 2) LOGI("[ppp] box rejected protocol 0x%04x\n", (body[0] << 8) | body[1]); return;
    default:
        send_ctl(PROTO_LCP, CODE_REJ, p.id++, raw, rawlen);
    }
}

// ---------------------------------------------------------------- PAP

static void pap_reply(bool ok, uint8_t id, const char *text) {
    uint8_t msg[64];
    size_t n = strlen(text);
    if (n > sizeof msg - 1) n = sizeof msg - 1;
    msg[0] = (uint8_t)n;
    memcpy(msg + 1, text, n);
    send_ctl(PROTO_PAP, ok ? 2 : 3, id, msg, n + 1);
}

static void pap_accept(uint8_t id, uint32_t now) {
    pap_reply(true, id, "Welcome");
    if (!p.pap_ok) { p.pap_ok = true; ipcp_start(now); }
}

static void pap_packet(uint8_t code, uint8_t id, const uint8_t *body, size_t blen, uint32_t now) {
    if (code != 1 || blen < 1) return;
    size_t ul = body[0];
    if (1 + ul + 1 > blen) return;
    char user[33];
    if (ul > sizeof user - 1) ul = sizeof user - 1;
    memcpy(user, body + 1, ul); user[ul] = 0;
    if (!p.pap_ok) LOGI("[ppp] PAP login: \"%s\"\n", user);
    pap_accept(id, now);
}

// ---------------------------------------------------------------- IPCP

static void ipcp_request(uint32_t now) {
    if (p.ipcp_our_acked || p.lcp_state != ST_OPENED) return;
    if (p.ipcp_retries++ >= 10) { down("IPCP negotiation timed out"); return; }
    p.ipcp_req_id = p.id++;
    uint8_t opts[6]; size_t n = 0;
    if (!p.ipcp_no_addr) n = put_opt(opts, 3, local_ip, 4);
    send_ctl(PROTO_IPCP, CONF_REQ, p.ipcp_req_id, opts, n);
    p.ipcp_timer = now + 3000;
}

static void ipcp_start(uint32_t now) {
    if (p.ipcp_started || p.lcp_state != ST_OPENED) return;
    p.ipcp_started = true;
    ipcp_request(now);
}

static void ipcp_check_open(void) {
    if (p.ip_up || !p.ipcp_our_acked || !p.ipcp_their_acked) return;
    p.ip_up = true;
    LOGI("[ppp] IPCP up: box %d.%d.%d.%d, gateway and DNS %d.%d.%d.%d\n", peer_ip[0], peer_ip[1], peer_ip[2], peer_ip[3],
         local_ip[0], local_ip[1], local_ip[2], local_ip[3]);
}

static void ipcp_packet(uint8_t code, uint8_t id, const uint8_t *body, size_t blen, uint32_t now) {
    switch (code) {
    case CONF_REQ: {
        static uint8_t rej[PPP_MAX_FRAME]; uint8_t nak[64];
        size_t nrej = 0, nnak = 0;
        for (size_t i = 0; i + 2 <= blen;) {
            uint8_t type = body[i], len = body[i + 1];
            if (len < 2 || i + len > blen) return;
            const uint8_t *want = type == 3 || type == 129 || type == 131 ? (type == 3 ? peer_ip : local_ip) : NULL;
            if (want && len == 6) {
                if (memcmp(body + i + 2, want, 4) && nnak + 6 <= sizeof nak) nnak += put_opt(nak + nnak, type, want, 4);
            } else { memcpy(rej + nrej, body + i, len); nrej += len; }
            i += len;
        }
        if (nrej) { send_ctl(PROTO_IPCP, CONF_REJ, id, rej, nrej); return; }
        if (nnak) { send_ctl(PROTO_IPCP, CONF_NAK, id, nak, nnak); return; }
        send_ctl(PROTO_IPCP, CONF_ACK, id, body, blen);
        p.ipcp_their_acked = true;
        ipcp_check_open();
        return;
    }
    case CONF_ACK: if (id == p.ipcp_req_id) { p.ipcp_our_acked = true; ipcp_check_open(); } return;
    case CONF_NAK: case CONF_REJ:
        if (id == p.ipcp_req_id) {
            for (size_t i = 0; i + 2 <= blen; i += body[i + 1] ? body[i + 1] : 2)
                if (code == CONF_REJ && body[i] == 3) p.ipcp_no_addr = true;
            p.ipcp_retries = 0;
            ipcp_request(now);
        }
        return;
    case TERM_REQ:
        send_ctl(PROTO_IPCP, TERM_ACK, id, NULL, 0);
        p.ip_up = false; p.ipcp_our_acked = p.ipcp_their_acked = false;
        return;
    case TERM_ACK: return;
    default: {
        static uint8_t r[PPP_MAX_FRAME];
        r[0] = code; r[1] = id; r[2] = (uint8_t)((blen + 4) >> 8); r[3] = (uint8_t)(blen + 4);
        memcpy(r + 4, body, blen);
        send_ctl(PROTO_IPCP, CODE_REJ, p.id++, r, blen + 4);
    }
    }
}

// ---------------------------------------------------------------- dispatch

static void packet(uint16_t proto, const uint8_t *data, size_t len) {
    uint32_t now = bridge_now_ms();
    if (proto == PROTO_IP) { if (p.ip_up) gw_input(data, len); return; }
    if (len < 4) return;
    uint8_t code = data[0], id = data[1];
    size_t plen = (size_t)((data[2] << 8) | data[3]);
    if (plen > len) plen = len;
    if (plen < 4) return;
    const uint8_t *body = data + 4;
    size_t blen = plen - 4;
    switch (proto) {
    case PROTO_LCP: lcp_packet(code, id, body, blen, data, len, now); return;
    case PROTO_PAP: if (p.lcp_state == ST_OPENED) pap_packet(code, id, body, blen, now); return;
    case PROTO_IPCP: if (p.lcp_state == ST_OPENED) ipcp_packet(code, id, body, blen, now); return;
    }
    LOGD("[ppp] rejecting protocol 0x%04x\n", proto);
    if (p.lcp_state == ST_OPENED) {
        static uint8_t r[PPP_MAX_FRAME];
        if (len > sizeof r - 2) len = sizeof r - 2;
        r[0] = (uint8_t)(proto >> 8); r[1] = (uint8_t)proto;
        memcpy(r + 2, data, len);
        send_ctl(PROTO_LCP, PROTO_REJ, p.id++, r, len + 2);
    }
}

static void frame(const uint8_t *f, size_t n) {
    if (n < 4) return;
    uint16_t fcs = 0xffff;
    for (size_t i = 0; i < n; i++) fcs = fcs_step(fcs, f[i]);
    if (fcs != 0xf0b8) {
        p.bad_fcs++;
        LOGD("[ppp] bad FCS (len %u)\n", (unsigned)n);
        // the frame as it came (unescaped), to find what the link did to it: hex, then the text
        for (size_t o = 0; o < n; o += 32) {
            char hx[32 * 3 + 1], tx[33];
            size_t k = 0;
            for (; k < 32 && o + k < n; k++) {
                snprintf(hx + k * 3, 4, "%02x ", f[o + k]);
                tx[k] = f[o + k] >= 0x20 && f[o + k] < 0x7f ? (char)f[o + k] : '.';
            }
            tx[k] = 0;
            LOGD("[ppp]   %04x  %-96s %s\n", (unsigned)o, hx, tx);
        }
        return;
    }
    p.rx_frames++;
    size_t i = 0;
    if (f[0] == 0xff && f[1] == 0x03) i = 2;
    uint16_t proto = f[i++];
    if (!(proto & 1)) proto = (uint16_t)((proto << 8) | f[i++]);
    if (i + 2 > n) return;
    packet(proto, f + i, n - 2 - i);
}

void ppp_rx_byte(uint8_t b) {
    if (!p.active) return;
    if (b == FLAG) {
        if (p.rx_len && !p.rx_drop) frame(p.rx, (size_t)p.rx_len);
        p.rx_len = 0; p.rx_esc = false; p.rx_drop = false;
        return;
    }
    if (p.rx_drop) return;
    if (b == ESC) { p.rx_esc = true; return; }
    if (p.rx_esc) { b ^= 0x20; p.rx_esc = false; }
    else if (b < 0x20) return;   // flow-control characters inserted by the link (RFC 1662 4.2)
    if (p.rx_len >= (int)sizeof p.rx) { p.rx_drop = true; return; }
    p.rx[p.rx_len++] = b;
}

void ppp_poll(uint32_t now) {
    if (!p.active) return;
    if (p.start_at && (int32_t)(now - p.start_at) >= 0) {
        p.start_at = 0;
        if (p.lcp_state == ST_STARTING) {
            LOGI("[ppp] link up, starting LCP\n");
            p.lcp_state = ST_REQSENT;
            lcp_request(now);
        }
    }
    if (p.lcp_timer && (int32_t)(now - p.lcp_timer) >= 0) { p.lcp_timer = 0; lcp_request(now); }
    if (p.ipcp_timer && (int32_t)(now - p.ipcp_timer) >= 0) { p.ipcp_timer = 0; ipcp_request(now); }
}

void ppp_status(void (*print)(const char *fmt, ...)) {
    static const char *st[] = { "closed", "starting", "negotiating", "opened" };
    print("ppp: LCP %s, IP %s; frames in %lu out %lu, bad FCS %lu, dropped out %lu\n",
          st[p.lcp_state], p.ip_up ? "up" : "down", (unsigned long)p.rx_frames, (unsigned long)p.tx_frames,
          (unsigned long)p.bad_fcs, (unsigned long)p.tx_dropped);
}
