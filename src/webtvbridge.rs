// The built-in WebTV Bridge: Joe's webtv-bridge firmware (3rdparty/webtv-bridge) running in the launcher as the
// peripheral on MAME's parallel port socket (-pport socket):
//
//   pins/time   this file runs the TOOT exchange with MAME, on MAME's emulated clock, including the local handshake
//               mode (0x02): bridge_rules.inc turns the firmware's state into engine rules MAME runs itself, and the
//               batches MAME reports back are replayed into the firmware and checked
//   platform    the firmware's settings, TCP, DNS, listening ports and WiFi list come here through one host callback
//               (bridge_host.c)
//   instances   the firmware keeps its state in globals, so every power-on (and firmware reboot) loads a fresh copy
//               of the embedded shared library
//
// The box's traffic goes out through the host's own network; the "WiFi" the box picks is only a label.
use std::{
	cell::RefCell,
	collections::HashMap,
	ffi::{c_char, c_int, CStr, CString},
	fs,
	io::{self, ErrorKind, Read, Write},
	net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket},
	path::{Path, PathBuf},
	sync::{atomic::{AtomicBool, AtomicU32, Ordering}, mpsc, Arc, Mutex},
	thread::{self, JoinHandle},
	time::{Duration, Instant}
};
use libloading::Library;

static FIRMWARE_IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/webtv_bridge.bin"));

// The firmware's two "cores" (main_sim.c): core 0's loop every 1 ms, core 1's serial sampling on a 4-us grid
const CORE_NS: u64 = 1_000_000;
const TICK_NS: u64 = 4_000;
const NEVER: u64 = u64::MAX;
// After an nSelectIn edge the diagnostic serial is sampled for 1 ms (a 19200-baud character is under 0.6 ms).
const SERIAL_ACTIVE_NS: u64 = 1_000_000;

// TOOT version 1 frames: u16 payload length, u8 kind, u8 flags, u32 id, u64 time (ns), then the payload
const HEADER_BYTES: usize = 16;
const FRAME_HELLO: u8 = 0x01;
const FRAME_HOST_PIN_STATE: u8 = 0x02;
const FRAME_HOST_SYNC_REQUEST: u8 = 0x03;
const FRAME_MODE_ACK: u8 = 0x04;
const FRAME_MODE_NAK: u8 = 0x05;
const FRAME_HOST_LOCAL_BATCH: u8 = 0x06;
const FRAME_SET_MODE: u8 = 0x81;
const FRAME_PERIPHERAL_PIN_STATE: u8 = 0x82;
const FRAME_PERIPHERAL_SYNC_REPLY: u8 = 0x83;
const FRAME_ENGINE_CONFIG: u8 = 0x84;
const HELLO_BYTES: usize = 10;
const CAP_LOCAL_HANDSHAKE: u32 = 1 << 0;
const MODE_PIN_PASSTHROUGH: u8 = 0x01;
const MODE_LOCAL_HANDSHAKE: u8 = 0x02;
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(5);
// A reply MAME doesn't take within this is a dead peer. Waiting for MAME's frames has no limit: it may be paused.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(100);

const MAX_EVENTS: usize = 4095;          // bridge_host.c's pin state queue
const PIN_STATE_BYTES: usize = 4;
const LOCAL_EVENT_BYTES: usize = 24;     // u64 time, 4 host pin bytes, u16 output count, 2 zero, u32 output hash, 4 final pin bytes
const MAX_BATCH_BYTES: usize = 49132;
const ENGINE_CONFIG_BYTES: usize = 49152;

// The firmware's web page and console ports (web.c), and the "network" offered for the host's own uplink.
const FIRMWARE_WEB_PORT: u16 = 8080;
const FIRMWARE_CONSOLE_PORT: u16 = 2323;
const HOST_UPLINK_NAME: &str = "Host uplink";

// net.h join states
const NET_JOIN_IDLE: isize = 0;
const NET_JOIN_OK: isize = 2;
const NET_JOIN_FAIL: isize = 5;

// sb.h's sb_state_t, for the log
const STARTUP_STATES: &[&str] = &["off", "armed", "wifi", "loading", "patching", "countdown", "settings", "saving", "installing", "os", "no-tier3"];

pub type LogFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct BridgeOptions {
	pub state_dir: PathBuf, // settings.bin and the per-instance firmware copies
	pub http_port: u16,     // where the bridge's web page listens (127.0.0.1)
	pub console_port: u16,  // where the bridge's console listens (127.0.0.1)
	pub log_level: i32,
	pub service: String,    // "" = the service saved in the bridge's settings
	pub log: LogFn
}

impl BridgeOptions {
	pub fn new(state_dir: PathBuf, log: LogFn) -> BridgeOptions {
		BridgeOptions {
			state_dir,
			http_port: FIRMWARE_WEB_PORT,
			console_port: FIRMWARE_CONSOLE_PORT,
			log_level: 1,
			service: "".into(),
			log
		}
	}
}

// A running bridge: listens on 127.0.0.1:port() for MAME's parallel port socket. Dropping it stops it.
pub struct WebTVBridge {
	port: u16,
	stop: Arc<AtomicBool>,
	thread: Option<JoinHandle<()>>
}

impl WebTVBridge {
	pub fn start(options: BridgeOptions) -> io::Result<WebTVBridge> {
		fs::create_dir_all(&options.state_dir)?;
		remove_stale_firmware_copies(&options.state_dir);

		let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
		listener.set_nonblocking(true)?;
		let port = listener.local_addr()?.port();

		let stop = Arc::new(AtomicBool::new(false));
		let thread_stop = stop.clone();
		let thread = thread::Builder::new()
			.name("webtv-bridge".into())
			.stack_size(8 * 1024 * 1024)
			.spawn(move || serve(listener, thread_stop, options))?;

		Ok(WebTVBridge {
			port,
			stop,
			thread: Some(thread)
		})
	}

	pub fn port(&self) -> u16 {
		self.port
	}
}

impl Drop for WebTVBridge {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

fn serve(listener: TcpListener, stop: Arc<AtomicBool>, options: BridgeOptions) {
	let log = options.log.clone();
	let ports = Arc::new(Mutex::new(HashMap::new()));

	while !stop.load(Ordering::SeqCst) {
		match listener.accept() {
			Ok((stream, _)) => {
				log("[webtv bridge] MAME connected");
				match run_session(stream, &stop, &options, &ports) {
					Ok(()) if !stop.load(Ordering::SeqCst) => log("[webtv bridge] MAME disconnected"),
					Ok(()) => {},
					Err(error) => log(&format!("[webtv bridge] session ended: {}", error))
				}
			},
			Err(error) if error.kind() == ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(20)),
			Err(error) => {
				log(&format!("[webtv bridge] accept failed: {}", error));
				thread::sleep(Duration::from_millis(200));
			}
		}
	}
}

////
//// Mode negotiation
////

