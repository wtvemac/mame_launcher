// By: Eric MacDonald (eMac)

// WebTV port board with UART, parallel and PCMIA

use std::{
	io::{self, ErrorKind, Write},
	net::{Ipv4Addr, TcpListener, TcpStream},
	sync::{
		atomic::{AtomicBool, Ordering},
		mpsc,
		Arc,
		Mutex
	},
	thread::{self, JoinHandle},
	time::{Duration, Instant}
};
use crate::debuglink::{
	EventFn,
	LinkEvent,
	REQ_PEKOE2,
	REQ_PEKOE2_COMPARE,
	REQ_PEKOE2_FLASH,
	REQ_PEKOE2_IMAGE,
	REQ_PEKOE2_RAM
};

const CMD_LOAD: u8 = 0x00;
const LOAD_HEADER_BYTES: usize = 9;
const IMAGE_HEADER_BYTES: usize = 17;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const WORD_BYTES: usize = 4;

// There's no byte sequence to determine if the box want's a download so we're using console text matching.
const DISK_LINE: &str = "DownloadDisk: using old-style Pronto parallel port";
const AWAITING_LINE: &str = "Awaiting Command.";
const CHECKSUM_PREFIX: &str = "download checksum = ";
const FLASH_CHECKSUM_PREFIX: &str = "flash checksum =";
const DOWNLOAD_ERROR_PREFIX: &str = "#### DOWNLOAD ERROR";
const MAX_LINE: usize = 256;

const FLASH_COMMANDS: &[&str] = &["d", "df", "timflash", "joeflash"];
const RAM_COMMANDS: &[&str] = &["dram"];
const IMAGE_COMMANDS: &[&str] = &["dlimage"];
const COMPARE_COMMANDS: &[&str] = &["dcf"];

const RAM_RANGES: &[(u32, u64)] = &[
	(0x80000000, 0x90000000), // cached
	(0xa0000000, 0xb0000000)  // uncached
];

pub const IMAGE_MAX_WIDTH: u32 = 560;
pub const IMAGE_MAX_HEIGHT: u32 = 420;
const IMAGE_BYTES_PER_PIXEL: usize = 2;
const Y_BLACK: f32 = 16.0;
const Y_RANGE: f32 = 219.0;
const UV_OFFSET: f32 = 128.0;
const CR_FACTOR: f32 = 0x166 as f32 / 256.0;
const CB_FACTOR: f32 = 0x1c7 as f32 / 256.0;

const BUILD_BRANCH: u32 = 0x1000;
const BUILD_BASE_OFFSET: usize = 0x30;
const KSEG_MASK: u32 = 0x1fffffff;
const KSEG1: u32 = 0xa0000000;
const BANK_MASK: u32 = 0xff800000;
const APP_FLASH_BANK: u32 = 0xbf000000;
const APP_FLASH_BANK_END: u64 = 0xbf800000;

const BOOT_ROM_START: u32 = 0xbfc00000;
const BOOT_ROM_END: u64 = 0xc0000000;

const SEND_CHUNK: usize = 16 * 1024;
const STOP_POLL: Duration = Duration::from_millis(100);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub enum Pekoe2Command {
	Download {
		name: String,
		data: Vec<u8>
	},
	Ram {
		name: String,
		data: Vec<u8>,
		address: u32
	},
	Image {
		name: String,
		width: u32,
		height: u32,
		pixels: Vec<u8>,
		address: u32
	}
}

#[derive(Clone, Copy, PartialEq)]
enum Waiting {
	Disk,
	Flash,
	Compare,
	Ram,
	Image
}

