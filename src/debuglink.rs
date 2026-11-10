use std::{
	collections::VecDeque,
	fs,
	io::{self, BufWriter, ErrorKind, Read, Write},
	net::{Ipv4Addr, TcpListener, TcpStream},
	path::PathBuf,
	sync::{
		atomic::{AtomicBool, AtomicU32, Ordering}, mpsc, Arc, Mutex},
	thread::{self, JoinHandle},
	time::{Duration, Instant}
};
use crossbeam_channel::Receiver;

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
const NO_DEADLINE: u64 = u64::MAX;
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(5);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_POLL: Duration = Duration::from_millis(100);

// Local handshake mode, when MAME has it. false runs everything as syncs (pin passthrough).
const USE_LOCAL_HANDSHAKE: bool = true;

const PIN_STATE_BYTES: usize = 4;
const LOCAL_EVENT_BYTES: usize = 24;     // u64 time, 4 host pin bytes, u16 output count, kind, 0, u32 output hash, 4 final pin bytes
const LOCAL_EVENT_KIND_HOST: u8 = 0;
const BATCH_GENERATION_BYTES: usize = 4;
const MAX_BATCH_BYTES: usize = BATCH_GENERATION_BYTES + 2047 * LOCAL_EVENT_BYTES;
const FNV_OFFSET_BASIS: u32 = 2166136261;
const FNV_PRIME: u32 = 16777619;

// Pin states: data, lines (control from the box, status from us), drive, reserved. Lines are levels.
type Pins = [u8; 4];
const PIN_DATA: usize = 0;
const PIN_LINES: usize = 1;
const PIN_DRIVE: usize = 2;

const CTRL_STROBE: u8 = 1 << 0;      // nStrobe (ECP HostClk)
const CTRL_AUTOFD: u8 = 1 << 1;      // nAutoFd (ECP HostAck)
const CTRL_INIT: u8 = 1 << 2;        // nInit (ECP nReverseRequest)
const CTRL_MASK: u8 = 0x0f;

const STAT_ACK: u8 = 1 << 0;         // nAck (ECP PeriphClk)
const STAT_BUSY: u8 = 1 << 1;        // Busy (ECP PeriphAck)
const STAT_PERROR: u8 = 1 << 2;      // PError (ECP nAckReverse)
const STAT_SELECT: u8 = 1 << 3;      // Select (ECP XFlag): low aborts a transfer
const STAT_FAULT: u8 = 1 << 4;       // nFault (ECP nPeriphRequest): high while we send
const STAT_MASK: u8 = 0x1f;

const DRIVE_ALL: u8 = 0xff;
const DRIVE_NONE: u8 = 0x00;

// Console swap
const CONSOLE_READY_BYTE: u8 = 0xa5;
const CHAR_NONE: u8 = 0x00;
const LINES_RELEASED: u8 = 0xff;
const HOST_CHAR_MASK: u8 = 0x7f;
const MAX_TYPED_CHARS: usize = 0x1000;
const CONSOLE_READY_STATUS: u8 = STAT_SELECT | STAT_FAULT;

// SyncWithHost
const SYNC_HOST_BYTE: u8 = 0x69;
const SYNC_BOX_BYTE: u8 = 0x96;
const SYNC_HOST_COUNT: usize = 16 + 1;   // SOLO ECP FIFO depth + 1

// Transfer requests
const REQ_DOWNLOAD: u8 = 0x81;
const REQ_DOWNLOAD_ID: u8 = 0x83;
const REQ_DOWNLOAD_T3: u8 = 0x85;
const REQ_UPLOAD: u8 = 0x88;
const REQUESTS: [u8; 4] = [REQ_DOWNLOAD, REQ_DOWNLOAD_ID, REQ_DOWNLOAD_T3, REQ_UPLOAD];

// Download: type byte (ignored by the box), big-endian 32-bit length, data
const DOWNLOAD_TYPE: u8 = 0x00;
const DOWNLOAD_HEADER_BYTES: usize = 5;
const MAX_DOWNLOAD_BYTES: usize = u32::MAX as usize;

// Upload: big-endian 32-bit name length, name, big-endian 32-bit data length, data
const UPLOAD_LENGTH_BYTES: u64 = 4;
const MAX_UPLOAD_NAME: u32 = 0x400;
const UPLOAD_DEFAULT_NAME: &str = "upload.bin";

// Select stays low this long after an abort, for the box's CheckAbort to see it.
const ABORT_HOLD_NS: u64 = 100_000_000;

// The local engine (lptsocket.h's lpt_handshake_engine)
const ENGINE_FORMAT_VERSION: u8 = 1;
const ENGINE_FIFO_COUNT: usize = 8;
const ENGINE_FIFO_BYTES: usize = 4096;
const ENGINE_RULE_BYTES: usize = 48;
const ENGINE_MAX_ACTIONS: usize = 8;
const TRIGGER_HOST: u8 = 0;
const TRIGGER_FALLBACK: u8 = 1;
const FIFO_BULK: u8 = 0;                 // download bytes (the one in flight at the head), or room for upload bytes
const FIFO_TYPED: u8 = 1;                // typed chars

// Engine states
const E_CON_READY: u8 = 0;
const E_CON_BOX: u8 = 1;
const E_CON_RELEASE_NUL: u8 = 2;         // the box sent NUL: it wants a typed char
const E_CON_RELEASE_CHAR: u8 = 3;
const E_CON_HOST: u8 = 4;
const E_REV_SENT: u8 = 5;                // a byte on the lines, waiting for HostAck high
const E_REV_ACKED: u8 = 6;               // waiting for HostAck low
const E_FWD_IDLE: u8 = 7;                // nStrobe high
const E_FWD_LATCHED: u8 = 8;             // nStrobe low, Busy high

const CONSOLE_FLUSH: Duration = Duration::from_millis(20);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const STALL_REPORT: Duration = Duration::from_secs(3);

pub enum LinkEvent {
	Log(String),
	// Console chars from the box
	Console(Vec<u8>),
	Connected,
	Disconnected,
	// The box waits for a file to download (request byte 0x81, 0x83 or 0x85)
	DownloadRequested { request: u8 },
	DownloadStarted { name: String, total: u64 },
	UploadStarted,
	// The box named its upload: time to ask where it goes
	UploadNamed { name: String },
	Progress { done: u64, total: Option<u64> },
	TransferFinished { message: String }
}

pub enum LinkCommand {
	Download {
		name: String,
		data: Vec<u8>
	},
	UploadDestination(Option<PathBuf>),
	CancelUpload
}

pub type EventFn = Arc<dyn Fn(LinkEvent) + Send + Sync>;

pub fn request_name(request: u8) -> &'static str {
	match request {
		REQ_DOWNLOAD => "dde/pardl",
		REQ_DOWNLOAD_ID => "dde id",
		REQ_DOWNLOAD_T3 => "dde t3",
		REQ_UPLOAD => "ud/udr",
		_ => "unknown"
	}
}

static COMMANDS: Mutex<Option<mpsc::Sender<LinkCommand>>> = Mutex::new(None);

pub fn send_command(command: LinkCommand) -> bool {
	match COMMANDS.lock() {
		Ok(commands) => commands.as_ref().is_some_and(|sender| sender.send(command).is_ok()),
		Err(_) => false
	}
}

pub struct DebugLink {
	port: u16,
	stop: Arc<AtomicBool>,
	thread: Option<JoinHandle<()>>
}