struct FrameHeader {
	length: usize,
	kind: u8,
	flags: u8,
	id: u32,
	time: u64
}

impl FrameHeader {
	fn parse(bytes: &[u8]) -> FrameHeader {
		FrameHeader {
			length: u16::from_be_bytes([bytes[0], bytes[1]]) as usize,
			kind: bytes[2],
			flags: bytes[3],
			id: get_u32(&bytes[4..]),
			time: get_u64(&bytes[8..])
		}
	}
}

fn get_u32(bytes: &[u8]) -> u32 {
	u32::from_be_bytes(bytes[..4].try_into().unwrap())
}

fn get_u64(bytes: &[u8]) -> u64 {
	u64::from_be_bytes(bytes[..8].try_into().unwrap())
}

fn is_timeout(error: &io::Error) -> bool {
	matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

// Reads exactly buffer.len() bytes; gives up when stopped or after the deadline. The stream has a STOP_POLL read timeout.
fn read_exact_until(stream: &mut TcpStream, buffer: &mut [u8], stop: &AtomicBool, deadline: Option<Instant>) -> io::Result<()> {
	let mut filled = 0;
	while filled < buffer.len() {
		if stop.load(Ordering::SeqCst) {
			return Err(io::Error::new(ErrorKind::Interrupted, "stopped"));
		}
		if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
			return Err(io::Error::new(ErrorKind::TimedOut, "mode negotiation stalled"));
		}
		match stream.read(&mut buffer[filled..]) {
			Ok(0) => return Err(io::Error::new(ErrorKind::UnexpectedEof, "connection closed during negotiation")),
			Ok(count) => filled += count,
			Err(error) if is_timeout(&error) || error.kind() == ErrorKind::Interrupted => {},
			Err(error) => return Err(error)
		}
	}
	Ok(())
}

// HELLO, then the peripheral picks the mode and waits for MAME's ACK/NAK before any pins. Returns the mode and HELLO's time.
fn negotiate(stream: &mut TcpStream, stop: &AtomicBool) -> Result<(u8, u64), String> {
	let mut header = [0u8; HEADER_BYTES];
	read_exact_until(stream, &mut header, stop, None).map_err(|e| e.to_string())?;
	let hello = FrameHeader::parse(&header);
	if hello.kind != FRAME_HELLO || hello.flags != 0 || hello.length != HELLO_BYTES {
		return Err("expected a TOOT HELLO".into());
	}
	// "TOOT", u16 version, u32 capabilities
	let mut payload = [0u8; HELLO_BYTES];
	read_exact_until(stream, &mut payload, stop, None).map_err(|e| e.to_string())?;
	if &payload[0..4] != b"TOOT" || u16::from_be_bytes([payload[4], payload[5]]) != 1 {
		return Err("unsupported TOOT protocol version".into());
	}
	let capabilities = get_u32(&payload[6..]);

	let mode = if capabilities & CAP_LOCAL_HANDSHAKE != 0 { MODE_LOCAL_HANDSHAKE } else { MODE_PIN_PASSTHROUGH };
	let mut request = [0u8; HEADER_BYTES + 1];
	request[..HEADER_BYTES].copy_from_slice(&header);
	request[0..4].copy_from_slice(&[0, 1, FRAME_SET_MODE, 0]);
	request[HEADER_BYTES] = mode;
	stream.set_write_timeout(Some(NEGOTIATION_TIMEOUT)).map_err(|e| e.to_string())?;
	stream.write_all(&request).map_err(|e| e.to_string())?;

	let mut reply = [0u8; HEADER_BYTES + 1];
	read_exact_until(stream, &mut reply, stop, Some(Instant::now() + NEGOTIATION_TIMEOUT)).map_err(|e| e.to_string())?;
	let reply_header = FrameHeader::parse(&reply);
	if reply_header.length != 1 || reply_header.flags != 0 || reply_header.id != hello.id || reply_header.time != hello.time
		|| !(reply_header.kind == FRAME_MODE_ACK || reply_header.kind == FRAME_MODE_NAK) || reply[HEADER_BYTES] != mode {
		return Err("invalid mode reply".into());
	}
	if reply_header.kind != FRAME_MODE_ACK {
		return Err("MAME rejected the requested parallel socket mode".into());
	}

	Ok((mode, hello.time))
}

////
//// The session: MAME's frames drive the firmware
////

// Firmware clock state, carried across a firmware reboot
struct Clock {
	now: u64,
	next_core: u64,
	next_tick: u64,
	serial_until: u64
}

enum RunEnd {
	Disconnected,
	Reboot,
	Stopped
}

struct Session<'a> {
	stream: &'a mut TcpStream,
	stop: &'a AtomicBool,
	log: &'a LogFn,
	local: bool,
	clock: Clock,
	host: [u8; 4],        // the box's lines as MAME last sent them
	pending: Vec<u8>,     // received bytes not yet handled (kept across a reboot: the frame that found it)
	exchanges: u64,
	started: Instant,
	states: Vec<u8>,      // scratch: firmware pin states
	output: Vec<u8>,      // scratch: the reply
	receive: Vec<u8>      // scratch: socket reads
}

fn run_session(mut stream: TcpStream, stop: &Arc<AtomicBool>, options: &BridgeOptions, ports: &Arc<Mutex<HashMap<u16, u16>>>) -> Result<(), String> {
	let log = &options.log;

	stream.set_nonblocking(false).map_err(|e| e.to_string())?;
	stream.set_nodelay(true).map_err(|e| e.to_string())?;
	stream.set_read_timeout(Some(STOP_POLL)).map_err(|e| e.to_string())?;

	let (mode, time) = negotiate(&mut stream, stop)?;
	log(if mode == MODE_LOCAL_HANDSHAKE { "[webtv bridge] using local handshake mode (0x02)" } else { "[webtv bridge] using raw pin mode (0x01)" });
	stream.set_write_timeout(Some(RESPONSE_TIMEOUT)).map_err(|e| e.to_string())?;

	// A new session powers the bridge on.
	let mut firmware = Firmware::load(options, stop, ports)?;
	firmware.set_time(time);
	let mut session = Session {
		stream: &mut stream,
		stop,
		log,
		local: mode == MODE_LOCAL_HANDSHAKE,
		clock: Clock { now: time, next_core: time + CORE_NS, next_tick: NEVER, serial_until: 0 },
		host: [255, 15, 0, 0],
		pending: Vec::with_capacity(65536),
		exchanges: 0,
		started: Instant::now(),
		states: vec![0u8; MAX_EVENTS * PIN_STATE_BYTES],
		output: Vec::with_capacity(MAX_EVENTS * (HEADER_BYTES + PIN_STATE_BYTES) + HEADER_BYTES + ENGINE_CONFIG_BYTES + HEADER_BYTES + 8),
		receive: vec![0u8; 65536]
	};
	session.arm_tick(&firmware);
	// The power-on state is the first thing MAME hears.
	let initial = firmware.last_state();
	firmware.seed_events(&initial)?;

	loop {
		let end = session.run(&firmware);
		firmware.check_callback()?;

		match end? {
			RunEnd::Reboot => {
				// The firmware restarted itself: a fresh copy, at the same time, seeing the box's current lines.
				let now = session.clock.now;
				drop(firmware);
				firmware = Firmware::load(options, stop, ports)?;
				firmware.set_time(now);
				session.clock = Clock { now, next_core: now + CORE_NS, next_tick: NEVER, serial_until: 0 };
				let mut events = firmware.read_events()?;
				session.arm_tick(&firmware);

				let host = session.host;
				if (15 ^ host[1]) & 8 != 0 {
					session.clock.serial_until = now + SERIAL_ACTIVE_NS;
				}
				firmware.sim_box(solo_control(host[1]), host[0] as c_int, (host[2] != 0) as c_int);
				events.extend_from_slice(&firmware.read_events()?);
				session.arm_tick(&firmware);
				firmware.check_callback()?;
				firmware.seed_events(&events)?;
			},
			RunEnd::Disconnected | RunEnd::Stopped => {
				log(&format!("[webtv bridge] {} exchanges", session.exchanges));
				return Ok(());
			}
		}
	}
}

