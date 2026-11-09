// IPv4 gateway for the PPP link, port of the emulator's src/net/ip.js: ICMP echo, a DNS
// server on UDP/53 (resolving over WiFi), and TCP termination spliced onto outbound
// connections. The box's TCP is simple (one connection at a time per page element, MSS 536,
// ~4 KB windows), so this is a minimal but complete TCP endpoint: go-back-N retransmission,
// fast retransmit on three duplicate ACKs, zero-window probes, FIN in both directions.
#include <string.h>
#include <stdio.h>
#include <stdbool.h>
#include <stddef.h>
#include "gw.h"
#include "ppp.h"
#include "net.h"
#include "bridge.h"
#include "bridge_config.h"
#include "log.h"

#define PROTO_ICMP 1
#define PROTO_TCP  6
#define PROTO_UDP  17
#define F_FIN 1
#define F_SYN 2
#define F_RST 4
#define F_PSH 8
#define F_ACK 16

enum { C_FREE, C_PENDING, C_SYNRCVD, C_ESTABLISHED, C_CLOSEWAIT };

typedef struct {
    int state;
    uint8_t box_ip[4], dst_ip[4];
    uint16_t sport, dport;
    uint32_t rcv_nxt, iss, snd_una, snd_nxt;
    uint32_t snd_wnd;
    uint16_t mss;
    uint8_t buf[GW_CONN_BUF];   // remote data from snd_una on (after the SYN): ring
    uint32_t buf_start, buf_len;
    bool remote_eof, fin_sent, fin_acked, box_fin;
    uint32_t rto_at, rto_ms;
    int dup_acks;
    uint32_t box_bytes, tx_bytes;
    net_conn_t *conn;
} conn_t;

static conn_t conns[GW_MAX_CONNS];
static uint16_t ip_id = 1;
static char service_host[64];
static uint32_t iss_seed = 0x12345678;


static inline uint16_t r16(const uint8_t *b) { return (uint16_t)((b[0] << 8) | b[1]); }
static inline uint32_t r32(const uint8_t *b) { return ((uint32_t)b[0] << 24) | ((uint32_t)b[1] << 16) | ((uint32_t)b[2] << 8) | b[3]; }
static inline void w16(uint8_t *b, uint16_t v) { b[0] = (uint8_t)(v >> 8); b[1] = (uint8_t)v; }
static inline void w32(uint8_t *b, uint32_t v) { b[0] = (uint8_t)(v >> 24); b[1] = (uint8_t)(v >> 16); b[2] = (uint8_t)(v >> 8); b[3] = (uint8_t)v; }
static inline bool seq_lt(uint32_t a, uint32_t b) { return (int32_t)(a - b) < 0; }
static inline bool seq_le(uint32_t a, uint32_t b) { return (int32_t)(a - b) <= 0; }

static uint32_t csum_add(uint32_t s, const uint8_t *b, size_t n) {
    for (size_t i = 0; i + 1 < n; i += 2) s += (uint32_t)((b[i] << 8) | b[i + 1]);
    if (n & 1) s += (uint32_t)(b[n - 1] << 8);
    return s;
}
static uint16_t csum_fold(uint32_t s) {
    while (s > 0xffff) s = (s & 0xffff) + (s >> 16);
    return (uint16_t)s;
}
static uint32_t pseudo(const uint8_t *src, const uint8_t *dst, uint8_t proto, size_t len) {
    uint8_t ph[12];
    memcpy(ph, src, 4); memcpy(ph + 4, dst, 4);
    ph[8] = 0; ph[9] = proto; w16(ph + 10, (uint16_t)len);
    return csum_add(0, ph, 12);
}

void gw_set_service(const char *host) {
    snprintf(service_host, sizeof service_host, "%s", host ? host : "");
}

// ---------------------------------------------------------------- IPv4 output

static uint8_t pkt_out[20 + 60 + 1460];

// payload is already at pkt_out + 20
static bool output(const uint8_t *src, const uint8_t *dst, uint8_t proto, size_t plen) {
    uint8_t *p = pkt_out;
    size_t total = 20 + plen;
    p[0] = 0x45; p[1] = 0; w16(p + 2, (uint16_t)total); w16(p + 4, ip_id++); p[6] = 0x40; p[7] = 0;  // DF
    p[8] = 64; p[9] = proto; p[10] = p[11] = 0;
    memcpy(p + 12, src, 4); memcpy(p + 16, dst, 4);
    w16(p + 10, (uint16_t)~csum_fold(csum_add(0, p, 20)));
    return ppp_send_ip(p, total);
}

