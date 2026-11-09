#include <stdio.h>
#include "log.h"

int log_level = LOG_INFO;

void log_printf(int level, const char *fmt, ...) {
    if (level > log_level) return;
    va_list ap;
    va_start(ap, fmt);
    vprintf(fmt, ap);
    va_end(ap);
}