// The box's control lines as the host frame has them, in SOLO CTRL order (main_sim.c's sim_box)
fn solo_control(control: u8) -> c_int {
	let control = control as c_int;
	(control & 9) | ((control & 2) << 1) | ((control & 4) >> 1)
}

// A frame header for a reply, with the request's id and time
fn put_header(output: &mut Vec<u8>, size: usize, kind: u8, request: &[u8]) {
	output.extend_from_slice(&(size as u16).to_be_bytes());
	output.extend_from_slice(&[kind, 0]);
	output.extend_from_slice(&request[4..HEADER_BYTES]);
}

impl Session<'_> {
	fn internal_deadline(&self) -> u64 {
		self.clock.next_core.min(self.clock.next_tick)
	}

	// The time up to which MAME may run without asking: core 0's next pass or the next tick that can change the pins.
	// In local mode the diagnostic serial's quiet stretches don't count (bridge_diag_output_deadline).
	fn deadline(&self, firmware: &Firmware) -> u64 {
		if self.local && firmware.mode() == 1 {
			let output = firmware.diag_output_deadline(self.clock.now, self.clock.next_tick);
			return self.clock.next_core.min(output);
		}
		self.internal_deadline()
	}

	// The diagnostic serial needs 4-us ticks while it's active: just after an nSelectIn edge, and while its TX is busy.
	fn arm_tick(&mut self, firmware: &Firmware) {
		if firmware.mode() == 1 && (self.clock.now < self.clock.serial_until || !firmware.diag_tx_idle()) {
			if self.clock.next_tick == NEVER {
				self.clock.next_tick = self.clock.now + TICK_NS;
			}
		} else {
			self.clock.next_tick = NEVER;
		}
	}

	// Waits until `need` bytes are pending; Some(end) when MAME disconnected or the bridge was stopped first.
	fn receive(&mut self, need: usize) -> Result<Option<RunEnd>, String> {
		while self.pending.len() < need {
			if self.stop.load(Ordering::SeqCst) {
				return Ok(Some(RunEnd::Stopped));
			}
			match self.stream.read(&mut self.receive) {
				Ok(0) => return Ok(Some(RunEnd::Disconnected)),
				Ok(count) => self.pending.extend_from_slice(&self.receive[..count]),
				Err(error) if error.kind() == ErrorKind::ConnectionReset => return Ok(Some(RunEnd::Disconnected)),
				Err(error) if is_timeout(&error) || error.kind() == ErrorKind::Interrupted => {},
				Err(error) => return Err(error.to_string())
			}
		}
		Ok(None)
	}

	fn send(&mut self) -> Result<Option<RunEnd>, String> {
		match self.stream.write_all(&self.output) {
			Ok(()) => Ok(None),
			Err(error) if matches!(error.kind(), ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::WriteZero) => Ok(Some(RunEnd::Disconnected)),
			Err(error) if is_timeout(&error) => Err("socket response stalled for five seconds".into()),
			Err(error) => Err(error.to_string())
		}
	}

	fn read_states(&mut self, firmware: &Firmware) -> Option<usize> {
		firmware.read_events_into(&mut self.states)
	}

	fn apply_host(&mut self, firmware: &Firmware, pins: [u8; 4], time: u64) {
		if (self.host[1] ^ pins[1]) & 8 != 0 {
			self.clock.serial_until = time + SERIAL_ACTIVE_NS;
		}
		self.host = pins;
		firmware.sim_box(solo_control(pins[1]), pins[0] as c_int, (pins[2] != 0) as c_int);
		self.arm_tick(firmware);
	}

	// Runs the firmware's own timers up to `time`. None of them may change the pins before it: MAME was promised none would.
	fn advance_time(&mut self, firmware: &Firmware, time: u64, last_state: &mut c_int) -> Result<Option<RunEnd>, String> {
		if time < self.clock.now || time > self.deadline(firmware) {
			return Err("host advanced past firmware deadline".into());
		}
		while self.internal_deadline() <= time {
			let due = self.internal_deadline();
			let before = firmware.event_count();
			self.clock.now = due;
			firmware.set_time(due);
			if self.clock.next_core == due {
				firmware.core0();
				self.clock.next_core += CORE_NS;
				self.arm_tick(firmware);
				let (callback_error, stopped) = platform_status();
				if stopped || self.stop.load(Ordering::SeqCst) {
					return Ok(Some(RunEnd::Stopped));
				}
				if callback_error {
					return Err("firmware platform callback failed".into());
				}
				let state = firmware.startup_state();
				if state != *last_state {
					let name = STARTUP_STATES.get(state as usize).copied().unwrap_or("unknown");
					(self.log)(&format!("[startup] {} (emulated {:.3} s, wall {:.3} s, {} exchanges)",
						name, due as f64 / 1e9, self.started.elapsed().as_secs_f64(), self.exchanges));
					*last_state = state;
				}
			}
			if self.clock.next_tick == due {
				firmware.tick();
				self.clock.next_tick = NEVER;
				self.arm_tick(firmware);
			}
			if due < time && firmware.event_count() != before {
				return Err("hidden firmware timer changed output pins".into());
			}
		}
		self.clock.now = time;
		firmware.set_time(time);
		Ok(None)
	}

	// Handles MAME's frames until it disconnects, the bridge is stopped, or the firmware asks for a reboot.
	fn run(&mut self, firmware: &Firmware) -> Result<RunEnd, String> {
		let mut batch = false;
		let mut configured = false;
		let mut last_state = firmware.startup_state();

		macro_rules! check {
			($result:expr) => {
				if let Some(end) = $result? {
					return Ok(end);
				}
			};
		}

		loop {
			check!(self.receive(HEADER_BYTES));
			let header = FrameHeader::parse(&self.pending);
			let limit = if self.local && header.kind == FRAME_HOST_LOCAL_BATCH { MAX_BATCH_BYTES } else { 8 };
			if header.flags != 0 || header.length > limit {
				return Err("invalid frame flags or payload length".into());
			}
			let size = HEADER_BYTES + header.length;
			check!(self.receive(size));
			let time = header.time;
			if time < self.clock.now || time > NEVER - CORE_NS {
				return Err("invalid or backwards frame time".into());
			}

			if header.kind == FRAME_HOST_LOCAL_BATCH {
				// What MAME's local engine did, to replay into the firmware: each event must give the same pins.
				if !self.local || batch || time > self.deadline(firmware) || !configured || header.length < 28
					|| (header.length - 4) % LOCAL_EVENT_BYTES != 0 || header.id != 0
					|| get_u32(&self.pending[HEADER_BYTES..]) != firmware.engine_generation() {
					return Err("invalid local batch or generation".into());
				}
				if self.read_states(firmware) != Some(0) {
					return Err("unobserved pins before local batch".into());
				}
				let mut position = HEADER_BYTES + 4;
				while position < size {
					let event: [u8; LOCAL_EVENT_BYTES] = self.pending[position..position + LOCAL_EVENT_BYTES].try_into().unwrap();
					let when = get_u64(&event);
					let pins: [u8; 4] = event[8..12].try_into().unwrap();
					if when < self.clock.now || when >= self.deadline(firmware) || when > time || event[14] != 0 || event[15] != 0
						|| (pins[1] & 0xf0) != 0 || (pins[2] != 0 && pins[2] != 255) || pins[3] != 0 {
						return Err("invalid local event time, flags or pins".into());
					}
					check!(self.advance_time(firmware, when, &mut last_state));
					self.apply_host(firmware, pins, when);

					let Some(count) = self.read_states(firmware) else {
						return Err("local replay output overflow".into());
					};
					let mut hash: u32 = 2166136261;
					for &byte in &self.states[..count * PIN_STATE_BYTES] {
						hash = (hash ^ byte as u32).wrapping_mul(16777619);
					}
					let expected_count = u16::from_be_bytes([event[12], event[13]]) as usize;
					let expected_hash = get_u32(&event[16..]);
					if count != expected_count || hash != expected_hash || firmware.state() != event[20..24] {
						return Err(format!("local handshake replay diverged at {} ns (count {}/{}, hash {:08x}/{:08x})",
							when, count, expected_count, hash, expected_hash));
					}
					position += LOCAL_EVENT_BYTES;
				}
				self.pending.drain(..size);
				continue;
			}

			// One exchange: pin states and then a sync request, all at one time.
			if !batch {
				if firmware.reboot_pending() {
					self.clock.now = time;
					return Ok(RunEnd::Reboot);
				}
				check!(self.advance_time(firmware, time, &mut last_state));
				batch = true;
			} else if time != self.clock.now {
				return Err("one exchange must have one timestamp".into());
			}

			if header.kind == FRAME_HOST_PIN_STATE && header.length == 4 && header.id == 0 {
				let pins: [u8; 4] = self.pending[HEADER_BYTES..HEADER_BYTES + 4].try_into().unwrap();
				if (pins[1] & 0xf0) != 0 || (pins[2] != 0 && pins[2] != 255) || pins[3] != 0 {
					return Err("invalid host pin state".into());
				}
				self.apply_host(firmware, pins, time);
			} else if header.kind == FRAME_HOST_SYNC_REQUEST && header.length == 0 {
				// Everything the firmware did since, then (local mode) fresh engine rules, then its next deadline.
				let Some(count) = self.read_states(firmware) else {
					return Err("too many pin transitions before synchronization".into());
				};
				let request: [u8; HEADER_BYTES] = self.pending[..HEADER_BYTES].try_into().unwrap();
				self.output.clear();
				for index in 0..count {
					put_header(&mut self.output, PIN_STATE_BYTES, FRAME_PERIPHERAL_PIN_STATE, &request);
					let state = &self.states[index * PIN_STATE_BYTES..(index + 1) * PIN_STATE_BYTES];
					self.output.extend_from_slice(state);
				}
				if self.local {
					// The payload goes in first (its size isn't known before), then its header in front of it.
					let start = self.output.len();
					self.output.resize(start + HEADER_BYTES + ENGINE_CONFIG_BYTES, 0);
					let config = firmware.engine_config(&mut self.output[start + HEADER_BYTES..]);
					if config == 0 {
						return Err("local engine configuration overflow".into());
					}
					let mut config_header = Vec::with_capacity(HEADER_BYTES);
					put_header(&mut config_header, config, FRAME_ENGINE_CONFIG, &request);
					self.output[start..start + HEADER_BYTES].copy_from_slice(&config_header);
					self.output.truncate(start + HEADER_BYTES + config);
					configured = true;
				}
				put_header(&mut self.output, 8, FRAME_PERIPHERAL_SYNC_REPLY, &request);
				let deadline = self.deadline(firmware);
				self.output.extend_from_slice(&deadline.to_be_bytes());
				check!(self.send());
				self.exchanges += 1;
				batch = false;
			} else {
				return Err("unexpected protocol frame".into());
			}
			self.pending.drain(..size);
		}
	}
}