impl DebugLink {
	// keys: what's typed in the launcher's console, as the console sends it
	pub fn start(events: EventFn, keys: Receiver<String>) -> io::Result<DebugLink> {
		let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
		listener.set_nonblocking(true)?;
		let port = listener.local_addr()?.port();

		let (sender, commands) = mpsc::channel();
		if let Ok(mut slot) = COMMANDS.lock() {
			*slot = Some(sender);
		}

		let stop = Arc::new(AtomicBool::new(false));
		let thread_stop = stop.clone();
		let thread = thread::Builder::new()
			.name("debug-link".into())
			.spawn(move || serve(listener, thread_stop, events, keys, commands))?;

		Ok(DebugLink {
			port,
			stop,
			thread: Some(thread)
		})
	}

	pub fn port(&self) -> u16 {
		self.port
	}
}

impl Drop for DebugLink {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
		if let Ok(mut slot) = COMMANDS.lock() {
			*slot = None;
		}
	}
}

struct Shared {
	stop: Arc<AtomicBool>,
	events: EventFn,
	keys: Receiver<String>,
	commands: mpsc::Receiver<LinkCommand>
}

fn serve(listener: TcpListener, stop: Arc<AtomicBool>, events: EventFn, keys: Receiver<String>, commands: mpsc::Receiver<LinkCommand>) {
	let shared = Shared { stop, events, keys, commands };
	let log = |text: String| (shared.events)(LinkEvent::Log(text));
	let mut transfers = Transfers::default();
	let mut typed = VecDeque::new(); // typed before MAME connected

	while !shared.stop.load(Ordering::SeqCst) {
		match listener.accept() {
			Ok((stream, _)) => {
				(shared.events)(LinkEvent::Connected);
				if let Err(error) = run_session(stream, &shared, &mut transfers, &mut typed) {
					log(format!("Debug Link: session ended: {}", error));
				}
				transfers.session_ended(&shared);
				(shared.events)(LinkEvent::Disconnected);
			},
			Err(error) if error.kind() == ErrorKind::WouldBlock => {
				// The console's key channel is bounded: it has to be drained even with nothing to give the keys to.
				while let Ok(text) = shared.keys.try_recv() {
					add_typed(&text, &mut typed);
				}
				transfers.poll(&shared, None);
				thread::sleep(Duration::from_millis(20));
			},
			Err(error) => {
				log(format!("Debug Link: accept failed: {}", error));
				thread::sleep(Duration::from_millis(200));
			}
		}
	}

	transfers.stopped();
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
	// console swap
	ConReady,              // 0xa5 on the data lines, nAck low
	ConBoxChar,            // box strobed: lines released, nAck high, box drives its char
	ConRelease,            // box char latched, nAck low
	ConHostChar,           // host char on the lines, nAck high

	// ECP transfers
	XferWaitFile,          // download asked for, no file yet: nFault low parks the box in SyncWithHost
	XferSyncSend,          // sending 0x69 (nFault high)
	XferSyncEcho,          // nFault low, waiting for the box's 0x96
	XferDownload,          // download: header and data going out in reverse
	XferUploadTurn,        // upload: waiting for the box to turn the link around after sync
	XferUploadReceive,     // upload: nFault low, taking forward bytes
	XferAbort              // Select low until the abort deadline
}

enum Notice {
	DownloadRequested(u8),
	DownloadStarted(u64),
	DownloadDone(u64),
	UploadRequested,
	UploadNamed(String),
	UploadDone(u64),
	UploadRefused(String)
}

struct Link {
	state: State,
	now: u64,
	host: Pins,
	have_host: bool,
	out: Pins,
	emitted: Vec<Pins>,

	box_char: u8,
	is_upload: bool,
	ack_hold: bool,
	finish_on_strobe_high: bool,

	reverse_busy: bool,
	reverse_ack_high: bool,
	reverse: Vec<u8>,
	reverse_pos: usize,
	download: Vec<u8>,

	upload_pos: u64,
	upload_name_length: u32,
	upload_data_length: u32,
	upload_name: Vec<u8>,
	upload_named: bool,
	upload_received: u64,

	abort_until: Option<u64>,
	typed: VecDeque<u8>,

	notices: Vec<Notice>,
	console_out: Vec<u8>,
	upload_data: Vec<u8>
}

impl Link {
	fn new() -> Link {
		let mut link = Link {
			state: State::ConReady,
			now: 0,
			host: [LINES_RELEASED, CTRL_MASK, DRIVE_NONE, 0],
			have_host: false,
			out: [CONSOLE_READY_BYTE, CONSOLE_READY_STATUS, DRIVE_ALL, 0],
			emitted: vec![],
			box_char: CHAR_NONE,
			is_upload: false,
			ack_hold: false,
			finish_on_strobe_high: false,
			reverse_busy: false,
			reverse_ack_high: false,
			reverse: vec![],
			reverse_pos: 0,
			download: vec![],
			upload_pos: 0,
			upload_name_length: 0,
			upload_data_length: 0,
			upload_name: vec![],
			upload_named: false,
			upload_received: 0,
			abort_until: None,
			typed: VecDeque::new(),
			notices: vec![],
			console_out: vec![],
			upload_data: vec![]
		};

		link.emitted.push(link.out);
		link
	}

	// The engine's actions, one for one: a change of our lines is emitted only if it changes something.
	fn emit_if_changed(&mut self, previous: Pins) {
		if self.out != previous {
			self.emitted.push(self.out);
		}
	}

	fn set_data(&mut self, data: u8, drive: u8) {
		let previous = self.out;
		self.out[PIN_DATA] = data;
		self.out[PIN_DRIVE] = drive;
		self.emit_if_changed(previous);
	}

	fn set_status(&mut self, mask: u8, value: u8) {
		let previous = self.out;
		self.out[PIN_LINES] = (self.out[PIN_LINES] & !mask) | (value & mask);
		self.emit_if_changed(previous);
	}

	// A byte on the lines and status lines set, as one change
	fn supply_status(&mut self, data: u8, mask: u8, value: u8) {
		let previous = self.out;
		self.out[PIN_DATA] = data;
		self.out[PIN_DRIVE] = DRIVE_ALL;
		self.out[PIN_LINES] = (self.out[PIN_LINES] & !mask) | (value & mask);
		self.emit_if_changed(previous);
	}

	fn release_data(&mut self) {
		self.set_data(LINES_RELEASED, DRIVE_NONE);
	}

	fn host_line(&self, line: u8) -> bool {
		self.host[PIN_LINES] & line != 0
	}

	fn in_console(&self) -> bool {
		matches!(self.state, State::ConReady | State::ConBoxChar | State::ConRelease | State::ConHostChar)
	}

	fn in_transfer(&self) -> bool {
		matches!(self.state, State::XferWaitFile | State::XferSyncSend | State::XferSyncEcho | State::XferDownload | State::XferUploadTurn | State::XferUploadReceive)
	}

	fn deadline(&self) -> u64 {
		self.abort_until.unwrap_or(NO_DEADLINE)
	}

	// Our own timer: the end of an abort.
	fn advance(&mut self, now: u64) {
		self.now = now;
		if self.abort_until.is_some_and(|until| now >= until) {
			self.abort_until = None;
			self.console_ready();
		}
	}

	fn host_update(&mut self, pins: Pins, now: u64) {
		self.now = now;
		let previous = self.host;
		self.host = pins;

		if !self.have_host {
			self.have_host = true;
		} else {
			let changed = (previous[PIN_LINES] ^ pins[PIN_LINES]) & CTRL_MASK;
			if changed & CTRL_INIT != 0 {
				self.input_init(self.host_line(CTRL_INIT));
			}
			if changed & CTRL_AUTOFD != 0 {
				self.input_autofd(self.host_line(CTRL_AUTOFD));
			}
			if changed & CTRL_STROBE != 0 {
				let high = self.host_line(CTRL_STROBE);
				if self.in_console() {
					self.console_strobe(!high, high);
				} else {
					self.forward_strobe(!high, high);
				}
			}
		}
	}

