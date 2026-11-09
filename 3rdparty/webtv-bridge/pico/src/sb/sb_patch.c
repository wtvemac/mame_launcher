// The patch table (src/stockbox/patchtable.js; tools/picogen.mjs writes it as sb_patches.bin):
// each known OS build's LPT patch as runs of words, looked up by what the bridge reads from the
// image in the box's RAM.
#include <string.h>
#include "sb.h"
#include "../gen/sb_assets.h"

static uint32_t be32(const uint8_t *p) { return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) | ((uint32_t)p[2] << 8) | p[3]; }

// header (64 bytes) + the trailer's two words -> what identifies a build
bool sb_identity(const uint8_t hdr[64], uint32_t trailer_words, uint32_t trailer_sum, sb_ident_t *id) {
    memset(id, 0, sizeof *id);
    if ((be32(hdr) >> 16) != 0x1000) return false;
    id->size = be32(hdr + 0x0c) * 4;
    id->codewords = be32(hdr + 0x10);
    id->code = be32(hdr + 0x08);
    if (id->size < 16 || id->codewords * 4 > id->size) return false;
    if (be32(hdr + 0x24) != 0x4e6f4653) {   // 'NoFS'
        uint32_t start = id->size - 8 - trailer_words * 4;
        if (trailer_words && trailer_words * 4 < id->size && start >= id->codewords * 4) {
            id->has_fs = true;
            id->fs_start = start;
            id->fs_words = trailer_words;
            id->fs_sum = trailer_sum;
        }
    }
    return true;
}

uint32_t sb_patch_count(void) { return be32(sb_patches + 8); }

static sb_patch_entry_t entry_at(uint32_t i) {
    const uint8_t *p = sb_patches + 12 + i * SB_PATCH_ENTRY;
    sb_patch_entry_t e = { be32(p), be32(p + 4), be32(p + 8), be32(p + 12), be32(p + 16), be32(p + 20), be32(p + 24), be32(p + 28),
                           be32(p + 32), be32(p + 36), be32(p + 40), be32(p + 44) };
    return e;
}

const sb_patch_entry_t *sb_patch_find(const sb_ident_t *id) {
    static sb_patch_entry_t found;
    if (memcmp(sb_patches, "LPTP", 4) || be32(sb_patches + 4) != SB_PATCH_TABLE_VERSION) return NULL;
    for (uint32_t i = 0; i < sb_patch_count(); i++) {
        sb_patch_entry_t e = entry_at(i);
        if (e.code == id->code && e.size == id->size && (e.flags & 1) == (uint32_t)id->has_fs && (!id->has_fs || e.fs == id->fs_sum)) {
            found = e;
            return &found;
        }
    }
    return NULL;
}

const char *sb_patch_name(const sb_patch_entry_t *e) { return (const char *)sb_patches + e->name; }