////
//// A firmware instance
////

type HostCallback = extern "C" fn(c_int, isize, isize, isize) -> isize;

#[derive(Clone, Copy)]
struct FirmwareFunctions {
	bridge_set_host: unsafe extern "C" fn(HostCallback),
	bridge_set_time: unsafe extern "C" fn(u64),
	bridge_read_events: unsafe extern "C" fn(*mut u8) -> c_int,
	bridge_event_count: unsafe extern "C" fn() -> u32,
	bridge_seed_events: unsafe extern "C" fn(*const u8, u32) -> c_int,
	bridge_get_state: unsafe extern "C" fn(*mut u8),
	bridge_reboot_pending: unsafe extern "C" fn() -> c_int,
	bridge_engine_config: unsafe extern "C" fn(*mut u8, u32) -> u32,
	bridge_engine_generation: unsafe extern "C" fn() -> u32,
	bridge_diag_output_deadline: unsafe extern "C" fn(u64, u64) -> u64,
	sim_init: unsafe extern "C" fn(c_int, *const c_char),
	sim_box: unsafe extern "C" fn(c_int, c_int, c_int),
	sim_tick: unsafe extern "C" fn(),
	sim_core0: unsafe extern "C" fn(),
	sim_mode: unsafe extern "C" fn() -> c_int,
	sim_state: unsafe extern "C" fn() -> c_int,
	port_diag_tx_idle: unsafe extern "C" fn() -> bool
}

thread_local! {
	// The platform of the instance running on this thread (the host callback has no context argument).
	static PLATFORM: RefCell<Option<Platform>> = const { RefCell::new(None) };
}