	fn console_ready(&mut self) {
		self.state = State::ConReady;
		self.reverse_busy = false;
		self.finish_on_strobe_high = false;
		self.ack_hold = false;

		self.set_data(CONSOLE_READY_BYTE, DRIVE_ALL);
		self.set_status(STAT_MASK, CONSOLE_READY_STATUS);
	}

	fn console_strobe(&mut self, fell: bool, rose: bool) {
		if fell && self.state == State::ConReady {
			self.state = State::ConBoxChar;
			self.release_data();
			self.set_status(STAT_ACK, STAT_ACK);
		} else if rose && self.state == State::ConBoxChar {
			self.box_char = self.host[PIN_DATA];
			self.state = State::ConRelease;
			self.set_status(STAT_ACK, 0);
		} else if fell && self.state == State::ConRelease {
			// Typed chars go only in swaps where the box sent NUL: it keeps one from an output swap only until the next swap.
			let host_char = if self.box_char == CHAR_NONE { self.typed.pop_front().unwrap_or(CHAR_NONE) } else { CHAR_NONE };
			self.state = State::ConHostChar;
			self.set_data(host_char, DRIVE_ALL);
			self.set_status(STAT_ACK, STAT_ACK);
		} else if rose {
			self.console_swap_done();
		}
	}

	fn console_swap_done(&mut self) {
		let box_char = self.box_char;
		let complete = self.state == State::ConHostChar || self.state == State::ConRelease;
		self.box_char = CHAR_NONE;

		if complete {
			if REQUESTS.contains(&box_char) {
				self.start_transfer(box_char);
				return;
			}
			// NUL: the box only wanted input. 0xff: nothing was driven.
			if box_char != CHAR_NONE && box_char != LINES_RELEASED {
				self.console_out.push(box_char);
			}
		}

		self.console_ready();
	}

	fn transfer_lines(&mut self, fault: bool) {
		self.release_data();
		let perror = if self.host_line(CTRL_INIT) { STAT_PERROR } else { 0 };
		self.set_status(STAT_BUSY | STAT_SELECT | STAT_PERROR | STAT_FAULT, STAT_SELECT | perror | if fault { STAT_FAULT } else { 0 });
	}

	fn start_transfer(&mut self, request: u8) {
		self.is_upload = request == REQ_UPLOAD;

		self.reverse = vec![SYNC_HOST_BYTE; SYNC_HOST_COUNT];
		self.reverse_pos = 0;
		self.reverse_busy = false;
		self.reverse_ack_high = false;
		self.download.clear();

		self.ack_hold = true;
		self.set_status(STAT_ACK, 0);

		if self.is_upload {
			self.state = State::XferSyncSend;
			self.transfer_lines(true);
			self.reverse_kick();
			self.notices.push(Notice::UploadRequested);
		} else {
			// nFault low keeps the box in SyncWithHost until there's a file
			self.state = State::XferWaitFile;
			self.transfer_lines(false);
			self.notices.push(Notice::DownloadRequested(request));
		}
	}

	// The file for the download the box waits for. false if it isn't waiting.
	fn provide_download(&mut self, data: Vec<u8>) -> bool {
		if self.state != State::XferWaitFile {
			return false;
		}

		self.download = data;
		self.state = State::XferSyncSend;
		self.set_status(STAT_FAULT, STAT_FAULT);
		self.reverse_kick();
		true
	}

	fn abort_transfer(&mut self) {
		self.state = State::XferAbort;
		self.reverse_busy = false;
		self.set_status(STAT_ACK | STAT_SELECT, STAT_ACK);
		self.abort_until = Some(self.now.saturating_add(ABORT_HOLD_NS));
	}

	fn cancel_upload(&mut self) -> bool {
		if !self.is_upload || !self.in_transfer() {
			return false;
		} else {
			self.abort_transfer();

			return true;
		}
	}

	fn finish_transfer(&mut self) {
		self.reverse.clear();
		self.reverse_pos = 0;
		self.download.clear();
		self.upload_name.clear();
		self.console_ready();
	}

	fn input_init(&mut self, high: bool) {
		if self.in_transfer() {
			self.set_status(STAT_PERROR, if high { STAT_PERROR } else { 0 });

			if self.state == State::XferUploadTurn && !high {
				self.state = State::XferUploadReceive;
				self.set_status(STAT_FAULT, 0);
			} else {
				self.reverse_kick();
			}
		}
	}

	fn input_autofd(&mut self, host_ack: bool) {
		if self.ack_hold && !self.host_line(CTRL_INIT) {
			if host_ack {
				self.ack_hold = false;
				self.set_status(STAT_ACK, STAT_ACK);
			}
		} else if !self.reverse_busy || !(self.reverse_handshake(host_ack)) {
			self.reverse_kick();
		}
	}

	fn reverse_handshake(&mut self, host_ack: bool) -> bool {
		if !self.reverse_ack_high && host_ack {
			self.reverse_ack_high = true;
			self.set_status(STAT_ACK, STAT_ACK);

			return true;
		} else if self.reverse_ack_high && !host_ack {
			self.reverse_busy = false;
			self.reverse_pos += 1;
			self.set_status(STAT_BUSY, 0);

			return self.reverse_byte_done();
		} else {
			return false;
		}
	}

	fn reverse_byte_done(&mut self) -> bool {
		if self.reverse_pos < self.reverse.len() {
			return false;
		} else {
			match self.state {
				State::XferSyncSend => {
					self.state = State::XferSyncEcho;
					self.set_status(STAT_FAULT, 0);

					true
				},
				State::XferDownload => {
					let size = (self.reverse.len() - DOWNLOAD_HEADER_BYTES) as u64;
					self.finish_transfer();
					self.notices.push(Notice::DownloadDone(size));

					true
				},
				_ => false
			}
		}
	}

	// EMAC

	fn reverse_kick(&mut self) {
		let sending = self.state == State::XferSyncSend || self.state == State::XferDownload;
		if !sending || self.reverse_busy || self.ack_hold {
			return;
		}

		// The box must be in ECP reverse (nReverseRequest low) and ready (HostAck low).
		let box_ready = !self.host_line(CTRL_INIT) && !self.host_line(CTRL_AUTOFD);
		if !box_ready || self.reverse_pos >= self.reverse.len() {
			return;
		}

		self.reverse_busy = true;
		self.reverse_ack_high = false;

		// Busy high flags a data byte; nAck low clocks it.
		self.supply_status(self.reverse[self.reverse_pos], STAT_BUSY | STAT_ACK, STAT_BUSY);
	}

	// ECP forward (box -> us): HostClk low, we raise PeriphAck; HostClk high, we drop it.
	fn forward_strobe(&mut self, fell: bool, rose: bool) {
		if !self.host_line(CTRL_INIT) {
			return;
		}

		if fell {
			let command = !self.host_line(CTRL_AUTOFD);
			self.set_status(STAT_BUSY, STAT_BUSY);
			self.forward_byte(self.host[PIN_DATA], command);
		} else if rose {
			self.set_status(STAT_BUSY, 0);
			if self.finish_on_strobe_high {
				self.finish_on_strobe_high = false;
				self.finish_transfer();
			}
		}
	}

