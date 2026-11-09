// LPT modem bridge firmware for the Raspberry Pi Pico 2 W (WEBTV_LPT_BRIDGE.md).
//
// Core 1 runs the port engine (port.c: the probe watch, the dongle, PARMOD) from RAM. Core 0 runs everything
// else in one polled loop: CYW43 WiFi + lwIP, the start-up that sets up a stock box and patches
// its OS in RAM (sb/), the modem/PPP/gateway stack (bridge.h) on PARMOD (portglue.c),
// and the setup console on USB CDC and a UART (UART1 on GP20/GP21 on rev 2 boards, UART0 on
// GP0/GP1 on rev 1).
#include <stdio.h>
#include <string.h>
#include <stdarg.h>
#include "pico/stdlib.h"
#include "pico/cyw43_arch.h"
#include "pico/flash.h"
#include "hardware/flash.h"
#include "hardware/watchdog.h"
#include "hardware/uart.h"
#include "pico/stdio_usb.h"
#include "pico/stdio_uart.h"
#include "bridge.h"
#include "link.h"
#include "link_hw.h"
#include "port.h"
#include "web.h"
#include "gw.h"
#include "net.h"
#include "settings.h"
#include "console.h"
#include "log.h"

bool net_init(void);

uint32_t bridge_now_ms(void) { return to_ms_since_boot(get_absolute_time()); }

void platform_reboot(void) {
    watchdog_reboot(0, 0, 10);
    for (;;) tight_loop_contents();
}

// ---------------------------------------------------------------- settings in the last flash sector

#define SETTINGS_OFFSET (PICO_FLASH_SIZE_BYTES - FLASH_SECTOR_SIZE)

bool settings_storage_read(void *buf, uint32_t len) {
    memcpy(buf, (const void *)(XIP_BASE + SETTINGS_OFFSET), len);
    return true;
}

static uint8_t page_buf[FLASH_SECTOR_SIZE];
static void do_flash(void *arg) {
    (void)arg;
    flash_range_erase(SETTINGS_OFFSET, FLASH_SECTOR_SIZE);
    flash_range_program(SETTINGS_OFFSET, page_buf, FLASH_SECTOR_SIZE);
}

bool settings_storage_write(const void *buf, uint32_t len) {
    if (len > sizeof page_buf) return false;
    memset(page_buf, 0xff, sizeof page_buf);
    memcpy(page_buf, buf, len);
    // core 1 runs only from RAM (PICO_FLASH_ASSUME_CORE1_SAFE), so it keeps answering the box
    return flash_safe_execute(do_flash, NULL, 500) == PICO_OK;
}

// ---------------------------------------------------------------- main loop

// LED on the CYW43: blinking = no WiFi, on = WiFi up, flicker = carrier and traffic
static void led(uint32_t now) {
    static uint32_t last_change;
    static bool on;
    bool want;
    if (!net_up()) want = (now / 500) & 1;
    else if (bridge_carrier()) want = (now / 100) % 10 != 0;
    else want = true;
    if (want != on && now - last_change > 20) {
        on = want;
        last_change = now;
        cyw43_arch_gpio_put(CYW43_WL_GPIO_LED_PIN, on);
    }
}

int main(void) {
    stdio_usb_init();
    // the link comes up first, so the box sees an idle bridge (busy low, no data, no carrier);
    // it also finds out which board this is, which says where the console UART goes
    link_init();
    stdio_uart_init_full(uart_get_instance(link_board.uart), 115200, link_board.uart_tx, link_board.uart_rx);
    gpio_pull_up(link_board.uart_rx);   // idle high with nothing attached, instead of a break
    if (!settings_load()) LOGI("no saved settings; use \"wifi <ssid> <password>\" then \"save\"\n");
    log_level = settings.log_level;
    gw_set_service(settings.service);
    bridge_init();
    LOGI("\nWebTV LPT modem bridge (PARMOD, stock-box start-up %s), %s, parallel2usb rev %d, RP2350 A%d\n",
         settings.no_startup ? "off" : "on", __DATE__, link_board.rev, rp2350_rom_version());
    net_init();
    port_service_start();
    LOGI("type \"help\" for commands\n> ");

    for (;;) {
        cyw43_arch_poll();
        uint32_t now = bridge_now_ms();
        net_poll(now);
        port_service(now);
        bridge_poll(now);
        port_service(now);
        web_poll(now);
        console_poll();
        int c = getchar_timeout_us(0);
        if (c >= 0) console_input(c);
        led(now);
    }
}
