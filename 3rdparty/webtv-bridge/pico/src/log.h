// Logging to the debug console (USB CDC and UART1 on the Pico, stderr on the host).
#pragma once
#include <stdarg.h>

enum { LOG_ERR = 0, LOG_INFO = 1, LOG_DEBUG = 2, LOG_TRACE = 3 };
extern int log_level;

void log_printf(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));

#define LOGE(...) log_printf(LOG_ERR, __VA_ARGS__)
#define LOGI(...) log_printf(LOG_INFO, __VA_ARGS__)
#define LOGD(...) log_printf(LOG_DEBUG, __VA_ARGS__)
#define LOGT(...) log_printf(LOG_TRACE, __VA_ARGS__)
