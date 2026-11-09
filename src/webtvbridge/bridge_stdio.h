// license:BSD-3-Clause
// Force-included into every bridge translation unit (-include). The firmware logs with
// printf/vprintf/putchar; inside the launcher those go to the launcher's console through the
// host callback instead of the process's stdout. Function-like macros, so `format(printf, ...)`
// attributes are left alone.
#pragma once
#include <stdio.h>
#include <stdarg.h>

int bridge_printf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
int bridge_vprintf(const char *fmt, va_list ap);
int bridge_putchar(int c);

#undef printf
#undef vprintf
#undef putchar
#define printf(...) bridge_printf(__VA_ARGS__)
#define vprintf(...) bridge_vprintf(__VA_ARGS__)
#define putchar(...) bridge_putchar(__VA_ARGS__)
