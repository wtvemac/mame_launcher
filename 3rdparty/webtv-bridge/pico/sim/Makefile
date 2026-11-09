# The bridge firmware built for the emulator (both cores, WebAssembly): src/machine/picosim.js
# loads out/lptbridge.mjs and puts it on the emulated box's parallel port (run.mjs --pico).
#   make            needs emscripten (emcc)
EMCC ?= emcc
SRC = ../src
SRCS = main_sim.c net_sim.c sb_patches_embed.c \
       $(SRC)/port.c $(SRC)/link.c $(SRC)/portglue.c $(SRC)/web.c \
       $(SRC)/modem.c $(SRC)/ppp.c $(SRC)/gw.c $(SRC)/bridge.c $(SRC)/settings.c $(SRC)/console.c $(SRC)/log.c $(SRC)/scanlist.c \
       $(wildcard $(SRC)/sb/*.c) $(SRC)/gen/sb_assets.c $(SRC)/gen/web_page.c
EXPORTS = _sim_init,_sim_box,_sim_tick,_sim_status,_sim_data,_sim_mode,_sim_core0,_sim_console,_sim_scan_crowd,_sim_state,_malloc,_free

out/lptbridge.mjs: $(SRCS) sim_lib.js $(SRC)/*.h $(SRC)/sb/*.h $(SRC)/gen/sb_patches.bin
	@mkdir -p out
	$(EMCC) -O2 -std=gnu2x -DLINK_HOST_SIM -I$(SRC) -I. -Wall -Wno-unused-parameter -Wno-c23-extensions $(SRCS) \
	  --js-library sim_lib.js -sMODULARIZE -sEXPORT_ES6 -sENVIRONMENT=node -sALLOW_MEMORY_GROWTH \
	  -sEXPORTED_FUNCTIONS=$(EXPORTS) -sEXPORTED_RUNTIME_METHODS=ccall,HEAPU8 -o $@

clean:
	rm -rf out

.PHONY: clean
