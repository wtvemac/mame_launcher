#pragma once

void console_input(int c);                                 // one character typed on the console
void console_poll(void);                                   // once round the main loop
void console_status(void (*print)(const char *fmt, ...));  // the bridge's state (status, the web page)

// provided by the platform
void platform_reboot(void);
void net_reconfigure(void);  // WiFi settings changed: reconnect