static INSTANCE_COUNTER: AtomicU32 = AtomicU32::new(0);
const FIRMWARE_COPY_PREFIX: &str = ".firmware-";

// Copies a crashed launcher left behind (Windows can't delete a loaded library, so copies are removed after unloading).
fn remove_stale_firmware_copies(state_dir: &Path) {
	if let Ok(entries) = fs::read_dir(state_dir) {
		for entry in entries.flatten() {
			if entry.file_name().to_string_lossy().starts_with(FIRMWARE_COPY_PREFIX) {
				let _ = fs::remove_file(entry.path());
			}
		}
	}
}

// The platform's flags the session needs between firmware calls: (a callback failed, stopped)
fn platform_status() -> (bool, bool) {
	PLATFORM.with(|platform| {
		platform.borrow().as_ref()
			.map(|platform| (platform.callback_error, platform.stop.load(Ordering::SeqCst)))
			.unwrap_or((false, false))
	})
}

struct Firmware {
	functions: FirmwareFunctions,
	library: Option<Library>,
	image_path: PathBuf
}

impl Firmware {
	fn load(options: &BridgeOptions, stop: &Arc<AtomicBool>, ports: &Arc<Mutex<HashMap<u16, u16>>>) -> Result<Firmware, String> {
		// Loading the same file again would hand back the loaded library's globals; a unique copy is a fresh firmware.
		let image_path = options.state_dir.join(format!("{}{}-{}.{}", FIRMWARE_COPY_PREFIX, std::process::id(),
			INSTANCE_COUNTER.fetch_add(1, Ordering::SeqCst), std::env::consts::DLL_EXTENSION));
		fs::write(&image_path, FIRMWARE_IMAGE).map_err(|e| format!("couldn't write the bridge firmware: {}", e))?;
		// SAFETY: our own embedded library; it has no initialisers of its own.
		let library = match unsafe { Library::new(&image_path) } {
			Ok(library) => library,
			Err(error) => {
				let _ = fs::remove_file(&image_path);
				return Err(format!("couldn't load the bridge firmware: {}", error));
			}
		};

		macro_rules! symbol {
			($name:ident) => {
				// SAFETY: the types match the C definitions (bridge_host.c, bridge_rules.inc, main_sim.c, port.c).
				*unsafe { library.get(concat!(stringify!($name), "\0").as_bytes()) }
					.map_err(|e| format!("bridge firmware is missing {}: {}", stringify!($name), e))?
			};
		}
		let functions = FirmwareFunctions {
			bridge_set_host: symbol!(bridge_set_host),
			bridge_set_time: symbol!(bridge_set_time),
			bridge_read_events: symbol!(bridge_read_events),
			bridge_event_count: symbol!(bridge_event_count),
			bridge_seed_events: symbol!(bridge_seed_events),
			bridge_get_state: symbol!(bridge_get_state),
			bridge_reboot_pending: symbol!(bridge_reboot_pending),
			bridge_engine_config: symbol!(bridge_engine_config),
			bridge_engine_generation: symbol!(bridge_engine_generation),
			bridge_diag_output_deadline: symbol!(bridge_diag_output_deadline),
			sim_init: symbol!(sim_init),
			sim_box: symbol!(sim_box),
			sim_tick: symbol!(sim_tick),
			sim_core0: symbol!(sim_core0),
			sim_mode: symbol!(sim_mode),
			sim_state: symbol!(sim_state),
			port_diag_tx_idle: symbol!(port_diag_tx_idle)
		};

		PLATFORM.with(|platform| *platform.borrow_mut() = Some(Platform::new(options, stop.clone(), ports.clone())));

		let firmware = Firmware { functions, library: Some(library), image_path };
		let service = CString::new(options.service.clone()).unwrap_or_default();
		// SAFETY: calls into the library loaded above, from the one thread that drives it.
		unsafe {
			(functions.bridge_set_host)(host_callback);
			(functions.bridge_set_time)(0);
			(functions.sim_init)(options.log_level, service.as_ptr());
		}
		firmware.check_callback()?;

		Ok(firmware)
	}

	// SAFETY (all of these): calls into this instance's library, which stays loaded while self exists.

	fn set_time(&self, ns: u64) {
		unsafe { (self.functions.bridge_set_time)(ns) }
	}

	fn mode(&self) -> c_int {
		unsafe { (self.functions.sim_mode)() }
	}

	fn startup_state(&self) -> c_int {
		unsafe { (self.functions.sim_state)() }
	}

	fn diag_tx_idle(&self) -> bool {
		unsafe { (self.functions.port_diag_tx_idle)() }
	}

	fn diag_output_deadline(&self, now: u64, tick: u64) -> u64 {
		unsafe { (self.functions.bridge_diag_output_deadline)(now, tick) }
	}

	fn sim_box(&self, control: c_int, data: c_int, drives: c_int) {
		unsafe { (self.functions.sim_box)(control, data, drives) }
	}

	fn core0(&self) {
		unsafe { (self.functions.sim_core0)() }
	}

	fn tick(&self) {
		unsafe { (self.functions.sim_tick)() }
	}

	fn reboot_pending(&self) -> bool {
		unsafe { (self.functions.bridge_reboot_pending)() != 0 }
	}

	fn event_count(&self) -> u32 {
		unsafe { (self.functions.bridge_event_count)() }
	}

	fn engine_generation(&self) -> u32 {
		unsafe { (self.functions.bridge_engine_generation)() }
	}

	// Writes an ENGINE_CONFIG payload (and advances the generation); its size, or 0 when it doesn't fit.
	// The buffer must hold ENGINE_CONFIG_BYTES; a call with less returns 0 without writing.
	fn engine_config(&self, buffer: &mut [u8]) -> usize {
		unsafe { (self.functions.bridge_engine_config)(buffer.as_mut_ptr(), buffer.len() as u32) as usize }
	}

	// The firmware's current pin state
	fn state(&self) -> [u8; 4] {
		let mut state = [0u8; 4];
		unsafe { (self.functions.bridge_get_state)(state.as_mut_ptr()) };
		state
	}

	// Pin states since the last read into buffer (MAX_EVENTS * 4 bytes): their count, or None after an overflow
	fn read_events_into(&self, buffer: &mut [u8]) -> Option<usize> {
		assert!(buffer.len() >= MAX_EVENTS * PIN_STATE_BYTES);
		let count = unsafe { (self.functions.bridge_read_events)(buffer.as_mut_ptr()) };
		(count >= 0).then_some(count as usize)
	}

	fn read_events(&self) -> Result<Vec<u8>, String> {
		let mut buffer = vec![0u8; MAX_EVENTS * PIN_STATE_BYTES];
		let count = self.read_events_into(&mut buffer).ok_or("the bridge produced too many pin transitions before synchronization")?;
		buffer.truncate(count * PIN_STATE_BYTES);
		Ok(buffer)
	}

	fn last_state(&self) -> [u8; 4] {
		let _ = self.read_events();
		self.state()
	}