// ---------------------------------------------------------------- ICMP

static void icmp(const uint8_t *src, const uint8_t *dst, const uint8_t *d, size_t n) {
    if (n < 8 || n > sizeof pkt_out - 20 || csum_fold(csum_add(0, d, n)) != 0xffff) return;
    if (d[0] != 8) return;   // only echo requests
    uint8_t *r = pkt_out + 20;
    memcpy(r, d, n);
    r[0] = 0; r[2] = r[3] = 0;
    w16(r + 2, (uint16_t)~csum_fold(csum_add(0, r, n)));
    output(dst, src, PROTO_ICMP, n);
}

// ---------------------------------------------------------------- UDP / DNS

static bool udp_output(const uint8_t *src, const uint8_t *dst, uint16_t sport, uint16_t dport, const uint8_t *payload, size_t n) {
    uint8_t *u = pkt_out + 20;
    if (8 + n > sizeof pkt_out - 20) return false;
    w16(u, sport); w16(u + 2, dport); w16(u + 4, (uint16_t)(8 + n)); u[6] = u[7] = 0;
    memmove(u + 8, payload, n);
    uint16_t c = (uint16_t)~csum_fold(csum_add(pseudo(src, dst, PROTO_UDP, 8 + n), u, 8 + n));
    w16(u + 6, c ? c : 0xffff);
    return output(src, dst, PROTO_UDP, 8 + n);
}

#define DNS_PENDING 4
static struct dns_q {
    bool used;
    uint8_t box_ip[4], server_ip[4];   // reply from the server it asked (its TellyScript picks it)
    uint16_t sport;
    uint8_t q[512];
    size_t qlen, qend;
    char name[256];
} dnsq[DNS_PENDING];

static void dns_reply(struct dns_q *d, int rcode, const uint8_t *ip) {
    static uint8_t r[512 + 16];
    size_t n = rcode == 4 ? 12 : d->qend;
    memcpy(r, d->q, n);
    r[2] = (uint8_t)(0x80 | (d->q[2] & 0x79)); r[3] = (uint8_t)(0x80 | rcode);   // QR, RD copied, RA
    w16(r + 4, rcode == 4 ? 0 : 1); w16(r + 6, ip ? 1 : 0); w16(r + 8, 0); w16(r + 10, 0);
    if (ip) {
        static const uint8_t ans[] = { 0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4 };
        memcpy(r + n, ans, sizeof ans); n += sizeof ans;
        memcpy(r + n, ip, 4); n += 4;
    }
    LOGD("[dns] %s -> %s\n", d->name, ip ? "found" : rcode == 3 ? "NXDOMAIN" : rcode ? "NOTIMP" : "no data");
    udp_output(d->server_ip, d->box_ip, 53, d->sport, r, n);
    d->used = false;
}

static void dns_resolved(void *ctx, bool ok, const uint8_t ip[4]) {
    struct dns_q *d = ctx;
    if (!d->used) return;
    dns_reply(d, ok ? 0 : 3, ok ? ip : NULL);
}

static void dns(const uint8_t *src, const uint8_t *dst, uint16_t sport, const uint8_t *q, size_t n) {
    if (n < 12 || n > 512) return;
    struct dns_q *d = NULL;
    for (int i = 0; i < DNS_PENDING; i++) if (!dnsq[i].used) { d = &dnsq[i]; break; }
    if (!d) return;   // the box will retry
    memset(d, 0, sizeof *d);
    d->used = true;
    memcpy(d->box_ip, src, 4); memcpy(d->server_ip, dst, 4); d->sport = sport;
    memcpy(d->q, q, n); d->qlen = n;
    // the first question's name
    size_t i = 12, o = 0;
    while (i < n && q[i] && o < sizeof d->name - 64) {
        uint8_t l = q[i];
        if (i + 1 + l > n) break;
        if (o) d->name[o++] = '.';
        memcpy(d->name + o, q + i + 1, l); o += l;
        i += 1u + l;
    }
    d->name[o] = 0;
    uint16_t qtype = i + 3 <= n ? r16(q + i + 1) : 0;
    d->qend = i + 5 <= n ? i + 5 : n;
    if (r16(q + 4) != 1 || (q[2] & 0x78)) { dns_reply(d, 4, NULL); return; }   // only plain queries
    if (qtype != 1 && qtype != 255) { dns_reply(d, 0, NULL); return; }         // A only: "no data"
    unsigned a, b, c, e;
    char tail;
    if (sscanf(d->name, "%u.%u.%u.%u%c", &a, &b, &c, &e, &tail) == 4 && a < 256 && b < 256 && c < 256 && e < 256) {
        uint8_t ip[4] = { (uint8_t)a, (uint8_t)b, (uint8_t)c, (uint8_t)e };
        dns_reply(d, 0, ip);
        return;
    }
    net_resolve(d->name, dns_resolved, d);
}