#[derive(Default)]
struct Watch {
	line: String,
	disk: bool,
	command: Option<String>,
	waiting: Option<Waiting>,
	expected_checksum: Option<u32>,
	expected_flash_checksum: Option<u32>,
	flash_tail_nonzero: bool,
	flash_matched: bool
}
struct Running {
	watch: Watch,
	events: EventFn,
	commands: mpsc::Sender<Pekoe2Command>
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub fn console_text(text: &str, rshift: usize) {
	let mut events = vec![];
	let handler = {
		let Ok(mut running) = RUNNING.lock() else { return };
		let Some(running) = running.as_mut() else { return };

		for _ in 0..rshift {
			running.watch.line.pop();
		}

		for c in text.chars() {
			if c == '\n' || c == '\r' {
				let line = std::mem::take(&mut running.watch.line);
				watch_line(&mut running.watch, line.trim(), &mut events);
			} else if running.watch.line.len() < MAX_LINE {
				running.watch.line.push(c);
			}
		}
		running.events.clone()
	};

	for event in events {
		handler(event);
	}
}

fn echoed_command(line: &str) -> Option<&str> {
	let typed = match line.rfind(['>', '%']) {
		Some(at) => &line[at + 1..],
		None => line
	};

	typed.split_whitespace().next()
}

fn parse_checksum(value: &str) -> Option<u32> {
	u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok()
}

fn watch_line(watch: &mut Watch, line: &str, events: &mut Vec<LinkEvent>) {
	if let Some(command) = echoed_command(line) {
		let known = [
			FLASH_COMMANDS,
			COMPARE_COMMANDS,
			RAM_COMMANDS,
			IMAGE_COMMANDS
		].iter().any(|commands| commands.contains(&command));
		
		if known {
			watch.command = Some(command.to_string());
		}
	}

	if line == DISK_LINE {
		watch.disk = true;
	} else if line == AWAITING_LINE {
		let command = watch.command.take();

		if watch.disk {
			watch.disk = false;
			watch.waiting = Some(Waiting::Disk);
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2 });
		} else if command.as_deref().is_some_and(|command| FLASH_COMMANDS.contains(&command)) {
			watch.waiting = Some(Waiting::Flash);
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2_FLASH });
		} else if command.as_deref().is_some_and(|command| COMPARE_COMMANDS.contains(&command)) {
			watch.waiting = Some(Waiting::Compare);
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2_COMPARE });
		} else if command.as_deref().is_some_and(|command| RAM_COMMANDS.contains(&command)) {
			watch.waiting = Some(Waiting::Ram);
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2_RAM });
		} else if command.as_deref().is_some_and(|command| IMAGE_COMMANDS.contains(&command)) {
			watch.waiting = Some(Waiting::Image);
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2_IMAGE });
		} else {
			events.push(LinkEvent::Log("PEKOE2: the box is waiting on the parallel port but its command wasn't seen, so nothing is sent. This can't be cancelled".into()));
		}
	} else if let Some(value) = line.strip_prefix(FLASH_CHECKSUM_PREFIX) {
		if let Some(expected) = watch.expected_flash_checksum.take() {
			let reported = parse_checksum(value);
			watch.flash_matched = reported == Some(expected);

			events.push(LinkEvent::Log(
				match reported {
					Some(reported) if reported == expected => {
						format!("PEKOE2: the flash checksum matches the file ({:#x})", expected)
					},
					Some(reported) => {
						format!("PEKOE2: WARNING: the flash checksum is {:#x}, the file's is {:#x}", reported, expected)
					},
					None => {
						format!("PEKOE2: couldn't read the box's flash checksum '{}'", value)
					}
				}
			));
		}
	} else if line.starts_with(DOWNLOAD_ERROR_PREFIX) {
		if std::mem::take(&mut watch.flash_matched) && watch.flash_tail_nonzero {
			events.push(LinkEvent::Log("PEKOE2: download error with valid checkum!".into()));
		}
	} else if let Some(value) = line.strip_prefix(CHECKSUM_PREFIX) {
		if let Some(expected) = watch.expected_checksum.take() {
			let reported = parse_checksum(value);

			events.push(LinkEvent::Log(
				match reported {
					Some(reported) if reported == expected => {
						format!("PEKOE2: the box's download checksum matches ({:#x})", expected)
					},
					Some(reported) => {
						format!("PEKOE2: WARNING: the box's download checksum is {:#x} the file's is {:#x}", reported, expected)
					},
					None => {
						format!("PEKOE2: couldn't read the box's download checksum '{}'", value)
					}
				}
			));
		}
	}
}