	fn seed_events(&self, events: &[u8]) -> Result<(), String> {
		if unsafe { (self.functions.bridge_seed_events)(events.as_ptr(), (events.len() / PIN_STATE_BYTES) as u32) } != 0 {
			return Err("pin state queue overflow".into());
		}
		Ok(())
	}

	fn check_callback(&self) -> Result<(), String> {
		match platform_status() {
			(true, _) => Err("bridge platform callback failed".into()),
			_ => Ok(())
		}
	}
}

impl Drop for Firmware {
	fn drop(&mut self) {
		// The instance's sockets close before the library goes, and its file after.
		PLATFORM.with(|platform| {
			if let Some(mut platform) = platform.borrow_mut().take() {
				platform.flush_log();
			}
		});
		drop(self.library.take());
		let _ = fs::remove_file(&self.image_path);
	}
}

extern "C" fn host_callback(op: c_int, a: isize, b: isize, c: isize) -> isize {
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		PLATFORM.with(|platform| {
			match platform.try_borrow_mut() {
				Ok(mut platform) => match platform.as_mut() {
					Some(platform) => platform.dispatch(op, a, b, c),
					None => -1
				},
				Err(_) => -1
			}
		})
	}));
	result.unwrap_or(-1)
}

////
//// The firmware's platform: settings, TCP, DNS, listening ports and the WiFi list, on the host
////

struct Connection {
	stream: Option<TcpStream>,
	connecting: Option<mpsc::Receiver<io::Result<TcpStream>>>,
	open: bool,
	failed: bool
}

#[repr(C)]
struct AccessPoint {
	ssid: [u8; 33],
	rssi: i8,
	secure: bool
}

struct Platform {
	settings_path: PathBuf,
	http_port: u16,
	console_port: u16,
	ports: Arc<Mutex<HashMap<u16, u16>>>, // firmware port -> the port it got, kept across firmware reboots
	log: LogFn,
	log_line: Vec<u8>,
	stop: Arc<AtomicBool>,
	next_id: isize,
	connections: HashMap<isize, Connection>,
	listeners: HashMap<isize, TcpListener>,
	resolves: HashMap<isize, mpsc::Receiver<Option<Ipv4Addr>>>,
	scan: Option<mpsc::Receiver<Vec<(String, i8, bool)>>>,
	access_points: Vec<(String, i8, bool)>,
	address: String,
	join_status: isize,
	reboot_requested: bool,
	callback_error: bool
}

impl Platform {
	fn new(options: &BridgeOptions, stop: Arc<AtomicBool>, ports: Arc<Mutex<HashMap<u16, u16>>>) -> Platform {
		Platform {
			settings_path: options.state_dir.join("settings.bin"),
			http_port: options.http_port,
			console_port: options.console_port,
			ports,
			log: options.log.clone(),
			log_line: vec![],
			stop,
			next_id: 1,
			connections: HashMap::new(),
			listeners: HashMap::new(),
			resolves: HashMap::new(),
			scan: None,
			access_points: vec![(HOST_UPLINK_NAME.into(), -30, false)],
			address: host_address(),
			join_status: NET_JOIN_IDLE,
			reboot_requested: false,
			callback_error: false
		}
	}

	fn allocate(&mut self) -> isize {
		let id = self.next_id;
		self.next_id += 1;
		id
	}

	fn fail(&mut self, op: c_int, message: &str) -> isize {
		self.callback_error = true;
		(self.log)(&format!("[webtv bridge] platform error, op {}: {}", op, message));
		-1
	}

	fn add_log_text(&mut self, text: &[u8]) {
		self.log_line.extend_from_slice(text);
		while let Some(end) = self.log_line.iter().position(|&b| b == b'\n') {
			let line: Vec<u8> = self.log_line.drain(..=end).collect();
			let line = String::from_utf8_lossy(&line);
			(self.log)(line.trim_end_matches(['\r', '\n']));
		}
	}

	fn flush_log(&mut self) {
		if !self.log_line.is_empty() {
			self.add_log_text(b"\n");
		}
	}

