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
use crate::debuglink::{EventFn, LinkEvent, REQ_PEKOE2};

const CMD_LOAD: u8 = 0x00;
const LOAD_HEADER_BYTES: usize = 9;
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];
const WORD_BYTES: usize = 4;

// There's no byte sequence to determine if the box want's a download so we're using console text matching.
const DISK_LINE: &str = "DownloadDisk: using old-style Pronto parallel port";
const AWAITING_LINE: &str = "Awaiting Command.";
const CHECKSUM_PREFIX: &str = "download checksum = ";
const MAX_LINE: usize = 256;

const SEND_CHUNK: usize = 16 * 1024;
const STOP_POLL: Duration = Duration::from_millis(100);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

pub enum Pekoe2Command {
	Download { name: String, data: Vec<u8> }
}

#[derive(Default)]
struct Watch {
	line: String,
	disk: bool,
	waiting: bool,
	expected_checksum: Option<u32>
}
struct Running {
	watch: Watch,
	events: EventFn,
	commands: mpsc::Sender<Pekoe2Command>
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);

pub fn console_text(text: &str) {
	let mut events = vec![];
	let handler = {
		let Ok(mut running) = RUNNING.lock() else { return };
		let Some(running) = running.as_mut() else { return };

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

fn watch_line(watch: &mut Watch, line: &str, events: &mut Vec<LinkEvent>) {
	if line == DISK_LINE {
		watch.disk = true;
	} else if line == AWAITING_LINE {
		if watch.disk {
			watch.disk = false;
			watch.waiting = true;
			events.push(LinkEvent::DownloadRequested { request: REQ_PEKOE2 });
		} else {
			events.push(LinkEvent::Log("PEKOE2: the box is waiting for a flash download on the parallel port. This can't be cancelled".into()));
		}
	} else if let Some(value) = line.strip_prefix(CHECKSUM_PREFIX) {
		if let Some(expected) = watch.expected_checksum.take() {
			let reported = u32::from_str_radix(value.trim().trim_start_matches("0x"), 16).ok();

			events.push(LinkEvent::Log(
				match reported {
					Some(reported) if reported == expected => {
						format!("PEKOE2: the box's download checksum matches ({:#x})", expected)
					},
					Some(reported) => {
						format!("PEKOE2: WARNING: the box's download checksum is {:#x}, the file's is {:#x}", reported, expected)
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
			Ok(Pekoe2Command::Download { name, data }) => {
				let waiting = RUNNING.lock().ok().and_then(|running| running.as_ref().map(|running| running.watch.waiting)).unwrap_or(false);
				if !waiting {
					events(LinkEvent::Log("PEKOE2: the box isn't waiting for a download any more".into()));
					continue;
				}

				let Some(connection) = stream.as_mut() else {
					events(LinkEvent::TransferFinished { message: "The Pekoe2 port isn't connected to MAME, so the file wasn't sent".into() });
					continue;
				};

				if let Err(error) = send_download(connection, &stop, &events, &name, data) {
					events(LinkEvent::TransferFinished { message: format!("Couldn't send {} to the box: {}", name, error) });

					stream = None;
				}
			},
			Err(mpsc::RecvTimeoutError::Timeout) => {},
			Err(mpsc::RecvTimeoutError::Disconnected) => break
		}
	}
}

fn send_download(stream: &mut TcpStream, stop: &AtomicBool, events: &EventFn, name: &str, mut data: Vec<u8>) -> io::Result<()> {
	let size = data.len();

	if !data.starts_with(&GZIP_MAGIC) {
		data.resize(size.div_ceil(WORD_BYTES) * WORD_BYTES, 0);
	}

	let count = u32::try_from(data.len()).map_err(|_| io::Error::new(ErrorKind::InvalidInput, "too big for a 32-bit count"))?;
	let checksum = data.iter().fold(0u32, |sum, &byte| sum.wrapping_add(byte as u32));

	let mut frame = Vec::with_capacity(LOAD_HEADER_BYTES + data.len());
	frame.push(CMD_LOAD);
	frame.extend_from_slice(&count.to_be_bytes());
	frame.extend_from_slice(&0u32.to_be_bytes());
	frame.extend_from_slice(&data);

	if let Ok(mut running) = RUNNING.lock() {
		if let Some(running) = running.as_mut() {
			running.watch.waiting = false;
			running.watch.expected_checksum = Some(checksum);
		}
	}

	events(LinkEvent::DownloadStarted { name: name.into(), total: size as u64 });

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

			let done = sent.saturating_sub(LOAD_HEADER_BYTES).min(size) as u64;

			events(LinkEvent::Progress { done, total: Some(size as u64) });
		}
	}

	events(LinkEvent::TransferFinished { message: format!("Sent {} to the box over the Pekoe2 port ({} bytes)", name, size) });

	Ok(())
}