	fn forward_byte(&mut self, data: u8, command: bool) {
		// ECP command bytes carry nothing for this link.
		if command {
			return;
		}

		match self.state {
			State::XferSyncEcho if data == SYNC_BOX_BYTE => self.sync_done(),
			State::XferUploadReceive => self.upload_byte(data),
			_ => {}
		}
	}

	fn sync_done(&mut self) {
		if self.is_upload {
			self.state = State::XferUploadTurn;
			self.upload_pos = 0;
			self.upload_name_length = 0;
			self.upload_data_length = 0;
			self.upload_name.clear();
			self.upload_named = false;
			self.upload_received = 0;
		} else {
			// One type byte (the box ignores it), the length, the data.
			let data = std::mem::take(&mut self.download);
			let size = data.len() as u32;
			let mut reverse = Vec::with_capacity(data.len() + DOWNLOAD_HEADER_BYTES);
			reverse.push(DOWNLOAD_TYPE);
			reverse.extend_from_slice(&size.to_be_bytes());
			reverse.extend_from_slice(&data);
			self.reverse = reverse;
			self.reverse_pos = 0;
			self.state = State::XferDownload;
			self.notices.push(Notice::DownloadStarted(size as u64));
		}

		// nFault high: the box waits for it before it turns the link around.
		self.set_status(STAT_FAULT, STAT_FAULT);
	}

	fn upload_data_start(&self) -> u64 {
		UPLOAD_LENGTH_BYTES * 2 + self.upload_name_length as u64
	}

	fn upload_byte(&mut self, data: u8) {
		let pos = self.upload_pos;
		self.upload_pos += 1;
		let name_start = UPLOAD_LENGTH_BYTES;
		let data_length_start = name_start + self.upload_name_length as u64;
		let data_start = data_length_start + UPLOAD_LENGTH_BYTES;

		if pos < name_start {
			self.upload_name_length = (self.upload_name_length << 8) | data as u32;
			if pos == name_start - 1 && self.upload_name_length > MAX_UPLOAD_NAME {
				self.notices.push(Notice::UploadRefused(format!("Upload refused: the box sent a {}-byte name", self.upload_name_length)));
				self.abort_transfer();
				return;
			}
		} else if pos < data_length_start {
			// The name ends with a NUL (its length is strlen + 1); keep it up to the first one.
			let index = pos - name_start;
			if data != CHAR_NONE && self.upload_name.len() as u64 == index {
				self.upload_name.push(data);
			}
		} else if pos < data_start {
			self.upload_data_length = (self.upload_data_length << 8) | data as u32;
		} else {
			self.upload_data.push(data);
			self.upload_received += 1;
		}

		// The name length is only known once its 4 bytes are in.
		if !self.upload_named && self.upload_pos >= name_start && self.upload_pos >= name_start + self.upload_name_length as u64 {
			self.upload_named = true;
			let name = safe_file_name(&String::from_utf8_lossy(&self.upload_name), UPLOAD_DEFAULT_NAME);
			self.notices.push(Notice::UploadNamed(name));
		}

		let header_done = self.upload_pos >= name_start && self.upload_pos >= self.upload_data_start();
		if header_done && self.upload_received >= self.upload_data_length as u64 {
			self.notices.push(Notice::UploadDone(self.upload_received));

			// Busy is still up for this byte; the console comes back when nStrobe rises.
			self.finish_on_strobe_high = true;
		}
	}

	// Upload bytes that only get parsed: the ones before the next byte that can change our lines (the last name length
	// byte can abort, the last byte of the header or of the data can finish).
	fn upload_plain_bytes(&self) -> u64 {
		let next = self.upload_pos;
		let special = if next < UPLOAD_LENGTH_BYTES {
			UPLOAD_LENGTH_BYTES - 1
		} else if next < self.upload_data_start() {
			self.upload_data_start() - 1
		} else {
			self.upload_data_start() + self.upload_data_length as u64 - 1
		};
		special.saturating_sub(next)
	}

	// Where the link is, for the log
	fn describe(&self) -> String {
		let line = |pins: &Pins, bit: u8| if pins[PIN_LINES] & bit != 0 { '1' } else { '0' };
		format!("{:?}: box nStrobe/nAutoFd/nInit/nSelectIn {}{}{}{} data {:02x}{}, ours nAck/Busy/PError/Select/nFault {}{}{}{}{} data {:02x}{}, reverse {}/{}{}{}{}, upload {}",
			self.state,
			line(&self.host, CTRL_STROBE), line(&self.host, CTRL_AUTOFD), line(&self.host, CTRL_INIT), line(&self.host, 1 << 3),
			self.host[PIN_DATA], if self.host[PIN_DRIVE] == DRIVE_NONE { " (released)" } else { "" },
			line(&self.out, STAT_ACK), line(&self.out, STAT_BUSY), line(&self.out, STAT_PERROR), line(&self.out, STAT_SELECT), line(&self.out, STAT_FAULT),
			self.out[PIN_DATA], if self.out[PIN_DRIVE] == DRIVE_NONE { " (released)" } else { "" },
			self.reverse_pos, self.reverse.len(),
			if self.reverse_busy { " busy" } else { "" }, if self.reverse_ack_high { " acked" } else { "" }, if self.ack_hold { " ack-hold" } else { "" },
			self.upload_pos)
	}

	fn progress(&self) -> Option<(u64, Option<u64>)> {
		match self.state {
			State::XferDownload => Some((
				self.reverse_pos.saturating_sub(DOWNLOAD_HEADER_BYTES) as u64,
				Some(self.reverse.len().saturating_sub(DOWNLOAD_HEADER_BYTES) as u64)
			)),
			State::XferUploadReceive | State::XferUploadTurn => {
				let header_done = self.upload_pos >= UPLOAD_LENGTH_BYTES && self.upload_pos >= self.upload_data_start();
				Some((self.upload_received, if header_done { Some(self.upload_data_length as u64) } else { None }))
			},
			_ => None
		}
	}

	////
	//// Engine rules for the loop Link is in. None when it's in none of them: everything is a sync then.
	////

	fn engine_config(&self, generation: u32) -> Option<Vec<u8>> {
		let mut rules = Rules::default();
		let mut fifos: [(Vec<u8>, usize); ENGINE_FIFO_COUNT] = Default::default();

		let control = self.host[PIN_LINES] & CTRL_MASK;
		let state = if self.in_console() {
			self.console_rules(&mut rules, &mut fifos, control)?
		} else if self.state == State::XferSyncSend || self.state == State::XferDownload {
			self.reverse_rules(&mut rules, &mut fifos, control)?
		} else if self.state == State::XferUploadReceive {
			self.forward_rules(&mut rules, &mut fifos, control)?
		} else {
			return None;
		};

		let mut config = Vec::with_capacity(8 + rules.bytes.len() + ENGINE_FIFO_COUNT * 4 + ENGINE_FIFO_BYTES * 2);
		config.push(ENGINE_FORMAT_VERSION);
		config.push(state);
		config.extend_from_slice(&rules.count.to_be_bytes());
		config.extend_from_slice(&generation.to_be_bytes());
		config.extend_from_slice(&rules.bytes);
		for (data, capacity) in fifos.iter() {
			config.extend_from_slice(&(data.len() as u16).to_be_bytes());
			config.extend_from_slice(&(*capacity as u16).to_be_bytes());
			config.extend_from_slice(data);
		}
		Some(config)
	}