	fn dispatch(&mut self, op: c_int, a: isize, b: isize, c: isize) -> isize {
		// SAFETY (all pointer ops): the firmware passes valid buffers of the sizes it gives (bridge_native.c).
		match op {
			// settings: read (a = buffer, b = size), write (a = data, b = size)
			2 => match fs::read(&self.settings_path) {
				Ok(data) if data.len() == b as usize => {
					unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), a as *mut u8, data.len()) };
					1
				},
				_ => 0
			},
			3 => {
				let data = unsafe { std::slice::from_raw_parts(a as *const u8, b as usize) };
				let temp = self.settings_path.with_extension("tmp");
				let written = private_file(&temp)
					.and_then(|mut file| file.write_all(data))
					.and_then(|_| fs::rename(&temp, &self.settings_path));
				match written {
					Ok(()) => 1,
					Err(error) => self.fail(op, &error.to_string())
				}
			},
			// reboot
			4 => {
				self.reboot_requested = true;
				0
			},
			// TCP: open (a = host name, b = port), state, recv, send, end, close
			5 => {
				let name = unsafe { CStr::from_ptr(a as *const c_char) }.to_string_lossy().to_string();
				let port = b as u16;
				let (sender, receiver) = mpsc::channel();
				thread::spawn(move || {
					let _ = sender.send(connect_ipv4(&name, port));
				});
				let id = self.allocate();
				self.connections.insert(id, Connection { stream: None, connecting: Some(receiver), open: false, failed: false });
				id
			},
			6 => {
				let log = self.log.clone();
				let Some(connection) = self.connections.get_mut(&a) else { return -1 };
				if connection.failed {
					return -1;
				}
				if connection.open {
					return 1;
				}
				match connection.connecting.as_ref().map(|receiver| receiver.try_recv()) {
					Some(Ok(Ok(stream))) => {
						connection.stream = Some(stream);
						connection.connecting = None;
						connection.open = true;
						1
					},
					Some(Ok(Err(error))) => {
						log(&format!("[webtv bridge] tcp: {}", error));
						connection.connecting = None;
						connection.failed = true;
						-1
					},
					Some(Err(mpsc::TryRecvError::Empty)) => 0,
					_ => {
						connection.failed = true;
						-1
					}
				}
			},
			7 => {
				let Some(connection) = self.connections.get_mut(&a) else { return -2 };
				let Some(stream) = connection.stream.as_mut().filter(|_| !connection.failed) else { return -2 };
				let buffer = unsafe { std::slice::from_raw_parts_mut(b as *mut u8, c as usize) };
				match stream.read(buffer) {
					Ok(count) => count as isize,
					Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => -1,
					Err(_) => {
						connection.failed = true;
						-2
					}
				}
			},
			8 => {
				let Some(connection) = self.connections.get_mut(&a) else { return 0 };
				let Some(stream) = connection.stream.as_mut().filter(|_| connection.open && !connection.failed) else { return 0 };
				let data = unsafe { std::slice::from_raw_parts(b as *const u8, c as usize) };
				match stream.write(data) {
					Ok(count) => count as isize,
					Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => 0,
					Err(_) => {
						connection.failed = true;
						0
					}
				}
			},
			9 => {
				if let Some(stream) = self.connections.get(&a).and_then(|connection| connection.stream.as_ref()) {
					let _ = stream.shutdown(Shutdown::Write);
				}
				0
			},
			10 => {
				self.connections.remove(&a);
				0
			},
			// DNS: resolve (a = name, b = DNS server setting), result (a = id, b = 4-byte buffer)
			11 => {
				let name = unsafe { CStr::from_ptr(a as *const c_char) }.to_string_lossy().to_string();
				let server = if b != 0 { unsafe { CStr::from_ptr(b as *const c_char) }.to_string_lossy().to_string() } else { "".into() };
				let (sender, receiver) = mpsc::channel();
				thread::spawn(move || {
					let _ = sender.send(resolve_ipv4(&name, &server, 0).ok());
				});
				let id = self.allocate();
				self.resolves.insert(id, receiver);
				id
			},
			12 => {
				let result = match self.resolves.get(&a).map(|receiver| receiver.try_recv()) {
					Some(Err(mpsc::TryRecvError::Empty)) | None => return -1,
					Some(Ok(result)) => result,
					Some(Err(mpsc::TryRecvError::Disconnected)) => None
				};
				self.resolves.remove(&a);
				match result {
					Some(address) => {
						unsafe { std::ptr::copy_nonoverlapping(address.octets().as_ptr(), b as *mut u8, 4) };
						1
					},
					None => 0
				}
			},
			// listen (a = port), accept (a = listener id)
			13 => self.listen(a as u16),
			14 => {
				let Some(listener) = self.listeners.get(&a) else { return -1 };
				match listener.accept() {
					Ok((stream, _)) => {
						let _ = stream.set_nonblocking(true);
						let _ = stream.set_nodelay(true);
						let id = self.allocate();
						self.connections.insert(id, Connection { stream: Some(stream), connecting: None, open: true, failed: false });
						id
					},
					Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => -1,
					Err(error) => {
						(self.log)(&format!("[webtv bridge] accept: {}", error));
						-1
					}
				}
			},
			// "WiFi": up, scan, scan results (a = list, b = max), address (a = buffer, b = size), join, join status, reconfigure
			15 => !self.address.is_empty() as isize,
			16 => {
				if self.scan.is_none() {
					let (sender, receiver) = mpsc::channel();
					thread::spawn(move || {
						let _ = sender.send(host_networks());
					});
					self.scan = Some(receiver);
				}
				0
			},
			17 => {
				if let Some(receiver) = &self.scan {
					match receiver.try_recv() {
						Err(mpsc::TryRecvError::Empty) => return -1,
						Ok(access_points) => self.access_points = access_points,
						Err(mpsc::TryRecvError::Disconnected) => {}
					}
					self.scan = None;
				}
				let count = (b.max(0) as usize).min(self.access_points.len());
				let out = a as *mut AccessPoint;
				for (index, (ssid, rssi, secure)) in self.access_points.iter().take(count).enumerate() {
					let mut name = [0u8; 33];
					let mut length = ssid.len().min(32);
					while !ssid.is_char_boundary(length) {
						length -= 1;
					}
					name[..length].copy_from_slice(&ssid.as_bytes()[..length]);
					unsafe { out.add(index).write(AccessPoint { ssid: name, rssi: *rssi, secure: *secure }) };
				}
				count as isize
			},
			18 => {
				if b > 0 {
					// The bridge's web page is on the loopback address.
					let address = b"127.0.0.1";
					let length = address.len().min(b as usize - 1);
					unsafe {
						std::ptr::copy_nonoverlapping(address.as_ptr(), a as *mut u8, length);
						*(a as *mut u8).add(length) = 0;
					}
				}
				0
			},
			19 => {
				let ssid = unsafe { CStr::from_ptr(a as *const c_char) }.to_string_lossy().to_string();
				self.join_status = if !ssid.is_empty() && !self.address.is_empty() { NET_JOIN_OK } else { NET_JOIN_FAIL };
				(self.log)(&format!("[webtv bridge] selected {:?}; traffic uses the host's network", ssid));
				0
			},
			20 => self.join_status,
			21 => {
				self.join_status = NET_JOIN_IDLE;
				0
			},
			// platform status: callback failed, reboot requested, interrupted
			22 => (self.callback_error as isize) | ((self.reboot_requested as isize) << 1) | ((self.stop.load(Ordering::SeqCst) as isize) << 2),
			// log text (a = text, b = length)
			30 => {
				let text = unsafe { std::slice::from_raw_parts(a as *const u8, b as usize) }.to_vec();
				self.add_log_text(&text);
				0
			},
			_ => self.fail(op, "unknown platform operation")
		}
	}

	fn listen(&mut self, port: u16) -> isize {
		let wanted = match port {
			FIRMWARE_WEB_PORT => self.http_port,
			FIRMWARE_CONSOLE_PORT => self.console_port,
			_ => port
		};
		let remembered = self.ports.lock().ok().and_then(|ports| ports.get(&port).copied());

		// The port it had before (firmware reboot), the one asked for, or any free one.
		let mut listener = None;
		for candidate in [remembered, Some(wanted), Some(0)].into_iter().flatten() {
			if let Ok(bound) = TcpListener::bind((Ipv4Addr::LOCALHOST, candidate)) {
				listener = Some(bound);
				break;
			}
		}
		let Some(listener) = listener else {
			(self.log)(&format!("[webtv bridge] couldn't listen for port {}", port));
			return -1;
		};
		let _ = listener.set_nonblocking(true);
		let actual = listener.local_addr().map(|address| address.port()).unwrap_or(0);

		if remembered != Some(actual) {
			if let Ok(mut ports) = self.ports.lock() {
				ports.insert(port, actual);
			}
			match port {
				FIRMWARE_WEB_PORT => (self.log)(&format!("[webtv bridge] web page: http://127.0.0.1:{}/", actual)),
				FIRMWARE_CONSOLE_PORT => (self.log)(&format!("[webtv bridge] console: telnet 127.0.0.1 {}", actual)),
				_ => (self.log)(&format!("[webtv bridge] listening on 127.0.0.1:{}", actual))
			}
		}

		let id = self.allocate();
		self.listeners.insert(id, listener);
		id
	}
}

// The host's address on its default route. Without one, the loopback address so a server on this machine still works.
fn host_address() -> String {
	UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
		.and_then(|socket| socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).map(|_| socket)) // no packets are sent
		.and_then(|socket| socket.local_addr())
		.ok()
		.map(|address| address.ip())
		.filter(|address| !address.is_unspecified())
		.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
		.to_string()
}

