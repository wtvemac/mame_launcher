// The bridge's web page and the box's console over TCP (web.c)
#pragma once
#include <stdint.h>

extern uint16_t web_port, web_console_port;   // 80 and 2323 unless changed before the first web_poll
void web_poll(uint32_t now_ms);               // each time round the main loop; starts listening once WiFi is up