	// The console swap. Box chars come from the replayed host states; FIFO_TYPED holds the typed chars.
	fn console_rules(&self, rules: &mut Rules, fifos: &mut [(Vec<u8>, usize); ENGINE_FIFO_COUNT], control: u8) -> Option<u8> {
		// A request byte already latched ends the swap in a transfer: that's a sync.
		let request_pending = matches!(self.state, State::ConRelease | State::ConHostChar) && REQUESTS.contains(&self.box_char);
		if request_pending || self.out[PIN_LINES] & !STAT_ACK != CONSOLE_READY_STATUS {
			return None;
		}

		let (state, strobe_high) = match self.state {
			State::ConReady => (E_CON_READY, true),
			State::ConBoxChar => (E_CON_BOX, false),
			State::ConRelease => (if self.box_char == CHAR_NONE { E_CON_RELEASE_NUL } else { E_CON_RELEASE_CHAR }, true),
			_ => (E_CON_HOST, false)
		};
		if (control & CTRL_STROBE != 0) != strobe_high {
			return None;
		}

		let high = (control & !CTRL_STROBE) | CTRL_STROBE;
		let low = control & !CTRL_STROBE;
		let typed_ack = [action_supply(FIFO_TYPED, DRIVE_ALL), action_set_status(STAT_ACK, STAT_ACK), action_pop(FIFO_TYPED)];
		let none_ack = [action_set_data(CHAR_NONE, DRIVE_ALL), action_set_status(STAT_ACK, STAT_ACK)];

		rules.add(Rule::host(E_CON_READY, E_CON_BOX, low).fell(CTRL_STROBE), &[action_set_data(LINES_RELEASED, DRIVE_NONE), action_set_status(STAT_ACK, STAT_ACK)]);
		rules.add(Rule::host(E_CON_READY, E_CON_READY, high), &[]);

		rules.add(Rule::host(E_CON_BOX, E_CON_RELEASE_NUL, high).rose(CTRL_STROBE).data(CHAR_NONE), &[action_set_status(STAT_ACK, 0)]);
		for request in REQUESTS {
			rules.add(Rule::fallback(E_CON_BOX, high).rose(CTRL_STROBE).data(request), &[]);
		}
		rules.add(Rule::host(E_CON_BOX, E_CON_RELEASE_CHAR, high).rose(CTRL_STROBE), &[action_set_status(STAT_ACK, 0)]);
		rules.add(Rule::host(E_CON_BOX, E_CON_BOX, low), &[]);

		rules.add(Rule::host(E_CON_RELEASE_NUL, E_CON_HOST, low).fell(CTRL_STROBE).nonempty(FIFO_TYPED), &typed_ack);
		rules.add(Rule::host(E_CON_RELEASE_NUL, E_CON_HOST, low).fell(CTRL_STROBE).empty(FIFO_TYPED), &none_ack);
		rules.add(Rule::host(E_CON_RELEASE_NUL, E_CON_RELEASE_NUL, high), &[]);
		rules.add(Rule::host(E_CON_RELEASE_CHAR, E_CON_HOST, low).fell(CTRL_STROBE), &none_ack);
		rules.add(Rule::host(E_CON_RELEASE_CHAR, E_CON_RELEASE_CHAR, high), &[]);

		rules.add(Rule::host(E_CON_HOST, E_CON_READY, high).rose(CTRL_STROBE), &[action_set_data(CONSOLE_READY_BYTE, DRIVE_ALL), action_set_status(STAT_MASK, CONSOLE_READY_STATUS)]);
		rules.add(Rule::host(E_CON_HOST, E_CON_HOST, low), &[]);

		let typed: Vec<u8> = self.typed.iter().take(ENGINE_FIFO_BYTES).copied().collect();
		let used = typed.len();
		fifos[FIFO_TYPED as usize] = (typed, used);

		Some(state)
	}

	// Reverse bytes (the 0x69s and the download). FIFO_BULK has the byte in flight at its head. The last byte in it
	// can't be followed by another, so its HostAck low is a sync: that's where the FIFO gets refilled.
	fn reverse_rules(&self, rules: &mut Rules, fifos: &mut [(Vec<u8>, usize); ENGINE_FIFO_COUNT], control: u8) -> Option<u8> {
		let remaining = self.reverse.len().saturating_sub(self.reverse_pos);
		if !self.reverse_busy || self.ack_hold || remaining < 2 || control & CTRL_INIT != 0 {
			return None;
		}

		let (state, autofd_high) = if self.reverse_ack_high { (E_REV_ACKED, true) } else { (E_REV_SENT, false) };
		if (control & CTRL_AUTOFD != 0) != autofd_high {
			return None;
		}

		let high = control | CTRL_AUTOFD;
		let low = control & !CTRL_AUTOFD;
		let drive = self.host[PIN_DRIVE];

		rules.add(Rule::host(E_REV_SENT, E_REV_ACKED, high).drive(drive).rose(CTRL_AUTOFD), &[action_set_status(STAT_ACK, STAT_ACK)]);
		rules.add(Rule::host(E_REV_SENT, E_REV_SENT, low).drive(drive), &[]);
		rules.add(Rule::host(E_REV_ACKED, E_REV_SENT, low).drive(drive).fell(CTRL_AUTOFD),
			&[action_pop(FIFO_BULK), action_set_status(STAT_BUSY, 0), action_supply_status(FIFO_BULK, STAT_BUSY | STAT_ACK, STAT_BUSY)]);
		rules.add(Rule::host(E_REV_ACKED, E_REV_ACKED, high).drive(drive), &[]);

		let bytes = self.reverse[self.reverse_pos..self.reverse_pos + remaining.min(ENGINE_FIFO_BYTES)].to_vec();
		let used = bytes.len();
		fifos[FIFO_BULK as usize] = (bytes, used);

		Some(state)
	}

	// Upload bytes. FIFO_BULK's capacity is the bytes that only get parsed, so the next one is a sync.
	fn forward_rules(&self, rules: &mut Rules, fifos: &mut [(Vec<u8>, usize); ENGINE_FIFO_COUNT], control: u8) -> Option<u8> {
		let plain = self.upload_plain_bytes().min(ENGINE_FIFO_BYTES as u64) as usize;
		if self.finish_on_strobe_high || plain == 0 || control & CTRL_INIT == 0 || control & CTRL_AUTOFD == 0 {
			return None;
		}

		let strobe_high = control & CTRL_STROBE != 0;
		let busy = self.out[PIN_LINES] & STAT_BUSY != 0;
		if busy == strobe_high {
			return None;
		}
		let state = if strobe_high { E_FWD_IDLE } else { E_FWD_LATCHED };

		let high = control | CTRL_STROBE;
		let low = control & !CTRL_STROBE;
		let drive = self.host[PIN_DRIVE];

		rules.add(Rule::host(E_FWD_IDLE, E_FWD_LATCHED, low).drive(drive).fell(CTRL_STROBE), &[action_set_status(STAT_BUSY, STAT_BUSY), action_push(FIFO_BULK)]);
		rules.add(Rule::host(E_FWD_IDLE, E_FWD_IDLE, high).drive(drive), &[]);
		rules.add(Rule::host(E_FWD_LATCHED, E_FWD_IDLE, high).drive(drive).rose(CTRL_STROBE), &[action_set_status(STAT_BUSY, 0)]);
		rules.add(Rule::host(E_FWD_LATCHED, E_FWD_LATCHED, low).drive(drive), &[]);

		fifos[FIFO_BULK as usize] = (vec![], plain);

		Some(state)
	}
}

// The box's file name, minus any path, in characters every host takes.
fn safe_file_name(box_name: &str, fallback: &str) -> String {
	let name = box_name.rsplit(['/', '\\', ':']).next().unwrap_or("");
	let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' }).collect();
	if name.is_empty() || name == "." || name == ".." {
		fallback.into()
	} else {
		name
	}
}


