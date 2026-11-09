// Keeping the tier-3 area current (WEBTV_LPT_BRIDGE.md §10.7). On a box whose boot ROM has no
// debug monitor, the loader comes from the disk's tier-3 area, written once with lptdisk.mjs.
// When the bridge's own loader is newer than the one there, the bridge rewrites the area itself,
// so a firmware update brings its loader along.
//
// The loader running is the one on the disk, possibly older, so its routines may sit at other
// addresses than this firmware's: nothing here calls into it. Only its memory protocol is used
// (READ, WRITE, CALL), which every loader since protocol 3 speaks:
//   1. Test 19 left the area's first sectors at SB_TIER3_RAM: READ them and compare with
//      sb_tier3. The same: nothing to do.
//   2. WRITE the updater (this firmware's loader, built at SB_UPD_BASE) and the new image
//      (SB_UPD_BUF), CALL the updater's disk_write, then its disk_read into SB_UPD_BUF2, and
//      READ that back to compare.
// The next start boots the new loader.
#include <string.h>
#include "sb.h"
#include "../gen/sb_assets.h"
#include "../log.h"

#define SECTORS ((uint32_t)(sizeof sb_tier3 + 511) / 512)

static struct {
    sb_t3_done_fn done;
    bool updated;
} Q;
static uint8_t buf[sizeof sb_tier3];

static void finish(const char *err) {
    sb_t3_done_fn d = Q.done;
    Q.done = NULL;
    if (err) LOGI("[tier3] %s\n", err);
    if (d) d(err, Q.updated);
}

static void verified(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(err); return; }
    if (memcmp(buf, sb_tier3, sizeof sb_tier3)) { finish("the tier-3 area reads back different from what was written"); return; }
    Q.updated = true;
    LOGI("[tier3] the tier-3 area now holds this bridge's loader (%u sectors at LBA %u)\n", (unsigned)SECTORS, (unsigned)SB_TIER3_LBA);
    finish(NULL);
}

static void reread(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { finish("reading the tier-3 area back failed"); return; }
    sb_mem_read(SB_UPD_BUF2, 4, sizeof sb_tier3 / 4, buf, verified, NULL);
}

static void written(void *ctx, const char *err, uint32_t v0) {
    (void)ctx;
    if (err || v0) { finish("writing the tier-3 area failed"); return; }
    sb_mem_call(SB_UPD_DISK_READ, SB_TIER3_LBA, SECTORS, SB_UPD_BUF2, 0, reread, NULL);
}

static void image_in(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(err); return; }
    sb_mem_call(SB_UPD_DISK_WRITE, SB_TIER3_LBA, SECTORS, SB_UPD_BUF, 0, written, NULL);
}

static void updater_in(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(err); return; }
    sb_mem_write(SB_UPD_BUF, 4, sizeof sb_tier3 / 4, sb_tier3, image_in, NULL);
}

static void compared(void *ctx, const char *err, uint32_t v0) {
    (void)ctx; (void)v0;
    if (err) { finish(err); return; }
    if (!memcmp(buf, sb_tier3, sizeof sb_tier3)) { finish(NULL); return; }   // current
    LOGI("[tier3] the loader on the disk isn't this bridge's: rewriting the tier-3 area\n");
    if (sb_t3_progress) sb_t3_progress();
    sb_mem_write(SB_UPD_BASE, 4, sizeof sb_updater / 4, sb_updater, updater_in, NULL);
}

void (*sb_t3_progress)(void);

void sb_t3_check(sb_t3_done_fn done) {
    memset(&Q, 0, sizeof Q);
    Q.done = done;
    // test 19 read the area to SB_TIER3_RAM (cached): read it there, through the cache
    sb_mem_read(SB_TIER3_RAM, 4, sizeof sb_tier3 / 4, buf, compared, NULL);
}