fn connect_ipv4(name: &str, port: u16) -> io::Result<TcpStream> {
	let address = (name, port).to_socket_addrs()?
		.find(|address| address.is_ipv4())
		.ok_or_else(|| io::Error::new(ErrorKind::NotFound, format!("{}: no IPv4 address", name)))?;
	let stream = TcpStream::connect(address)?;
	stream.set_nonblocking(true)?;
	stream.set_nodelay(true)?;
	Ok(stream)
}

// The settings file, readable only by the user where the OS has such permissions (it holds WiFi passwords).
fn private_file(path: &Path) -> io::Result<fs::File> {
	let mut options = fs::OpenOptions::new();
	options.write(true).create(true).truncate(true);
	#[cfg(unix)]
	std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
	options.open(path)
}

// The networks on the bridge's WiFi screen: the host's uplink, which is what the box really uses, and on Linux
// NetworkManager's cached WiFi list (only names to pick; no connection is changed).
fn host_networks() -> Vec<(String, i8, bool)> {
	let mut networks: HashMap<String, (i8, bool)> = HashMap::new();
	#[cfg(target_os = "linux")]
	if let Ok(output) = std::process::Command::new("nmcli").args(["-t", "--escape", "yes", "-f", "SSID,SIGNAL,SECURITY", "device", "wifi", "list", "--rescan", "no"]).output() {
		if output.status.success() {
			for line in String::from_utf8_lossy(&output.stdout).lines() {
				let fields = split_nmcli(line);
				if fields.len() != 3 || fields[0].is_empty() {
					continue;
				}
				let Ok(signal) = fields[1].parse::<i32>() else { continue };
				let rssi = (signal / 2 - 100).clamp(-128, 127) as i8;
				let secure = !(fields[2].is_empty() || fields[2] == "--");
				if networks.get(&fields[0]).is_none_or(|(known, _)| rssi > *known) {
					networks.insert(fields[0].clone(), (rssi, secure));
				}
			}
		}
	}
	networks.insert(HOST_UPLINK_NAME.into(), (-30, false));

	let mut networks: Vec<(String, i8, bool)> = networks.into_iter().map(|(name, (rssi, secure))| (name, rssi, secure)).collect();
	networks.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
	networks.truncate(32);
	networks
}

#[cfg(target_os = "linux")]
fn split_nmcli(line: &str) -> Vec<String> {
	let mut fields = vec![String::new()];
	let mut escaped = false;
	for character in line.chars() {
		if escaped {
			fields.last_mut().unwrap().push(character);
			escaped = false;
		} else if character == '\\' {
			escaped = true;
		} else if character == ':' {
			fields.push(String::new());
		} else {
			fields.last_mut().unwrap().push(character);
		}
	}
	fields
}

// An IPv4 lookup through the host's resolver, or (DNS over TCP) through the DNS server set in the bridge.
fn resolve_ipv4(name: &str, server: &str, depth: u32) -> io::Result<Ipv4Addr> {
	let invalid = |message: &str| io::Error::new(ErrorKind::InvalidData, message.to_string());

	if let Ok(address) = name.parse::<Ipv4Addr>() {
		return Ok(address);
	}
	if server.is_empty() || server == "auto" {
		return (name, 0).to_socket_addrs()?
			.find_map(|address| match address { SocketAddr::V4(address) => Some(*address.ip()), _ => None })
			.ok_or_else(|| io::Error::new(ErrorKind::NotFound, "no IPv4 address"));
	}
	if depth > 8 {
		return Err(invalid("too many DNS aliases"));
	}

	let ident: u16 = rand::random();
	let mut query = Vec::new();
	for value in [ident, 0x0100, 1, 0, 0, 0] {
		query.extend_from_slice(&value.to_be_bytes());
	}
	for label in name.trim_end_matches('.').split('.') {
		if label.is_empty() || label.len() > 63 {
			return Err(invalid("invalid DNS query name"));
		}
		query.push(label.len() as u8);
		query.extend_from_slice(label.as_bytes());
	}
	query.extend_from_slice(&[0, 0, 1, 0, 1]);

	let address = (server, 53).to_socket_addrs()?.next().ok_or_else(|| invalid("bad DNS server"))?;
	let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(3))?;
	stream.set_read_timeout(Some(Duration::from_secs(3)))?;
	stream.set_write_timeout(Some(Duration::from_secs(3)))?;
	let mut message = (query.len() as u16).to_be_bytes().to_vec();
	message.extend_from_slice(&query);
	stream.write_all(&message)?;
	let mut length = [0u8; 2];
	stream.read_exact(&mut length)?;
	let mut response = vec![0u8; u16::from_be_bytes(length) as usize];
	stream.read_exact(&mut response)?;

	let u16_at = |pos: usize| -> io::Result<u16> {
		response.get(pos..pos + 2).map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]])).ok_or_else(|| invalid("short DNS response"))
	};
	if u16_at(0)? != ident || u16_at(2)? & 0x8000 == 0 || u16_at(2)? & 15 != 0 {
		return Err(io::Error::other("DNS query failed"));
	}
	let questions = u16_at(4)?;
	let answers = u16_at(6)?;
	let mut offset = 12;
	for _ in 0..questions {
		offset = dns_name(&response, offset)?.1 + 4;
	}
	let mut alias = None;
	for _ in 0..answers {
		offset = dns_name(&response, offset)?.1;
		let kind = u16_at(offset)?;
		let class = u16_at(offset + 2)?;
		let size = u16_at(offset + 8)? as usize;
		offset += 10;
		if class == 1 && kind == 1 && size == 4 {
			let bytes = response.get(offset..offset + 4).ok_or_else(|| invalid("short DNS response"))?;
			return Ok(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
		}
		if class == 1 && kind == 5 {
			alias = Some(dns_name(&response, offset)?.0);
		}
		offset += size;
	}
	match alias {
		Some(alias) => resolve_ipv4(&alias, server, depth + 1),
		None => Err(io::Error::new(ErrorKind::NotFound, "DNS response has no IPv4 address"))
	}
}

// A (possibly compressed) DNS name at pos: the name and the offset after it.
fn dns_name(data: &[u8], mut pos: usize) -> io::Result<(String, usize)> {
	let invalid = || io::Error::new(ErrorKind::InvalidData, "invalid DNS name");
	let mut labels = vec![];
	let mut end = None;
	for _ in 0..128 {
		let length = *data.get(pos).ok_or_else(invalid)? as usize;
		if length & 0xc0 == 0xc0 {
			if end.is_none() {
				end = Some(pos + 2);
			}
			pos = ((length & 63) << 8) | *data.get(pos + 1).ok_or_else(invalid)? as usize;
		} else if length == 0 {
			return Ok((labels.join("."), end.unwrap_or(pos + 1)));
		} else if length <= 63 {
			let label = data.get(pos + 1..pos + 1 + length).ok_or_else(invalid)?;
			labels.push(String::from_utf8_lossy(label).to_string());
			pos += 1 + length;
		} else {
			break;
		}
	}
	Err(invalid())
}