pub fn send_command(command: Pekoe2Command) -> bool {
	match RUNNING.lock() {
		Ok(running) => {
			running.as_ref().is_some_and(|running| running.commands.send(command).is_ok())
		},
		Err(_) => {
			false
		}
	}
}

pub struct Pekoe2Link {
	port: u16,
	stop: Arc<AtomicBool>,
	thread: Option<JoinHandle<()>>
}

impl Pekoe2Link {
	pub fn start(events: EventFn) -> io::Result<Pekoe2Link> {
		let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
		listener.set_nonblocking(true)?;
		let port = listener.local_addr()?.port();

		let (sender, commands) = mpsc::channel();
		if let Ok(mut running) = RUNNING.lock() {
			*running = Some(Running { watch: Watch::default(), events: events.clone(), commands: sender });
		}

		let stop = Arc::new(AtomicBool::new(false));
		let thread_stop = stop.clone();
		let thread = thread::Builder::new()
			.name("pekoe2".into())
			.spawn(move || serve(listener, thread_stop, events, commands))?;

		Ok(Pekoe2Link { port, stop, thread: Some(thread) })
	}

	pub fn port(&self) -> u16 {
		self.port
	}
}

impl Drop for Pekoe2Link {
	fn drop(&mut self) {
		self.stop.store(true, Ordering::SeqCst);

		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}

		if let Ok(mut running) = RUNNING.lock() {
			*running = None;
		}
	}
}

fn serve(listener: TcpListener, stop: Arc<AtomicBool>, events: EventFn, commands: mpsc::Receiver<Pekoe2Command>) {
	let mut stream: Option<TcpStream> = None;

	while !stop.load(Ordering::SeqCst) {
		if stream.is_none() {
			match listener.accept() {
				Ok((connection, _)) => {
					let _ = connection.set_nonblocking(false);
					let _ = connection.set_nodelay(true);
					let _ = connection.set_write_timeout(Some(STOP_POLL));

					stream = Some(connection);
				},
				Err(error) if error.kind() == ErrorKind::WouldBlock => {
					{}
				},
				Err(error) => {
					events(LinkEvent::Log(format!("PEKOE2: listender accept failed: {}", error)))
				}
			}
		}

		match commands.recv_timeout(STOP_POLL) {
			Ok(command) => {
				let waiting = RUNNING.lock().ok().and_then(|running| running.as_ref().and_then(|running| running.watch.waiting));
				let Some(waiting) = waiting else {
					events(LinkEvent::Log("PEKOE2: the box isn't waiting for a download any more".into()));
					continue;
				};

				let Some(connection) = stream.as_mut() else {
					events(LinkEvent::TransferFinished { message: "The Pekoe2 port isn't connected to MAME, so the file wasn't sent".into() });
					continue;
				};

				let (name, result) = match (command, waiting) {
					(Pekoe2Command::Download { name, data }, Waiting::Disk | Waiting::Flash | Waiting::Compare) => {

						match download_address(&events, &name, &data, waiting) {
							Some(address) => {
								let result = send_download(connection, &stop, &events, &name, data, waiting, address);
								(name, result)
							},
							None => continue
						}

					},
					(Pekoe2Command::Ram { name, data, address }, Waiting::Ram) => {
						let result = send_ram(connection, &stop, &events, &name, data, address);

						(name, result)
					},
					(Pekoe2Command::Image { name, width, height, pixels, address }, Waiting::Image) => {
						let result = send_image(connection, &stop, &events, &name, width, height, pixels, address);

						(name, result)
					},
					_ => {
						events(LinkEvent::Log("PEKOE2: that isn't the kind of data the box is waiting for".into()));

						continue;
					}
				};

				if let Err(error) = result {
					events(LinkEvent::TransferFinished { message: format!("Couldn't send {} to the box: {}", name, error) });

					stream = None;
				}
			},
			Err(mpsc::RecvTimeoutError::Timeout) => {},
			Err(mpsc::RecvTimeoutError::Disconnected) => break
		}
	}
}


