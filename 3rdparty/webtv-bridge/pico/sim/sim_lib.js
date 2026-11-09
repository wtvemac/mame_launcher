// The firmware's platform imports (main_sim.c, net_sim.c), handed to the emulator's side:
// src/machine/picosim.js passes `simHost` when it instantiates the module.
addToLibrary({
  js_now_us: () => Module.simHost.nowUs(),
  js_pins_changed: () => Module.simHost.pinsChanged(),
  js_settings_read: (buf, len) => {
    const d = Module.simHost.settingsRead(len);
    if (!d) return 0;
    HEAPU8.set(d.subarray(0, len), buf);
    return 1;
  },
  js_settings_write: (buf, len) => (Module.simHost.settingsWrite(HEAPU8.slice(buf, buf + len)) ? 1 : 0),
  js_reboot: () => Module.simHost.reboot(),

  js_tcp_open__deps: ['$UTF8ToString'],
  js_tcp_open: (host, port) => Module.simHost.tcpOpen(UTF8ToString(host), port),
  js_tcp_state: (id) => Module.simHost.tcpState(id),
  js_tcp_recv: (id, buf, max) => {
    const r = Module.simHost.tcpRecv(id, max);
    if (typeof r === 'number') return r;
    HEAPU8.set(r, buf);
    return r.length;
  },
  js_tcp_send: (id, buf, n) => Module.simHost.tcpSend(id, HEAPU8.slice(buf, buf + n)),
  js_tcp_end: (id) => Module.simHost.tcpEnd(id),
  js_tcp_close: (id) => Module.simHost.tcpClose(id),
  js_tcp_error__deps: ['$stringToNewUTF8'],
  js_tcp_error: (id) => stringToNewUTF8(Module.simHost.tcpError(id)),
  js_resolve__deps: ['$UTF8ToString'],
  js_resolve: (name) => Module.simHost.resolve(UTF8ToString(name)),
  js_resolve_result: (id, ip) => {
    const r = Module.simHost.resolveResult(id);
    if (r === null) return -1;
    if (!r) return 0;
    HEAPU8.set(r, ip);
    return 1;
  },
  js_listen: (port) => Module.simHost.listen(port),
  js_accept: (lid) => Module.simHost.accept(lid),
});
