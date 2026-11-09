// Persistent settings (last flash sector on the Pico, a file on the host).
#pragma once
#include <stdint.h>
#include <stdbool.h>

#define SETTINGS_MAGIC 0x4c50544du  // the bytes "LPTM" (kept: what saved settings start with)
#define SETTINGS_VERSION 2         // 2 added dns; version 1 settings are carried over

typedef struct {
    uint32_t magic, version;
    char ssid[33];
    char password[65];
    char service[64];      // WebTV service host for ports 1600-1699 (empty = where the box asks)
    char country[3];       // WiFi country code, e.g. "US"
    uint8_t log_level;
    uint8_t no_startup;    // 1 = don't take a stock box through its start-up (§10.7); in what used to
                           // be padding, so settings saved before it read as 0 (start-up on)
    char dns[16];          // DNS server, dotted quad; empty = the router's (from DHCP), or 8.8.8.8
                           // when the router gives none
    uint8_t box_off;       // what the bridge leaves out of the OS it starts (BOX_OFF_*); in what used
                           // to be padding, so settings saved before it read as 0 (all of it on)
    uint32_t crc;
} settings_t;
_Static_assert(__builtin_offsetof(settings_t, crc) == 192, "settings_t's layout moved: saved settings would be lost");

#define BOX_OFF_SONY      0x01   // don't decode Sony's WebTV remote (20-bit SIRC)
#define BOX_OFF_PHILIPS   0x02   // don't decode Philips RC-5 remotes
#define BOX_OFF_AUTOPOWER 0x04   // the OS waits in standby for the power key, as it does on its own
#define BOX_OFF_TEST18    0x08   // don't use a debug boot ROM's monitor (test 18): start through test 19
                                 // and the disk's tier-3 area, as a production ROM must (console `entry`)

extern settings_t settings;

void settings_defaults(void);
bool settings_load(void);    // false = nothing valid stored (defaults loaded)
bool settings_save(void);

// platform storage
bool settings_storage_read(void *buf, uint32_t len);
bool settings_storage_write(const void *buf, uint32_t len);
uint32_t settings_crc(const settings_t *s);