fn download_address(events: &EventFn, name: &str, data: &[u8], waiting: Waiting) -> Option<u32> {
	if waiting == Waiting::Disk {
		return Some(0);
	} else {
		let address = if waiting == Waiting::Compare {
			compare_address(data)
		} else {
			flash_target_address(data)
		};

		match address {
			Ok(address) => Some(address),
			Err(reason) => {
				let request = if waiting == Waiting::Compare { REQ_PEKOE2_COMPARE } else { REQ_PEKOE2_FLASH };

				events(LinkEvent::Log(format!("PEKOE2: {} {}. Choose another file.", name, reason)));
				events(LinkEvent::DownloadRequested { request });

				None
			}
		}
	}
}

fn word(data: &[u8], offset: usize) -> u32 {
	u32::from_be_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
}

fn flash_address(data: &[u8]) -> Result<u32, String> {
	if data.starts_with(&GZIP_MAGIC) {
		return Err("flash downloads need the uncompressed build image".into());
	} else if data.len() < BUILD_BASE_OFFSET + WORD_BYTES || word(data, 0) >> 16 != BUILD_BRANCH {
		return Err("isn't a WebTV build image (no build header), so there's no flash address for it".into());
	} else {
		let address = (word(data, BUILD_BASE_OFFSET) & KSEG_MASK) | KSEG1;

		if address & BANK_MASK != APP_FLASH_BANK {
			return Err(format!("is linked at {:#x}, outside the AppROM flash ({:#x}-{:#x})", word(data, BUILD_BASE_OFFSET), APP_FLASH_BANK, APP_FLASH_BANK_END - 1));
		} else if address as u64 + data.len().div_ceil(WORD_BYTES) as u64 * WORD_BYTES as u64 > APP_FLASH_BANK_END {
			return Err(format!("doesn't fit in the AppROM flash from {:#x}", address));
		}

		Ok(address)
	}	
}

fn boot_rom_address(data: &[u8]) -> Option<u32> {
	if data.starts_with(&GZIP_MAGIC) || data.len() < BUILD_BASE_OFFSET + WORD_BYTES || word(data, 0) >> 16 != BUILD_BRANCH {
		return None;
	} else {
		let uncached = (word(data, BUILD_BASE_OFFSET) & KSEG_MASK) | KSEG1;
		let size = data.len().div_ceil(WORD_BYTES) as u64 * WORD_BYTES as u64;

		(uncached >= BOOT_ROM_START && uncached as u64 + size <= BOOT_ROM_END).then_some(uncached)
	}
}

fn flash_target_address(data: &[u8]) -> Result<u32, String> {
	match flash_address(data) {
		Ok(address) => {
			Ok(address)
		},
		Err(reason) => {
			match boot_rom_address(data) {
				Some(address) => {
					Ok(address)
				},
				None if data.len() >= BUILD_BASE_OFFSET + WORD_BYTES && word(data, 0) >> 16 == BUILD_BRANCH && !data.starts_with(&GZIP_MAGIC) => {
					Err(format!("is linked at {:#x}, not in the AppROM flash or the BootROM (use dram to load it into RAM)", word(data, BUILD_BASE_OFFSET)))
				},
				None => {
					Err(reason)
				}
			}
		}
	}
}