#[derive(Default)]
struct Rules {
	bytes: Vec<u8>,
	count: u16
}

struct Rule {
	state: u8,
	next: u8,
	control: u8,
	data: Option<u8>,
	drive: Option<u8>,
	rose: u8,
	fell: u8,
	nonempty: u8,
	empty: u8,
	trigger: u8
}

impl Rule {
	fn host(state: u8, next: u8, control: u8) -> Rule {
		Rule { state, next, control, data: None, drive: None, rose: 0, fell: 0, nonempty: 0, empty: 0, trigger: TRIGGER_HOST }
	}

	// Hands the update to the endpoint
	fn fallback(state: u8, control: u8) -> Rule {
		Rule { trigger: TRIGGER_FALLBACK, ..Rule::host(state, state, control) }
	}

	fn rose(self, lines: u8) -> Rule { Rule { rose: lines, ..self } }
	fn fell(self, lines: u8) -> Rule { Rule { fell: lines, ..self } }
	fn data(self, data: u8) -> Rule { Rule { data: Some(data), ..self } }
	fn drive(self, drive: u8) -> Rule { Rule { drive: Some(drive), ..self } }
	fn nonempty(self, fifo: u8) -> Rule { Rule { nonempty: self.nonempty | (1 << fifo), ..self } }
	fn empty(self, fifo: u8) -> Rule { Rule { empty: self.empty | (1 << fifo), ..self } }
}

impl Rules {
	fn add(&mut self, rule: Rule, actions: &[[u8; 4]]) {
		debug_assert!(actions.len() <= ENGINE_MAX_ACTIONS);
		let mut bytes = [0u8; ENGINE_RULE_BYTES];
		bytes[0] = rule.state;
		bytes[1] = rule.next;
		bytes[2] = CTRL_MASK;
		bytes[3] = rule.control & CTRL_MASK;
		if let Some(data) = rule.data {
			bytes[4] = 0xff;
			bytes[5] = data;
		}
		if let Some(drive) = rule.drive {
			bytes[6] = 0xff;
			bytes[7] = drive;
		}
		bytes[8] = rule.rose;
		bytes[9] = rule.fell;
		bytes[10] = rule.nonempty;
		bytes[11] = rule.empty;
		bytes[12] = actions.len() as u8;
		bytes[13] = rule.trigger;
		for (index, action) in actions.iter().enumerate() {
			bytes[16 + index * 4..20 + index * 4].copy_from_slice(action);
		}
		self.bytes.extend_from_slice(&bytes);
		self.count += 1;
	}
}

fn action_set_data(data: u8, drive: u8) -> [u8; 4] {
	[1, data, drive, 0]
}

fn action_set_status(mask: u8, value: u8) -> [u8; 4] {
	[2, mask, value & mask, 0]
}

fn action_supply(fifo: u8, drive: u8) -> [u8; 4] {
	[3, fifo, drive, 0]
}

fn action_pop(fifo: u8) -> [u8; 4] {
	[4, fifo, 0, 0]
}

fn action_push(fifo: u8) -> [u8; 4] {
	[5, fifo, 0, 0]
}

fn action_supply_status(fifo: u8, mask: u8, value: u8) -> [u8; 4] {
	[7, fifo, mask, value & mask]
}

struct Upload {
	temp: PathBuf,
	file: Option<BufWriter<fs::File>>,
	name: String,
	finished: bool,
	size: u64,
	destination: Option<PathBuf>
}

#[derive(Default)]
struct Transfers {
	download: Option<(String, Vec<u8>)>,   // picked, for the next exchange
	download_name: String,
	upload: Option<Upload>,
	cancel_upload: bool                    // for the next exchange
}

static UPLOAD_COUNTER: AtomicU32 = AtomicU32::new(0);

impl Transfers {
	fn poll(&mut self, shared: &Shared, mut link: Option<&mut Link>) {

		while let Ok(command) = shared.commands.try_recv() {
			match command {
				LinkCommand::Download { name, data } => {
					if data.len() > MAX_DOWNLOAD_BYTES {
						(shared.events)(LinkEvent::Log(format!("Debug Link: {} is too big to download ({} bytes)", name, data.len())));
					} else {
						self.download = Some((name, data));
					}
				},
				LinkCommand::UploadDestination(Some(path)) => {
					if let Some(upload) = self.upload.as_mut() {
						upload.destination = Some(path);
					}
					self.finalize(shared);
				},
				LinkCommand::UploadDestination(None) | LinkCommand::CancelUpload => {
					match self.upload.as_ref() {
						Some(upload) if upload.finished => {
							let label = self.upload_label();
							self.discard();
							(shared.events)(LinkEvent::TransferFinished { message: format!("{} discarded", label) });
						},
						Some(_) => self.cancel_upload = true,
						None => {}
					}
				}
			}
		}

		if let Some(link) = link.as_deref_mut() {
			if let Some((name, data)) = self.download.take() {
				if link.provide_download(data) {
					self.download_name = name;
				} else {
					(shared.events)(LinkEvent::Log("Debug Link: the box isn't waiting for a download any more".into()));
				}
			}

			if self.cancel_upload {
				self.cancel_upload = false;
				if link.cancel_upload() {
					let label = self.upload_label();
					self.discard();
					(shared.events)(LinkEvent::TransferFinished { message: format!("{} cancelled", label) });
				}
			}
		}
	}

	fn take_notices(&mut self, shared: &Shared, link: &mut Link) {
		if !link.upload_data.is_empty() {
			let mut failed = None;
			if let Some(upload) = self.upload.as_mut() {
				if let Some(file) = upload.file.as_mut() {
					if let Err(error) = file.write_all(&link.upload_data) {
						failed = Some(error.to_string());
					}
				}
			}
			link.upload_data.clear();
			if let Some(error) = failed {
				(shared.events)(LinkEvent::Log(format!("Debug Link: couldn't write the upload: {}", error)));
				if let Some(upload) = self.upload.as_mut() {
					upload.file = None;
				}
				self.cancel_upload = true;
			}
		}

		for notice in std::mem::take(&mut link.notices) {
			match notice {
				Notice::DownloadRequested(request) => {
					self.download = None;

					(shared.events)(LinkEvent::DownloadRequested { request });
				},
				Notice::DownloadStarted(total) => {
					(shared.events)(LinkEvent::DownloadStarted { name: self.download_name.clone(), total });
				},
				Notice::DownloadDone(size) => {
					(shared.events)(LinkEvent::TransferFinished { message: format!("Downloaded {} to the box ({} bytes)", self.download_name, size) });
				},
				Notice::UploadRequested => {
					self.discard();
					self.cancel_upload = false;
					let temp = std::env::temp_dir().join(format!("wtv-debuglink-{}-{}.part", std::process::id(), UPLOAD_COUNTER.fetch_add(1, Ordering::SeqCst)));

					let file = match fs::File::create(&temp) {
						Ok(file) => Some(BufWriter::new(file)),
						Err(error) => {
							(shared.events)(LinkEvent::Log(format!("Debug Link: couldn't create {}: {}", temp.display(), error)));
							self.cancel_upload = true;
							None
						}
					};
					self.upload = Some(Upload { temp, file, name: "".into(), finished: false, size: 0, destination: None });

					(shared.events)(LinkEvent::UploadStarted);
				},
				Notice::UploadNamed(name) => {
					if let Some(upload) = self.upload.as_mut() {
						upload.name = name.clone();
					}

					(shared.events)(LinkEvent::UploadNamed { name });
				},
				Notice::UploadDone(size) => {
					let mut failed = None;
					if let Some(upload) = self.upload.as_mut() {
						if let Some(mut file) = upload.file.take() {
							if let Err(error) = file.flush() {
								failed = Some(error.to_string());
							}
						}

						upload.finished = true;
						upload.size = size;
					}
					match failed {
						Some(error) => {
							self.discard();
							(shared.events)(LinkEvent::TransferFinished { message: format!("Couldn't write the upload: {}", error) });
						},
						None => self.finalize(shared)
					}
				},
				Notice::UploadRefused(message) => {
					self.discard();

					(shared.events)(LinkEvent::TransferFinished { message });
				}
			}
		}
	}