static void udp(const uint8_t *src, const uint8_t *dst, const uint8_t *d, size_t n) {
    if (n < 8) return;
    uint16_t sport = r16(d), dport = r16(d + 2), ulen = r16(d + 4);
    if (ulen < 8 || ulen > n) return;
    if (dport == 53) { dns(src, dst, sport, d + 8, ulen - 8u); return; }
    LOGT("[udp] to port %u dropped\n", dport);
}

// ---------------------------------------------------------------- TCP output

static bool tcp_output(const uint8_t *src, const uint8_t *dst, uint16_t sport, uint16_t dport, uint32_t seq, uint32_t ack,
                       uint8_t flags, uint16_t win, const conn_t *data_from, uint32_t data_off, uint32_t data_len, uint16_t mss) {
    uint8_t *t = pkt_out + 20;
    size_t hl = mss ? 24 : 20;
    if (hl + data_len > sizeof pkt_out - 20) return false;
    w16(t, sport); w16(t + 2, dport); w32(t + 4, seq); w32(t + 8, ack);
    t[12] = (uint8_t)((hl / 4) << 4); t[13] = flags; w16(t + 14, win); t[16] = t[17] = 0; t[18] = t[19] = 0;
    if (mss) { t[20] = 2; t[21] = 4; w16(t + 22, mss); }
    if (data_len) {
        // copy out of the connection's ring
        uint32_t pos = (data_from->buf_start + data_off) % GW_CONN_BUF;
        uint32_t first = GW_CONN_BUF - pos < data_len ? GW_CONN_BUF - pos : data_len;
        memcpy(t + hl, data_from->buf + pos, first);
        memcpy(t + hl + first, data_from->buf, data_len - first);
    }
    size_t len = hl + data_len;
    w16(t + 16, (uint16_t)~csum_fold(csum_add(pseudo(src, dst, PROTO_TCP, len), t, len)));
    return output(src, dst, PROTO_TCP, len);
}

static void rst_for(const uint8_t *src, const uint8_t *dst, uint16_t sport, uint16_t dport, uint32_t seq, uint32_t ack,
                    uint8_t flags, size_t dlen) {
    uint32_t len = (uint32_t)dlen + ((flags & F_SYN) ? 1 : 0) + ((flags & F_FIN) ? 1 : 0);
    if (flags & F_ACK) tcp_output(dst, src, dport, sport, ack, 0, F_RST, 0, NULL, 0, 0, 0);
    else tcp_output(dst, src, dport, sport, 0, seq + len, F_RST | F_ACK, 0, NULL, 0, 0, 0);
}

static bool out(conn_t *c, uint8_t flags, uint32_t seq, uint32_t off, uint32_t len, uint16_t mss) {
    return tcp_output(c->dst_ip, c->box_ip, c->dport, c->sport, seq, c->rcv_nxt, flags, 16384, c, off, len, mss);
}

// ---------------------------------------------------------------- TCP connections

static const char *cname(const conn_t *c) {
    static char s[32];
    snprintf(s, sizeof s, "%d.%d.%d.%d:%u", c->dst_ip[0], c->dst_ip[1], c->dst_ip[2], c->dst_ip[3], c->dport);
    return s;
}

static void finish(conn_t *c) {
    if (c->state == C_FREE) return;
    LOGI("[tcp] %s closed (box sent %lu, received %lu bytes)\n", cname(c), (unsigned long)c->box_bytes, (unsigned long)c->tx_bytes);
    c->state = C_FREE;
    if (c->conn) { net_tcp_close(c->conn); c->conn = NULL; }
}

static void reset_conn(conn_t *c) {
    if (c->state == C_FREE) return;
    out(c, F_RST | F_ACK, c->snd_nxt, 0, 0, 0);
    finish(c);
}