fn compare_address(data: &[u8]) -> Result<u32, String> {
	if let Ok(address) = flash_target_address(data) {
		return Ok(address);
	} else if data.starts_with(&GZIP_MAGIC) {
		return Err("dcf needs the uncompressed build image".into());
	} else if data.len() < BUILD_BASE_OFFSET + WORD_BYTES || word(data, 0) >> 16 != BUILD_BRANCH {
		return Err("isn't a WebTV build image (no build header), so there's nothing to compare it with".into());
	} else {
		let base = word(data, BUILD_BASE_OFFSET);
		let size = data.len().div_ceil(WORD_BYTES) as u64 * WORD_BYTES as u64;

		if ram_address_error(base, size as usize).is_none() {
			return Ok(base);
		} else {
			return Err(format!("is linked at {:#x} not in the AppROM flash, the BootROM or RAM", base));
		}
	}
}

fn compare_target(address: u32) -> &'static str {
	if address & BANK_MASK == APP_FLASH_BANK {
		"the AppROM flash"
	} else if address >= BOOT_ROM_START {
		"the BootROM"
	} else {
		"in RAM"
	}
}

fn send_download(stream: &mut TcpStream, stop: &AtomicBool, events: &EventFn, name: &str, mut data: Vec<u8>, waiting: Waiting, address: u32) -> io::Result<()> {
	let size = data.len();

	if !data.starts_with(&GZIP_MAGIC) {
		data.resize(size.div_ceil(WORD_BYTES) * WORD_BYTES, 0);
	}

	let count = u32::try_from(data.len()).map_err(|_| io::Error::new(ErrorKind::InvalidInput, "too big for a 32-bit count"))?;
	let checksum = data.iter().fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));

	let flash_bytes = if waiting == Waiting::Compare { data.len() } else { data.len().saturating_sub(WORD_BYTES) };
	let flash_checksum = data[..flash_bytes].iter().fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));
	let flash_tail_nonzero = data[flash_bytes..].iter().any(|&byte| byte != 0);

	let mut frame = Vec::with_capacity(LOAD_HEADER_BYTES + data.len());
	frame.push(CMD_LOAD);
	frame.extend_from_slice(&count.to_be_bytes());
	frame.extend_from_slice(&address.to_be_bytes());
	frame.extend_from_slice(&data);

	if let Ok(mut running) = RUNNING.lock() {
		if let Some(running) = running.as_mut() {
			running.watch.waiting = None;
			running.watch.expected_checksum = Some(checksum);
			running.watch.expected_flash_checksum = if waiting == Waiting::Disk { None } else { Some(flash_checksum) };
			running.watch.flash_tail_nonzero = flash_tail_nonzero;
			running.watch.flash_matched = false;
		}
	}

	match waiting {
		Waiting::Flash => {
			events(LinkEvent::Log(format!("PEKOE2: flashing {} into {} at {:#x}", name, compare_target(address), address)))
		},
		Waiting::Compare => {
			events(LinkEvent::Log(format!("PEKOE2: comparing {} with {} at {:#x} the box lists each word that differs", name, compare_target(address), address)))
		},
		_ => {
			{}
		}
	}

	events(LinkEvent::DownloadStarted { name: name.into(), total: size as u64 });

	send_frame(stream, stop, events, &frame, LOAD_HEADER_BYTES, size)?;

	events(LinkEvent::TransferFinished { message: format!("Sent {} to the box over the Pekoe2 port ({} bytes)", name, size) });

	Ok(())
}

pub fn ram_link_address(data: &[u8]) -> Option<u32> {
	if data.len() < BUILD_BASE_OFFSET + WORD_BYTES || word(data, 0) >> 16 != BUILD_BRANCH {
		return None;
	}

	let address = word(data, BUILD_BASE_OFFSET);
	ram_address_error(address, data.len()).is_none().then_some(address)
}

pub fn ram_address_error(address: u32, size: usize) -> Option<String> {
	let end = address as u64 + size as u64;
	if RAM_RANGES.iter().any(|&(start, limit)| address >= start && end <= limit) {
		None
	} else {
		Some(format!("{:#010x}-{:#010x} isn't in RAM (0x80000000-0x8fffffff or 0xa0000000-0xafffffff)", address, end.saturating_sub(1)))
	}
}

