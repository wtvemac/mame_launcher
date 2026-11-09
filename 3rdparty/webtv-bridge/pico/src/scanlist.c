// The networks a scan found, as the radio reports them, made into the list the TV shows.
//
// The radio reports a network once per access point and often more than once, in the order it
// sweeps the channels, not by signal. So each report is merged into the list by name (keeping the
// strongest signal); hidden networks (no name) are left out; and once the list is full a stronger
// network takes the weakest one's place, so a crowded building can't push a strong network out.
#include <string.h>
#include "net.h"

void scan_list_add(net_ap_t *list, int *n, int max, const uint8_t *ssid, size_t len, int rssi, bool secure) {
    if (!len) return;
    if (len > 32) len = 32;
    for (int i = 0; i < *n; i++) {
        if (strlen(list[i].ssid) != len || memcmp(list[i].ssid, ssid, len)) continue;
        if (rssi > list[i].rssi) { list[i].rssi = (int8_t)rssi; list[i].secure = secure; }
        return;
    }
    int at = *n;
    if (*n >= max) {   // full: replace the weakest, if this one is stronger
        at = 0;
        for (int i = 1; i < *n; i++) if (list[i].rssi < list[at].rssi) at = i;
        if (rssi <= list[at].rssi) return;
    } else (*n)++;
    memcpy(list[at].ssid, ssid, len);
    list[at].ssid[len] = 0;
    list[at].rssi = (int8_t)rssi;
    list[at].secure = secure;
}

// strongest first
void scan_list_sort(net_ap_t *list, int n) {
    for (int a = 1; a < n; a++)
        for (int b = a; b > 0 && list[b].rssi > list[b - 1].rssi; b--) {
            net_ap_t t = list[b]; list[b] = list[b - 1]; list[b - 1] = t;
        }
}