static void arm_rto(conn_t *c) { if (!c->rto_at) c->rto_at = bridge_now_ms() + c->rto_ms; }

static uint32_t in_flight(const conn_t *c) { return c->snd_nxt - c->snd_una; }

static void send_syn(conn_t *c) {
    out(c, F_SYN | F_ACK, c->iss, 0, 0, 1460);
    c->snd_nxt = c->iss + 1;
    arm_rto(c);
}

static void pump(conn_t *c) {
    if (c->state != C_ESTABLISHED && c->state != C_CLOSEWAIT) return;
    for (;;) {
        uint32_t off = in_flight(c) - (c->fin_sent ? 1 : 0);
        if (c->fin_sent) break;
        uint32_t avail = c->buf_len > off ? c->buf_len - off : 0;
        uint32_t wnd = c->snd_wnd > in_flight(c) ? c->snd_wnd - in_flight(c) : 0;
        uint32_t n = avail < wnd ? avail : wnd;
        if (n > c->mss) n = c->mss;
        if (!n) break;
        if (ppp_frame_room() < 60 + n) break;   // the queue to the box is full: try again later
        if (!out(c, F_ACK | F_PSH, c->snd_nxt, off, n, 0)) break;
        c->snd_nxt += n;
        c->tx_bytes += n;
    }
    if (c->remote_eof && !c->fin_sent && in_flight(c) == c->buf_len) {
        if (out(c, F_FIN | F_ACK, c->snd_nxt, 0, 0, 0)) { c->snd_nxt++; c->fin_sent = true; }
    }
    if (in_flight(c) || (c->buf_len && c->snd_wnd == 0)) arm_rto(c);
}

// net callbacks
static void on_open(void *ctx) {
    conn_t *c = ctx;
    if (c->state != C_PENDING) return;
    c->state = C_SYNRCVD;
    LOGI("[tcp] %s connected\n", cname(c));
    send_syn(c);
}

static size_t on_data(void *ctx, const uint8_t *d, size_t n) {
    conn_t *c = ctx;
    if (c->state == C_FREE) return n;
    uint32_t room = GW_CONN_BUF - c->buf_len;
    if (n > room) n = room;
    for (size_t i = 0; i < n; i++) c->buf[(c->buf_start + c->buf_len + i) % GW_CONN_BUF] = d[i];
    c->buf_len += (uint32_t)n;
    pump(c);
    return n;
}

static void on_end(void *ctx) {
    conn_t *c = ctx;
    if (c->state == C_FREE) return;
    c->remote_eof = true;
    pump(c);
}

static void on_close(void *ctx, const char *err) {
    conn_t *c = ctx;
    if (c->state == C_FREE) return;
    c->conn = NULL;   // net has already forgotten it
    if (c->state == C_PENDING || err) {
        LOGI("[tcp] %s %s: %s\n", cname(c), c->state == C_PENDING ? "connect failed" : "remote error", err ? err : "closed");
        reset_conn(c);
        return;
    }
    c->remote_eof = true;
    pump(c);
}

static const net_handlers_t handlers = { on_open, on_data, on_end, on_close };

static void rto(conn_t *c) {
    c->rto_at = 0;
    // don't count time the data spends queued towards the box
    if (modem_out_used() > 0 && c->state != C_SYNRCVD) { arm_rto(c); return; }
    if (c->state == C_SYNRCVD) { LOGD("[tcp] %s resending SYN-ACK\n", cname(c)); send_syn(c); return; }
    if (in_flight(c) == 0 && c->snd_wnd == 0 && c->buf_len) {   // zero-window probe: one byte
        out(c, F_ACK, c->snd_una, 0, 1, 0);
        arm_rto(c);
        return;
    }
    if (in_flight(c) == 0) return;
    LOGD("[tcp] %s retransmit from %lu bytes in flight\n", cname(c), (unsigned long)in_flight(c));
    if (c->fin_sent && !c->fin_acked) c->fin_sent = false;   // go-back-N: rewind and resend
    c->snd_nxt = c->snd_una;
    c->rto_ms = c->rto_ms * 2 > 30000 ? 30000 : c->rto_ms * 2;
    pump(c);
}

static void to_remote(conn_t *c, const uint8_t *d, size_t n) {
    size_t sent = c->conn ? net_tcp_send(c->conn, d, n) : n;
    c->rcv_nxt += (uint32_t)sent;     // anything not taken gets retransmitted by the box
    c->box_bytes += (uint32_t)sent;
}