	fn finalize(&mut self, shared: &Shared) {
		let ready = self.upload.as_ref().is_some_and(|upload| upload.finished && upload.destination.is_some());
		if ready {
			let upload = self.upload.take().unwrap();
			let destination = upload.destination.clone().unwrap();
			let moved = fs::rename(&upload.temp, &destination)
				.or_else(|_| fs::copy(&upload.temp, &destination).map(|_| ()));
			let _ = fs::remove_file(&upload.temp);

			let message = match moved {
				Ok(()) => format!("Saved upload {} to {} ({} bytes)", upload.name, destination.display(), upload.size),
				Err(error) => format!("Couldn't save upload {} to {}: {}", upload.name, destination.display(), error)
			};
			(shared.events)(LinkEvent::TransferFinished { message });
		}
	}

	fn upload_label(&self) -> String {
		match self.upload.as_ref() {
			Some(upload) if !upload.name.is_empty() => format!("Upload {}", upload.name),
			_ => "Upload".into()
		}
	}

	fn discard(&mut self) {
		if let Some(mut upload) = self.upload.take() {
			upload.file = None;
			let _ = fs::remove_file(&upload.temp);
		}
	}

	fn session_ended(&mut self, shared: &Shared) {
		self.download = None;
		self.cancel_upload = false;

		if self.upload.as_ref().is_some_and(|upload| !upload.finished) {
			let label = self.upload_label();
			self.discard();
			(shared.events)(LinkEvent::TransferFinished { message: format!("{} interrupted: MAME disconnected", label) });
		}
	}

	fn stopped(&mut self) {
		self.discard();
	}
}


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