pub fn raw_frame_buffer_image(name: &str, data: &[u8]) -> Result<(u32, u32, Vec<u8>, Option<String>), String> {
	let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);

	let named_size = stem
		.split(|c: char| !c.is_ascii_alphanumeric())
		.filter_map(|token| {
			let (width, height) = token.split_once(['x', 'X'])?;
			Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?))
		})
		.last();

	let row_bytes = IMAGE_MAX_WIDTH as usize * IMAGE_BYTES_PER_PIXEL;
	let (width, height) = match named_size {
		Some(size) => size,
		None if !data.is_empty() && data.len() % row_bytes == 0 => (IMAGE_MAX_WIDTH, (data.len() / row_bytes) as u32),
		None => return Err(format!("{} has no size: name it with one, like name_320x240.yuyv", name))
	};

	if width == 0 || height == 0 || width % 2 != 0 {
		return Err(format!("{} is {}x{} the width has to be even (two pixels per Y Cb Y Cr word)", name, width, height));
	}

	let expected = width as usize * height as usize * IMAGE_BYTES_PER_PIXEL;
	if data.len() != expected {
		return Err(format!("{} is {} bytes, but {}x{} needs {}", name, data.len(), width, height, expected));
	}

	let out_width = width.min(IMAGE_MAX_WIDTH) & !1;
	let out_height = height.min(IMAGE_MAX_HEIGHT);
	let in_row = width as usize * IMAGE_BYTES_PER_PIXEL;
	let out_row = out_width as usize * IMAGE_BYTES_PER_PIXEL;
	let pixels: Vec<u8> = data.chunks_exact(in_row).take(out_height as usize).flat_map(|row| row[..out_row].iter().copied()).collect();

	let note = (out_width != width || out_height != height)
		.then(|| format!("{} is {}x{}, cropped to {}x{} to fit the screen", name, width, height, out_width, out_height));

	Ok((out_width, out_height, pixels, note))
}

pub fn frame_buffer_image(rgba: &[u8], width: u32, height: u32) -> (u32, u32, Vec<u8>) {
	let scale = (IMAGE_MAX_WIDTH as f32 / width as f32).min(IMAGE_MAX_HEIGHT as f32 / height as f32).min(1.0);
	let out_height = ((height as f32 * scale) as u32).max(1);
	let out_width = (((width as f32 * scale) as u32) & !1).max(2);

	let sample = |x: u32, y: u32| -> (f32, f32, f32) {
		let source_x = ((x as f32 / scale) as u32).min(width - 1);
		let source_y = ((y as f32 / scale) as u32).min(height - 1);
		let at = (source_y as usize * width as usize + source_x as usize) * 4;
		let alpha = rgba[at + 3] as f32 / 255.0;
		(rgba[at] as f32 * alpha, rgba[at + 1] as f32 * alpha, rgba[at + 2] as f32 * alpha)
	};
	let luma = |(r, g, b): (f32, f32, f32)| 0.299 * r + 0.587 * g + 0.114 * b;
	let byte = |value: f32| value.round().clamp(0.0, 255.0) as u8;

	let mut pixels = Vec::with_capacity(out_width as usize * out_height as usize * IMAGE_BYTES_PER_PIXEL);

	for y in 0..out_height {
		for x in (0..out_width).step_by(2) {
			let first = sample(x, y);
			let second = sample(x + 1, y);
			let (y1, y2) = (luma(first), luma(second));
			let cb = ((first.2 - y1) + (second.2 - y2)) / 2.0 / CB_FACTOR;
			let cr = ((first.0 - y1) + (second.0 - y2)) / 2.0 / CR_FACTOR;

			pixels.push(byte(Y_BLACK + y1 * Y_RANGE / 255.0));
			pixels.push(byte(UV_OFFSET + cb));
			pixels.push(byte(Y_BLACK + y2 * Y_RANGE / 255.0));
			pixels.push(byte(UV_OFFSET + cr));
		}
	}

	(out_width, out_height, pixels)
}