static void conn_input(conn_t *c, uint8_t flags, uint32_t seq, uint32_t ack, uint16_t win, const uint8_t *data, size_t dlen) {
    if (flags & F_RST) { LOGI("[tcp] %s reset by box\n", cname(c)); finish(c); return; }
    if (flags & F_SYN) { if (c->state == C_SYNRCVD) send_syn(c); return; }   // retransmitted SYN
    if (c->state == C_PENDING) return;
    if (flags & F_ACK) {
        if (seq_lt(c->snd_una, ack) && seq_le(ack, c->snd_nxt)) {
            uint32_t acked = ack - c->snd_una;
            if (c->state == C_SYNRCVD) { acked--; c->state = C_ESTABLISHED; LOGI("[tcp] %s established\n", cname(c)); }
            if (c->fin_sent && ack == c->snd_nxt) { c->fin_acked = true; if (acked) acked--; }
            if (acked > c->buf_len) acked = c->buf_len;
            c->buf_start = (c->buf_start + acked) % GW_CONN_BUF;
            c->buf_len -= acked;
            c->snd_una = ack;
            c->rto_at = 0; c->rto_ms = 3000; c->dup_acks = 0;
            if (acked && c->conn) net_tcp_kick(c->conn);   // room for more remote data
        } else if (ack == c->snd_una && !dlen && in_flight(c) && win == c->snd_wnd && ++c->dup_acks == 3) {
            // fast retransmit: the box is missing the segment at snd_una
            if (c->fin_sent && !c->fin_acked) c->fin_sent = false;
            c->snd_nxt = c->snd_una;
        }
        c->snd_wnd = win;
    }
    bool need_ack = false;
    if (dlen) {
        if (seq == c->rcv_nxt) {
            if (!c->box_fin) to_remote(c, data, dlen);
        } else if (seq_lt(seq, c->rcv_nxt) && seq_lt(c->rcv_nxt, seq + (uint32_t)dlen)) {
            uint32_t skip = c->rcv_nxt - seq;   // partial overlap with what we already have
            to_remote(c, data + skip, dlen - skip);
        }
        need_ack = true;
    }
    if ((flags & F_FIN) && seq + (uint32_t)dlen == c->rcv_nxt && !c->box_fin) {
        c->box_fin = true;
        c->rcv_nxt++;
        if (c->state == C_ESTABLISHED) c->state = C_CLOSEWAIT;
        LOGD("[tcp] %s box closed its side\n", cname(c));
        if (c->conn) net_tcp_end(c->conn);
        need_ack = true;
    } else if (flags & F_FIN) need_ack = true;
    if (need_ack) out(c, F_ACK, c->snd_nxt, 0, 0, 0);
    if (c->box_fin && c->fin_acked) { finish(c); return; }
    pump(c);
}

