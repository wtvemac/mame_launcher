// Single-producer single-consumer byte ring. Safe between the two cores (or a core and an
// interrupt) as long as each side only touches its own index. Size must be a power of two.
#pragma once
#include <stdint.h>
#include <stddef.h>

typedef struct {
    uint8_t *buf;
    uint32_t mask;
    volatile uint32_t head;  // written by the producer
    volatile uint32_t tail;  // written by the consumer
} ring_t;

#define RING_INIT(storage) { (storage), sizeof(storage) - 1, 0, 0 }

static inline void ring_barrier(void) { __atomic_thread_fence(__ATOMIC_SEQ_CST); }
static inline uint32_t ring_used(const ring_t *r) { return r->head - r->tail; }
static inline uint32_t ring_free(const ring_t *r) { return r->mask + 1 - (r->head - r->tail); }
static inline int ring_empty(const ring_t *r) { return r->head == r->tail; }

static inline int ring_put(ring_t *r, uint8_t b) {
    uint32_t h = r->head;
    if (h - r->tail > r->mask) return 0;
    r->buf[h & r->mask] = b;
    ring_barrier();
    r->head = h + 1;
    return 1;
}

static inline int ring_get(ring_t *r) {
    uint32_t t = r->tail;
    if (t == r->head) return -1;
    ring_barrier();
    int b = r->buf[t & r->mask];
    ring_barrier();
    r->tail = t + 1;
    return b;
}

static inline int ring_peek(const ring_t *r, uint32_t off) {
    if (off >= ring_used(r)) return -1;
    return r->buf[(r->tail + off) & r->mask];
}

static inline size_t ring_write(ring_t *r, const uint8_t *d, size_t n) {
    size_t i = 0;
    while (i < n && ring_put(r, d[i])) i++;
    return i;
}

static inline size_t ring_read(ring_t *r, uint8_t *d, size_t n) {
    size_t i = 0;
    int b;
    while (i < n && (b = ring_get(r)) >= 0) d[i++] = (uint8_t)b;
    return i;
}

// consumer side: drop everything currently queued
static inline void ring_flush(ring_t *r) { r->tail = r->head; }