fn read_exact_until(stream: &mut TcpStream, buffer: &mut [u8], stop: &AtomicBool, deadline: Option<Instant>) -> io::Result<()> {
	let mut filled = 0;
	while filled < buffer.len() {
		if stop.load(Ordering::SeqCst) {
			return Err(io::Error::new(ErrorKind::Interrupted, "stopped"));
		} else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
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

// HELLO, then we pick the mode and wait for MAME's ACK/NAK before any pins.
fn negotiate(stream: &mut TcpStream, stop: &AtomicBool) -> Result<u8, String> {
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

	let mode = if USE_LOCAL_HANDSHAKE && capabilities & CAP_LOCAL_HANDSHAKE != 0 { MODE_LOCAL_HANDSHAKE } else { MODE_PIN_PASSTHROUGH };
	let mut request = [0u8; HEADER_BYTES + 1];
	request[..HEADER_BYTES].copy_from_slice(&header);
	request[0..4].copy_from_slice(&[0, 1, FRAME_SET_MODE, 0]);
	request[HEADER_BYTES] = mode;
	stream.set_write_timeout(Some(NEGOTIATION_TIMEOUT)).map_err(|e| e.to_string())?;
	stream.write_all(&request).map_err(|e| e.to_string())?;

	let mut reply = [0u8; HEADER_BYTES + 1];
	read_exact_until(stream, &mut reply, stop, Some(Instant::now() + NEGOTIATION_TIMEOUT)).map_err(|e| e.to_string())?;

	let reply_header = FrameHeader::parse(&reply);
	if reply_header.length != 1 || reply_header.flags != 0 || reply_header.id != hello.id || reply_header.time != hello.time || !(reply_header.kind == FRAME_MODE_ACK || reply_header.kind == FRAME_MODE_NAK) || reply[HEADER_BYTES] != mode {
		return Err("invalid mode reply".into());
	} else if reply_header.kind != FRAME_MODE_ACK {
		return Err("MAME rejected the requested parallel socket mode".into());
	}

	Ok(mode)
}

fn put_header(output: &mut Vec<u8>, size: usize, kind: u8, request: &[u8]) {
	output.extend_from_slice(&(size as u16).to_be_bytes());
	output.extend_from_slice(&[kind, 0]);
	output.extend_from_slice(&request[4..HEADER_BYTES]);
}

fn pins_hash(states: &[Pins]) -> u32 {
	let mut hash = FNV_OFFSET_BASIS;
	for state in states {
		for &byte in state {
			hash = (hash ^ byte as u32).wrapping_mul(FNV_PRIME);
		}
	}

	hash
}

fn valid_host_pins(pins: &Pins) -> bool {
	(pins[PIN_LINES] & !CTRL_MASK) == 0 && (pins[PIN_DRIVE] == DRIVE_NONE || pins[PIN_DRIVE] == DRIVE_ALL) && pins[3] == 0
}

fn add_typed(text: &str, typed: &mut VecDeque<u8>) {
	let bytes = text.as_bytes();

	if bytes == [0x0a] {
		typed.push_back(0x0d);
	} else if bytes != [0x10] { // shift
		for &byte in bytes {
			if byte != CHAR_NONE && byte & !HOST_CHAR_MASK == 0 && typed.len() < MAX_TYPED_CHARS {
				typed.push_back(byte);
			}
		}
	}
}

enum End {
	Disconnected,
	Stopped
}

struct Session<'a> {
	stream: &'a mut TcpStream,
	shared: &'a Shared,
	transfers: &'a mut Transfers,
	local: bool,
	link: Link,
	in_exchange: bool,
	generation: u32,
	configured: bool,
	incoming: VecDeque<u8>,
	last_time: u64,
	console_flushed: Instant,
	progress_sent: Instant,
	progress: Option<(u64, Option<u64>)>,
	marker: (State, usize, u64),
	marker_changed: Instant,
	stall_reported: bool,
	exchanges: u64,
	pending: Vec<u8>,
	receive: Vec<u8>,
	output: Vec<u8>
}

fn run_session(mut stream: TcpStream, shared: &Shared, transfers: &mut Transfers, typed: &mut VecDeque<u8>) -> Result<(), String> {
	stream.set_nonblocking(false).map_err(|e| e.to_string())?;
	stream.set_nodelay(true).map_err(|e| e.to_string())?;
	stream.set_read_timeout(Some(STOP_POLL)).map_err(|e| e.to_string())?;

	let mode = negotiate(&mut stream, &shared.stop)?;
	stream.set_write_timeout(Some(RESPONSE_TIMEOUT)).map_err(|e| e.to_string())?;

	let mut session = Session {
		stream: &mut stream,
		shared,
		transfers,
		local: mode == MODE_LOCAL_HANDSHAKE,
		link: Link::new(),
		in_exchange: false,
		generation: 0,
		configured: false,
		incoming: std::mem::take(typed),
		last_time: 0,
		console_flushed: Instant::now(),
		progress_sent: Instant::now(),
		progress: None,
		marker: (State::ConReady, 0, 0),
		marker_changed: Instant::now(),
		stall_reported: false,
		exchanges: 0,
		pending: Vec::with_capacity(65536),
		receive: vec![0u8; 65536],
		output: Vec::with_capacity(65536)
	};

	let result = session.run();
	session.flush_console();

	result.map(|_| ())
}

impl Session<'_> {
	fn housekeeping(&mut self) {
		while let Ok(text) = self.shared.keys.try_recv() {
			add_typed(&text, &mut self.incoming);
		}

		self.transfers.poll(self.shared, None);

		if self.console_flushed.elapsed() >= CONSOLE_FLUSH {
			self.flush_console();
		}

		let marker = (self.link.state, self.link.reverse_pos, self.link.upload_pos);
		if marker != self.marker {
			self.marker = marker;
			self.marker_changed = Instant::now();
			self.stall_reported = false;
		} else if !self.stall_reported && !self.link.in_console() && self.link.state != State::XferWaitFile && self.marker_changed.elapsed() >= STALL_REPORT {
			self.stall_reported = true;
			(self.shared.events)(
				LinkEvent::Log(
					format!("Debug Link: no progress for {} s at {} ms ({} exchanges, {}, engine generation {}): {}",
						STALL_REPORT.as_secs(),
						self.link.now / 1_000_000,
						self.exchanges,
						if self.local { "local" } else { "raw" },
						self.generation,
						self.link.describe()
					)
				)
			);
		}

		let progress = self.link.progress();
		if progress.is_some() && progress != self.progress && self.progress_sent.elapsed() >= PROGRESS_INTERVAL {
			let (done, total) = progress.unwrap();
			(self.shared.events)(LinkEvent::Progress { done, total });
			self.progress = progress;
			self.progress_sent = Instant::now();
		} else if progress.is_none() {
			self.progress = None;
		}
	}

	fn flush_console(&mut self) {
		if !self.link.console_out.is_empty() {
			(self.shared.events)(LinkEvent::Console(std::mem::take(&mut self.link.console_out)));
		}

		self.console_flushed = Instant::now();
	}

	fn receive(&mut self, need: usize) -> Result<Option<End>, String> {
		while self.pending.len() < need {
			if self.shared.stop.load(Ordering::SeqCst) {
				return Ok(Some(End::Stopped));
			}

			match self.stream.read(&mut self.receive) {
				Ok(0) => return Ok(Some(End::Disconnected)),
				Ok(count) => self.pending.extend_from_slice(&self.receive[..count]),
				Err(error) if error.kind() == ErrorKind::ConnectionReset => return Ok(Some(End::Disconnected)),
				Err(error) if is_timeout(&error) || error.kind() == ErrorKind::Interrupted => self.housekeeping(),
				Err(error) => return Err(error.to_string())
			}
		}

		Ok(None)
	}

	fn send(&mut self) -> Result<Option<End>, String> {
		match self.stream.write_all(&self.output) {
			Ok(()) => Ok(None),
			Err(error) if matches!(error.kind(), ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::WriteZero) => Ok(Some(End::Disconnected)),
			Err(error) if is_timeout(&error) => Err("socket response stalled for five seconds".into()),
			Err(error) => Err(error.to_string())
		}
	}

	fn begin_exchange(&mut self, time: u64) {
		self.in_exchange = true;
		self.link.advance(time);

		while let Some(byte) = self.incoming.pop_front() {
			if self.link.typed.len() >= MAX_TYPED_CHARS {
				self.incoming.clear();
				break;
			}

			self.link.typed.push_back(byte);
		}

		self.transfers.poll(self.shared, Some(&mut self.link));
		self.transfers.take_notices(self.shared, &mut self.link);
	}

	fn run(&mut self) -> Result<End, String> {
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
			let limit = if self.local && header.kind == FRAME_HOST_LOCAL_BATCH { MAX_BATCH_BYTES } else { PIN_STATE_BYTES };

			if header.flags != 0 || header.length > limit {
				return Err("invalid frame flags or payload length".into());
			}

			let size = HEADER_BYTES + header.length;
			check!(self.receive(size));

			let time = header.time;
			if time < self.last_time {
				return Err("backwards frame time".into());
			}
			self.last_time = time;

			match header.kind {
				FRAME_HOST_LOCAL_BATCH => {
					self.replay_batch(&header, size)?;
				},
				FRAME_HOST_PIN_STATE if header.length == PIN_STATE_BYTES && header.id == 0 => {
					if !self.in_exchange {
						self.begin_exchange(time);
					}
					let pins: Pins = self.pending[HEADER_BYTES..HEADER_BYTES + PIN_STATE_BYTES].try_into().unwrap();
					if !valid_host_pins(&pins) {
						return Err("invalid host pin state".into());
					}
					self.link.host_update(pins, time);
					self.transfers.take_notices(self.shared, &mut self.link);
				},
				FRAME_HOST_SYNC_REQUEST if header.length == 0 => {
					if !self.in_exchange {
						self.begin_exchange(time);
					}
					let request: [u8; HEADER_BYTES] = self.pending[..HEADER_BYTES].try_into().unwrap();
					self.reply(&request);
					check!(self.send());
					self.in_exchange = false;
					self.exchanges += 1;
				},
				_ => return Err("unexpected protocol frame".into())
			}

			self.pending.drain(..size);
			self.housekeeping();
		}
	}

	fn reply(&mut self, request: &[u8; HEADER_BYTES]) {
		self.output.clear();

		for state in std::mem::take(&mut self.link.emitted) {
			put_header(&mut self.output, PIN_STATE_BYTES, FRAME_PERIPHERAL_PIN_STATE, request);
			self.output.extend_from_slice(&state);
		}

		if self.local {
			let generation = self.generation.wrapping_add(1);

			if let Some(config) = self.link.engine_config(generation) {
				put_header(&mut self.output, config.len(), FRAME_ENGINE_CONFIG, request);
				self.output.extend_from_slice(&config);
				self.generation = generation;
				self.configured = true;
			}
		}

		put_header(&mut self.output, 8, FRAME_PERIPHERAL_SYNC_REPLY, request);

		self.output.extend_from_slice(&self.link.deadline().to_be_bytes());
	}

	fn replay_batch(&mut self, header: &FrameHeader, size: usize) -> Result<(), String> {
		if !self.local || self.in_exchange || !self.configured || header.id != 0 || header.length < BATCH_GENERATION_BYTES + LOCAL_EVENT_BYTES || (header.length - BATCH_GENERATION_BYTES) % LOCAL_EVENT_BYTES != 0 || get_u32(&self.pending[HEADER_BYTES..]) != self.generation {
			return Err("invalid local batch or generation".into());
		} else if !self.link.emitted.is_empty() {
			return Err("unsent line changes before a local batch".into());
		}

		let mut position = HEADER_BYTES + BATCH_GENERATION_BYTES;
		while position < size {
			let event: [u8; LOCAL_EVENT_BYTES] = self.pending[position..position + LOCAL_EVENT_BYTES].try_into().unwrap();
			let when = get_u64(&event);
			let pins: Pins = event[8..12].try_into().unwrap();

			if event[14] != LOCAL_EVENT_KIND_HOST || event[15] != 0 || !valid_host_pins(&pins) || when < self.link.now || when > header.time {
				return Err("invalid local event time, kind or pins".into());
			}

			self.link.host_update(pins, when);
			let states = std::mem::take(&mut self.link.emitted);
			let expected_count = u16::from_be_bytes([event[12], event[13]]) as usize;
			let expected_hash = get_u32(&event[16..]);
			let hash = pins_hash(&states);

			if states.len() != expected_count || hash != expected_hash || self.link.out != event[20..24] {
				return Err(format!("local handshake replay diverged at {} ns in {:?} (count {}/{}, hash {:08x}/{:08x})",
					when, self.link.state, states.len(), expected_count, hash, expected_hash));
			}

			position += LOCAL_EVENT_BYTES;
		}

		self.transfers.take_notices(self.shared, &mut self.link);

		Ok(())
	}
}
