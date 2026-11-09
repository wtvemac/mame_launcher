#include <string.h>
#include <stddef.h>
#include "settings.h"

settings_t settings;

// version 1: as now, without dns
typedef struct {
    uint32_t magic, version;
    char ssid[33], password[65], service[64], country[3];
    uint8_t log_level, no_startup;
    uint32_t crc;
} settings_v1_t;

static uint32_t crc_bytes(const void *d, size_t n) {
    const uint8_t *p = d;
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < n; i++) {
        crc ^= p[i];
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xedb88320u & (0u - (crc & 1)));
    }
    return ~crc;
}

uint32_t settings_crc(const settings_t *s) { return crc_bytes(s, offsetof(settings_t, crc)); }

void settings_defaults(void) {
    memset(&settings, 0, sizeof settings);
    settings.magic = SETTINGS_MAGIC;
    settings.version = SETTINGS_VERSION;
    strcpy(settings.country, "US");
    settings.log_level = 1;
}

bool settings_load(void) {
    union { settings_t now; settings_v1_t v1; } u;
    memset(&u, 0, sizeof u);
    if (!settings_storage_read(&u, sizeof u)) { settings_defaults(); return false; }
    settings_t s = u.now;
    if (u.v1.magic == SETTINGS_MAGIC && u.v1.version == 1 && u.v1.crc == crc_bytes(&u.v1, offsetof(settings_v1_t, crc))) {
        settings_v1_t o = u.v1;   // carry version 1 over: the same fields, dns empty
        memset(&s, 0, sizeof s);
        memcpy(s.ssid, o.ssid, sizeof s.ssid); memcpy(s.password, o.password, sizeof s.password);
        memcpy(s.service, o.service, sizeof s.service); memcpy(s.country, o.country, sizeof s.country);
        s.log_level = o.log_level; s.no_startup = o.no_startup;
        s.magic = SETTINGS_MAGIC; s.version = SETTINGS_VERSION; s.crc = settings_crc(&s);
    }
    if (s.magic == SETTINGS_MAGIC && s.version == SETTINGS_VERSION && s.crc == settings_crc(&s)) {
        settings = s;
        settings.ssid[sizeof settings.ssid - 1] = 0;
        settings.password[sizeof settings.password - 1] = 0;
        settings.service[sizeof settings.service - 1] = 0;
        settings.country[sizeof settings.country - 1] = 0;
        settings.dns[sizeof settings.dns - 1] = 0;
        return true;
    }
    settings_defaults();
    return false;
}

bool settings_save(void) {
    settings.magic = SETTINGS_MAGIC;
    settings.version = SETTINGS_VERSION;
    settings.crc = settings_crc(&settings);
    return settings_storage_write(&settings, sizeof settings);
}