static void tcp_input(const uint8_t *src, const uint8_t *dst, const uint8_t *d, size_t n) {
    if (n < 20) return;
    if ((uint16_t)~csum_fold(csum_add(pseudo(src, dst, PROTO_TCP, n), d, n)) != 0) { LOGD("[tcp] bad checksum\n"); return; }
    uint16_t sport = r16(d), dport = r16(d + 2);
    uint32_t seq = r32(d + 4), ack = r32(d + 8);
    size_t hl = (size_t)(d[12] >> 4) * 4;
    if (hl < 20 || hl > n) return;
    uint8_t flags = d[13] & 0x3f;
    uint16_t win = r16(d + 14);
    const uint8_t *data = d + hl;
    size_t dlen = n - hl;

    conn_t *c = NULL;
    for (int i = 0; i < GW_MAX_CONNS; i++) {
        conn_t *x = &conns[i];
        if (x->state != C_FREE && x->sport == sport && x->dport == dport && !memcmp(x->box_ip, src, 4) && !memcmp(x->dst_ip, dst, 4)) { c = x; break; }
    }
    if (c && (flags & (F_SYN | F_ACK)) == F_SYN && seq + 1 != c->rcv_nxt) { finish(c); c = NULL; }  // port reused
    if (c) { conn_input(c, flags, seq, ack, win, data, dlen); return; }
    if (flags & F_RST) return;
    if ((flags & (F_SYN | F_ACK)) != F_SYN) { rst_for(src, dst, sport, dport, seq, ack, flags, dlen); return; }

    // a new connection
    for (int i = 0; i < GW_MAX_CONNS; i++) if (conns[i].state == C_FREE) { c = &conns[i]; break; }
    if (!c) { LOGI("[tcp] too many connections\n"); rst_for(src, dst, sport, dport, seq, ack, flags, dlen); return; }
    memset(c, 0, offsetof(conn_t, buf));
    c->buf_start = c->buf_len = 0;
    c->remote_eof = c->fin_sent = c->fin_acked = c->box_fin = false;
    c->rto_at = 0; c->dup_acks = 0; c->box_bytes = c->tx_bytes = 0; c->conn = NULL;
    memcpy(c->box_ip, src, 4); memcpy(c->dst_ip, dst, 4);
    c->sport = sport; c->dport = dport;
    c->rcv_nxt = seq + 1;
    iss_seed = iss_seed * 1103515245u + 12345u + bridge_now_ms();
    c->iss = iss_seed;
    c->snd_una = c->snd_nxt = c->iss;
    c->snd_wnd = win;
    c->mss = 536;
    for (size_t i = 20; i < hl;) {
        uint8_t k = d[i];
        if (k == 0) break;
        if (k == 1) { i++; continue; }
        if (i + 1 >= hl || d[i + 1] < 2) break;
        if (k == 2 && d[i + 1] == 4) c->mss = r16(d + i + 2);
        i += d[i + 1];
    }
    if (c->mss < 64) c->mss = 64;
    if (c->mss > 1460) c->mss = 1460;
    c->rto_ms = 3000;
    c->state = C_PENDING;

    char host[64];
    if (dport >= WTVP_PORT_MIN && dport <= WTVP_PORT_MAX && service_host[0]) snprintf(host, sizeof host, "%s", service_host);
    else snprintf(host, sizeof host, "%d.%d.%d.%d", dst[0], dst[1], dst[2], dst[3]);
    LOGI("[tcp] box port %u connecting to %s (via %s, mss %u, win %u)\n", sport, cname(c), host, c->mss, win);
    c->conn = net_tcp_open(host, dport, &handlers, c);
    if (!c->conn) { LOGI("[tcp] %s: no network\n", cname(c)); reset_conn(c); }
}

// ---------------------------------------------------------------- entry points

void gw_input(const uint8_t *p, size_t n) {
    if (n < 20 || (p[0] >> 4) != 4) return;
    size_t ihl = (size_t)(p[0] & 15) * 4, total = r16(p + 2);
    if (ihl < 20 || total > n || total < ihl || csum_fold(csum_add(0, p, ihl)) != 0xffff) { LOGD("[ip] bad header\n"); return; }
    if (r16(p + 6) & 0x3fff) { LOGD("[ip] fragment dropped\n"); return; }
    const uint8_t *src = p + 12, *dst = p + 16, *d = p + ihl;
    size_t dl = total - ihl;
    switch (p[9]) {
    case PROTO_ICMP: icmp(src, dst, d, dl); return;
    case PROTO_UDP: udp(src, dst, d, dl); return;
    case PROTO_TCP: tcp_input(src, dst, d, dl); return;
    }
}

void gw_reset(void) {
    for (int i = 0; i < GW_MAX_CONNS; i++) finish(&conns[i]);
    for (int i = 0; i < DNS_PENDING; i++) dnsq[i].used = false;
}

void gw_poll(uint32_t now) {
    for (int i = 0; i < GW_MAX_CONNS; i++) {
        conn_t *c = &conns[i];
        if (c->state == C_FREE) continue;
        if (c->rto_at && (int32_t)(now - c->rto_at) >= 0) rto(c);
        if (c->state != C_FREE) pump(c);   // continue anything held back by a full queue
    }
}

void gw_status(void (*print)(const char *fmt, ...)) {
    int n = 0;
    for (int i = 0; i < GW_MAX_CONNS; i++) if (conns[i].state != C_FREE) n++;
    print("tcp: %d connection%s%s%s\n", n, n == 1 ? "" : "s", service_host[0] ? ", WebTV service at " : "", service_host);
    for (int i = 0; i < GW_MAX_CONNS; i++) {
        const conn_t *c = &conns[i];
        if (c->state == C_FREE) continue;
        static const char *st[] = { "", "connecting", "syn-rcvd", "established", "close-wait" };
        print("  %s %s, box sent %lu, received %lu, %lu buffered\n", cname(c), st[c->state],
              (unsigned long)c->box_bytes, (unsigned long)c->tx_bytes, (unsigned long)c->buf_len);
    }
}
