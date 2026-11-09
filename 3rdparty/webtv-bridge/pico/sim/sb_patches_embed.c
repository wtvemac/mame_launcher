// The patch table for the WebAssembly build: sb_patches.S's .incbin, as C23 #embed.
#include <stdint.h>

const uint8_t sb_patches[] = {
#embed "../src/gen/sb_patches.bin"
};
const uint8_t sb_patches_end[1];