fn send_frame(stream: &mut TcpStream, stop: &AtomicBool, events: &EventFn, frame: &[u8], header_bytes: usize, size: usize) -> io::Result<()> {
	let mut sent = 0;
	let mut reported = Instant::now();
	while sent < frame.len() {
		if stop.load(Ordering::SeqCst) {
			return Err(io::Error::new(ErrorKind::Interrupted, "MAME stopped"));
		}

		let end = (sent + SEND_CHUNK).min(frame.len());

		match stream.write(&frame[sent..end]) {
			Ok(0) => {
				return Err(io::Error::new(ErrorKind::WriteZero, "MAME closed the port"))
			},
			Ok(written) => {
				sent += written
			},
			Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {
				{}
			},
			Err(error) => {
				return Err(error)
			}
		}

		if reported.elapsed() >= PROGRESS_INTERVAL {
			reported = Instant::now();

			let done = sent.saturating_sub(header_bytes).min(size) as u64;

			events(LinkEvent::Progress { done, total: Some(size as u64) });
		}
	}

	Ok(())
}

fn clear_waiting() {
	if let Ok(mut running) = RUNNING.lock() {
		if let Some(running) = running.as_mut() {
			running.watch.waiting = None;
			running.watch.expected_checksum = None;
			running.watch.expected_flash_checksum = None;
		}
	}
}

fn send_ram(stream: &mut TcpStream, stop: &AtomicBool, events: &EventFn, name: &str, data: Vec<u8>, address: u32) -> io::Result<()> {
	let count = u32::try_from(data.len()).map_err(|_| io::Error::new(ErrorKind::InvalidInput, "too big for a 32-bit count"))?;

	let mut frame = Vec::with_capacity(LOAD_HEADER_BYTES + data.len());

	frame.push(CMD_LOAD);
	frame.extend_from_slice(&count.to_be_bytes());
	frame.extend_from_slice(&address.to_be_bytes());
	frame.extend_from_slice(&data);

	clear_waiting();

	events(LinkEvent::Log(format!("PEKOE2: loading {} into RAM at {:#010x}", name, address)));
	events(LinkEvent::DownloadStarted { name: name.into(), total: data.len() as u64 });

	send_frame(stream, stop, events, &frame, LOAD_HEADER_BYTES, data.len())?;

	events(LinkEvent::TransferFinished { message: format!("Sent {} to the box's RAM at {:#010x} ({} bytes)", name, address, data.len()) });

	Ok(())
}

fn send_image(stream: &mut TcpStream, stop: &AtomicBool, events: &EventFn, name: &str, width: u32, height: u32, pixels: Vec<u8>, address: u32) -> io::Result<()> {
	if width == 0 || height == 0 || width > IMAGE_MAX_WIDTH || height > IMAGE_MAX_HEIGHT || pixels.len() != width as usize * height as usize * IMAGE_BYTES_PER_PIXEL {
		return Err(io::Error::new(ErrorKind::InvalidInput, "the image doesn't fit the screen"));
	}

	let mut frame = Vec::with_capacity(IMAGE_HEADER_BYTES + pixels.len());

	frame.push(CMD_LOAD);
	frame.extend_from_slice(&(pixels.len() as u32).to_be_bytes());
	frame.extend_from_slice(&address.to_be_bytes());
	frame.extend_from_slice(&width.to_be_bytes());
	frame.extend_from_slice(&height.to_be_bytes());
	frame.extend_from_slice(&pixels);

	clear_waiting();

	events(LinkEvent::DownloadStarted { name: name.into(), total: pixels.len() as u64 });

	send_frame(stream, stop, events, &frame, IMAGE_HEADER_BYTES, pixels.len())?;

	events(LinkEvent::TransferFinished { message: format!("Sent {} to the box's screen ({}x{} at {:#010x})", name, width, height, address) });

	Ok(())
}
