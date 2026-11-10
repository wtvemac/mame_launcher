// By: Eric MacDonald (eMac)

#![windows_subsystem = "windows"]

// NOTE: windows no longer has software renderer fallback: https://github.com/slint-ui/slint/issues/8708
// You need to run this launcher with the SLINT_BACKEND=winit-software env to force the software renderer.

mod config;
mod netstack;
mod wtv;
mod externalpopup;
mod mamecapture;
mod debuglink;
mod pekoe2;
#[cfg(webtv_bridge)]
mod webtvbridge;

use std::{
	collections::HashMap, 
	fs::File,
	io::{BufReader, Read, Write, Cursor, Seek, SeekFrom}, 
	path::{Path, PathBuf}, process::{Command, Stdio},
	env,
	process::{ChildStdout, ChildStderr},
	net::TcpListener,
	sync::{
		Arc,
		Mutex,
		atomic::{AtomicBool, Ordering::Relaxed}
	},
	time::{Duration, Instant}
};
use rand::distr::{Alphanumeric, SampleString};
use regex::Regex;
use native_dialog::{DialogBuilder, FileDialogBuilder};
use sysinfo::{Pid, System};
use rodio;
use serialport;
use portpicker;
use hex;
use which::which;
use open;
use slint::Model;
use crossbeam_channel::{
	bounded,
	Receiver
};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::fs;

#[cfg(target_os = "linux")]
use libxdo_sys;
#[cfg(target_os = "windows")]
use winapi::um::winuser::{FindWindowW, PostMessageW, VkKeyScanA, MapVirtualKeyA};
//use winapi::um::winuser::{EnumWindows, GetWindowThreadProcessId, PostMessageW, VkKeyScanA, MapVirtualKeyA};
#[cfg(target_os = "macos")]
use {
	core_graphics::{
		event::{CGEvent, CGEventFlags, KeyCode},
		event_source::{CGEventSource, CGEventSourceStateID}
	},
	core_foundation::{
		dictionary::{CFDictionaryAddValue, CFDictionaryCreateMutable},
		base::{CFRelease, TCFTypeRef},
		number::kCFBooleanTrue
	},
	accessibility_sys::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt}
};

use config::{DEFAULT_DNS0000_TARGET, LauncherConfig, MAMEMachineNode, MAMEOptions, NetworkOptions, Paths, PersistentConfig};
use netstack::NetworkStack;
use wtv::{
	buildio::{
		BuildIO,
		BuildIODataCollation,
		diskio::CompressedHunkDiskIO,
		romio::ROMIO,
		flashdiskio::FlashdiskIO
	},
	buildmeta::{
		BuildMeta,
		BuildMetaLayout,
		BuildInfo,
		BuildFlag
	},
	ssid::{SSIDInfo, SSIDBoxType, SSIDManufacture},
	msntv2boxid::{MSNTV2BoxIdInfo}
};

slint::include_modules!();

const STACK_SIZE: usize = 32 * 1024 * 1024;
const ALWAYS_GENERATE_ID_FILES: bool = true;
const SSID_ROM_FILE: &'static str = "ds2401.bin";
const MSNTV2_ETHERNET_EEPROM_FILE: &'static str = "pci_1e.0_slot8_i82801_eth_eeprom";
const MSNTV2_ALWAYS_RESET_EEPROM: bool = true;
const MSNTV2_ETHERNET_DEVICE_TAG: &'static str = ":pci:1e.0:slot8:i82801_eth";
const DEFAULT_BOOTORM_FILE_NAME: &'static str = "bootrom.o";
const DEFAULT_MSNTV2_BOOTORM_FILE_NAME: &'static str = "bios-emac-mame-only.bin";
const BOOTROM_BASE_ADDRESS: u32 = 0x9fc00000;
const BOOTROM_FLASH_FILE_PREFIX: &'static str = "bootrom_flash";
// wtv2 (Plus) boxes will be detected and ran from this launcher but we assume (and can only verify) a flash-based approm 
const APPROM1_FLASH_BASE_ADDRESS: u32 = 0x9f000000;
const APPROM2_FLASH_BASE_ADDRESS: u32 = 0x9fe00000;
const APPROM3_DISK_BASE_ADDRESS_MIN: u32 = 0x80300000;
const APPROM3_DISK_BASE_ADDRESS_MAX: u32 = 0x84400000;
const APPROM1_FLASH_FILE_PREFIX: &'static str = "bank0_flash";
const APPROM2_FLASH_FILE_PREFIX: &'static str = "approm_flash";
const APPROM3_FLASH_FILE_PREFIX: &'static str = "bank1_flash";
const APPROM_HDIMG_PREFIX: &'static str = "hdimg";
const ALLOW_APPROM2_FILES: bool = false;
const DEFAULT_BOX_ORDER: [&'static str; 20] = [
	"wtv1bf0",
	"wtv1sony",
	"wtv1phil",
	"wtv2lc2",
	"wtv2sony",
	"wtv2phil",
	"msntv2",
	"wtv2jpp",
	"wtv2jpc",
	"wtv2wld",
	"wtv1bfe",
	"wtv1pal",
	"wtv1dev",
	"wtv1dv2",
	"wtv2drb",
	"wtv2npl",
	"wtv2ncl",
	"wtv2esr",
	"wtv2utv",
	"wtv2uvd"
];
const DEFAULT_FLASHDISK_SIZE: u64 = 8 * 1024 * 1024;
const PUBLIC_TOUCHPP_ADDRESS: &'static str = "wtv.ooguy.com:1122";
const DEFAULT_DEBUG_ENDPOINT: &'static str = "Launcher Console";
const DEFAULT_CAPTURE_DEVICE: &'static str = "auto";
// The divider between a captured MAME window and the console
const CAPTURED_DIVIDER_HEIGHT: f32 = 6.0;
const LINUX_AUDIO_IN_SOUND_MODULES: [&'static str; 2] = [
	"pipewire",
	"portaudio"
];
// MAME names output audio devices as "o:<name>" and input audio devices as "i:<name>"
const MAME_AUDIO_OUTPUT_NODE_PREFIX: &'static str = "o:";
const MAME_AUDIO_INPUT_NODE_PREFIX: &'static str = "i:";
const WTVPORT_BOXES: &[&str] = &[
	"wtv1bf0",
	"wtv1sony",
	"wtv1phil",
	"wtv1pal",
	"wtv1dev",
	"wtv1dv2",
	"wtv2lc2",
	"wtv2sony",
	"wtv2phil"
];
const WTVPORT_SLOT_NAME: &'static str = "wtvport1";
const PARALLEL_SLOT_NAME: &'static str = "pport";
const DEFAULT_SLOT_ORDER: [&'static str; 5] = [
	"deskjet6xx",
	"socket",
	"hostlpt",

	"printer",
	"pekoe2"
];
const DEFAULT_PARALLEL_SOCKET_ENDPOINT: &'static str = "127.0.0.1:3000";
// Parallel port socket endpoint value for the launcher's built-in WebTV Bridge (the "webtv-bridge" feature).
const WEBTV_BRIDGE_SOCKET_ENDPOINT: &'static str = "webtv-bridge";
#[cfg(webtv_bridge)]
const WEBTV_BRIDGE_STATE_DIR: &'static str = "webtv-bridge";
// Parallel port socket endpoint value for the launcher's built-in debug link (WebTV's 1997 parallel port debug link).
const DEBUG_LINK_SOCKET_ENDPOINT: &'static str = "debug-link";
const PEKOE2_WTVPORT_OPTION: &'static str = "pekoe2";
const SERIAL_PORT_PREFIX: &'static str = "serial.";
const DEFAULT_SERIAL_PORT_OPTIONS: &'static str = "b115200,cs8";
const DEBUG_READ_BUFFER_SIZE: usize = 1024;
const CONSOLE_READ_BUFFER_SIZE: usize = 1024;
const CONSOLE_UPDATE_INTERVAL_USEC: u128 = 10000;
const CONSOLE_MAX_LINES: usize = 5000;
const CONSOLE_MAX_LINE_LENGTH: usize = 2048;
const CONSOLE_MAX_PENDING_SIZE: usize = 1024 * 1024;
#[cfg(target_os = "linux")]
const CONSOLE_KEY_DELAY: u32 = 100 * 1000;
#[cfg(target_os = "windows")]
const CONSOLE_KEY_DELAY: u64 = 25 * 1000;
#[cfg(target_os = "macos")]
const CONSOLE_KEY_DELAY: u64 = 55 * 1000;
const CONSOLE_ERROR_PATTERNS: &[&str] = &[
	"TellyScript: Couldn't get",
	"TellyScript: TIMEOUT",
	"TellyScript: PAP authentification failure",
	"error",
	"Exception:",
	r"TellyScript: ParseResult -- \d+ unknown"
];
const CONSOLE_WARNING_PATTERNS: &[&str] = &[
	"warning",
	"not found",
	"not available",
	"NOT_AVAILABLE",
	"NOT_FOUND",
	"Timed out",
	"Rate Renegotiation"
];


// These files are packaged into the executable so you only need to distribute one file.
const FART_INTRO1: &'static [u8] = include_bytes!("../sounds/fart-intro1.mp3");
const FART_INTRO2: &'static [u8]  = include_bytes!("../sounds/fart-intro2.mp3");
const FART_INTRO3: &'static [u8]  = include_bytes!("../sounds/fart-intro3.mp3");
const FART1: &'static [u8]  = include_bytes!("../sounds/fart1.mp3");
const FART2: &'static [u8]  = include_bytes!("../sounds/fart2.mp3");
const FART3: &'static [u8]  = include_bytes!("../sounds/fart3.mp3");

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum BuildStorageType {
	UnknownStorageType,
	StrippedFlashBuild,
	MaskRomBuild,
	DiskBuild,
	FlashdiskBuild,
	MSNTV2BIOSBuild,
	MSNTV2BIOSBypassBuild,
	MSNTV2DiskOSBuild,
	PCBIOSBuild,
	PCDiskOSBuild
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum BuildStorageState {
	UnknownBuildState,
	BuildLooksGood,
	FileNotFound,
	RomSizeMismatch,
	RomHashMismatch,
	StrippedFlashCyclopsed,
	StrippedFlashMissing,
	CantReadBuild,
	CodeChecksumMismatch,
	RomfsChecksumMismatch,
	BadBaseAddress,
	SignatureMismatch
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum SSIDStorageState {
	UnknownSSIDState,
	SSIDLooksGood,
	FileNotFound,
	ManufactureMismatch,
	CRCMismatch,
	FileCRCMismatch,
	BoxTypeMismatch,
	CantReadSSID
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum SlotType {
	ModemSerial,
	DebugSerial,
	MoIPBridge,
	LIRCDevice,
	Unknown
}

// Selectable build item with data to verify its integrity.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct VerifiableBuildItem {
	pub hint: slint::SharedString,
	pub value: slint::SharedString,
	pub description: slint::SharedString,
	pub status: String,
	pub can_revert: bool,
	pub hash: String,
	pub build_storage_type: BuildStorageType,
	pub build_storage_state: BuildStorageState,
	pub build_info: Option<BuildInfo>
}

// Selectable SSID item with data to verify its integrity.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct VerifiableSSIDItem {
	pub hint: slint::SharedString,
	pub value: slint::SharedString,
	pub description: slint::SharedString,
	pub ssid_storage_state: SSIDStorageState,
	pub ssid_info: Option<SSIDInfo>,
	pub boxid_info: Option<MSNTV2BoxIdInfo>
}

// Slot item that uses the null_modem bitbanger.
// Keeping track of these so we can select a slot to use for the modem or debug serial port.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MachineSlotItem {
	pub hint: slint::SharedString,
	pub value: slint::SharedString,
	pub description: slint::SharedString,
	pub name: String,
	pub slot_name: String,
	pub slot_type: SlotType
}

struct PendingConsoleText {
	// (if this is from stderr (true/false), "line text")
	segments: Vec<(bool, String)>,
	rshift: usize,
	scroll_mode: MAMEConsoleScrollMode,
	flush_scheduled: bool,
	last_flush: Option<Instant>,
	last_char_cr: bool
}

static PENDING_CONSOLE_TEXT: Mutex<PendingConsoleText> = Mutex::new(PendingConsoleText {
	segments: Vec::new(),
	rshift: 0,
	scroll_mode: MAMEConsoleScrollMode::NoScrollCheck,
	flush_scheduled: false,
	last_flush: None,
	last_char_cr: false
});

static CONSOLE_ERROR_REGEX: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
	Regex::new(&format!("(?i){}", CONSOLE_ERROR_PATTERNS.join("|"))).unwrap()
});
static CONSOLE_WARNING_REGEX: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
	Regex::new(&format!("(?i){}", CONSOLE_WARNING_PATTERNS.join("|"))).unwrap()
});

#[derive(Debug)]
pub struct MachineID {
    pub box_iteration: String,
    pub box_name: String,
}

struct ConsoleRow {
	line: ConsoleLine,
	from_stderr: bool
}

#[derive(Default)]
struct ConsoleLines {
	lines: std::cell::RefCell<std::collections::VecDeque<ConsoleRow>>,
	notify: slint::ModelNotify
}

impl slint::Model for ConsoleLines {
	type Data = ConsoleLine;

	fn row_count(&self) -> usize {
		self.lines.borrow().len()
	}

	fn row_data(&self, row: usize) -> Option<Self::Data> {
		self.lines.borrow().get(row).map(|row| row.line.clone())
	}

	fn model_tracker(&self) -> &dyn slint::ModelTracker {
		&self.notify
	}

	fn as_any(&self) -> &dyn std::any::Any {
		self
	}
}

impl ConsoleLines {
	fn add_text(&self, text: &String, rshift: usize, from_stderr: bool) -> usize {
		let (changed_row, removed_count, added_count, trimmed_count, shifted_count) = {
			let mut lines = self.lines.borrow_mut();
			let old_count = lines.len();

			let skip_to = console_last_lines_start(text, CONSOLE_MAX_LINES);
			if skip_to > 0 {
				lines.clear();
			}
			let text = &text[skip_to..];

			let (mut current_line, mut current_stderr) = lines.pop_back().map(|row| (row.line.text.to_string(), row.from_stderr)).unwrap_or_default();

			for _ in 0..rshift {
				if current_line.pop().is_none() {
					match lines.pop_back() {
						Some(row) => (current_line, current_stderr) = (row.line.text.to_string(), row.from_stderr),
						None => break
					}
				}
			}

			let changed_row = lines.len();

			// An empty line keeps nothing from where its (deleted) text came from.
			current_stderr = current_stderr && !current_line.is_empty();

			let mut text_lines = text.split('\n');
			let first_text_line = text_lines.next().unwrap_or("");
			current_line += first_text_line;
			current_stderr = current_stderr || (from_stderr && !first_text_line.is_empty());
			for text_line in text_lines {
				ConsoleLines::push_line(&mut lines, &current_line, current_stderr);
				current_line = text_line.to_string();
				current_stderr = from_stderr && !text_line.is_empty();
			}
			ConsoleLines::push_line(&mut lines, &current_line, current_stderr);

			let removed_count = old_count - changed_row;
			let added_count = lines.len() - changed_row;

			let trimmed_count = lines.len().saturating_sub(CONSOLE_MAX_LINES);
			lines.drain(..trimmed_count);

			let shifted_count = if skip_to > 0 { old_count + trimmed_count } else { trimmed_count };

			(changed_row, removed_count, added_count, trimmed_count, shifted_count)
		};

		if removed_count > 0 {
			self.notify.row_removed(changed_row, removed_count);
		}
		self.notify.row_added(changed_row, added_count);
		if trimmed_count > 0 {
			self.notify.row_removed(0, trimmed_count);
		}

		shifted_count
	}

	fn push_line(lines: &mut std::collections::VecDeque<ConsoleRow>, line: &str, from_stderr: bool) {
		let expanded_line;
		let mut line = if line.contains('\t') {
			let mut column = 0;
			expanded_line = line.chars().fold(String::new(), |mut expanded, c| {
				if c == '\t' {
					let spaces = 8 - (column % 8);
					expanded.extend(std::iter::repeat(' ').take(spaces));
					column += spaces;
				} else {
					expanded.push(c);
					column += 1;
				}
				expanded
			});
			expanded_line.as_str()
		} else {
			line
		};

		while line.len() > CONSOLE_MAX_LINE_LENGTH {
			let mut split_at = CONSOLE_MAX_LINE_LENGTH;
			while !line.is_char_boundary(split_at) {
				split_at -= 1;
			}
			lines.push_back(ConsoleLines::make_row(&line[..split_at], from_stderr));
			line = &line[split_at..];
		}
		lines.push_back(ConsoleLines::make_row(line, from_stderr));
	}

	// Level: 0 = normal text, 1 =warning and 2 = error.
	fn make_row(text: &str, from_stderr: bool) -> ConsoleRow {
		let level = if from_stderr || CONSOLE_ERROR_REGEX.is_match(text) {
			2
		} else if CONSOLE_WARNING_REGEX.is_match(text) {
			1
		} else {
			0
		};

		ConsoleRow {
			line: ConsoleLine { text: text.into(), level },
			from_stderr
		}
	}
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
	if cfg!(target_os = "macos") {
		let _ = start_in_main();
	} else {
		let _ = start_in_thread();
	}

	Ok(())
}

fn start_in_main() -> Result<(), slint::PlatformError> {
	let _ = start_ui();

	Ok(())
}

fn start_in_thread() -> Result<(), slint::PlatformError> {
	// Spawn a new thread so we can set the stack size without needing to modify the ~/.cargo/config.toml file.
	let _ = 
		std::thread::Builder::new()
		.stack_size(STACK_SIZE)
		.spawn(move || {

			let _ = start_ui();

		})
		.unwrap()
		.join();

	Ok(())
}

fn enable_loading(ui_weak: &slint::Weak<MainWindow>, message: String){
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		ui.set_loading_message(message.into());
		ui.set_loading_depth(ui.get_loading_depth() + 1);
	});
}

fn disable_loading(ui_weak: &slint::Weak<MainWindow>) {
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		ui.set_loading_depth(ui.get_loading_depth() - 1);
	});
}

fn get_bootroms(config: &LauncherConfig, selected_machine: &MAMEMachineNode) -> Result<Vec<VerifiableBuildItem>, Box<dyn std::error::Error>> {
	let mut bootroms: Vec<VerifiableBuildItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let mut biossets = HashMap::new();
	for biosset in selected_machine.clone().biosset.unwrap_or(vec![]).iter() {
		let biosset_name = 
			biosset.name
			.clone()
			.unwrap_or("".into());
		let biosset_description = 
			biosset.description
			.clone()
			.unwrap_or("".into());

		if selected_box == "msntv2" {
			biossets.insert(biosset_name + ".bin", biosset_description);
		} else {
			biossets.insert(biosset_name + ".o", biosset_description);
		}
	}

	let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

	for rom in selected_machine.clone().rom.unwrap_or(vec![]).iter() {
		let rom_name = 
			rom.name
			.clone()
			.unwrap_or("".into());

		let rom_region = 
			rom.region
			.clone()
			.unwrap_or("".into());

		if rom_name != *SSID_ROM_FILE && rom_region != "serial_id" {
			let rom_status = 
				rom.status
				.clone()
				.unwrap_or("".into());

			let mut description: slint::SharedString = slint::SharedString::new();

			if biossets.contains_key(&rom_name) {
				description =
					biossets
					.get(&rom_name)
					.unwrap_or(&"".into())
					.into();
			} else if rom_name != DEFAULT_BOOTORM_FILE_NAME && rom_name != DEFAULT_MSNTV2_BOOTORM_FILE_NAME {
				continue;
			}

			let mut bootrom = VerifiableBuildItem {
				hint: "".into(),
				value: if rom_name == DEFAULT_MSNTV2_BOOTORM_FILE_NAME { "bios-emac".into() } else { rom_name.clone().into() },
				status: rom_status.clone().into(),
				description: description.into(),
				can_revert: false,
				hash: "".into(),
				build_storage_type: BuildStorageType::MaskRomBuild,
				build_storage_state: BuildStorageState::UnknownBuildState,
				build_info: None
			};

			let bootrom_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &rom_name.clone().to_string();

			if Path::new(&bootrom_path).exists() {
				match BuildMeta::open_rom(bootrom_path, None, validate_build_image) {
					Ok(build_meta) => {
						bootrom.build_info = Some(build_meta.build_info[0].clone());

						if selected_box == "msntv2" {
							if build_meta.layout == BuildMetaLayout::MSNTV2BIOSLayout && build_meta.build_info[0].build_header.build_version > 0 {
								bootrom.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
								if bootrom.value.contains("emac") {
									bootrom.build_storage_type = BuildStorageType::MSNTV2BIOSBypassBuild;
								} else {
									bootrom.build_storage_type = BuildStorageType::MSNTV2BIOSBuild;
								}
							} else {
								bootrom.build_storage_type = BuildStorageType::PCBIOSBuild;
							}

							bootrom.build_storage_state = BuildStorageState::BuildLooksGood;
						} else {
							if (build_meta.build_info[0].build_header.build_flags & BuildFlag::Debug) != 0x00 {
								bootrom.hint = (build_meta.build_info[0].build_header.build_version.clone().to_string() + "d").into();
							} else {
								bootrom.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
							}
		
							if validate_build_image && build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
								bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
							} else if validate_build_image && build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
								bootrom.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
							} else if build_meta.build_info[0].build_header.build_base_address != BOOTROM_BASE_ADDRESS {
								bootrom.build_storage_state = BuildStorageState::BadBaseAddress;
							} else {
								bootrom.build_storage_state = BuildStorageState::BuildLooksGood;
							}
						}
					},
					Err(_) => {
						bootrom.build_info = None;
						bootrom.build_storage_state = BuildStorageState::CantReadBuild;
						bootrom.hint = "".into();
					}
				}
			} else {
				bootrom.build_storage_state = BuildStorageState::FileNotFound;
			}

			bootroms.push(bootrom);
		}
	}

	// No ROM bootroms available. Check if this is a developer box and look for flash files.
	// MAME doesn't tell me about this so we're just guessing if these files are used.
	if bootroms.iter().count() == 0 {
		if Regex::new(r"^wtv\d+dev$").unwrap().is_match(selected_box.as_str()) {
			let mut bootrom = VerifiableBuildItem {
				hint: "".into(),
				value: BOOTROM_FLASH_FILE_PREFIX.into(),
				status: "".into(),
				can_revert: false,
				description: "BootROM Flash Build".into(),
				hash: "".into(),
				build_storage_type: BuildStorageType::StrippedFlashBuild,
				build_storage_state: BuildStorageState::UnknownBuildState,
				build_info: None
			};

			let bootrom_path_prefix = mame_directory_path + "/nvram/" + &selected_box + "/" + BOOTROM_FLASH_FILE_PREFIX;

			let bootrom_path0 = bootrom_path_prefix.clone() + "0";
			let bootrom_path1 = bootrom_path_prefix.clone() + "1";

			let bootrom_path0_exists = Path::new(&bootrom_path0).exists();
			let bootrom_path1_exists = Path::new(&bootrom_path1).exists();

			if bootrom_path0_exists || bootrom_path1_exists {
				if bootrom_path0_exists && bootrom_path1_exists {
					match BuildMeta::open_rom(bootrom_path_prefix, Some(BuildIODataCollation::StrippedROMs), validate_build_image) {
						Ok(build_meta) => {
							bootrom.build_info = Some(build_meta.build_info[0].clone());
							if (build_meta.build_info[0].build_header.build_flags & BuildFlag::Debug) != 0x00 {
								bootrom.hint = (build_meta.build_info[0].build_header.build_version.clone().to_string() + "d").into();
							} else {
								bootrom.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
							}
		
							if validate_build_image && build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
								bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
							} else if validate_build_image && build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
								bootrom.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
							} else if build_meta.build_info[0].build_header.build_base_address != BOOTROM_BASE_ADDRESS {
								bootrom.build_storage_state = BuildStorageState::BadBaseAddress;
							} else {
								bootrom.build_storage_state = BuildStorageState::BuildLooksGood;
							}
						},
						Err(_) => {
							bootrom.build_info = None;
							bootrom.build_storage_state = BuildStorageState::CantReadBuild;
							bootrom.hint = "".into();
						}
					}
				} else {
					bootrom.build_storage_state = BuildStorageState::StrippedFlashCyclopsed;
				}
			} else {
				bootrom.build_storage_state = BuildStorageState::StrippedFlashMissing;
			}

			bootroms.push(bootrom);
		} else if Regex::new(r"^wtv\d+wld$").unwrap().is_match(selected_box.as_str()) {
			let bootrom = VerifiableBuildItem {
				hint: "".into(),
				value: "None".into(),
				status: "".into(),
				can_revert: false,
				description: "".into(),
				hash: "".into(),
				build_storage_type: BuildStorageType::StrippedFlashBuild,
				build_storage_state: BuildStorageState::BuildLooksGood,
				build_info: None
			};

			bootroms.push(bootrom);
		}
	}

	Ok(bootroms)
}

fn get_flash_approms(config: &LauncherConfig, selected_machine: &MAMEMachineNode, selected_bootrom_index: usize) -> Result<Vec<VerifiableBuildItem>, Box<dyn std::error::Error>> {
	let mut approms: Vec<VerifiableBuildItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let mut approm = VerifiableBuildItem {
		hint: "".into(),
		value: APPROM1_FLASH_FILE_PREFIX.into(),
		status: "".into(),
		can_revert: false,
		description: "AppROM Flash Build".into(),
		hash: "".into(),
		build_storage_type: BuildStorageType::StrippedFlashBuild,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None
	};

	let approm1_path_prefix: String;
	let approm2_path_prefix: String;

	if selected_bootrom_index > 0 {
		approm1_path_prefix = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string() + "/" + APPROM1_FLASH_FILE_PREFIX;
		approm2_path_prefix = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string() + "/" + APPROM2_FLASH_FILE_PREFIX;
	} else {
		approm1_path_prefix = mame_directory_path.clone() + "/nvram/" + &selected_box + "/" + APPROM1_FLASH_FILE_PREFIX;
		approm2_path_prefix = mame_directory_path.clone() + "/nvram/" + &selected_box + "/" + APPROM2_FLASH_FILE_PREFIX;
	}

	let approm_path_prefix;
	
	// MAME doesn't provide details about the flash device used, so we must guess this. If approm2 files are there, then use approm2 paths otherwise use an approm1 path.
	if ALLOW_APPROM2_FILES && (Path::new(&(approm2_path_prefix.clone() + "0")).exists() || Path::new(&(approm2_path_prefix.clone() + "1")).exists()) {
		approm_path_prefix = approm2_path_prefix.clone();
	} else {
		approm_path_prefix = approm1_path_prefix.clone();
	}

	let approm_path0 = approm_path_prefix.clone() + "0";
	let approm_path1 = approm_path_prefix.clone() + "1";

	let approm_path0_exists = Path::new(&approm_path0).exists();
	let approm_path1_exists = Path::new(&approm_path1).exists();

	let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

	if approm_path0_exists || approm_path1_exists {
		if approm_path0_exists && approm_path1_exists {
			if Regex::new(r"^wtv\d+wld$").unwrap().is_match(selected_box.as_str()) {
				let approm_path_prefix = mame_directory_path.clone() + "/nvram/" + &selected_box + "/" + APPROM3_FLASH_FILE_PREFIX;
	
				let approm_path2 = approm_path_prefix.clone() + "0";
				let approm_path3 = approm_path_prefix.clone() + "1";
			
				let approm_path2_exists = Path::new(&approm_path2).exists();
				let approm_path3_exists = Path::new(&approm_path3).exists();

				if approm_path2_exists || approm_path3_exists {
					if approm_path2_exists && approm_path3_exists {
						approm.hint = "".into();
						approm.value = "WinCE".into();
						approm.status = "unverified".into();
						approm.description = "".into();
						approm.build_storage_type = BuildStorageType::StrippedFlashBuild;
						approm.build_storage_state = BuildStorageState::BuildLooksGood;
						approm.build_info = None;
					} else {
						approm.build_storage_state = BuildStorageState::StrippedFlashCyclopsed;
					}
				} else {
					approm.build_storage_state = BuildStorageState::StrippedFlashMissing;
				}
			} else {
				match BuildMeta::open_rom(approm_path_prefix.clone(), Some(BuildIODataCollation::StrippedROMs), validate_build_image) {
					Ok(build_meta) => {
						approm.build_info = Some(build_meta.build_info[0].clone());
						if (build_meta.build_info[0].build_header.build_flags & BuildFlag::Debug) != 0x00 {
							approm.hint = (build_meta.build_info[0].build_header.build_version.clone().to_string() + "d").into();
						} else {
							approm.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
						}

						if validate_build_image && build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
							approm.build_storage_state = BuildStorageState::CodeChecksumMismatch;
						} else if validate_build_image && build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
							approm.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
						} else if build_meta.build_info[0].build_header.build_base_address != APPROM1_FLASH_BASE_ADDRESS && build_meta.build_info[0].build_header.build_base_address != APPROM2_FLASH_BASE_ADDRESS {
							approm.build_storage_state = BuildStorageState::BadBaseAddress;
						} else {
							approm.build_storage_state = BuildStorageState::BuildLooksGood;
						}
					},
					Err(_) => {
						approm.build_info = None;
						approm.build_storage_state = BuildStorageState::CantReadBuild;
						approm.hint = "".into();
					}
				}
			}
		} else {
			approm.build_storage_state = BuildStorageState::StrippedFlashCyclopsed;
		}
	} else {
		approm.build_storage_state = BuildStorageState::StrippedFlashMissing;
	}

	approms.push(approm);

	Ok(approms)
}

fn populate_approms_from_disk_file(approms: &mut Vec<VerifiableBuildItem>, file_path: String, collation: Option<BuildIODataCollation>, prefix: String, discription: String, is_preset_disk: bool, validate_build_image: bool, is_msntv2: bool)  -> Result<(), Box<dyn std::error::Error>> {
	let can_revert = match is_preset_disk {
		true => {
			let diff_file_path = CompressedHunkDiskIO::find_diff_file(file_path.clone()).unwrap_or("".into());

			diff_file_path != "" && Path::new(&diff_file_path).exists()
		},
		_ => false
	};

	match BuildMeta::open_disk(file_path.clone(), collation, validate_build_image, is_msntv2 && validate_build_image) {
		Ok(build_meta) => {
			let mut build_index = 0;
			for buildinfo in build_meta.build_info.iter() {
				let mut approm: VerifiableBuildItem = VerifiableBuildItem {
					hint: "".into(),
					value: (prefix.clone() + "[" + &build_index.to_string() + "]").into(),
					status: "".into(),
					can_revert: can_revert,
					description: if build_meta.layout == BuildMetaLayout::MSNTV2DiskLayout {
						format!("{} ({})", build_meta.msntv2_info.build_version, discription.clone()).into()
					} else {
						discription.clone().into()
					},
					hash: "".into(),
					build_storage_type: if build_meta.layout == BuildMetaLayout::MSNTV2DiskLayout {
						BuildStorageType::MSNTV2DiskOSBuild
					} else if is_msntv2 { // If we're adding MSNTV2 OSs but the disk format isn't MSNTV2 then assume a PC-formatted disk.
						BuildStorageType::PCDiskOSBuild
					} else {
						BuildStorageType::DiskBuild
					},
					build_storage_state: BuildStorageState::UnknownBuildState,
					build_info: None
				};

				if build_index == build_meta.selected_build_index {
					approm.status = "selected".to_string();
				}

				approm.build_info = Some(buildinfo.clone());
				if buildinfo.build_header.build_version > 0 {
					if (buildinfo.build_header.build_flags & BuildFlag::Debug) != 0x00 {
						approm.hint = (buildinfo.build_header.build_version.clone().to_string() + "d").into();
					} else {
						approm.hint = buildinfo.build_header.build_version.clone().to_string().into();
					}
				}

				if build_meta.build_count == 0 {
					approm.build_storage_state = BuildStorageState::CantReadBuild;
				} else if validate_build_image && buildinfo.build_header.code_checksum != buildinfo.calculated_code_checksum {
					approm.build_storage_state = BuildStorageState::CodeChecksumMismatch;
				} else if validate_build_image && buildinfo.romfs_header.romfs_checksum != buildinfo.calculated_romfs_checksum {
					approm.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
				} else if is_msntv2 && build_meta.layout == BuildMetaLayout::MSNTV2DiskLayout && build_meta.msntv2_info.signature_validates != Some(true) {
					approm.build_storage_state = BuildStorageState::SignatureMismatch;
				} else if !is_msntv2 && build_meta.layout != BuildMetaLayout::MSNTV2DiskLayout && buildinfo.build_header.build_base_address < APPROM3_DISK_BASE_ADDRESS_MIN || buildinfo.build_header.build_base_address > APPROM3_DISK_BASE_ADDRESS_MAX {
					approm.build_storage_state = BuildStorageState::BadBaseAddress;
				} else {
					approm.build_storage_state = BuildStorageState::BuildLooksGood;
				}

				approms.push(approm);

				build_index += 1;

				if build_index >= build_meta.build_count {
					break;
				}
			}
		},
		_ => {
			//
		}
	};

	Ok(())
}

fn get_disk_approms(config: &LauncherConfig, selected_machine: &MAMEMachineNode, selected_hdimg_path: String) -> Result<Vec<VerifiableBuildItem>, Box<dyn std::error::Error>> {
	let mut approms: Vec<VerifiableBuildItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();
	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let disk_collation = match selected_box.as_str() {
		"msntv2" => BuildIODataCollation::Raw,
		"wtv2utv" | "wtv2uvd" => BuildIODataCollation::ByteSwapped1632,
		 _ => BuildIODataCollation::ByteSwapped16
	};

	let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

	if selected_machine.disk.iter().count() > 0 {
		match selected_machine.disk.clone() {
			Some(disks) => {
				let disk_name = disks[0].name.clone().unwrap_or("".into());
				let disk_file = disk_name.clone() + ".chd";

				let preset_img_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &disk_file;

				let _ = populate_approms_from_disk_file(
					&mut approms, 
					preset_img_path, 
					Some(disk_collation), 
					disk_name.clone(), 
					"From preset ".to_owned() + &disk_file.clone() + " file",
					true,
					validate_build_image,
					selected_box == "msntv2"
				);
			},
			_ => {
				//
			}
		};
	}

	if selected_hdimg_path != "" {
		let _ = populate_approms_from_disk_file(
			&mut approms, 
			selected_hdimg_path, 
			Some(disk_collation), 
			APPROM_HDIMG_PREFIX.to_string(), 
			"From your HDD image file.".into(),
			false,
			validate_build_image,
			selected_box == "msntv2"
		);
	}

	Ok(approms)
}

fn get_flashdisk_approms(config: &LauncherConfig, selected_machine: &MAMEMachineNode, selected_bootrom_index: usize) -> Result<Vec<VerifiableBuildItem>, Box<dyn std::error::Error>> {
	let mut approms: Vec<VerifiableBuildItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();
	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let mut approm = VerifiableBuildItem {
		hint: "".into(),
		value: "".into(),
		status: "".into(),
		can_revert: false,
		description: "".into(),
		hash: "".into(),
		build_storage_type: BuildStorageType::FlashdiskBuild,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None
	};

	let file_path;
	if selected_bootrom_index > 0 {
		file_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string() + "/mdoc_flash0";
	} else {
		file_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "/mdoc_flash0";
	}

	let flashdisk_size = match get_flashdisk_size(&selected_machine) {
		Ok(flashdisk_size) => flashdisk_size as u64,
		_ => DEFAULT_FLASHDISK_SIZE
	};

	let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

	if Path::new(&file_path).exists() {
		if flashdisk_size > Path::new(&file_path).metadata().unwrap().len() {
			// The file is borked so remove it. There are some cases where this isn't intended but most times this will correct some issues.
			let _ = std::fs::remove_file(&file_path);
			approm.build_storage_state = BuildStorageState::FileNotFound;
		} else {
			match BuildMeta::open_flashdisk(file_path, Some(BuildIODataCollation::Raw), validate_build_image) {
				Ok(build_meta) => {
					let mut build_index = 0;
					for buildinfo in build_meta.build_info.iter() {

						if build_index == build_meta.selected_build_index {
							approm.status = "selected".to_string();
						}

						approm.build_info = Some(buildinfo.clone());
						if (buildinfo.build_header.build_flags & BuildFlag::Debug) != 0x00 {
							approm.hint = (buildinfo.build_header.build_version.clone().to_string() + "d").into();
						} else {
							approm.hint = buildinfo.build_header.build_version.clone().to_string().into();
						}
						approm.value = ("mdoc[".to_owned() + &build_index.to_string() + "]").into();

						if build_meta.build_count == 0 {
							approm.build_storage_state = BuildStorageState::CantReadBuild;
						} else if buildinfo.build_header.build_base_address < APPROM3_DISK_BASE_ADDRESS_MIN || buildinfo.build_header.build_base_address > APPROM3_DISK_BASE_ADDRESS_MAX {
							approm.build_storage_state = BuildStorageState::BadBaseAddress;
						} else {
							approm.build_storage_state = BuildStorageState::BuildLooksGood;
						}


						build_index += 1;

						if build_index >= build_meta.build_count {
							break;
						}
					};
				},
				_ => {
					//
				}
			};
		}
	} else {
		approm.build_storage_state = BuildStorageState::FileNotFound;
	}

	approms.push(approm);

	Ok(approms)
}

fn get_ssids(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, selected_bootrom_index: usize) -> Result<Vec<VerifiableSSIDItem>, Box<dyn std::error::Error>> {
	let mut ssids: Vec<VerifiableSSIDItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let mut ssid = VerifiableSSIDItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		ssid_storage_state: SSIDStorageState::UnknownSSIDState,
		ssid_info: None,
		boxid_info: None
	};

	if selected_box == "msntv2" {
		let boxid_file: String = MSNTV2_ETHERNET_EEPROM_FILE.into();

		let boxid_file_path;
		if selected_bootrom_index > 0 {
			boxid_file_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string() + "/" + &boxid_file.clone();
		} else {
			boxid_file_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "/" + &boxid_file.clone();
		}

		if ALWAYS_GENERATE_ID_FILES && !Path::new(&boxid_file_path).exists() {
			match MSNTV2BoxIdInfo::generate() {
				Ok(boxid_info) => {
					let _ = save_boxid_for_box(boxid_info.raw, selected_box.clone(), selected_bootrom_index, ui_weak.clone(), true, true);
				},
				_ => {

				}
			}
		}

		if Path::new(&boxid_file_path).exists() {
			let mut f = File::open(boxid_file_path)?;

			let mut raw_eeprom = [0x00; 128];

			let _ = f.read(&mut raw_eeprom)?;

			match MSNTV2BoxIdInfo::new_from_eeprom(raw_eeprom) {
				Ok(boxid_info) => {
					ssid.boxid_info = Some(boxid_info.clone());
					ssid.value = boxid_info.value.clone().into();
					ssid.description = ("MAC Address=".to_owned() + &boxid_info.mac_address_str).into();

					if boxid_info.eeprom_crc != boxid_info.calculated_eeprom_crc {
						ssid.ssid_storage_state = SSIDStorageState::FileCRCMismatch;
					} else if boxid_info.crc != boxid_info.calculated_crc {
						ssid.ssid_storage_state = SSIDStorageState::CRCMismatch;
					} else {
						ssid.ssid_storage_state = SSIDStorageState::SSIDLooksGood;
					}
				},
				_ => {
					ssid.ssid_storage_state = SSIDStorageState::CantReadSSID;
				}
			}
		} else {
			ssid.ssid_storage_state = SSIDStorageState::FileNotFound;
		}

		ssids.push(ssid);
	} else {
		let mut ssid_file: String = SSID_ROM_FILE.into();

		for rom in selected_machine.clone().rom.unwrap_or(vec![]).iter() {
			let rom_region = 
				rom.region
				.clone()
				.unwrap_or("".into());

			if rom_region == "serial_id" {
				ssid_file = 
					rom.name
					.clone()
					.unwrap_or(SSID_ROM_FILE.into());

				break;
			}
		}

		let ssid_file_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &ssid_file.clone();

		if ALWAYS_GENERATE_ID_FILES && !Path::new(&ssid_file_path).exists() {
			let ssid_manufacture = if Regex::new(r"^wtv\d+phil$").unwrap().is_match(selected_box.as_str()) {
				SSIDManufacture::Phillips
			} else {
				SSIDManufacture::Sony
			};

			match SSIDInfo::generate(SSIDBoxType::MAME, ssid_manufacture) {
				Ok(ssid_info) => {
					let _ = save_ssid_for_box(ssid_info.raw, selected_box.clone(), ui_weak.clone(), true, true);
				},
				_ => {

				}
			}
		}

		if Path::new(&ssid_file_path).exists() {
			let mut f = File::open(ssid_file_path)?;

			let mut raw_ssid = [0x00; 0x08];

			let _ = f.read(&mut raw_ssid)?;

			match SSIDInfo::new(raw_ssid) {
				Ok(ssid_info) => {
					ssid.ssid_info = Some(ssid_info.clone());
					ssid.value = ssid_info.value.into();
					ssid.description = ("box type=".to_owned() + &SSIDBoxType::to_string(&ssid_info.box_type) + ", box id=" + &ssid_info.box_id.to_string() + ", manufacture=" + &SSIDManufacture::to_string(&ssid_info.manufacture)).into();

					if ssid_info.crc != ssid_info.calculated_crc {
						ssid.ssid_storage_state = SSIDStorageState::CRCMismatch;
					} else if ssid_info.box_type != SSIDBoxType::MAME {
						ssid.ssid_storage_state = SSIDStorageState::BoxTypeMismatch;
					} else {
						if Regex::new(r"^wtv\d+sony$").unwrap().is_match(selected_box.as_str())  && ssid_info.manufacture != SSIDManufacture::Sony {
							ssid.ssid_storage_state = SSIDStorageState::ManufactureMismatch;
						} else if Regex::new(r"^wtv\d+phil$").unwrap().is_match(selected_box.as_str()) && ssid_info.manufacture != SSIDManufacture::Phillips {
							ssid.ssid_storage_state = SSIDStorageState::ManufactureMismatch;
						} else {
							ssid.ssid_storage_state = SSIDStorageState::SSIDLooksGood;
						}
					}
				},
				_ => {
					ssid.ssid_storage_state = SSIDStorageState::CantReadSSID;
				}
			}
		} else {
			ssid.ssid_storage_state = SSIDStorageState::FileNotFound;
		}

		ssids.push(ssid);
	}

	Ok(ssids)
}

fn get_slots(selected_machine: &MAMEMachineNode) -> Result<Vec<MachineSlotItem>, Box<dyn std::error::Error>> {
	let mut slots: Vec<MachineSlotItem> = vec![];

	let mut bitbangers = HashMap::new();
	for device in selected_machine.clone().device.unwrap_or(vec![]).iter() {
		let device_type = 
			device.dtype
			.clone()
			.unwrap_or("".into());
		let mut briefname = "".to_string();
		for instance in device.instance.clone().unwrap_or(vec![]).iter() {
			briefname =
				instance.briefname
				.clone()
				.unwrap_or("".into());
			if briefname != "" {
				break;
			}
		}
		let device_tag = 
			device.tag
			.clone()
			.unwrap_or("".into());

		if device_type == "bitbanger" {
			if briefname != "" {
				match Regex::new(r"^(?<slot_name>.+?)\:null_modem\:stream$").unwrap().captures(device_tag.as_str()) {
					Some(matches) => {
						bitbangers.insert(matches["slot_name"].to_string(), briefname);
					}
					None => {
					}
				}
			}
		} else if device_type == "moipbridge" {
			if briefname != "" {
				let slot = MachineSlotItem {
					hint: "".into(),
					value: (device_tag.clone() + "; " + &briefname.clone().to_owned()).into(),
					description: "".into(),
					name: briefname.clone().into(),
					slot_name: device_tag.clone().into(),
					slot_type: SlotType::MoIPBridge
				};

				slots.push(slot);
			}
		} else if device_type == "lirc" {
			if briefname != "" {
				let slot = MachineSlotItem {
					hint: "".into(),
					value: (device_tag.clone() + "; " + &briefname.clone().to_owned()).into(),
					description: "".into(),
					name: briefname.clone().into(),
					slot_name: device_tag.clone().into(),
					slot_type: SlotType::LIRCDevice
				};

				slots.push(slot);
			}
		}
	}

	for xslot in selected_machine.clone().slot.unwrap_or(vec![]).iter() {
		let mut found_null_modem_option = false;
		for slotoption in xslot.slotoption.clone().unwrap_or(vec![]).iter() {
			let slotoption_devname =
				slotoption.devname
				.clone()
				.unwrap_or("".into());
			if slotoption_devname == "null_modem" {
				found_null_modem_option = true;
				break;
			}
		}
		if found_null_modem_option {
			let slot_name = 
				xslot.name
				.clone()
				.unwrap_or("".into());
			if bitbangers.contains_key(&slot_name) {
				let mut slot = MachineSlotItem {
					hint: "".into(),
					value: (slot_name.clone() + "; " + &bitbangers[&slot_name.clone()].clone().to_owned()).into(),
					description: "".into(),
					name: bitbangers[&slot_name.clone()].clone().into(),
					slot_name: slot_name.clone().into(),
					slot_type: SlotType::Unknown
				};
				if Regex::new(r"(modem|mdm|comb)").unwrap().is_match(slot_name.clone().as_str()) {
					slot.slot_type = SlotType::ModemSerial;
				} else if Regex::new(r"(debug|dbg|pekoe|coma)").unwrap().is_match(slot_name.clone().as_str()) {
					slot.slot_type = SlotType::DebugSerial;
				}

				slots.push(slot);
			}
		}
	}

	Ok(slots)
}

fn get_slot_options(config: &LauncherConfig, selected_machine: &MAMEMachineNode, slot_name: &str) -> Vec<(String, String)> {
	let mut slot_options: Vec<(String, String)> = vec![];

	let config_machines = config.mame.machine.clone().unwrap_or(vec![]);

	for xslot in selected_machine.slot.clone().unwrap_or(vec![]).iter() {
		if xslot.name.clone().unwrap_or("".into()) != slot_name {
			continue;
		}

		for slotoption in xslot.slotoption.clone().unwrap_or(vec![]).iter() {
			let slotoption_name =
				slotoption.name
				.clone()
				.unwrap_or("".into());
			let slotoption_devname =
				slotoption.devname
				.clone()
				.unwrap_or("".into());
			if slotoption_name == "" {
				continue;
			}

			let mut slotoption_description = "".to_string();
			for config_machine in config_machines.iter() {
				if config_machine.name.clone().unwrap_or("".into()) == slotoption_devname {
					slotoption_description =
						config_machine.description
						.clone()
						.unwrap_or("".into());
					break;
				}
			}

			slot_options.push((slotoption_name, slotoption_description));
		}

		break;
	}

	slot_options.sort_by(
		| cmp_a, cmp_b | {
			let a_preset_order = DEFAULT_SLOT_ORDER.iter().position(|&val| val == cmp_a.0.as_str()).unwrap_or(usize::MAX);
			let b_preset_order = DEFAULT_SLOT_ORDER.iter().position(|&val| val == cmp_b.0.as_str()).unwrap_or(usize::MAX);

			if a_preset_order == b_preset_order {
				cmp_a.0.partial_cmp(&cmp_b.0).unwrap()
			} else {
				a_preset_order.cmp(&b_preset_order)
			}
		}
	);

	slot_options
}

fn split_custom_options(custom_options: &str) -> Vec<String> {
	let mut arguments: Vec<String> = vec![];
	let mut argument = String::new();
	// Quoted text can make an empty argument ("") so track if there's an argument separately.
	let mut in_argument = false;
	let mut quote: Option<char> = None;

	for character in custom_options.chars() {
		match quote {
			Some(quote_character) => {
				if character == quote_character {
					quote = None;
				} else {
					argument.push(character);
				}
			},
			None => {
				if character == '"' || character == '\'' {
					quote = Some(character);
					in_argument = true;
				} else if character.is_whitespace() {
					if in_argument {
						arguments.push(std::mem::take(&mut argument));
						in_argument = false;
					}
				} else {
					argument.push(character);
					in_argument = true;
				}
			}
		}
	}

	if in_argument {
		arguments.push(argument);
	}

	arguments
}

fn to_mame_endpoint_path(endpoint: &str) -> String {
	if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(endpoint) {
		"socket.".to_owned() + endpoint
	} else {
		endpoint.to_string()
	}
}

// How many bitbangers a slot option adds, including the ones its own slots add with their default options (for example pekoe2 adds 2).
fn get_slot_option_bitbanger_count(config_machines: &Vec<MAMEMachineNode>, devname: &str, depth: u32) -> usize {
	if depth > 8 {
		return 0;
	}

	let Some(option_machine) = config_machines.iter().find(|machine| machine.name.as_deref() == Some(devname)) else {
		return 0;
	};

	let mut count = option_machine.device_ref.clone().unwrap_or(vec![]).iter()
		.filter(|device_ref| device_ref.name.as_deref() == Some("bitbanger"))
		.count();

	for xslot in option_machine.slot.clone().unwrap_or(vec![]).iter() {
		for slotoption in xslot.slotoption.clone().unwrap_or(vec![]).iter() {
			if slotoption.default.as_deref() == Some("yes") {
				count += get_slot_option_bitbanger_count(config_machines, &slotoption.devname.clone().unwrap_or("".into()), depth + 1);
			}
		}
	}

	count
}

fn get_bitbanger_brief_names(config_machines: &Vec<MAMEMachineNode>, selected_machine: &MAMEMachineNode, selected_slot_options: &Vec<(String, String)>) -> (HashMap<String, String>, HashMap<String, Vec<String>>) {
	let mut bitbangers: Vec<(Option<String>, Option<String>)> = vec![];

	let device_refs = selected_machine.device_ref.clone().unwrap_or(vec![]);
	let slot_tags: Vec<(String, String)> = selected_slot_options.iter()
		.map(|(slot_name, option_name)| (":".to_owned() + slot_name.trim_start_matches(':'), option_name.clone()))
		.collect();

	let in_slot = |tag: &str, slot_tag: &str| tag == slot_tag || tag.starts_with(&(slot_tag.to_owned() + ":"));

	for (index, device_ref) in device_refs.iter().enumerate() {
		let tag = device_ref.tag.clone().unwrap_or("".into());

		// Bitbangers from the slot's default option are replaced by the selected option's.
		let replaced = slot_tags.iter().any(|(slot_tag, _)| in_slot(&tag, slot_tag));
		if !replaced && device_ref.name.as_deref() == Some("bitbanger") {
			bitbangers.push((Some(tag.clone()), None));
		}

		for (slot_tag, option_name) in slot_tags.iter() {
			let next_tag = device_refs.get(index + 1).and_then(|next| next.tag.clone()).unwrap_or("".into());
			if in_slot(&tag, slot_tag) && !in_slot(&next_tag, slot_tag) {
				let devname = selected_machine.slot.clone().unwrap_or(vec![]).iter()
					.filter(|xslot| ":".to_owned() + xslot.name.clone().unwrap_or("".into()).trim_start_matches(':') == *slot_tag)
					.flat_map(|xslot| xslot.slotoption.clone().unwrap_or(vec![]))
					.find(|slotoption| slotoption.name.as_deref() == Some(option_name.as_str()))
					.and_then(|slotoption| slotoption.devname);

				if let Some(devname) = devname {
					for _ in 0..get_slot_option_bitbanger_count(config_machines, &devname, 0) {
						bitbangers.push((None, Some(slot_tag.clone())));
					}
				}
			}
		}
	}

	// MAME numbers them when there is more than one.
	let brief_name = |index: usize| if bitbangers.len() > 1 { format!("bitb{}", index + 1) } else { "bitb".to_string() };

	let mut box_bitbangers: HashMap<String, String> = HashMap::new();
	let mut added_bitbangers: HashMap<String, Vec<String>> = HashMap::new();
	for (index, (tag, slot_tag)) in bitbangers.iter().enumerate() {
		if let Some(tag) = tag {
			box_bitbangers.insert(tag.clone(), brief_name(index));
		} else if let Some(slot_tag) = slot_tag {
			added_bitbangers.entry(slot_tag.clone()).or_default().push(brief_name(index));
		}
	}

	(box_bitbangers, added_bitbangers)
}

fn find_bitbanger_brief_name(box_bitbangers: &HashMap<String, String>, tag_suffix: &str) -> Option<String> {
	box_bitbangers.iter()
		.find(|(tag, _)| tag.ends_with(tag_suffix))
		.map(|(_, brief_name)| brief_name.clone())
}

fn find_device_brief_name(selected_machine: &MAMEMachineNode, device_type: &str) -> Option<String> {
	selected_machine.device.clone().unwrap_or(vec![]).iter()
		.filter(|device| device.dtype.as_deref() == Some(device_type))
		.flat_map(|device| device.instance.clone().unwrap_or(vec![]))
		.find_map(|instance| instance.briefname.filter(|briefname| briefname != ""))
}

fn find_audio_input_tags(selected_machine: &MAMEMachineNode) -> Vec<String> {
	selected_machine.device_ref.clone().unwrap_or(vec![]).iter()
		.filter(|device_ref| device_ref.name.as_deref() == Some("microphone"))
		.filter_map(|device_ref| device_ref.tag.clone())
		.filter(|tag| tag.ends_with(":audio_input") || tag.ends_with(":div_audio_input"))
		.collect()
}

fn get_audio_in_sound_module(mame_path: &String) -> Option<String> {
	if cfg!(target_os = "linux") {
		let sound_modules = LauncherConfig::mame_sound_modules(mame_path);
		LINUX_AUDIO_IN_SOUND_MODULES.iter()
			.find(|sound_module| sound_modules.iter().any(|available| available == *sound_module))
			.map(|sound_module| sound_module.to_string())
	} else {
		Some("".to_string())
	}
}

// (hint, value) for the audio in devices the sound module can use. The value is MAME's node name for the device.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn get_audio_in_devices(sound_module: &str) -> Vec<(String, String)> {
	use rodio::cpal::traits::{DeviceTrait, HostTrait};

	#[allow(unused_mut)]
	let mut audio_in_devices: Vec<(String, String)> = vec![];

	if cfg!(target_os = "linux") {
		#[cfg(target_os = "linux")]
		if sound_module == "pipewire" {
			audio_in_devices = get_pulse_audio_in_devices();
		}

		return audio_in_devices;
	}

	{
		if let Ok(devices) = rodio::cpal::default_host().input_devices() {
			for device in devices {
				if let Ok(description) = device.description() {
					let is_input_device = if cfg!(target_os = "linux") {
						description.direction() == rodio::cpal::DeviceDirection::Input
					} else {
						description.supports_input()
					};
					let device_name = description.name().to_string();
					if !is_input_device || device_name == "" {
						continue;
					}

					let node_prefix = if description.supports_output() { MAME_AUDIO_OUTPUT_NODE_PREFIX } else { MAME_AUDIO_INPUT_NODE_PREFIX };
					let (hint, value) = if cfg!(any(target_os = "macos", target_os = "windows")) {
						match device.id() {
							Ok(device_id) => (device_name, node_prefix.to_owned() + device_id.id()),
							Err(_) => continue
						}
					} else {
						(device_name.clone(), node_prefix.to_owned() + &device_name)
					};

					if !audio_in_devices.iter().any(|(_, existing_value)| *existing_value == value) {
						audio_in_devices.push((hint, value));
					}
				}
			}
		}
	}

	audio_in_devices
}

#[cfg(target_os = "linux")]
fn get_pulse_audio_in_devices() -> Vec<(String, String)> {
	let mut audio_in_devices: Vec<(String, String)> = vec![];

	let pactl_output = match Command::new("pactl").env("LC_ALL", "C").arg("list").arg("sources").output() {
		Ok(pactl_output) if pactl_output.status.success() => String::from_utf8_lossy(&pactl_output.stdout).to_string(),
		_ => return audio_in_devices
	};

	// (description, is monitor)
	let mut sources: Vec<(String, bool)> = vec![];
	for line in pactl_output.lines() {
		let line = line.trim();
		if line.starts_with("Source #") {
			sources.push(("".into(), false));
		} else if let Some(source) = sources.last_mut() {
			if let Some(description) = line.strip_prefix("Description: ") {
				source.0 = description.trim().to_string();
			} else if let Some(monitor_of_sink) = line.strip_prefix("Monitor of Sink: ") {
				source.1 = source.1 || monitor_of_sink.trim() != "n/a";
			} else if line == "device.class = \"monitor\"" {
				source.1 = true;
			}
		}
	}

	for (description, is_monitor) in sources.iter() {
		let node_name = MAME_AUDIO_INPUT_NODE_PREFIX.to_owned() + description;
		if !is_monitor && description != "" && !audio_in_devices.iter().any(|(_, value)| *value == node_name) {
			audio_in_devices.push((description.clone(), node_name));
		}
	}

	audio_in_devices
}

fn get_video_in_devices() -> Vec<(String, String)> {
	#[allow(unused_mut)]
	let mut video_in_devices: Vec<(String, String)> = vec![];

	#[cfg(target_os = "linux")]
	{
		use std::os::unix::fs::FileTypeExt;

		let mut video_devices: Vec<std::path::PathBuf> = fs::read_dir("/dev")
			.into_iter()
			.flatten()
			.flatten()
			.filter(|entry| entry.file_name().to_string_lossy().starts_with("video"))
			.filter(|entry| entry.file_type().map(|file_type| file_type.is_char_device()).unwrap_or(false))
			.map(|entry| entry.path())
			.collect();
		video_devices.sort();

		for video_device in video_devices.iter() {
			let device_file_name = video_device.file_name().unwrap_or_default().to_string_lossy().to_string();
			let device_card_name = fs::read_to_string("/sys/class/video4linux/".to_owned() + &device_file_name + "/name")
				.unwrap_or("".into())
				.trim()
				.to_string();

			video_in_devices.push((device_card_name, video_device.to_string_lossy().to_string()));
		}
	}

	video_in_devices
}

fn get_serial_port_endpoints() -> Vec<HintedItem> {
	let mut serial_port_endpoints: Vec<HintedItem> = vec![];

	match serialport::available_ports() {
		Ok(ports) => {
			for (index, port) in ports.iter().enumerate() {
				let serial_port_type: String = match port.port_type {
					serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
					serialport::SerialPortType::PciPort => "PCI".into(),
					serialport::SerialPortType::UsbPort(_) => "USB".into(),
					_ => "".into()
				};

				serial_port_endpoints.push(
					HintedItem {
						display_hint_only: true,
						hint: (port.port_name.clone() + ": " + &(if serial_port_type != "" { "[".to_owned() + &serial_port_type + "] " } else { "".into() }) + "Serial Port " + &index.to_string()).into(),
						tooltip: (if serial_port_type != "" { "Serial Port Type: ".to_owned() + &serial_port_type } else { "".into() }).into(),
						value: (SERIAL_PORT_PREFIX.to_owned() + &port.port_name + "," + DEFAULT_SERIAL_PORT_OPTIONS).into(),
						icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
					}
				);
			}
		},
		_ => { }
	}

	serial_port_endpoints
}

fn populate_selected_box_bootroms(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool) -> Result<(usize, BuildStorageType, BuildStorageState), Box<dyn std::error::Error>> {
	let config_persistent_mame = config.persistent.mame_options.clone();

	let selected_box = selected_machine.name.clone().unwrap_or("".into());

	let mut selected_bootrom_index: usize = 0;
	let mut selected_bootrom = VerifiableBuildItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		status: "".into(),
		can_revert: false,
		hash: "".into(),
		build_storage_type: BuildStorageType::UnknownStorageType,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None,
	};

	let selected_bootrom_name = match config_persistent_mame.selected_bootroms {
		Some(bootroms) => {
			if bootroms.contains_key(&selected_box) {
				bootroms[&selected_box].clone()
			} else {
				"".into()
			}
		},
		_ => {
			"".into()
		}
	};

	let available_bootroms = match get_bootroms(config, selected_machine) {
		Ok(bootroms) => bootroms,
		Err(_e) => vec![]
	};
	for (index, bootrom) in available_bootroms.iter().enumerate() {
		if bootrom.value.to_string() == selected_bootrom_name {
			selected_bootrom = bootrom.clone();
			selected_bootrom_index = index;
		}
	}
	if available_bootroms.iter().count() > 0 && selected_bootrom.value == "" {
		selected_bootrom = available_bootroms[0].clone();
		selected_bootrom_index = 0;
	}

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_mame = ui.global::<UIMAMEOptions>();

		let selectable_bootroms: slint::VecModel<HintedItem> = Default::default();
		for available_bootrom in available_bootroms.iter() {
			selectable_bootroms.push(
				HintedItem {
					display_hint_only: false,
					hint: available_bootrom.hint.clone(),
					tooltip: available_bootrom.description.clone(),
					value: available_bootrom.value.clone(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}

		let image_name = if selected_box == "msntv2" {
			"BIOS".to_string()
		} else {
			"BootROM".to_string()
		};

		let image_file = if selected_box == "msntv2" {
			"bios.bin".to_string()
		} else {
			"bootrom.o".to_string()
		};

		ui_mame.set_selectable_bootroms(slint::ModelRc::new(slint::VecModel::from(selectable_bootroms)));
		if available_bootroms.iter().count() > 0 {
			ui.set_mame_broken(false);

			ui_mame.set_selected_bootrom(selected_bootrom.value.clone().into());
			ui_mame.set_selected_bootrom_index(selected_bootrom_index.clone() as i32);
			if selected_bootrom.value == "None" {
				ui_mame.set_bootrom_import_state(BuildImportState::ImportUnavailable);
			} else {
				ui_mame.set_bootrom_import_state(BuildImportState::WillReplace);
			}

			if !supress_warnings {
				match selected_bootrom.build_storage_state {
					BuildStorageState::UnknownBuildState => {
						ui.set_launcher_state_message(format!("Unknown {} state. Please choose a new {} file if it doesn't run!", image_name, image_file).into());
					},
					BuildStorageState::BuildLooksGood => {
						// No need to show a warning message for this.
					},
					BuildStorageState::FileNotFound => {
						ui.set_launcher_state_message(format!("The {} image doesn't exist. Please choose a {} file!", image_name, image_file).into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::RomSizeMismatch => {
						ui.set_launcher_state_message(format!("{} size mismatch! MAME will probably reject this {}!", image_name, image_name).into());
					},
					BuildStorageState::RomHashMismatch => {
						ui.set_launcher_state_message(format!("{} hash mismatch! MAME will probably reject this {}!", image_name, image_name).into());
					},
					BuildStorageState::StrippedFlashCyclopsed => {
						ui.set_launcher_state_message(format!("Found one {} flash file but couldn't find the other. Choosing a new {} file may fix this.", image_name, image_file).into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::StrippedFlashMissing => {
						ui.set_launcher_state_message(format!("Couldn't find a {}. The flash files may be missing? Choosing a new {} file may fix this.", image_name, image_file).into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CantReadBuild => {
						ui.set_launcher_state_message(format!("Error parsing {} image? Please choose a {} file!", image_name, image_file).into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CodeChecksumMismatch => {
						ui.set_launcher_state_message(format!("{} code checksum mismatch! Did you choose an image that's too large? Choose a new {} file if it doesn't run!", image_name, image_file).into());
					},
					BuildStorageState::RomfsChecksumMismatch => {
						ui.set_launcher_state_message(format!("{} ROMFS checksum mismatch! Did you choose an image that's too large? Choose a new {} file if it doesn't run!", image_name, image_file).into());
					}
					BuildStorageState::BadBaseAddress => {
						ui.set_launcher_state_message(format!("{} base address incorrect! Did you import an AppROM image as a BootROM? Choose a new {} file if it doesn't run!", image_name, image_file).into());
					}
					BuildStorageState::SignatureMismatch => {
						//
					}
				}
			}
		} else {
			ui_mame.set_selected_bootrom("".into());
			ui_mame.set_selected_bootrom_index(0);
			ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);

			if !supress_warnings {
				ui.set_launcher_state_message(format!("I asked MAME to list its usable {}s for this box and it gave me nothing! Broken MAME executable?", image_name).into());
			}
		}
	});

	Ok((selected_bootrom_index, selected_bootrom.build_storage_type.clone(), selected_bootrom.build_storage_state.clone()))
}

fn get_approm_build_flags(approm: &VerifiableBuildItem) -> u32 {
	approm.build_info.as_ref().map(|build_info| build_info.build_header.build_flags).unwrap_or(0)
}

fn set_selected_approm_build_flags(ui_mame: &UIMAMEOptions, selected_box: &str, build_flags: u32) {
	let is_msntv2 = selected_box == "msntv2";

	ui_mame.set_build_supports_wtvpointer(!is_msntv2 && (build_flags & BuildFlag::FARTAPIPointer) != 0x00);
	ui_mame.set_build_supports_wtvnet(!is_msntv2 && (build_flags & BuildFlag::FARTAPINet) != 0x00);
}

fn populate_selected_box_approms(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool, selected_bootrom_index: usize, selected_bootrom_type: BuildStorageType) -> Result<BuildStorageState, Box<dyn std::error::Error>> {
	let config_persistent_mame = config.persistent.mame_options.clone();
	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let selected_box = selected_machine.name.clone().unwrap_or("".into());

	let mut selected_approm = VerifiableBuildItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		can_revert: false,
		status: "".into(),
		hash: "".into(),
		build_storage_type: BuildStorageType::UnknownStorageType,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None,
	};

	let mut uses_disk_approms = false;
	let mut uses_mdoc_approms = false;
	let can_choose_hdimg;
	let can_revert_approm;

	for device in selected_machine.device.clone().unwrap_or(vec![]).iter() {
		if device.dtype.clone().unwrap_or("".to_string()) == "harddisk" {
			uses_disk_approms = true;
			break;
		}
	}

	let available_approms;
	if uses_disk_approms {
		if selected_machine.disk.iter().count() > 0 {
			match selected_machine.disk.clone() {
				Some(disks) => {
					let disk_name = disks[0].name.clone().unwrap_or("".into());

					if disks[0].modifiable.clone().unwrap_or("".into()) == "yes" {
						can_choose_hdimg = true;
					} else {
						can_choose_hdimg = !Path::new(&(mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &disk_name + ".chd")).exists();
					}
				},
				_ => {
					can_choose_hdimg = true;
				}
			}
		} else {
			can_choose_hdimg = true;
		}

		let selected_hdimg_path;
		if can_choose_hdimg {
			selected_hdimg_path = match config_persistent_mame.selected_hdimg_paths {
				Some(ref hdimg_paths) => {
					if hdimg_paths.contains_key(&selected_box) {
						if Path::new(&hdimg_paths[&selected_box]).exists() {
							hdimg_paths[&selected_box].clone()
						} else {
							"".to_string()
						}
					} else {
						"".to_string()
					}
				},
				_ => {
					"".to_string()
				}
			};
		} else {
			selected_hdimg_path = "".into();
		}

		available_approms = match get_disk_approms(config, &selected_machine, selected_hdimg_path) {
			Ok(approms) => approms,
			Err(_e) => vec![]
		};

		let selected_hdimg_enabled = match config_persistent_mame.selected_hdimg_enabled {
			Some(ref hdimg_enabled) => {
				if hdimg_enabled.contains_key(&selected_box) {
					hdimg_enabled[&selected_box].clone()
				} else {
					false
				}
			},
			_ => {
				false
			}
		};
		
		for approm in available_approms.iter() {
			let from_selected_hdimg = match Regex::new(r"^(?<name>.+?)\[(?<index>\d+?)\]").unwrap().captures(approm.value.as_str()) {
				Some(matches) => &matches["name"] == APPROM_HDIMG_PREFIX,
				_ => false
			};

			if approm.status == "selected" {
				// If we want to select the user hdimg then only check approms from the user hdimg.
				// Otherwise, only check from the preset image.
				if from_selected_hdimg && selected_hdimg_enabled || !from_selected_hdimg && !selected_hdimg_enabled {
					selected_approm = approm.clone();
				}
			}
		}
	} else {
		can_choose_hdimg = false;

		if selected_machine.device_ref.iter().count() > 0 {
			for device_ref in selected_machine.device_ref.clone().unwrap_or(vec![]).iter() {
				if device_ref.name.clone().unwrap_or("".to_string()) == "mdoc_collection" {
					uses_mdoc_approms = true;
					break;
				}
			}
		}

		if uses_mdoc_approms {
			available_approms = match get_flashdisk_approms(config, &selected_machine, selected_bootrom_index) {
				Ok(approms) => approms,
				Err(_e) => vec![]
			};

			for approm in available_approms.iter() {
				if approm.status == "selected" {
					selected_approm = approm.clone();
				}
			}
		} else {
			available_approms = match get_flash_approms(config, &selected_machine, selected_bootrom_index) {
				Ok(approms) => approms,
				Err(_e) => vec![]
			};
		}
	}

	// If we didn't find an approm to select above and there's more than one approm available then select the first.
	if selected_approm.value == "" && available_approms.iter().count() > 0 {
		selected_approm = available_approms[0].clone();
	}

	can_revert_approm = selected_approm.value != "" && selected_approm.can_revert;

	if selected_bootrom_type == BuildStorageType::MSNTV2BIOSBypassBuild && selected_approm.build_storage_state == BuildStorageState::SignatureMismatch {
		selected_approm.build_storage_state = BuildStorageState::BuildLooksGood;
	}

	let selected_approm_build_flags = if available_approms.iter().count() > 0 {
		get_approm_build_flags(&selected_approm)
	} else {
		0
	};

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {

		let ui_mame = ui.global::<UIMAMEOptions>();


		ui_mame.set_uses_disk_approms(uses_disk_approms);
		ui_mame.set_uses_mdoc_approms(uses_mdoc_approms);
		ui_mame.set_can_choose_hdimg(can_choose_hdimg);
		ui_mame.set_can_revert_approm(can_revert_approm);

		// Convert available approms into a list the UI can use.
		let selectable_approms: slint::VecModel<HintedItem> = Default::default();
		for available_approm in available_approms.iter() {
			selectable_approms.push(
				HintedItem {
					display_hint_only: false,
					hint: available_approm.hint.clone(),
					tooltip: available_approm.description.clone(),
					value: available_approm.value.clone(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}

		let image_name = if selected_box == "msntv2" {
			"OS".to_string()
		} else {
			"AppROM".to_string()
		};

		let image_file = if selected_box == "msntv2" {
			"hd.img".to_string()
		} else {
			"approm.o".to_string()
		};

		let pc_msntv2_disk = selected_box == "msntv2" && (selected_bootrom_type == BuildStorageType::PCBIOSBuild || selected_approm.build_storage_type == BuildStorageType::PCDiskOSBuild);

		ui_mame.set_selectable_approms(slint::ModelRc::new(slint::VecModel::from(selectable_approms)));
		let selectable_approm_build_flags: Vec<i32> = available_approms.iter().map(|available_approm| get_approm_build_flags(available_approm) as i32).collect();
		ui_mame.set_selectable_approm_build_flags(slint::ModelRc::new(slint::VecModel::from(selectable_approm_build_flags)));
		set_selected_approm_build_flags(&ui_mame, &selected_box, selected_approm_build_flags);
		if available_approms.iter().count() > 0 {
			ui_mame.set_selected_approm(selected_approm.value.clone().into());
			if selected_approm.value == "WinCE" || pc_msntv2_disk {
				ui_mame.set_approm_import_state(BuildImportState::ImportUnavailable);
			} else {
				ui_mame.set_approm_import_state(BuildImportState::WillReplace);
			}

			if !supress_warnings {
				match selected_approm.build_storage_state {
					BuildStorageState::UnknownBuildState => {
						ui.set_launcher_state_message(format!("Unknown {} state. Choose a new {} file if it doesn't run!", image_name, image_file).into());
					},
					BuildStorageState::BuildLooksGood => {
						// No need to show a warning message for this.
					},
					BuildStorageState::FileNotFound => {
						ui.set_launcher_state_message(format!("The {} image doesn't exist. Please choose a {} file!", image_name, image_file).into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::RomSizeMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::RomHashMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::StrippedFlashCyclopsed => {
						ui.set_launcher_state_message(format!("Found one {} flash file but couldn't find the other. Choosing a new {} file may fix this.", image_name, image_file).into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::StrippedFlashMissing => {
						ui.set_launcher_state_message(format!("Couldn't find an {}. The flash files may be missing? Choosing a new {} file may fix this.", image_name, image_file).into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CantReadBuild => {
						ui.set_launcher_state_message(format!("Error parsing {} image? Choose a new {} file if it doesn't run!", image_name, image_file).into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CodeChecksumMismatch => {
						ui.set_launcher_state_message(format!("{} code checksum mismatch! Did you choose an image that's too large? Choose a new {} file if it doesn't run!", image_name, image_file).into());
					},
					BuildStorageState::RomfsChecksumMismatch => {
						ui.set_launcher_state_message(format!("{} ROMFS checksum mismatch! Did you choose an image that's too large? Choose a new {} file if it doesn't run!", image_name, image_file).into());
					},
					BuildStorageState::BadBaseAddress => {
						ui.set_launcher_state_message(format!("{} base address incorrect! Did you choose an image for the wrong box? Choose a new {} file if it doesn't run!", image_name, image_file).into());
						// The case where they select a bfe approm for a bf0 bootrom or a bf0 approm for a bfe bootrom wil still break. Check for this case?
					},
					BuildStorageState::SignatureMismatch => {
						ui.set_launcher_state_message("Signature mismatch! Choose a different NK.BIN and BOOT.SIG or use the emac BIOS".into());
					},
				}
			}
		} else {
			ui_mame.set_selected_approm("".into());

			if uses_disk_approms {
				ui_mame.set_approm_import_state(BuildImportState::ImportUnavailable);

				if !supress_warnings {
					if can_choose_hdimg {
						ui.set_launcher_state_message(format!("No {}s available. Please choose a hard disk image!", image_name).into());
					} else {
						ui.set_launcher_state_message(format!("No {}s available. Unable to find a disk!", image_name).into());
					}
				}
			} else {
				ui_mame.set_approm_import_state(BuildImportState::WillCreate);

				if !supress_warnings {
					ui.set_launcher_state_message(format!("No {}s available. Please choose a new {}!", image_name, image_file).into());
				}
			}
		}

		ui_mame.set_selected_hdimg_path(
			match config_persistent_mame.selected_hdimg_paths {
				Some(ref hdimg_paths) => {
					if hdimg_paths.contains_key(&selected_box) {
						if Path::new(&hdimg_paths[&selected_box]).exists() {
							hdimg_paths[&selected_box].clone().into()
						} else {
							"".into()
						}
					} else {
						"".into()
					}
				},
				_ => "".into()
			}
		);

		ui_mame.set_selected_hdimg_enabled(
			match config_persistent_mame.selected_hdimg_enabled {
				Some(ref hdimg_enabled) => {
					if hdimg_enabled.contains_key(&selected_box) {
						hdimg_enabled[&selected_box].clone().into()
					} else {
						false
					}
				},
				_ => false
			}
		);
	});

	Ok(selected_approm.build_storage_state.clone())
}

fn populate_selected_box_ssids(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool, selected_bootrom_index: usize) -> Result<SSIDStorageState, Box<dyn std::error::Error>> {
	let mut selected_ssid = VerifiableSSIDItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		ssid_storage_state: SSIDStorageState::UnknownSSIDState,
		ssid_info: None,
		boxid_info: None
	};

	let available_ssids = match get_ssids(ui_weak, config, &selected_machine, selected_bootrom_index) {
		Ok(ssids) => ssids,
		Err(_e) => vec![]
	};
	for ssid in available_ssids.iter() {
		selected_ssid = ssid.clone();
		break;
	}

	let machine_name = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	if selected_ssid.value == "" && available_ssids.iter().count() > 0 {
		selected_ssid = available_ssids[0].clone();
	}

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_mame = ui.global::<UIMAMEOptions>();

		let selectable_ssids: slint::VecModel<HintedItem> = Default::default();
		for available_ssid in available_ssids.iter() {
			selectable_ssids.push(
				HintedItem {
					display_hint_only: false,
					hint: available_ssid.hint.clone(),
					tooltip: available_ssid.description.clone(),
					value: available_ssid.value.clone(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
			break;
		}

		let id_name = if machine_name == "msntv2" {
			"BoxID".to_string()
		} else {
			"SSID".to_string()
		};

		ui_mame.set_selectable_ssids(slint::ModelRc::new(slint::VecModel::from(selectable_ssids)));
		if available_ssids.iter().count() > 0 {
			ui_mame.set_ssid_in_file(selected_ssid.value.clone().into());
			ui_mame.set_selected_ssid(selected_ssid.value.clone().into());

			if !supress_warnings {
				match selected_ssid.ssid_storage_state {
					SSIDStorageState::UnknownSSIDState => {
						ui.set_launcher_state_message(format!("Unknown {} state. You can generate a new one below!", id_name).into());
					},
					SSIDStorageState::SSIDLooksGood => {
						// No need to show a warning message for this.
					},
					SSIDStorageState::FileNotFound => {
						ui.set_launcher_state_message(format!("{} file missing. You can generate a new one below!", id_name).into());
					},
					SSIDStorageState::ManufactureMismatch => {
						ui.set_launcher_state_message(format!("{} manufacture mismatch. You can generate a new one below!", id_name).into());
					},
					SSIDStorageState::CRCMismatch => {
						match selected_ssid.ssid_info.clone() {
							Some(ssid_info) => {
								let crc_hex: String = format!("{:02x}", (ssid_info.calculated_crc.clone() as i32)).into();
								ui.set_launcher_state_message(("SSID CRC mismatch (need ".to_owned() + &crc_hex + "). You can generate a new one below!").into());
							},
							_ => {
								match selected_ssid.boxid_info.clone() {
									Some(boxid_info) => {
										let crc_hex: String = format!("{:02x}", (boxid_info.calculated_crc.clone() as i32)).into();
										ui.set_launcher_state_message(("BoxID CRC mismatch (need ".to_owned() + &crc_hex + "). You can generate a new one below!").into());
									},
									_ => {
										ui.set_launcher_state_message(format!("{} CRC mismatch. You can generate a new one below!", id_name).into());
									}
								}
							}
						}
					},
					SSIDStorageState::FileCRCMismatch => {
						match selected_ssid.boxid_info.clone() {
							Some(boxid_info) => {
								let crc_hex: String = format!("{:02x}", (boxid_info.calculated_eeprom_crc.clone() as i32)).into();
								ui.set_launcher_state_message(("BoxID EEPROM CRC mismatch (need ".to_owned() + &crc_hex + "). You can generate a new one below!").into());
							},
							_ => {
								ui.set_launcher_state_message(format!("{} EEPROM CRC mismatch. You can generate a new one below!", id_name).into());
							}
						}
					},
					SSIDStorageState::BoxTypeMismatch => {
						//ui.set_launcher_state_message("SSID not a MAME SSID. You can generate a new one below!".into());
					},
					SSIDStorageState::CantReadSSID => {
						ui.set_launcher_state_message(format!("Error parsing {}? You can generate a new one below!", id_name).into());
					},
				}
			}
		} else {
			ui_mame.set_ssid_in_file("".into());
			ui_mame.set_selected_ssid("".into());

			if !supress_warnings {
				ui.set_launcher_state_message(format!("{} not found. You can generate a new one below!", id_name).into());
			}
		}

		let available_ssid_manufactures = SSIDManufacture::to_list(false, false);
		let mut selected_ssid_manufacture;
		match selected_ssid.ssid_info {
			Some(ssid_info) => {
				selected_ssid_manufacture = SSIDManufacture::to_item(ssid_info.manufacture);
			}
			None => {
				selected_ssid_manufacture = SSIDManufacture::to_item(SSIDManufacture::Generic);
			}
		}
		let selectable_ssid_manufactures: slint::VecModel<HintedItem> = Default::default();
		let mut ssid_manufacture_matched = false;
		let mut first_ssid_manufacture = SSIDManufacture::Generic;
		for available_ssid_manufacture in available_ssid_manufactures.iter() {
			if Regex::new(r"^wtv\d+sony$").unwrap().is_match(machine_name.as_str()) && available_ssid_manufacture.manufacture != SSIDManufacture::Sony {
				continue;
			} else if Regex::new(r"^wtv\d+phil$").unwrap().is_match(machine_name.as_str()) && available_ssid_manufacture.manufacture != SSIDManufacture::Phillips {
				continue;
			} else if selected_ssid_manufacture.manufacture == available_ssid_manufacture.manufacture {
				ssid_manufacture_matched = true;
			}

			if first_ssid_manufacture == SSIDManufacture::Generic {
				first_ssid_manufacture = available_ssid_manufacture.manufacture.clone();
			}

			selectable_ssid_manufactures.push(
				HintedItem {
					display_hint_only: false,
					hint: available_ssid_manufacture.name.clone().into(),
					tooltip: available_ssid_manufacture.description.clone().into(),
					value: available_ssid_manufacture.hex_value.clone().into(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}
		if !ssid_manufacture_matched || selected_ssid_manufacture.manufacture == SSIDManufacture::Generic {
			if first_ssid_manufacture != SSIDManufacture::Generic {
				selected_ssid_manufacture = SSIDManufacture::to_item(first_ssid_manufacture);
			} else {
				selected_ssid_manufacture = SSIDManufacture::to_item(SSIDManufacture::Sony);
			}
		}
		ui_mame.set_selectable_ssid_manufactures(slint::ModelRc::new(slint::VecModel::from(selectable_ssid_manufactures)));
		ui_mame.set_selected_ssid_manufacture(selected_ssid_manufacture.hex_value.into());
	});

	Ok(selected_ssid.ssid_storage_state.clone())
}

fn populate_selected_box_slots(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool) -> Result<bool, Box<dyn std::error::Error>> {
	let mut found_modem_slot = false;
	let mut selected_modem_startpoint: MachineSlotItem  = MachineSlotItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		name: "".into(),
		slot_name: "".into(),
		slot_type: SlotType::Unknown
	};
	let mut selected_debug_startpoint = MachineSlotItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		name: "".into(),
		slot_name: "".into(),
		slot_type: SlotType::Unknown
	};

	let mut available_slots = match get_slots(&selected_machine) {
		Ok(slots) => slots,
		Err(_e) => vec![]
	};

	let machine_name = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	// Reverse the slot order for the wld (italian) box so the Pekoe debug slot is selected first, modem slots have no effect since there's only one.
	if Regex::new(r"\d+wld$").unwrap().is_match(machine_name.as_str()) {
		available_slots.reverse();
	}

	// Softmodem boxes have a MoIP bridge, which lets the modem connect to a sip: endpoint.
	let mut selected_moip_startpoint = "".to_string();
	// Newer (wtv varient) MAME versions allow LIRC input. Allowing you to use a IR receiver direct with the guest OS in MAME.
	let mut selected_lirc_startpoint = "".to_string();
	for slot in available_slots.iter() {
		if slot.slot_type == SlotType::ModemSerial && selected_modem_startpoint.slot_type == SlotType::Unknown {
			selected_modem_startpoint = slot.clone();
		} else if slot.slot_type == SlotType::DebugSerial && selected_debug_startpoint.slot_type == SlotType::Unknown {
			selected_debug_startpoint = slot.clone();
		} else if slot.slot_type == SlotType::MoIPBridge && selected_moip_startpoint == "" {
			selected_moip_startpoint = slot.value.to_string();
		} else if slot.slot_type == SlotType::LIRCDevice && selected_lirc_startpoint == "" {
			selected_lirc_startpoint = slot.value.to_string();
		}
	}

	if selected_moip_startpoint != "" {
		found_modem_slot = true;
	}

	// WebTV port and parallel port slot options
	let wtvport_options: Vec<(String, String)> = if WTVPORT_BOXES.contains(&machine_name.as_str()) {
		get_slot_options(config, selected_machine, WTVPORT_SLOT_NAME)
	} else {
		vec![]
	};
	let can_use_wtvport = wtvport_options.len() > 0;
	// wtv1 boxes don't have a parallel port, it's added with the printer adapter.
	let wtvport_has_parallel = can_use_wtvport && machine_name.starts_with("wtv1");
	let parallel_options = if machine_name.starts_with("wtv1") || machine_name.starts_with("wtv2") {
		get_slot_options(config, selected_machine, PARALLEL_SLOT_NAME)
	} else {
		vec![]
	};
	let has_onboard_parallel = parallel_options.len() > 0 && machine_name.starts_with("wtv2");

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_mame = ui.global::<UIMAMEOptions>();

		// This handles cases where we need to point the modem or debug serial selection into.
		// For example, on the wtv2wld box it has two debug points but we can only use one at a time
		// So this allows you to select which debug port to use.
		// It is never more than one for the modem case but the code is here in case we add one.

		let selected_modem_bitb_startpoint = ui_mame.get_selected_modem_bitb_startpoint().to_string();
		let mut selected_modem_bitb_startpoint_valid = false;
		let selected_debug_bitb_startpoint = ui_mame.get_selected_debug_bitb_startpoint().to_string();
		let mut selected_debug_bitb_startpoint_valid = false;

		let mut selectable_modem_bitb_startpoints: Vec<HintedItem> = vec![];
		let mut selectable_debug_bitb_startpoints: Vec<HintedItem> = vec![];
		for available_slot in available_slots.iter() {
			if available_slot.slot_type == SlotType::ModemSerial {
				if available_slot.value == selected_modem_bitb_startpoint {
					selected_modem_bitb_startpoint_valid = true;
				}

				found_modem_slot = true;
				selectable_modem_bitb_startpoints.push(
					HintedItem {
						display_hint_only: false,
						hint: available_slot.hint.clone().into(),
						tooltip: "".into(),
						value: available_slot.value.clone().into(),
						icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
					}
				);
			} else if available_slot.slot_type == SlotType::DebugSerial {
				if available_slot.value == selected_debug_bitb_startpoint {
					selected_debug_bitb_startpoint_valid = true;
				}

				selectable_debug_bitb_startpoints.push(
					HintedItem {
						display_hint_only: false,
						hint: available_slot.hint.clone().into(),
						tooltip: "".into(),
						value: available_slot.value.clone().into(),
						icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
					}
				);
			}
		}

		// In practice there's only one modem startpoint but this allows more than one. Validate the startpoints before setting the startpoint value.
		ui_mame.set_selectable_modem_bitb_startpoints(slint::ModelRc::new(slint::VecModel::from(selectable_modem_bitb_startpoints)));
		if selected_modem_startpoint.slot_type == SlotType::ModemSerial && !selected_modem_bitb_startpoint_valid {
			ui_mame.set_selected_modem_bitb_startpoint(selected_modem_startpoint.value.clone().into());
		}

		// There's only one -moip startpoint so no need to validate it against a known list. The right value is forced.
		ui_mame.set_selected_modem_moip_startpoint(selected_moip_startpoint.into());

		// Most of the time there's only one debug startpoint but in cases like wtv2wld that has two debug ports, we validate it against the list before setting the startpoint value.
		ui_mame.set_selectable_debug_bitb_startpoints(slint::ModelRc::new(slint::VecModel::from(selectable_debug_bitb_startpoints)));
		if selected_debug_startpoint.slot_type == SlotType::DebugSerial && !selected_debug_bitb_startpoint_valid {
			ui_mame.set_selected_debug_bitb_startpoint(selected_debug_startpoint.value.clone().into());
		}

		// There's only one -lirc startpoint so no need to validate it against a know list. The right value is forced.
		ui_mame.set_selected_lirc_startpoint(selected_lirc_startpoint.into());

		// WebTV port
		let selected_wtvport_option = ui_mame.get_selected_wtvport_option().to_string();
		let mut selectable_wtvport_options: Vec<HintedItem> = vec![];
		for (option_name, option_description) in wtvport_options.iter() {
			selectable_wtvport_options.push(
				HintedItem {
					display_hint_only: false,
					hint: option_description.strip_prefix("WebTV Port ").unwrap_or(option_description).to_string().into(),
					tooltip: "".into(),
					value: option_name.clone().into(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}
		if !wtvport_options.iter().any(|(option_name, _)| *option_name == selected_wtvport_option) {
			ui_mame.set_selected_wtvport_option(wtvport_options.first().map(|(option_name, _)| option_name.clone()).unwrap_or("".into()).into());
		}
		ui_mame.set_selectable_wtvport_options(slint::ModelRc::new(slint::VecModel::from(selectable_wtvport_options)));
		ui_mame.set_can_use_wtvport(can_use_wtvport);
		ui_mame.set_wtvport_has_parallel(wtvport_has_parallel);

		// Parallel port
		let selected_parallel_option = ui_mame.get_selected_parallel_option().to_string();
		let mut selectable_parallel_options: Vec<HintedItem> = vec![];
		for (option_name, option_description) in parallel_options.iter() {
			selectable_parallel_options.push(
				HintedItem {
					display_hint_only: false,
					hint: option_description.clone().into(),
					tooltip: "".into(),
					value: option_name.clone().into(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}
		if !parallel_options.iter().any(|(option_name, _)| *option_name == selected_parallel_option) {
			ui_mame.set_selected_parallel_option(parallel_options.first().map(|(option_name, _)| option_name.clone()).unwrap_or("".into()).into());
		}
		ui_mame.set_selectable_parallel_options(slint::ModelRc::new(slint::VecModel::from(selectable_parallel_options)));
		ui_mame.set_has_onboard_parallel(has_onboard_parallel);

		if !supress_warnings {
			if !found_modem_slot {
				ui.set_launcher_state_message("I asked MAME to list its usable modems for this box and it gave me nothing! Broken MAME executable?".into());
			}
		}
	});

	Ok(found_modem_slot)
}

fn populate_selected_box_config(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_box: &String) -> Result<(), Box<dyn std::error::Error>> {
	let config_mame: config::MAMEDocument = config.mame.clone();

	for machine in config_mame.machine.unwrap_or(vec![]).iter() {
		let machine_name = 
			machine.name
			.clone()
			.unwrap_or("".into());
		if machine_name == *selected_box {
			//
			// Populate UI with bootroms for the selected box
			//
			let supress_bootrom_warnings = false;
			
			let (selected_bootrom_index, selected_bootrom_type, selected_bootrom_state) = match populate_selected_box_bootroms(ui_weak, config, machine, supress_bootrom_warnings) {
				Ok((selected_bootrom_index, selected_bootrom_type, selected_bootrom_state)) => (selected_bootrom_index, selected_bootrom_type, selected_bootrom_state),
				Err(_e) => (0, BuildStorageType::UnknownStorageType, BuildStorageState::UnknownBuildState)
			};

			//
			// Populate UI with approms for the selected box
			//
			// Only one warning can be displayed at a time so bootrom warnings take precedence (if we have a bad bootrom, nothing will boot).
			let supress_approm_warnings = supress_bootrom_warnings || selected_bootrom_state != BuildStorageState::BuildLooksGood;
			let selected_approm_state = match populate_selected_box_approms(ui_weak, config, machine, supress_approm_warnings, selected_bootrom_index, selected_bootrom_type) {
				Ok(selected_approm_state) => selected_approm_state,
				Err(_e) => BuildStorageState::UnknownBuildState
			};

			//
			// Populate UI with SSIDs for the selected box
			//
			// Only show if the BootROM and AppROM states are good.
			let supress_ssid_warnings = supress_approm_warnings || selected_approm_state != BuildStorageState::BuildLooksGood;
			let selected_ssid_state = match populate_selected_box_ssids(ui_weak, config, machine, supress_ssid_warnings, selected_bootrom_index) {
				Ok(selected_ssid_state) => selected_ssid_state,
				Err(_e) => SSIDStorageState::UnknownSSIDState
			};

			//
			// Populate UI with slots (modem and debug serial endpoints) for the selected box
			//
			// Only show warnings if the BootROM, AppROM and SSID states are good.
			let supress_slot_warnings = supress_ssid_warnings || selected_ssid_state != SSIDStorageState::SSIDLooksGood;
			let found_modem_slot = match populate_selected_box_slots(ui_weak, config, machine, supress_slot_warnings) {
				Ok(_) => true,
				Err(_e) => false
			};

			if !supress_ssid_warnings && found_modem_slot {
				let mut can_connect = true;
				for feature in machine.clone().feature.unwrap_or(vec![]).iter() {
					if feature.ftype.clone().unwrap_or("".into()) == "lan" && feature.status.clone().unwrap_or("".into()) == "unemulated" {
						can_connect = false;
						break;
					}
				}

				if !can_connect {
					let _ = ui_weak.upgrade_in_event_loop(move |ui| {
						ui.set_launcher_state_message("The modem in the box isn't emulated. You can't connect to any remote service.".into());
					});
				}
			}
		}
	}

	Ok(())
}

pub fn get_machine_id(machine_name: &str) -> Option<MachineID> {
	let wtv_re: Regex = Regex::new(r"^wtv(?<box_iteration>\d+)(?<box_name>.+)").unwrap();

	if let Some(caps) = wtv_re.captures(machine_name) {
        Some(MachineID {
            box_iteration: caps.name("box_iteration")?.as_str().to_string(),
            box_name: caps.name("box_name")?.as_str().to_string(),
        })
    } else if let Some(box_name) = machine_name.strip_prefix("msn") {
        Some(MachineID {
            box_iteration: "3".to_string(),
            box_name: box_name.to_string(),
        })
    } else {
        None
    }
}

fn populate_config(ui_weak: &slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	enable_loading(&ui_weak, "Loading...".into());
	
	let config = LauncherConfig::new().unwrap();
	let config_mame = config.mame.clone();
	let config_persistent_paths = config.persistent.paths.clone();
	let config_persistent_mame = config.persistent.mame_options.clone();
	let mame_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());

	// Host device lookups can be slow so do them before jumping into the UI thread.
	let audio_in_sound_module = get_audio_in_sound_module(&mame_path);
	// Older MAME versions don't have -parent_window
	let mame_supports_parent_window = LauncherConfig::mame_supports_option(&mame_path, "parent_window");
	// wtvmame FART API options
	let mame_supports_wtvpointer = LauncherConfig::mame_supports_option(&mame_path, "wtvpointer");
	let mame_supports_wtvnet = LauncherConfig::mame_supports_option(&mame_path, "wtvnet");
	let mame_supports_sm_record = LauncherConfig::mame_supports_option(&mame_path, "sm_record");
	let audio_in_devices = get_audio_in_devices(audio_in_sound_module.as_deref().unwrap_or(""));
	let video_in_devices = get_video_in_devices();

	let ui_weak_cpy = ui_weak.clone();
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_paths = ui.global::<UIPaths>();
		let ui_mame = ui.global::<UIMAMEOptions>();
		
		ui.set_launcher_state_message("".into());

		////
		//
		// Setup->Paths
		//
		////

		let mut python_path: String = config_persistent_paths.python_path.unwrap_or("".into());
		if python_path == "" {
			match which("python") {
				Ok(pathbuf_path) => {
					match pathbuf_path.to_str() {
						Some(path) => {
							python_path = path.into();
						},
						_ => {}
					}
				}
				_ => {
					match which("python3") {
						Ok(pathbuf_path) => {
							match pathbuf_path.to_str() {
								Some(path) => {
									python_path = path.into();
								},
								_ => {}
							}
						}
						_ => { }
					}
				}
			}
		}

		ui_paths.set_mame_path(config_persistent_paths.mame_path.unwrap_or("".into()).into());
		ui_paths.set_python_path(python_path.into());
		ui_paths.set_rommy_path(config_persistent_paths.rommy_path.unwrap_or("".into()).into());
		ui_paths.set_last_opened_exe_path(config_persistent_paths.last_opened_exe_path.unwrap_or("".into()).into());
		ui_paths.set_last_opened_rom_path(config_persistent_paths.last_opened_rom_path.unwrap_or("".into()).into());
		ui_paths.set_last_opened_img_path(config_persistent_paths.last_opened_img_path.unwrap_or("".into()).into());

		////
		//
		// Start->Selected Modem
		//
		////

		let mut selectable_modem_bitb_endpoints: Vec<HintedItem> = vec![
			HintedItem {
				display_hint_only: false,
				hint: "Public TouchPPP Server".into(),
				tooltip: "".into(),
				value: PUBLIC_TOUCHPP_ADDRESS.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			},
			HintedItem {
				display_hint_only: false,
				hint: "Local TouchPPP Server".into(),
				tooltip: "".into(),
				value: "127.0.0.1:1122".into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		];

		// Boxes with a MoIP bridge can use the same endpoints as above or a sip: endpoint.
		let mut selectable_modem_moip_box_endpoints = selectable_modem_bitb_endpoints.clone();
		selectable_modem_moip_box_endpoints.push(
			HintedItem {
				display_hint_only: false,
				hint: "Redialed SIP".into(),
				tooltip: "".into(),
				value: "sip:redialed:redialed2@sip.webtv.zone:16555".into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		);
		selectable_modem_moip_box_endpoints.push(
			HintedItem {
				display_hint_only: false,
				hint: "eMac's Public SIP".into(),
				tooltip: "".into(),
				value: "sip:hacktv:hacktv@moip.mywire.org:16151".into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		);

		for serial_port_endpoint in get_serial_port_endpoints().into_iter() {
			selectable_modem_bitb_endpoints.push(serial_port_endpoint.clone());
			selectable_modem_moip_box_endpoints.push(serial_port_endpoint);
		}
		ui_mame.set_selectable_modem_bitb_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_modem_bitb_endpoints)));
		// Restore even if it's not in the list since it could have been typed in manually.
		let selected_modem_bitb_endpoint = config_persistent_mame.selected_modem_bitb_endpoint.unwrap_or("".into());
		if selected_modem_bitb_endpoint != "" {
			ui_mame.set_selected_modem_bitb_endpoint(selected_modem_bitb_endpoint.clone().into());
		}

		ui_mame.set_selectable_modem_moip_box_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_modem_moip_box_endpoints)));
		let selected_modem_moip_endpoint = config_persistent_mame.selected_modem_moip_endpoint.unwrap_or("".into());
		if selected_modem_moip_endpoint != "" {
			ui_mame.set_selected_modem_moip_endpoint(selected_modem_moip_endpoint.clone().into());
		}

		////
		//
		// Start->Selected Debug Endpoint
		//
		////

		let mut selectable_debug_bitb_endpoints: Vec<HintedItem> = vec![
			HintedItem {
				display_hint_only: false,
				hint: "".into(),
				tooltip: "".into(),
				value: DEFAULT_DEBUG_ENDPOINT.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			},
			HintedItem {
				display_hint_only: false,
				hint: "Local ser2net".into(),
				tooltip: "".into(),
				value: "127.0.0.1:2000".into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		];
		selectable_debug_bitb_endpoints.extend(get_serial_port_endpoints());
		ui_mame.set_selectable_debug_bitb_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_debug_bitb_endpoints.clone())));
		let selected_debug_bitb_endpoint = config_persistent_mame.selected_debug_bitb_endpoint.unwrap_or("".into());
		for selectable_debug_bitb_endpoint in selectable_debug_bitb_endpoints.iter() {
			if selected_debug_bitb_endpoint == *selectable_debug_bitb_endpoint.value {
				ui_mame.set_selected_debug_bitb_endpoint(selected_debug_bitb_endpoint.clone().into());
			}
		}

		////
		//
		// Start->Selected LIRC Endpoint
		//
		////

		let mut selectable_lirc_endpoints: Vec<HintedItem> = vec![ ];
		#[cfg(target_os = "linux")]
		{
			use std::os::unix::fs::FileTypeExt;

			let mut lirc_devices: Vec<std::path::PathBuf> = fs::read_dir("/dev")
				.into_iter()
				.flatten()
				.flatten()
				.filter(|entry| entry.file_name().to_string_lossy().starts_with("lirc"))
				.filter(|entry| entry.file_type().map(|file_type| file_type.is_char_device()).unwrap_or(false))
				.map(|entry| entry.path())
				.collect();
			lirc_devices.sort();

			for (index, lirc_device) in lirc_devices.iter().enumerate() {
				selectable_lirc_endpoints.push(
					HintedItem {
						display_hint_only: false,
						hint: ("LIRC Device ".to_owned() + &index.to_string()).into(),
						tooltip: "".into(),
						value: lirc_device.to_string_lossy().to_string().into(),
						icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
					}
				);
			}
		}
		// A server like this doesn't exist as far as I know but can be built for non-Linux platforms.
		// A WinLIRC server exists @ 8765 but it does not send MODE2 data.
		selectable_lirc_endpoints.push(
			HintedItem {
				display_hint_only: false,
				hint: "Local MODE2".into(),
				tooltip: "".into(),
				value: "127.0.0.1:8766".to_string().into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		);
		ui_mame.set_selectable_lirc_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_lirc_endpoints.clone())));
		let selected_lirc_endpoint = config_persistent_mame.selected_lirc_endpoint.unwrap_or("".into());
		let mut selected_lirc_endpoint_valid = false;
		for selectable_lirc_endpoint in selectable_lirc_endpoints.iter() {
			if selected_lirc_endpoint == *selectable_lirc_endpoint.value {
				selected_lirc_endpoint_valid = true;
				break;
			}
		}
		if selected_lirc_endpoint_valid {
			ui_mame.set_selected_lirc_endpoint(selected_lirc_endpoint.clone().into());
		} else {
			ui_mame.set_selected_lirc_endpoint(selectable_lirc_endpoints[0].value.clone().into());
		}

		////
		//
		// Start->Ports and Devices
		//
		////

		// Audio In
		ui_mame.set_audio_in_available(audio_in_sound_module.is_some());
		ui_mame.set_mame_supports_parent_window(mame_supports_parent_window);
		ui_mame.set_mame_supports_wtvpointer(mame_supports_wtvpointer);
		ui_mame.set_mame_supports_wtvnet(mame_supports_wtvnet);
		ui_mame.set_mame_supports_sm_record(mame_supports_sm_record);
		ui_mame.set_audio_in_sound_module(audio_in_sound_module.clone().unwrap_or("".into()).into());
		let mut selectable_audio_in_endpoints: Vec<HintedItem> = vec![
			HintedItem {
				display_hint_only: false,
				hint: "Default Audio Input".into(),
				tooltip: "".into(),
				value: DEFAULT_CAPTURE_DEVICE.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		];
		for (audio_in_hint, audio_in_value) in audio_in_devices.iter() {
			selectable_audio_in_endpoints.push(
				HintedItem {
					display_hint_only: true,
					hint: audio_in_hint.clone().into(),
					tooltip: "".into(),
					value: audio_in_value.clone().into(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}
		ui_mame.set_selectable_audio_in_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_audio_in_endpoints)));
		let mut selected_audio_in_endpoint = config_persistent_mame.selected_audio_in_endpoint.clone().unwrap_or("".into());
		if !audio_in_devices.iter().any(|(_, value)| *value == selected_audio_in_endpoint) {
			let prefixed_endpoint = MAME_AUDIO_INPUT_NODE_PREFIX.to_owned() + &selected_audio_in_endpoint;
			if audio_in_devices.iter().any(|(_, value)| *value == prefixed_endpoint) {
				selected_audio_in_endpoint = prefixed_endpoint;
			}
		}
		ui_mame.set_selected_audio_in_endpoint(if selected_audio_in_endpoint != "" { selected_audio_in_endpoint.into() } else { DEFAULT_CAPTURE_DEVICE.into() });

		// Video In
		let mut selectable_video_in_endpoints: Vec<HintedItem> = vec![
			HintedItem {
				display_hint_only: false,
				hint: "Default Capture Device".into(),
				tooltip: "".into(),
				value: DEFAULT_CAPTURE_DEVICE.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		];
		for (video_in_hint, video_in_value) in video_in_devices.iter() {
			selectable_video_in_endpoints.push(
				HintedItem {
					display_hint_only: false,
					hint: video_in_hint.clone().into(),
					tooltip: "".into(),
					value: video_in_value.clone().into(),
					icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
				}
			);
		}
		ui_mame.set_selectable_video_in_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_video_in_endpoints)));
		let selected_video_in_endpoint = config_persistent_mame.selected_video_in_endpoint.clone().unwrap_or("".into());
		ui_mame.set_selected_video_in_endpoint(if selected_video_in_endpoint != "" { selected_video_in_endpoint.into() } else { DEFAULT_CAPTURE_DEVICE.into() });

		// WebTV SmartCard and CA SmartCard
		let selectable_smartcard_endpoints: Vec<HintedItem> = get_serial_port_endpoints();
		let default_card_endpoint: String = selectable_smartcard_endpoints.first().map(|item| item.value.to_string()).unwrap_or("".into());
		ui_mame.set_selectable_smartcard_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_smartcard_endpoints)));
		let selected_smartcard_endpoint = config_persistent_mame.selected_smartcard_endpoint.clone().unwrap_or("".into());
		ui_mame.set_selected_smartcard_endpoint(if selected_smartcard_endpoint != "" { selected_smartcard_endpoint.into() } else { default_card_endpoint.clone().into() });
		let selected_cacard_endpoint = config_persistent_mame.selected_cacard_endpoint.clone().unwrap_or("".into());
		ui_mame.set_selected_cacard_endpoint(if selected_cacard_endpoint != "" { selected_cacard_endpoint.into() } else { default_card_endpoint.clone().into() });

		// WebTV Port and Parallel Port
		ui_mame.set_selected_wtvport_option(config_persistent_mame.selected_wtvport_option.clone().unwrap_or("".into()).into());
		ui_mame.set_selected_parallel_option(config_persistent_mame.selected_parallel_option.clone().unwrap_or("".into()).into());
		#[allow(unused_mut)]
		let mut selectable_parallel_socket_endpoints: Vec<HintedItem> = vec![
			HintedItem {
				display_hint_only: false,
				hint: "Local TOOT server".into(),
				tooltip: "".into(),
				value: DEFAULT_PARALLEL_SOCKET_ENDPOINT.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			},
			HintedItem {
				display_hint_only: false,
				hint: "Debug Link (builtin)".into(),
				tooltip: "Runs WebTV's 1997 parallel port debug link in the launcher.".into(),
				value: DEBUG_LINK_SOCKET_ENDPOINT.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		];
		#[cfg(webtv_bridge)]
		selectable_parallel_socket_endpoints.push(
			HintedItem {
				display_hint_only: false,
				hint: "JoeB bridge (builtin)".into(),
				tooltip: "Runs the WebTV Bridge firmware in the launcher. The box goes online through this computer's network.".into(),
				value: WEBTV_BRIDGE_SOCKET_ENDPOINT.into(),
				icon: slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0))
			}
		);
		ui_mame.set_selectable_parallel_socket_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_parallel_socket_endpoints)));
		let selected_parallel_socket_endpoint = config_persistent_mame.selected_parallel_socket_endpoint.clone().unwrap_or("".into());
		ui_mame.set_selected_parallel_socket_endpoint(if selected_parallel_socket_endpoint != "" { selected_parallel_socket_endpoint.into() } else { DEFAULT_PARALLEL_SOCKET_ENDPOINT.into() });

		////
		//
		// Start->Options
		//
		////

		ui_mame.set_verbose_mode(config_persistent_mame.verbose_mode.unwrap_or(false).into());
		ui_mame.set_use_drc(config_persistent_mame.use_drc.unwrap_or(true).into());
		ui_mame.set_debug_mode(config_persistent_mame.debug_mode.unwrap_or(false).into());
		ui_mame.set_validate_build_image(config_persistent_mame.validate_build_image.unwrap_or(true).into());
		ui_mame.set_console_input(config_persistent_mame.console_input.unwrap_or(false).into());
		ui_mame.set_disable_sound(config_persistent_mame.disable_sound.unwrap_or(false).into());
		let windowing_mode = config_persistent_mame.windowing_mode.clone().unwrap_or("captured".into());
		ui_mame.set_windowing_mode(match windowing_mode.as_str() {
			"captured" | "window" | "fullscreen" => windowing_mode.into(),
			_ => "captured".into()
		});
		if let Some(captured_console_height) = config_persistent_mame.captured_console_height {
			ui_mame.set_captured_console_height(captured_console_height);
		}
		ui_mame.set_captured_window_width(config_persistent_mame.captured_window_width.unwrap_or(0.0));
		ui_mame.set_captured_window_height(config_persistent_mame.captured_window_height.unwrap_or(0.0));
		ui_mame.set_use_decoupled_sound(config_persistent_mame.use_decoupled_sound.unwrap_or(false).into());
		ui_mame.set_softmodem_record(config_persistent_mame.softmodem_record.unwrap_or(false).into());
		ui_mame.set_enable_wtvpointer(config_persistent_mame.enable_wtvpointer.unwrap_or(true).into());
		ui_mame.set_enable_wtvnet(config_persistent_mame.enable_wtvnet.unwrap_or(true).into());
		ui_mame.set_enable_lirc(config_persistent_mame.enable_lirc.unwrap_or(false).into());
		ui_mame.set_enable_audio_in(config_persistent_mame.enable_audio_in.unwrap_or(false).into());
		ui_mame.set_enable_video_in(config_persistent_mame.enable_video_in.unwrap_or(false).into());
		ui_mame.set_enable_smartcard(config_persistent_mame.enable_smartcard.unwrap_or(false).into());
		ui_mame.set_enable_cacard(config_persistent_mame.enable_cacard.unwrap_or(false).into());
		ui_mame.set_enable_wtvport(config_persistent_mame.enable_wtvport.unwrap_or(false).into());
		ui_mame.set_enable_parallel(config_persistent_mame.enable_parallel.unwrap_or(false).into());

		ui_mame.set_custom_options(config_persistent_mame.custom_options.unwrap_or("".into()).into());
		ui_mame.set_show_advanced_options(config_persistent_mame.show_advanced_options.unwrap_or(false).into());
	});

	////
	//
	// Start->Selected box
	//
	////

	struct SortableHintedItem {
		pub hint: slint::SharedString,
		pub tooltip: slint::SharedString,
		pub value: slint::SharedString,
		pub icon: String,
		pub sort_value: String
	}

	let mut boxes: Vec<SortableHintedItem> = vec![];
	let mut selected_box: String = "".into();
	if mame_path != "" {
		for machine in config_mame.machine.unwrap_or(vec![]).iter() {
			if machine.runnable.clone().unwrap_or("".into()) != "no" {
				let machine_name = 
					machine.name
					.clone()
					.unwrap_or("".into());
				let machine_description =
					machine.description
					.clone()
					.unwrap_or("".into());

				let mut can_connect = true;
				for feature in machine.clone().feature.unwrap_or(vec![]).iter() {
					if feature.ftype.clone().unwrap_or("".into()) == "lan" && feature.status.clone().unwrap_or("".into()) == "unemulated" {
						can_connect = false;
						break;
					}
				}

				if Regex::new(r"^(wtv\d+[a-zA-Z0-9]+|msn[a-zA-Z0-9]{3})$").unwrap().is_match(machine_name.as_str()) {
					let mut sort_value: String = String::new();

					let mut icon_path = "".to_string();

					match get_machine_id(machine_name.as_str()) {
						Some(machine_id) => {
							// This sets up sorting so the wtv1 boxes are first, wtv2 boxes are second and sony and phillips boxes come before any other box type.

							sort_value.push_str("0");
							sort_value.push_str(&machine_id.box_iteration);

							if machine_id.box_name == *"tv2" {
								icon_path = "ui/images/box-icons/msntv2".to_string();
							} else if machine_id.box_name == *"utv" || machine_id.box_name == *"uvd" {
								icon_path = "ui/images/box-icons/utv".to_string();
							} else if machine_id.box_name == *"wld" {
								icon_path = "ui/images/box-icons/wld".to_string();
							} else if machine_id.box_name == *"bfe" {
								icon_path = "ui/images/box-icons/bfe".to_string();
							} else if machine_id.box_iteration == *"1" {
								icon_path = "ui/images/box-icons/wtv1".to_string();
							} else if machine_id.box_iteration == *"2" {
								icon_path = "ui/images/box-icons/wtv2".to_string();
							}

							if icon_path != "" {
								if !can_connect {
									icon_path += "-nc.svg";
								} else {
									icon_path += ".svg";
								}
							}

							#[cfg(target_os = "macos")]
							{
								let executable_dir = 
									LauncherConfig::get_parent_from_pathbuf(env::current_exe()
										.unwrap_or("".into()))
										.unwrap_or("".into());

								icon_path = executable_dir + "/" + &icon_path;
							}


							if machine_id.box_name == *"sony" {
								sort_value.push_str("0");
							} else if machine_id.box_name == *"phil" {
								sort_value.push_str("1");
							} else {
								sort_value.push_str("2");
								sort_value.push_str(&machine_id.box_name);
							}
						}
						None => {
						}
					}

					boxes.push(
						SortableHintedItem {
							hint: machine_description.clone().into(),
							tooltip: "".into(),
							value: machine_name.clone().into(),
							icon: icon_path.into(),
							sort_value: sort_value.clone().into(),
						}
					);

					if config_persistent_mame.selected_box == machine_name.clone().into() {
						selected_box = machine_name.into();
					}
				}
			}
		}
		boxes.sort_by(
			| cmp_a, cmp_b | {
				let a_preset_order = DEFAULT_BOX_ORDER.iter().position(|&val| val == cmp_a.value.as_str()).unwrap_or(usize::MAX);
				let b_preset_order = DEFAULT_BOX_ORDER.iter().position(|&val| val == cmp_b.value.as_str()).unwrap_or(usize::MAX);

				if a_preset_order == b_preset_order {
					if cmp_a.sort_value == cmp_b.sort_value {
						cmp_a.hint.partial_cmp(&cmp_b.hint).unwrap()
					} else {
						cmp_a.sort_value.partial_cmp(&cmp_b.sort_value).unwrap()
					}
				} else {
					a_preset_order.cmp(&b_preset_order)
				}
			}
		);
	}


	if boxes.iter().count() > 0 {
		if selected_box == "" {
			selected_box = boxes[0].value.clone().into();
		}

		let selected_box_cpy = selected_box.clone();
		let _ = ui_weak.upgrade_in_event_loop(move |ui| {
			let ui_mame = ui.global::<UIMAMEOptions>();

			ui.set_mame_broken(false);
			
			let selectable_boxes: slint::VecModel<HintedItem> = Default::default();
			for selectable_box in boxes.iter() {
				selectable_boxes.push(
					HintedItem {
						display_hint_only: false,
						hint: selectable_box.hint.clone(),
						tooltip: selectable_box.tooltip.clone(),
						value: selectable_box.value.clone(),
						icon: match selectable_box.icon.as_str() {
							"" => slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0)),
							icon_path => slint::Image::load_from_path(Path::new(&icon_path)).unwrap_or(slint::Image::from_rgb8(slint::SharedPixelBuffer::new(0, 0)))
						}
					}
				);
			}
	
			ui_mame.set_selectable_boxes(slint::ModelRc::new(slint::VecModel::from(selectable_boxes)));

			ui_mame.set_selected_box(selected_box_cpy.clone().into());
		});

		let ui_weak_copy = ui_weak.clone();
		let _ = populate_selected_box_config(&ui_weak_copy, &config, &selected_box);

		let _ = check_rommy(ui_weak_cpy);
	} else {
		let _ = ui_weak.upgrade_in_event_loop(move |ui| {
			let ui_mame = ui.global::<UIMAMEOptions>();

			ui.set_mame_broken(true);
			if mame_path == "" {
				ui.set_launcher_state_message("No MAME executable! Please setup the path to your WebTV MAME executable.".into());
			} else {
				if Path::new(&mame_path).exists() {
					ui.set_launcher_state_message("I asked MAME to list WebTV boxes and it gave me nothing! Broken MAME executable?".into());
				} else {
					ui.set_launcher_state_message("MAME executable not found! Please setup the correct path to your WebTV MAME executable.".into());
				}
			}

			let empty_bootroms: slint::VecModel<HintedItem> = Default::default();
			ui_mame.set_selectable_bootroms(slint::ModelRc::new(slint::VecModel::from(empty_bootroms)));
			let empty_approms: slint::VecModel<HintedItem> = Default::default();
			ui_mame.set_selectable_approms(slint::ModelRc::new(slint::VecModel::from(empty_approms)));
			let empty_ssids: slint::VecModel<HintedItem> = Default::default();
			ui_mame.set_selectable_ssids(slint::ModelRc::new(slint::VecModel::from(empty_ssids)));
		});
	}

	disable_loading(&ui_weak);

	Ok(())
}

fn load_config(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let _ = std::thread::spawn(move || {
		let _ = populate_config(&ui_weak);
	});

	Ok(())
}

fn check_custom_ssid(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui_weak_cpy = ui_weak.clone();
	let _ = ui_weak_cpy.upgrade_in_event_loop(move |ui| {
		let stored_ssid = ui.global::<UIMAMEOptions>().get_ssid_in_file().to_string();
		let requested_ssid = ui.global::<UIMAMEOptions>().get_selected_ssid().to_string();
		let selected_box = ui.global::<UIMAMEOptions>().get_selected_box().to_string();

		if stored_ssid != requested_ssid {
			if selected_box == "msntv2" {
				match MSNTV2BoxIdInfo::new_from_string(requested_ssid) {
					Ok(boxid_info) => {
						let _ = save_boxid(boxid_info.raw, ui_weak.clone(), false, false);
					},
					_ => {

					}
				}
			} else {
				match hex::decode(requested_ssid) {
					Ok(raw_data) => {
						let mut raw_ssid: [u8; 0x08] = [0x00; 0x08];

						for (index, byte) in raw_data.iter().enumerate() {
							raw_ssid[index] = *byte;

							if index >= 7 {
								break;
							}
						}

						let _ = save_ssid(raw_ssid, ui_weak.clone(), true, false);
					}
					_ => { }
				}
			}
		}
	});

	Ok(())
}

fn save_boxid(raw_box_id: [u8; 8], ui_weak: slint::Weak<MainWindow>, is_blocking: bool, silent: bool) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_mame = ui.global::<UIMAMEOptions>();
	let selected_box = ui_mame.get_selected_box().to_string();
	let selected_bootrom_index: usize = ui_mame.get_selected_bootrom_index() as usize;

	save_boxid_for_box(raw_box_id, selected_box, selected_bootrom_index, ui_weak, is_blocking, silent)
}

fn save_boxid_for_box(raw_box_id: [u8; 8], selected_box: String, selected_bootrom_index: usize, ui_weak: slint::Weak<MainWindow>, is_blocking: bool, silent: bool) -> Result<(), Box<dyn std::error::Error>> {
	let save_thread = std::thread::spawn(move || {
		if !silent {
			enable_loading(&ui_weak, "Saving".into());
		}

		let config = LauncherConfig::new().unwrap();

		let config_persistent_paths = config.persistent.paths.clone();
		let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
		let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

		let ssid_file: String = MSNTV2_ETHERNET_EEPROM_FILE.into();

		let ssid_directory_path: String;
		if selected_bootrom_index > 0 {
			ssid_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string();
		} else {
			ssid_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box;
		}
		let ssid_file_path = ssid_directory_path.clone() + "/" + &ssid_file.clone();

		match std::fs::create_dir_all(ssid_directory_path) {
			Ok(_) => {
				let mut eeprom_blob: Option<[u8; 128]> = if !MSNTV2_ALWAYS_RESET_EEPROM && Path::new(&ssid_file_path).exists() {
					match File::open(&ssid_file_path) {
						Ok(mut f) => {
							let mut raw_eeprom = [0x00; 128];

							match f.read(&mut raw_eeprom) {
								Ok(_) => {
									match MSNTV2BoxIdInfo::new_from_eeprom(raw_eeprom) {
										Ok(mut boxid_info) => {
											let _ = boxid_info.set_box_id(raw_box_id, true);

											Some(boxid_info.eeprom_blob)
										},
										_ => {
											None
										}
									}
								},
								Err(_) => {
									None
								}
							}
						},
						Err(_) => {
							None
						}
					}
				} else {
					None
				};

				if eeprom_blob == None {
					eeprom_blob = match MSNTV2BoxIdInfo::new(raw_box_id) {
						Ok(boxid_info) => {
							Some(boxid_info.eeprom_blob)
						}
						_ => {
							None
						}
					};
				}

				if eeprom_blob != None {
					match File::create(ssid_file_path) {
						Ok(mut f) => {
							let _ = f.write(&eeprom_blob.unwrap_or([0x00; 128]));
						},
						_ => {
							// Problem creating destination.
						}
					};
				}
			},
			_ => {
				// Problem creating destination path.
			}
		}

		if !silent {
			disable_loading(&ui_weak);

			let _ = save_config(ui_weak.clone(), true, None, None, None);
		}
	});

	if is_blocking {
		let _ = save_thread.join();
	}

	Ok(())
}

fn save_ssid(raw_ssid: [u8; 0x08], ui_weak: slint::Weak<MainWindow>, is_blocking: bool, silent: bool) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_mame = ui.global::<UIMAMEOptions>();
	let selected_box = ui_mame.get_selected_box().to_string();

	save_ssid_for_box(raw_ssid, selected_box, ui_weak, is_blocking, silent)
}

fn save_ssid_for_box(raw_ssid: [u8; 0x08], selected_box: String, ui_weak: slint::Weak<MainWindow>, is_blocking: bool, silent: bool) -> Result<(), Box<dyn std::error::Error>> {
	let save_thread = std::thread::spawn(move || {
		if !silent {
			enable_loading(&ui_weak, "Saving".into());
		}

		let config = LauncherConfig::new().unwrap();

		let config_persistent_paths = config.persistent.paths.clone();
		let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
		let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

		let mut ssid_file: String = SSID_ROM_FILE.into();

		let config_mame = config.mame.clone();
		for machine in config_mame.machine.unwrap_or(vec![]).iter() {
			let machine_name = 
				machine.name
				.clone()
				.unwrap_or("".into());

			if machine_name == selected_box {
				for rom in machine.clone().rom.unwrap_or(vec![]).iter() {
					let rom_region = 
						rom.region
						.clone()
						.unwrap_or("".into());
			
					if rom_region == "serial_id" {
						ssid_file = 
							rom.name
							.clone()
							.unwrap_or(SSID_ROM_FILE.into());
			
						break;
					}
				}
			}
		}

		let ssid_directory_path = mame_directory_path.clone() + "/roms/" + &selected_box;
		let ssid_file_path = ssid_directory_path.clone() + "/" + &ssid_file.clone();

		match std::fs::create_dir_all(ssid_directory_path) {
			Ok(_) => {
				match File::create(ssid_file_path) {
					Ok(mut f) => {
						let _ = f.write(&raw_ssid);
					},
					_ => {
						// Problem creating destination.
					}
				};
			},
			_ => {
				// Problem creating destination path.
			}
		}

		if !silent {
			disable_loading(&ui_weak);

			let _ = save_config(ui_weak.clone(), true, None, None, None);
		}
	});

	if is_blocking {
		let _ = save_thread.join();
	}

	Ok(())
}

fn import_bootrom(source_path: String, ui_weak: slint::Weak<MainWindow>, remove_source: bool) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
		let ui_weak = ui.as_weak();

		let ui_mame = ui.global::<UIMAMEOptions>();
		let selected_box = ui_mame.get_selected_box().to_string();
		let try_bootrom_file: String = ui_mame.get_selected_bootrom().into();

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Saving".into());

			let config = LauncherConfig::new().unwrap();

			let config_persistent_paths = config.persistent.paths.clone();
			let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
			let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());


			let mut bootrom_file: String = "".into();
			let mut bootrom_collation = BuildIODataCollation::Raw;
			let bootrom_rom_size = if selected_box == "wtv2jpc" || selected_box == "wtv2jpp" {
				0x400000
			} else if selected_box == "msntv2" {
				0x100000
			} else {
				0x200000
			};

			let mut bootroms: Vec<String> = vec![];

			let config_mame = config.mame.clone();
			for machine in config_mame.machine.unwrap_or(vec![]).iter() {
				let machine_name = 
					machine.name
					.clone()
					.unwrap_or("".into());

				if machine_name == selected_box {
					let mut biossets: HashMap<_, _> = HashMap::new();

					for biosset in machine.clone().biosset.unwrap_or(vec![]).iter() {
						let biosset_name = 
							biosset.name
							.clone()
							.unwrap_or("".into()); 
						let biosset_description = 
							biosset.description
							.clone()
							.unwrap_or("".into());
				
						if selected_box == "msntv2" {
							biossets.insert(biosset_name + ".bin", biosset_description);
						} else {
							biossets.insert(biosset_name + ".o", biosset_description);
						}
					}

					for rom in machine.clone().rom.unwrap_or(vec![]).iter() {
						let rom_name = 
							rom.name
							.clone()
							.unwrap_or("".into());

						let rom_region = 
							rom.region
							.clone()
							.unwrap_or("".into());

						if rom_name != *SSID_ROM_FILE && rom_region != "serial_id" {
							if !biossets.contains_key(&rom_name) && rom_name != DEFAULT_BOOTORM_FILE_NAME && rom_name != DEFAULT_MSNTV2_BOOTORM_FILE_NAME {
								continue;
							}

							if rom_name == try_bootrom_file {
								bootrom_file = try_bootrom_file.clone();
							}
							

							bootroms.push(rom_name);
						}
					}
				}
			}


			if bootrom_file == "" && bootroms.iter().count() > 0 {
				bootrom_file = bootroms[0].clone();
			}

			let mut bootrom_directory_path: String = "".into();
			let mut bootrom_file_path: String = "".into();

			let is_dev_box = Regex::new(r"^wtv\d+dev$").unwrap().is_match(selected_box.as_str());

			if bootrom_file == "" && is_dev_box {
				bootrom_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box;
				bootrom_file_path = bootrom_directory_path.clone() + "/" + &bootrom_file.clone() + "/" + &BOOTROM_FLASH_FILE_PREFIX;
				bootrom_collation = BuildIODataCollation::StrippedROMs;
			} else if bootrom_file != "" {
				bootrom_directory_path = mame_directory_path.clone() + "/roms/" + &selected_box;
				bootrom_file_path = bootrom_directory_path.clone() + "/" + &bootrom_file.clone();
				bootrom_collation = BuildIODataCollation::Raw;
			}

			if bootrom_directory_path != "" && bootrom_file_path != "" {
				match std::fs::create_dir_all(bootrom_directory_path) {
					Ok(_) => {
						match ROMIO::create(bootrom_file_path, Some(bootrom_collation), bootrom_rom_size) {
							Ok(mut destf) => {
								match File::open(source_path.clone()) {
									Ok(mut srcf) => {
										let mut buffer: Vec<u8> = vec![0x00; bootrom_rom_size as usize];

										let _ = srcf.read(&mut buffer);
										let _ = destf.write(&mut buffer);

										if remove_source {
											match std::fs::remove_file(source_path.clone()) {
												_ => { }
											};
										}
									},
									_ => {
										// Problem opening source.
									}
								};
							},
							_ => {
								// Problem opening destination.
							}
						};
					},
					_ => {
						// Problem creating destination path.
					}
				}
			}

			disable_loading(&ui_weak);

			let _ = save_config(ui_weak.clone(), true, None, None, None);
		});
	});

	Ok(())
}

fn set_disk_selected_approm(selected_box: &String, file_path: &String, selected_index: u8, validate_build_image: bool) -> Result<(), Box<dyn std::error::Error>> {
	let disk_collation = match selected_box.as_str() {
		"msntv2" => BuildIODataCollation::Raw,
		"wtv2utv" | "wtv2uvd" => BuildIODataCollation::ByteSwapped1632,
		 _ => BuildIODataCollation::ByteSwapped16
	};

	match BuildMeta::open_disk(file_path.to_string(), Some(disk_collation), validate_build_image, false) {
		Ok(mut buildmeta) => {
			if buildmeta.selected_build_index != selected_index {
				let _ = buildmeta.set_selected_build_index(selected_index);
			}
		},
		_ => {
			// IDEA: create the disk image with parition table and boot partition.
			// Problem opening destination.
		}
	};

	Ok(())
}

fn import_flash_approm(config: &LauncherConfig, selected_box: &String, selected_bootrom_index: usize, source_data: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
	let config_persistent_paths = config.persistent.paths.clone();
	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let approm_directory_path: String;
	if selected_bootrom_index > 0 {
		approm_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string();
	} else {
		approm_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box;
	}

	let approm_file_path = approm_directory_path.clone() + "/" + APPROM1_FLASH_FILE_PREFIX;
	let approm_collation = BuildIODataCollation::StrippedROMs;
	let approm_rom_size;

	let is_dev_box = Regex::new(r"^wtv\d+dev$").unwrap().is_match(selected_box.as_str());
	let is_pal_box = Regex::new(r"^wtv\d+pal$").unwrap().is_match(selected_box.as_str());

	if is_dev_box || is_pal_box {
		approm_rom_size = 4 * 1024 * 1204;
	} else {
		approm_rom_size = 2 * 1024 * 1204;
	}

	if approm_directory_path != "" && approm_file_path != "" {
		match std::fs::create_dir_all(approm_directory_path) {
			Ok(_) => {
				match ROMIO::create(approm_file_path, Some(approm_collation), approm_rom_size) {
					Ok(mut destf) => {
						let _ = destf.seek(0);
						let _ = destf.write(source_data);
					},
					_ => {
						// Problem opening destination.
					}
				};
			},
			_ => {
				// Problem creating destination path.
			}
		}
	}

	Ok(())
}

fn import_msntv2_os(hdimg_path: &String, file_path: &String) -> Result<(), Box<dyn std::error::Error>> {
	let config = LauncherConfig::new().unwrap();

	let mut target_hdimg_path= (*hdimg_path).clone();

	if hdimg_path == "" {
		for machine in config.mame.machine.unwrap_or(vec![]).iter() {
			let machine_name = 
				machine.name
				.clone()
				.unwrap_or("".into());

			if machine_name == "msntv2" {
				if machine.disk.iter().count() > 0 {
					match machine.disk.clone() {
						Some(disks) => {
							let disk_name = disks[0].name.clone().unwrap_or("".into());
							let disk_file = disk_name.clone() + ".chd";

							let config_persistent_paths = config.persistent.paths.clone();
							let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
							let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());
			
							let preset_img_path = mame_directory_path.clone() + "/roms/" + &machine_name + "/" + &disk_file;

							target_hdimg_path = preset_img_path.clone();
						},
						_ => {
							//
						}
					};
				}
			}
		}
	}

	let dir_path = LauncherConfig::get_parent(file_path.clone()).unwrap_or("".into());

	if dir_path == "" {
		return Err("Couldn't find parent directory of imported image!".into());
	}

	// IDEA: create the disk image with MBR and FAT partition.
	let mut hdimg_buildmeta = BuildMeta::open_disk(target_hdimg_path, Some(BuildIODataCollation::Raw), false, false)
		.map_err(|_| "Couldn't open disk image")?;

	let msntv2_info = BuildMeta::read_local_msntv2disk_info(dir_path.clone(), false, false, false)
		.map_err(|_| "Couldn't find or parse BOOT.SIG files")?;

	if !msntv2_info.all_files_exist {
		return Err("All files in BOOT.SIG (NK.BIN etc...) must be in the selected directory".into());
	}

	let boot_fs = hdimg_buildmeta.open_msntv2_boot_fs()?;
	let root = boot_fs.root_dir();

	let file_names = msntv2_info.build_files.iter()
		.map(|build_file| build_file.file_name.as_str())
		.filter(|file_name| !file_name.eq_ignore_ascii_case("BOOT.SIG"))
		.chain(std::iter::once("BOOT.SIG"));

	for file_name in file_names {
		let local_path = BuildMeta::find_local_file(Path::new(&dir_path), file_name).ok_or(format!("{} not found", file_name))?;
		let mut local_file = fs::File::open(local_path)?;

		let mut hdimg_file = root.create_file(file_name)?;
		hdimg_file.truncate()?;
		std::io::copy(&mut local_file, &mut hdimg_file)?;
		hdimg_file.flush()?;
	}

	drop(root);
	boot_fs.unmount()?;

	Ok(())
}

fn import_disk_approm(selected_box: &String, file_path: &String, source_data: &mut Vec<u8>, validate_build_image: bool) -> Result<(), Box<dyn std::error::Error>> {
	let disk_collation = match selected_box.as_str() {
		"msntv2" => BuildIODataCollation::Raw,
		"wtv2utv" | "wtv2uvd" => BuildIODataCollation::ByteSwapped1632,
		 _ => BuildIODataCollation::ByteSwapped16
	};

	match BuildMeta::open_disk(file_path.to_string(), Some(disk_collation), validate_build_image, false) {
		Ok(mut buildmeta) => {
			let _ = buildmeta.write_build(source_data);
		},
		_ => {
			// Problem opening destination.
		}
	};

	Ok(())
}

fn import_flashdisk_approm(config: &LauncherConfig, selected_box: &String, selected_bootrom_index: usize, size: u64, source_data: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
	let config_persistent_paths = config.persistent.paths.clone();
	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let disk_directory_path: String;
	if selected_bootrom_index > 0 {
		disk_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string();
	} else {
		disk_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box;
	}

	let disk_file_path = disk_directory_path.clone() + "/mdoc_flash0";

	let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

	if Path::new(&disk_file_path).exists() {
		match BuildMeta::open_flashdisk(disk_file_path, Some(BuildIODataCollation::Raw), validate_build_image) {
			Ok(mut buildmeta) => {
				let _ = buildmeta.write_build(source_data);
			},
			_ => {
				// Problem opening destination.
			}
		};
	} else if disk_directory_path != "" && disk_file_path != "" {
		match std::fs::create_dir_all(disk_directory_path) {
			Ok(_) => {
				match FlashdiskIO::create(disk_file_path, Some(BuildIODataCollation::Raw), size) {
					Ok(io) => {
						match BuildMeta::new(io, Some(BuildMetaLayout::FlashdiskLayout), validate_build_image, false) {
							Ok(mut buildmeta) => {
								let _ = buildmeta.write_build(source_data);
							},
							_ => {
								// Problem opening destination.
							}
						};
					},
					_ => {
						// Problem opening destination.
					}
				};
			},
			_ => {
				// Problem creating destination path.
			}
		}
	}

	Ok(())
}

fn get_flashdisk_size(selected_machine: &MAMEMachineNode) -> Result<usize, Box<dyn std::error::Error>> {
	if selected_machine.device_ref.iter().count() > 0 {
		for device_ref in selected_machine.device_ref.clone().unwrap_or(vec![]).iter() {
			let device_ref_name = device_ref.name.clone().unwrap_or("".to_string());
			if device_ref_name == "mdoc_2810_0016" {
				return Ok(16 * 1024 * 1024);
			} else if device_ref_name == "mdoc_2810_0008" {
				return Ok(8 * 1024 * 1024);
			} else if device_ref_name == "mdoc_2810_0004" {
				return Ok(4 * 1024 * 1024);
			} else if device_ref_name == "mdoc_2810_0002" {
				return Ok(2 * 1024 * 1024);
			}
		}
	}

	Ok(DEFAULT_FLASHDISK_SIZE as usize)
}

fn import_approm(source_path: String, ui_weak: slint::Weak<MainWindow>, remove_source: bool, correct_checksums: bool) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
		let ui_weak = ui.as_weak();

		let ui_mame = ui.global::<UIMAMEOptions>();
		let selected_box = ui_mame.get_selected_box().to_string();
		let selected_bootrom_index: usize = ui_mame.get_selected_bootrom_index() as usize;

		let uses_disk_approms = ui_mame.get_uses_disk_approms();
		let uses_mdoc_approms = ui_mame.get_uses_mdoc_approms();

		let selected_hdimg_path: String = ui_mame.get_selected_hdimg_path().to_string();
		let selected_hdimg_enabled = ui_mame.get_selected_hdimg_enabled();

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Saving".into());

			let config = LauncherConfig::new().unwrap();

			if selected_box == "msntv2" {
				let msntv2_selhdimg_path = if selected_hdimg_enabled { selected_hdimg_path } else { "".to_string() };

				if let Err(e) = import_msntv2_os(&msntv2_selhdimg_path, &source_path) {
					let message = e.to_string();

					disable_loading(&ui_weak);

					let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
						ui.set_launcher_state_message(message.into());
					});

					return;
				}
			} else {
				match File::open(source_path.clone()) {
					Ok(mut srcf) => {
						let source_size = srcf.metadata().unwrap().len();

						if source_size > 0 {
							// Don't read more than 64MB
							let mut source_data: Vec<u8> = vec![0x00; source_size.max(64 * 1024 * 1024) as usize];

							let _ = srcf.seek(SeekFrom::Start(0));
							let _ = srcf.read(&mut source_data);

							// This serves as a convenience like it does in my WebTV Disk Editor.
							// On import it fixes the checksum automatically so the build will boot.
							if correct_checksums {
								match BuildMeta::open_rom(source_path.clone(), None, correct_checksums) {
									Ok(build_meta) => {
										let correct_code_checksum = build_meta.build_info[0].calculated_code_checksum;
										let correct_romfs_checksum = build_meta.build_info[0].calculated_romfs_checksum;
										let romfs_offset = build_meta.build_info[0].romfs_offset as usize;

										if correct_code_checksum != 0x00000000 {
											source_data[0x08] = ((correct_code_checksum >> 0x18) & 0xff) as u8;
											source_data[0x09] = ((correct_code_checksum >> 0x10) & 0xff) as u8;
											source_data[0x0a] = ((correct_code_checksum >> 0x08) & 0xff) as u8;
											source_data[0x0b] = ((correct_code_checksum >> 0x00) & 0xff) as u8;
										}
			
										if correct_romfs_checksum != 0x00000000 && romfs_offset > 0x00 && romfs_offset <= (source_size as usize) {
											source_data[romfs_offset - 0x04] = ((correct_romfs_checksum >> 0x18) & 0xff) as u8;
											source_data[romfs_offset - 0x03] = ((correct_romfs_checksum >> 0x10) & 0xff) as u8;
											source_data[romfs_offset - 0x02] = ((correct_romfs_checksum >> 0x08) & 0xff) as u8;
											source_data[romfs_offset - 0x01] = ((correct_romfs_checksum >> 0x00) & 0xff) as u8;
										}
									},
									_ => { }
								};
							}

							if uses_disk_approms {
								if selected_hdimg_enabled && selected_hdimg_path != "" {
									let _ = import_disk_approm(&selected_box, &selected_hdimg_path, &mut source_data, correct_checksums);
								} else {
									for machine in config.mame.machine.unwrap_or(vec![]).iter() {
										let machine_name = 
											machine.name
											.clone()
											.unwrap_or("".into());

										if machine_name == *selected_box {
											if machine.disk.iter().count() > 0 {
												match machine.disk.clone() {
													Some(disks) => {
														let disk_name = disks[0].name.clone().unwrap_or("".into());
														let disk_file = disk_name.clone() + ".chd";

														let config_persistent_paths = config.persistent.paths.clone();
														let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
														let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());
										
														let preset_img_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &disk_file;

														let _ = import_disk_approm(&selected_box, &preset_img_path, &mut source_data, correct_checksums);
													},
													_ => {
														//
													}
												};
											}
										}
									}
								}
							} else if uses_mdoc_approms {
								for machine in config.clone().mame.machine.unwrap_or(vec![]).iter() {
									let machine_name = 
										machine.name
										.clone()
										.unwrap_or("".into());

									if machine_name == *selected_box {
										let flashdisk_size = match get_flashdisk_size(&machine) {
											Ok(flashdisk_size) => flashdisk_size as u64,
											_ => DEFAULT_FLASHDISK_SIZE
										};

										let _ = import_flashdisk_approm(&config, &selected_box, selected_bootrom_index, flashdisk_size, &mut source_data);
									}
								}
							} else {
								let _ = import_flash_approm(&config, &selected_box, selected_bootrom_index, &mut source_data);
							}
						}

						if remove_source {
							match std::fs::remove_file(source_path.clone()) {
								_ => { }
							};
						}
					},
					_ => {
						// Problem opening source.
					}
				};
			}

			disable_loading(&ui_weak);

			let _ = save_config(ui_weak.clone(), true, None, None, None);
		});
	});

	Ok(())
}

fn save_config(ui_weak: slint::Weak<MainWindow>, reload: bool, new_bootroms: Option<HashMap<String, String>>, new_hdimg_paths: Option<HashMap<String, String>>, new_hdimg_enabled: Option<HashMap<String, bool>>) -> Result<(), Box<dyn std::error::Error>> {
	enable_loading(&ui_weak.clone(), "Saving Config".into());

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_paths = ui.global::<UIPaths>();
		let ui_mame = ui.global::<UIMAMEOptions>();

		let selected_bootroms = match new_bootroms {
			Some(bootroms) => {
				Some(bootroms)
			},
			_ => {
				 match LauncherConfig::get_persistent_config() {
					Ok(config) => {
						config.mame_options.selected_bootroms
					},
					_ => {
						Some(HashMap::new())
					}
				}
			}
		};

		let selected_hdimg_paths = match new_hdimg_paths {
			Some(hdimg_paths) => {
				Some(hdimg_paths)
			},
			_ => {
				 match LauncherConfig::get_persistent_config() {
					Ok(config) => {
						config.mame_options.selected_hdimg_paths
					},
					_ => {
						Some(HashMap::new())
					}
				}
			}
		};

		let selected_hdimg_enabled = match new_hdimg_enabled {
			Some(hdimg_enabled) => {
				Some(hdimg_enabled)
			},
			_ => {
				 match LauncherConfig::get_persistent_config() {
					Ok(config) => {
						config.mame_options.selected_hdimg_enabled
					},
					_ => {
						Some(HashMap::new())
					}
				}
			}
		};

		let network = match LauncherConfig::get_persistent_config() {
			Ok(config) => config.network,
			_ => None
		}.map(NetworkOptions::with_defaults).or(Some(NetworkOptions::default_options()));

		let new_config = PersistentConfig {
			paths: Paths {
				mame_path: Some(ui_paths.get_mame_path().into()),
				python_path: Some(ui_paths.get_python_path().into()),
				rommy_path: Some(ui_paths.get_rommy_path().into()),
				last_opened_exe_path: Some(ui_paths.get_last_opened_exe_path().into()),
				last_opened_rom_path: Some(ui_paths.get_last_opened_rom_path().into()),
				last_opened_img_path: Some(ui_paths.get_last_opened_img_path().into())
			},
			mame_options: MAMEOptions {
				selected_box: Some(ui_mame.get_selected_box().into()),
				selected_bootroms: selected_bootroms,
				selected_modem_bitb_endpoint: Some(ui_mame.get_selected_modem_bitb_endpoint().into()),
				selected_modem_moip_endpoint: Some(ui_mame.get_selected_modem_moip_endpoint().into()),
				selected_debug_bitb_endpoint: Some(ui_mame.get_selected_debug_bitb_endpoint().into()),
				selected_lirc_endpoint: Some(ui_mame.get_selected_lirc_endpoint().into()),
				selected_hdimg_paths: selected_hdimg_paths,
				selected_hdimg_enabled: selected_hdimg_enabled,
				verbose_mode: Some(ui_mame.get_verbose_mode().into()),
				use_drc: Some(ui_mame.get_use_drc().into()),
				debug_mode: Some(ui_mame.get_debug_mode().into()),
				validate_build_image: Some(ui_mame.get_validate_build_image().into()),
				console_input: Some(ui_mame.get_console_input().into()),
				disable_sound: Some(ui_mame.get_disable_sound().into()),
				windowing_mode: Some(ui_mame.get_windowing_mode().into()),
				captured_console_height: Some(ui_mame.get_captured_console_height()),
				captured_window_width: if ui_mame.get_captured_window_width() > 0.0 { Some(ui_mame.get_captured_window_width()) } else { None },
				captured_window_height: if ui_mame.get_captured_window_height() > 0.0 { Some(ui_mame.get_captured_window_height()) } else { None },
				use_decoupled_sound: Some(ui_mame.get_use_decoupled_sound().into()),
				softmodem_record: Some(ui_mame.get_softmodem_record().into()),
				enable_wtvpointer: Some(ui_mame.get_enable_wtvpointer().into()),
				enable_wtvnet: Some(ui_mame.get_enable_wtvnet().into()),
				enable_lirc: Some(ui_mame.get_enable_lirc().into()),
				selected_audio_in_endpoint: Some(ui_mame.get_selected_audio_in_endpoint().into()),
				enable_audio_in: Some(ui_mame.get_enable_audio_in().into()),
				selected_video_in_endpoint: Some(ui_mame.get_selected_video_in_endpoint().into()),
				enable_video_in: Some(ui_mame.get_enable_video_in().into()),
				selected_smartcard_endpoint: Some(ui_mame.get_selected_smartcard_endpoint().into()),
				enable_smartcard: Some(ui_mame.get_enable_smartcard().into()),
				selected_cacard_endpoint: Some(ui_mame.get_selected_cacard_endpoint().into()),
				enable_cacard: Some(ui_mame.get_enable_cacard().into()),
				selected_wtvport_option: Some(ui_mame.get_selected_wtvport_option().into()),
				enable_wtvport: Some(ui_mame.get_enable_wtvport().into()),
				selected_parallel_option: Some(ui_mame.get_selected_parallel_option().into()),
				selected_parallel_socket_endpoint: Some(ui_mame.get_selected_parallel_socket_endpoint().into()),
				enable_parallel: Some(ui_mame.get_enable_parallel().into()),
				custom_options: Some(ui_mame.get_custom_options().into()),
				show_advanced_options: Some(ui_mame.get_show_advanced_options().into())
			},
			network: network
		};

		let _ = LauncherConfig::save_persistent_config(&new_config); // May freeze up UI

		if reload {
			let _ = load_config(ui.as_weak());
		}
	});

	disable_loading(&ui_weak.clone());

	Ok(())
}

fn choose_executable_file(ui_weak: slint::Weak<MainWindow>) -> Result<String, Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_paths = ui.global::<UIPaths>();

	let last_opened_exe_path = get_choose_file_location(ui_paths.get_last_opened_exe_path().into());

	let chooser: FileDialogBuilder;

	chooser = 
		DialogBuilder::file()
		.set_location(&last_opened_exe_path)
		.set_filename("");


	let selected_file_pathbuf = chooser.open_single_file().show().unwrap_or(None);

	let mut file_path: String = "".into();
	if selected_file_pathbuf != None {
		let mut selected_file_path: String = "".into();

		match selected_file_pathbuf {
			Some(path) => {
				match path.to_str() {
					Some(path_str) => {
						selected_file_path = path_str.into();
					},
					_ => { }
				}


			},
			_ => { }
		}

		if selected_file_path != "" && Path::new(&selected_file_path).exists() {
			ui_paths.set_last_opened_exe_path(LauncherConfig::get_parent(selected_file_path.clone()).unwrap_or("".into()).into());

			file_path = selected_file_path.clone();
		}
	}

	Ok(file_path)
}

fn get_rommy_file(in_file_path: String, python_path: String, rommy_path: String) -> Result<String, Box<dyn std::error::Error>> {
	let mut image_path: String = "".into();

	match env::temp_dir().to_str() {
		Some(tmp_dir) => {
			let selected_file_directory = LauncherConfig::get_parent(in_file_path.clone()).unwrap_or("".into());

			let random_file_name = Alphanumeric.sample_string(&mut rand::rng(), 16) + ".o";
			let random_file_path = tmp_dir.to_owned() + "/" + &random_file_name;

			let mut command = Command::new(python_path);

			command.arg(rommy_path).arg(selected_file_directory).arg(random_file_path.clone());

			#[cfg(target_os = "windows")]
			command.creation_flags(0x08000000); // CREATE_NO_WINDOW

			match command.output() {
				Ok(_) => {
					if Path::new(&random_file_path).exists() {
						image_path = random_file_path.clone();
					} else {
						// Rommy didn't produce a .o file?
						// Can generate specific error here.
					}
				},
				_ => {
					// Problem running rommy?
					// Can generate specific error here.
				}
			}
		},
		_ => {
			// For some reason I couldn't get the tmp dir string?;
			// Can generate specific error here.
		}
	};

	Ok(image_path)
}

fn start_bootrom_import(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	match choose_build_file(ui_weak.clone()) {
		Ok(selected_file_path) => {
			if selected_file_path != "" {
				let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
					let ui_weak: slint::Weak<MainWindow> = ui.as_weak();

					let ui_paths = ui.global::<UIPaths>();
					let ui_mame = ui.global::<UIMAMEOptions>();

					let rommy_enabled = ui_paths.get_rommy_enabled();
					let python_path: String = ui_paths.get_python_path().into();
					let rommy_path: String = ui_paths.get_rommy_path().into();
					let selected_box = ui_mame.get_selected_box().clone().to_string();

					if rommy_enabled && selected_box != "msntv2" && !Regex::new(r"\.(o|bin|img)$").unwrap().is_match(&selected_file_path) {
						let ui_weak_cpy = ui_weak.clone();
						let _ = std::thread::spawn(move || {
							enable_loading(&ui_weak, "Running Rommy".into());

							match get_rommy_file(selected_file_path, python_path, rommy_path) {
								Ok(rommy_file_path) => {
									if rommy_file_path != "" {
										let _ = import_bootrom(rommy_file_path.clone(), ui_weak_cpy, true);
									} else {
										ui_weak.unwrap().set_launcher_state_message("There was a problem running Rommy.".into());
									}
								},
								_ => {
									ui_weak.unwrap().set_launcher_state_message("There was a problem running Rommy.".into());
								}
							}

							disable_loading(&ui_weak);
						});
					} else {
						let _ = import_bootrom(selected_file_path.clone(), ui_weak, false);
					}
				});
			}
		},
		_ => { }
	}

	Ok(())
}

fn start_approm_select(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let selected_approm = ui_weak.unwrap().global::<UIMAMEOptions>().get_selected_approm();

	{
		let ui = ui_weak.unwrap();
		let ui_mame = ui.global::<UIMAMEOptions>();
		let build_flags = ui_mame.get_selectable_approms().iter()
			.position(|approm| approm.value == selected_approm)
			.and_then(|index| ui_mame.get_selectable_approm_build_flags().row_data(index))
			.map(|build_flags| build_flags as u32)
			.unwrap_or(0);
		set_selected_approm_build_flags(&ui_mame, &ui_mame.get_selected_box(), build_flags);
	}

	let _ = std::thread::spawn(move || {
		enable_loading(&ui_weak, "Selecting Approm".into());

		match LauncherConfig::new() {
			Ok(config) => {
				let selected_index;
				let hdimg_enabled;
				match Regex::new(r"^(?<name>.+?)\[(?<index>\d+?)\]").unwrap().captures(selected_approm.as_str()) {
					Some(matches) => {
						hdimg_enabled = &matches["name"] == APPROM_HDIMG_PREFIX;
						selected_index = (&matches["index"]).parse::<u8>().unwrap();
					},
					_ => {
						hdimg_enabled = false;
						selected_index = 0;
					}
				};

				let selected_box = config.persistent.mame_options.selected_box.clone().unwrap_or("".into());
		
				let mut selected_hdimg_enabled = match config.persistent.mame_options.selected_hdimg_enabled {
					Some(hdimg_enabled) => hdimg_enabled,
					_ => HashMap::new()
				};
				selected_hdimg_enabled.insert(selected_box.clone(), hdimg_enabled);

				let validate_build_image = config.persistent.mame_options.validate_build_image.unwrap_or(true);

				if hdimg_enabled {
					let selected_hdimg_path = match config.persistent.mame_options.selected_hdimg_paths {
						Some(hdimg_paths) => hdimg_paths[&selected_box].clone(),
						_ => "".into()
					};

					if selected_hdimg_path != "" {
						let _ = set_disk_selected_approm(&selected_box.clone(), &selected_hdimg_path, selected_index, validate_build_image);
					}
				} else {
					for machine in config.mame.machine.unwrap_or(vec![]).iter() {
						let machine_name = 
							machine.name
							.clone()
							.unwrap_or("".into());

						if machine_name == *selected_box {
							if machine.disk.iter().count() > 0 {
								match machine.disk.clone() {
									Some(disks) => {
										let disk_name = disks[0].name.clone().unwrap_or("".into());
										let disk_file = disk_name.clone() + ".chd";

										let config_persistent_paths = config.persistent.paths.clone();
										let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
										let mame_directory_path = LauncherConfig::get_parent(mame_executable_path.clone()).unwrap_or("".into());

										let preset_img_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &disk_file;

										let _ = set_disk_selected_approm(&selected_box.clone(), &preset_img_path, selected_index, validate_build_image);
									},
									_ => {
										//
									}
								}
							}
						}
					}
				}

				let _ = save_config(ui_weak.clone(), true, None, None, Some(selected_hdimg_enabled));
			},
			_ => {
			}
		};

		disable_loading(&ui_weak);
	});

	Ok(())
}

fn start_approm_import(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	match choose_build_file(ui_weak.clone()) {
		Ok(selected_file_path) => {
			if selected_file_path != "" {
				let _ = ui_weak.upgrade_in_event_loop(move |ui| {
					let ui_weak: slint::Weak<MainWindow> = ui.as_weak();

					let ui_paths = ui.global::<UIPaths>();
					let ui_mame = ui.global::<UIMAMEOptions>();

					let rommy_enabled = ui_paths.get_rommy_enabled();
					let python_path: String = ui_paths.get_python_path().into();
					let rommy_path: String = ui_paths.get_rommy_path().into();
					let selected_box = ui_mame.get_selected_box().clone().to_string();

					if rommy_enabled && selected_box != "msntv2" && !Regex::new(r"\.(o|bin|img)$").unwrap().is_match(&selected_file_path) {
						let validate_build_image = ui_mame.get_validate_build_image();
						let ui_weak_cpy = ui_weak.clone();
						let _ = std::thread::spawn(move || {
							enable_loading(&ui_weak, "Running Rommy".into());

							match get_rommy_file(selected_file_path, python_path, rommy_path) {
								Ok(rommy_file_path) => {
									if rommy_file_path != "" {
										let _ = import_approm(rommy_file_path.clone(), ui_weak_cpy, true, validate_build_image);
									} else {
										
									}
								},
								_ => {

								}
							}

							disable_loading(&ui_weak);
						});
					} else {
						// There's a case where someone might want to choose a flash_bank0 file since people are distributing them around.
						// I'm not going to handle that case. Have the user choose an actual .o file.
						let _ = import_approm(selected_file_path.clone(), ui_weak, false, ui_mame.get_validate_build_image());
					}
				});
			}
		},
		_ => { }
	}

	Ok(())
}

fn revert_approm(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let _ = std::thread::spawn(move || {
		enable_loading(&ui_weak, "Reverting".into());

		match LauncherConfig::new() {
			Ok(config) => {
				let selected_box = config.persistent.mame_options.selected_box.clone().unwrap_or("".into());

				for machine in config.mame.machine.unwrap_or(vec![]).iter() {
					let machine_name = 
						machine.name
						.clone()
						.unwrap_or("".into());

					if machine_name == *selected_box {
						if machine.disk.iter().count() > 0 {
							match machine.disk.clone() {
								Some(disks) => {
									let disk_name = disks[0].name.clone().unwrap_or("".into());
									let disk_file = disk_name.clone() + ".chd";

									let config_persistent_paths = config.persistent.paths.clone();
									let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
									let mame_directory_path = LauncherConfig::get_parent(mame_executable_path.clone()).unwrap_or("".into());

									let preset_img_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &disk_file;

									let diff_file_path = CompressedHunkDiskIO::find_diff_file(preset_img_path.clone()).unwrap_or("".into());

									if diff_file_path != "" && Path::new(&diff_file_path).exists() {
										let _ = std::fs::rename(&diff_file_path, diff_file_path.clone() + ".bak");

										let _ = load_config(ui_weak.clone());
									}
								},
								_ => {
									//
								}
							}
						}
					}
				}
			},
			_ => { }
		};

		disable_loading(&ui_weak);
	});

	Ok(())
}

fn choose_hdimg(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_paths = ui.global::<UIPaths>();
	let ui_mame = ui.global::<UIMAMEOptions>();

	let last_opened_img_path = get_choose_file_location(ui_paths.get_last_opened_img_path().into());

	let chooser = 
		DialogBuilder::file()
		.set_location(&last_opened_img_path)
		.set_filename("")
		.add_filter("WebTV HD Image", &["img", "dd", "bin"]);

	let selected_file_pathbuf = chooser.open_single_file().show().unwrap_or(None);

	if selected_file_pathbuf != None {
		let mut selected_file_path: String = "".into();

		match selected_file_pathbuf {
			Some(path) => {
				match path.to_str() {
					Some(path_str) => {
						selected_file_path = path_str.into();
					},
					_ => { }
				}
			},
			_ => { }
		}

		if selected_file_path != "" && Path::new(&selected_file_path).exists() {
			ui_paths.set_last_opened_img_path(LauncherConfig::get_parent(selected_file_path.clone()).unwrap_or("".into()).into());

			match LauncherConfig::get_persistent_config() {
				Ok(config) => {
					let selected_box = ui_mame.get_selected_box().clone().to_string();

					let mut selected_hdimg_paths = match config.mame_options.selected_hdimg_paths {
						Some(hdimg_paths) => hdimg_paths,
						_ => HashMap::new()
					};
					selected_hdimg_paths.insert(selected_box.clone(), selected_file_path.clone());
					ui_mame.set_selected_hdimg_path(selected_file_path.clone().into());

					let mut selected_hdimg_enabled = match config.mame_options.selected_hdimg_enabled {
						Some(hdimg_enabled) => hdimg_enabled,
						_ => HashMap::new()
					};
					selected_hdimg_enabled.insert(selected_box.clone(), true);
					ui_mame.set_selected_hdimg_enabled(true);

					let _ = save_config(ui_weak.clone(), true, None, Some(selected_hdimg_paths), Some(selected_hdimg_enabled));
				},
				_ => {
					//
				}
			};

		}
	}

	Ok(())
}

fn unset_hdimg(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_mame = ui.global::<UIMAMEOptions>();

	match LauncherConfig::get_persistent_config() {
		Ok(config) => {
			let selected_box = ui_mame.get_selected_box().clone().to_string();

			let mut selected_hdimg_paths = match config.mame_options.selected_hdimg_paths {
				Some(hdimg_paths) => hdimg_paths,
				_ => HashMap::new()
			};
			selected_hdimg_paths.remove(&selected_box.clone());
			ui_mame.set_selected_hdimg_path("".into());

			let mut selected_hdimg_enabled = match config.mame_options.selected_hdimg_enabled {
				Some(hdimg_enabled) => hdimg_enabled,
				_ => HashMap::new()
			};
			selected_hdimg_enabled.remove(&selected_box.clone());
			ui_mame.set_selected_hdimg_enabled(false);

			let _ = save_config(ui_weak.clone(), true, None, Some(selected_hdimg_paths), Some(selected_hdimg_enabled));
		},
		_ => {
			//
		}
	};

	Ok(())
}

fn get_choose_file_location(path: String) -> String {
	Path::new(&path)
		.ancestors()
		.find(|ancestor| ancestor.is_dir())
		.and_then(|ancestor| ancestor.to_str())
		.unwrap_or("~")
		.to_string()
}

fn choose_build_file(ui_weak: slint::Weak<MainWindow>) -> Result<String, Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_paths = ui.global::<UIPaths>();
	let ui_mame = ui.global::<UIMAMEOptions>();

	let last_opened_rom_path = get_choose_file_location(ui_paths.get_last_opened_rom_path().into());
	let selected_box = ui_mame.get_selected_box().clone().to_string();

	let rommy_enabled = ui_paths.get_rommy_enabled();

	let chooser: FileDialogBuilder;

	if rommy_enabled && selected_box != "msntv2" {
		chooser = 
			DialogBuilder::file()
			.set_title("Choose build image")
			.set_location(&last_opened_rom_path)
			.set_filename("")
			.add_filter("WebTV Build Files", &["o", "bin", "img", "rom", "brom", "json"])
			.add_filter("WebTV Build Image", &["o", "bin", "img"])
			.add_filter("WebTV partXXX File", &["rom", "brom"])
			.add_filter("Rommy dt.json File", &["json"]);
	} else {
		if selected_box == "msntv2" {
			chooser = 
				DialogBuilder::file()
				.set_title("Choose NK.BIN OS image")
				.set_location(&last_opened_rom_path)
				.set_filename("")
				.add_filter("MSNTV2 OS Image", &["sig", "bin", "SIG", "BIN"]);
		} else {
			chooser = 
				DialogBuilder::file()
				.set_title("Choose build image")
				.set_location(&last_opened_rom_path)
				.set_filename("")
				.add_filter("WebTV Build Image", &["o", "bin", "img"]);
		}
	}

	let selected_file_pathbuf = chooser.open_single_file().show().unwrap_or(None);

	let mut returned_file_path: String = "".into();
	if selected_file_pathbuf != None {
		let mut selected_file_path: String = "".into();

		match selected_file_pathbuf {
			Some(path) => {
				match path.to_str() {
					Some(path_str) => {
						selected_file_path = path_str.into();
					},
					_ => { }
				}
			},
			_ => { }
		}

		if selected_file_path != "" && Path::new(&selected_file_path).exists() {
			ui_paths.set_last_opened_rom_path(LauncherConfig::get_parent(selected_file_path.clone()).unwrap_or("".into()).into());

			returned_file_path = selected_file_path.clone();
		}
	}

	Ok(returned_file_path)

}

fn check_rommy(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui_weak_cpy = ui_weak.clone();
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_paths = ui.global::<UIPaths>();

		let python_path: String = ui_paths.get_python_path().into();
		let rommy_path: String = ui_paths.get_rommy_path().into();

		ui_paths.set_rommy_enabled(false);

		if python_path != "" && rommy_path != "" {
			if Path::new(&python_path).exists() {
				if Path::new(&rommy_path).exists() {
					let _ = std::thread::spawn(move || {
						let mut command = Command::new(python_path);

						command.arg(rommy_path).arg("--help");

						#[cfg(target_os = "windows")]
						command.creation_flags(0x08000000); // CREATE_NO_WINDOW

						match command.output() {
							Ok(rommy_output) => {
								let _ = ui_weak_cpy.upgrade_in_event_loop(move |ui| {
									match std::str::from_utf8(&rommy_output.stdout) {
										Ok(rommy_stdout) => {
											if Regex::new(r"usage: rommy.py").unwrap().is_match(rommy_stdout) {
												ui.global::<UIPaths>().set_rommy_enabled(true);
											} else {
												ui.set_launcher_state_message("Couldn't get information from Rommy. Check your paths. Rommy approm section will be disabled.".into());
											}
										},
										_ => {
											ui.set_launcher_state_message("Couldn't get information from Rommy. Check your paths. Rommy approm section will be disabled.".into());
										}
									}
								});
							},
							_ => {
								let _ = ui_weak_cpy.upgrade_in_event_loop(move |ui| {
									ui.set_launcher_state_message("Couldn't get information from Rommy. Check your paths. Rommy approm section will be disabled.".into());
								});
							}
						}
					});
				} else {
					ui.set_launcher_state_message("The path to Rommy is broken! Rommy approm section will be disabled.".into());
				}
			} else {
				ui.set_launcher_state_message("The path to Python is broken! Rommy approm section will be disabled.".into());
			}
		}
	});

	Ok(())
}

fn fart_enable(ui_weak: slint::Weak<MainWindow>, is_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		ui.global::<UIAbout>().set_fart_enabled(is_enabled);
	});

	Ok(())
}

fn do_fart(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let _ = std::thread::spawn(move || {
		let _ = fart_enable(ui_weak.clone(), true);

		let sink_handle = rodio::DeviceSinkBuilder::open_default_sink().unwrap();

		let asink = rodio::Player::connect_new(&sink_handle.mixer());

		let rand_intro_index = (rand::random_range(1..1000) % 3) + 1;
		let fart_intro = match rand_intro_index {
			1 => FART_INTRO1,
			2 => FART_INTRO2,
			3 => FART_INTRO3,
			_ => FART_INTRO1
		};
		asink.append(rodio::Decoder::new(BufReader::new(Cursor::new(fart_intro))).unwrap());

		let rand_fart_index = (rand::random_range(1..1000) % 3) + 1;
		let fart = match rand_fart_index {
			1 => FART1,
			2 => FART2,
			3 => FART3,
			_ => FART3
		};
		asink.append(rodio::Decoder::new(BufReader::new(Cursor::new(fart))).unwrap());
		
		asink.sleep_until_end();

		let _ = fart_enable(ui_weak.clone(), false);
	});

	Ok(())
}

fn get_mame_pid(ui_weak: slint::Weak<MainWindow>) -> Result<u32, Box<dyn std::error::Error>> {
	let ui: MainWindow = ui_weak.unwrap();

	let found_pid: u32 = ui.get_mame_pid() as u32;

	Ok(found_pid)
}

fn set_mame_pid(ui_weak: slint::Weak<MainWindow>, pid: u32) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		ui.set_mame_pid(pid as i32);
	});

	Ok(())
}

fn add_console_text(ui_weak: slint::Weak<MainWindow>, text: String, scroll_mode: MAMEConsoleScrollMode, rshift: usize) -> Result<(), Box<dyn std::error::Error>> {
	add_console_text_from(ui_weak, text, scroll_mode, rshift, false)
}

fn add_console_text_from(ui_weak: slint::Weak<MainWindow>, text: String, scroll_mode: MAMEConsoleScrollMode, rshift: usize, from_stderr: bool) -> Result<(), Box<dyn std::error::Error>> {
	if text != "" || rshift > 0 {
		let mut pending = PENDING_CONSOLE_TEXT.lock().unwrap_or_else(|e| e.into_inner());

		for _ in 0..rshift {
			loop {
				match pending.segments.last_mut() {
					Some((_, segment_text)) => {
						if segment_text.pop().is_some() {
							break;
						}
						pending.segments.pop();
					},
					None => {
						pending.rshift += 1;
						break;
					}
				}
			}
		}

		let mut text = text.as_str();
		if pending.last_char_cr && text.starts_with('\n') {
			text = &text[1..];
		}
		pending.last_char_cr = text.ends_with('\r');
		let text: std::borrow::Cow<str> = if text.contains('\r') {
			text.replace("\r\n", "\n").replace('\r', "\n").into()
		} else {
			text.into()
		};
		match pending.segments.last_mut() {
			Some((segment_stderr, segment_text)) if *segment_stderr == from_stderr => *segment_text += &text,
			_ => if !text.is_empty() {
				pending.segments.push((from_stderr, text.into_owned()));
			}
		}

		if pending.segments.iter().map(|(_, segment_text)| segment_text.len()).sum::<usize>() > CONSOLE_MAX_PENDING_SIZE {
			let mut remaining_lines = CONSOLE_MAX_LINES;
			for i in (0..pending.segments.len()).rev() {
				let newline_count = pending.segments[i].1.matches('\n').count();
				if newline_count >= remaining_lines {
					let start = console_last_lines_start(&pending.segments[i].1, remaining_lines);
					pending.segments[i].1.drain(..(start - 1));
					pending.segments.drain(..i);
					pending.rshift = 0;
					break;
				}
				remaining_lines -= newline_count;
			}
		}

		if scroll_mode == MAMEConsoleScrollMode::ForceScroll || pending.scroll_mode == MAMEConsoleScrollMode::NoScrollCheck {
			pending.scroll_mode = scroll_mode;
		}

		if !pending.flush_scheduled {
			pending.flush_scheduled = true;

			let interval = Duration::from_micros(CONSOLE_UPDATE_INTERVAL_USEC as u64);
			let delay = match pending.last_flush {
				Some(last_flush) => interval.saturating_sub(last_flush.elapsed()),
				None => Duration::ZERO
			};

			let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
				if delay.is_zero() {
					flush_console_text(&ui);
				} else {
					slint::Timer::single_shot(delay, move || {
						if let Some(ui) = ui_weak.upgrade() {
							flush_console_text(&ui);
						}
					});
				}
			});
		}
	}

	Ok(())
}

fn console_last_lines_start(text: &str, max_lines: usize) -> usize {
	text.rmatch_indices('\n').nth(max_lines - 1).map(|(i, _)| i + 1).unwrap_or(0)
}

fn flush_console_text(ui: &MainWindow) {
	let (segments, rshift, scroll_mode) = {
		let mut pending = PENDING_CONSOLE_TEXT.lock().unwrap_or_else(|e| e.into_inner());

		let scroll_mode = pending.scroll_mode;
		pending.scroll_mode = MAMEConsoleScrollMode::NoScrollCheck;
		pending.flush_scheduled = false;
		pending.last_flush = Some(Instant::now());

		(std::mem::take(&mut pending.segments), std::mem::take(&mut pending.rshift), scroll_mode)
	};

	if segments.is_empty() && rshift == 0 {
		return;
	}

	let lines_model = ui.get_mame_console_lines();
	if let Some(lines) = lines_model.as_any().downcast_ref::<ConsoleLines>() {
		let mut shifted_count = 0;
		if segments.is_empty() {
			shifted_count += lines.add_text(&String::new(), rshift, false);
		}
		for (i, (from_stderr, text)) in segments.iter().enumerate() {
			shifted_count += lines.add_text(text, if i == 0 { rshift } else { 0 }, *from_stderr);
		}
		if shifted_count > 0 {
			ui.invoke_console_lines_trimmed(shifted_count.min(i32::MAX as usize) as i32);
		}
	}

	ui.invoke_console_updated(scroll_mode);
}

fn clear_console_text(ui: &MainWindow) {
	{
		let mut pending = PENDING_CONSOLE_TEXT.lock().unwrap_or_else(|e| e.into_inner());

		pending.segments.clear();
		pending.rshift = 0;
		pending.last_char_cr = false;
	}

	ui.set_mame_console_lines(slint::ModelRc::new(ConsoleLines::default()));
	ui.invoke_console_clear_selection();
}

// The box's debug console text as the console shows it.
#[derive(Default)]
struct DebugConsoleFilter {
	last_byte: u8
}

impl DebugConsoleFilter {
	// The text, and how many chars before it a backspace took back.
	fn filter(&mut self, bytes: &[u8]) -> (String, usize) {
		let mut new_bytes: Vec<u8> = vec![];
		let mut rshift = 0;

		for &b in bytes {
			match b {
				0x00..=0x07 | 0x0b..=0x0c | 0x0e..=0x1f | 0x7f..=0xff => { // ignored control characters
					//
				},
				0x08 => { // handle delete key
					if new_bytes.len() > 0 {
						new_bytes.pop();
					} else {
						rshift += 1;
					}
				},
				0x0a | 0x0d => { // newline
					// Don't repeat newline chars.
					if (self.last_byte == 0x0a || self.last_byte == 0x0d) && self.last_byte != b {
						continue;
					}

					new_bytes.push(b);
				},
				_ => { // all other characters
					new_bytes.push(b);
				}
			};

			self.last_byte = b;
		}

		(String::from_utf8_lossy(&new_bytes).to_string(), rshift)
	}
}

fn output_mame_debug(ui_weak: slint::Weak<MainWindow>, debug_bitb_port: u16, drx: Option<Receiver<String>>) -> Result<(), Box<dyn std::error::Error>> {
	match TcpListener::bind("127.0.0.1:".to_owned() + &debug_bitb_port.to_string()) {
		Ok(listener) => {
			let _ = std::thread::spawn(move || {
				match listener.accept() {
					Ok((mut mame, _)) => {
						let takes_keys = drx.is_some();
						if takes_keys {
							let _ = ui_weak.upgrade_in_event_loop(move |ui| {
								ui.set_using_mame_debug(true);
							});
						}

						let _ = mame.set_nonblocking(true);

						let mut filter = DebugConsoleFilter::default();
						let mut active_buf = [0; DEBUG_READ_BUFFER_SIZE];
						let mut waiting_buf: Vec<u8> = vec![];
						loop {
							match mame.read(&mut active_buf) {
								Ok(bytes_read) => {
									if bytes_read > 0 {
										waiting_buf.extend(&active_buf[0..bytes_read]);
									} else {
										break;
									}
								},
								Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
									std::thread::sleep(Duration::from_millis(1));
								},
								_ => {
									break;
								}
							};

							if waiting_buf.len() > 0 {
								let (text, rshift) = filter.filter(&waiting_buf);
								waiting_buf.clear();

								pekoe2::console_text(&text);
								let _ = add_console_text(ui_weak.clone(), text, MAMEConsoleScrollMode::ConditionalScroll, rshift);
							}

							match drx.as_ref().map(|drx| drx.try_recv()) {
								Some(Ok(received)) => {
									let bytes = received.as_bytes();
									if bytes.len() == 1 && bytes[0] == 0x0a {
										// send 0x0d instead of 0x0a for enter because the WebTV OS responds to that better.
										let _ = mame.write(&[0x0d; 1]);
									} else if bytes.len() > 1 || bytes[0] != 0x10 { // don't send shift key press
										let _ = mame.write(&bytes);
									}
								},
								_ => { }
							};
						}

						if takes_keys {
							let _ = ui_weak.upgrade_in_event_loop(move |ui| {
								ui.set_using_mame_debug(false);
							});
						}
					},
					Err(e) => {
						let _ = add_console_text(ui_weak.clone(), ("ERROR: Couldn't accept debug console client!\n".to_owned() + &e.to_string()).into(), MAMEConsoleScrollMode::ForceScroll, 0);
					}
				};
			});
		},
		Err(e) => {
			let _ = add_console_text(ui_weak.clone(), ("ERROR: Couldn't start debug console server!\n".to_owned() + &e.to_string()).into(), MAMEConsoleScrollMode::ForceScroll, 0);
		}
	};

	Ok(())
}

fn output_mame_stdio(ui_weak: slint::Weak<MainWindow>, stdout: Option<ChildStdout>, stderr: Option<ChildStderr>) -> Result<bool, Box<dyn std::error::Error>> {
	let process_has_error = Arc::new(AtomicBool::new(false));

	match (stdout, stderr) {
		(Some(stdout), Some(stderr)) => {
			let ui_weak_cpy = ui_weak.clone();
			let process_has_error = process_has_error.clone();
			let mut stderr_reader = BufReader::new(stderr);
			let _ = std::thread::spawn(move || {
				loop {
				let mut stderr_buf: [u8; CONSOLE_READ_BUFFER_SIZE] = [0x00; CONSOLE_READ_BUFFER_SIZE];
					match stderr_reader.read(&mut stderr_buf) {
						Ok(bytes_read) => {
							if bytes_read == 0 {
								break;
							} else {
								let console_text = String::from_utf8_lossy(&stderr_buf[0..bytes_read]).to_string();

								// EMAC: stdout and stderr can get jumbled with this implementation...
								let _ = add_console_text_from(ui_weak_cpy.clone(), console_text, MAMEConsoleScrollMode::ConditionalScroll, 0, true);

								process_has_error.store(true, Relaxed);
							}
						},
						_ => {
							break;
						}
					}
				}
			});

			let mut stdout_reader = BufReader::new(stdout);
			// No thread is spawned here so we block current thread until MAME finishes outputting.
			let mut stdout_buf: [u8; CONSOLE_READ_BUFFER_SIZE] = [0x00; CONSOLE_READ_BUFFER_SIZE];
			loop {
				match stdout_reader.read(&mut stdout_buf) {
					Ok(bytes_read) => {
						if bytes_read == 0 {
							break;
						} else {
							let console_text = String::from_utf8_lossy(&stdout_buf[0..bytes_read]).to_string();

							let _ = add_console_text(ui_weak.clone(), console_text, MAMEConsoleScrollMode::ConditionalScroll, 0);
						}
					},
					_ => {
						break;
					}
				}
			}
		},
		_ => { }
	};

	Ok(process_has_error.load(Relaxed))
}

fn spawn_mame_from_command(ui_weak: slint::Weak<MainWindow>, debug_bitb_port: u16, drx: Receiver<String>, mut mame_command: Command, network_stack: Option<NetworkStack>, mame_services: Vec<Box<dyn Send>>) -> Result<(), Box<dyn std::error::Error>> {
	let _ = std::thread::spawn(move || {
		let mut full_mame_command_line: String;

		full_mame_command_line = mame_command.get_program().to_str().unwrap_or("".into()).to_string() + " ";
		full_mame_command_line += &mame_command.get_args().map(|arg_str| arg_str.to_str().unwrap_or("".into())).collect::<Vec<_>>().join(" ");

		let _ = add_console_text(ui_weak.clone(), " \n \nStarting MAME: '".to_owned() + &full_mame_command_line + "'\n", MAMEConsoleScrollMode::ForceScroll, 0);

		mame_command.stderr(Stdio::piped());
		mame_command.stdout(Stdio::piped());

		if debug_bitb_port > 0 {
			let _ = output_mame_debug(ui_weak.clone(), debug_bitb_port, Some(drx));
		}

		let process_has_error = match mame_command.spawn() {
			Ok(mame) => {
				let _= set_mame_pid(ui_weak.clone(), mame.id());

				let ui_weak_cpy = ui_weak.clone();
				let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
					let ui_mame = ui.global::<UIMAMEOptions>();
					if !(ui_mame.get_verbose_mode().into() || ui_mame.get_console_input().into() || ui_mame.get_debug_mode().into() || uses_debug_link(&ui_mame)) {
						let _ = add_console_text(ui_weak_cpy.clone(), "\n\nMAME is now running...\n\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);
					}
				});

				output_mame_stdio(ui_weak.clone(), mame.stdout, mame.stderr).unwrap_or(false)
			},
			Err(_) => false
		};

		let _= set_mame_pid(ui_weak.clone(), 0);

		// Stops the launcher network and the other services that ran with MAME.
		drop(network_stack);
		drop(mame_services);

		let _ = add_console_text(ui_weak.clone(), " \nMAME Ended\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);

		// MAME's window is gone
		// The window size is remembered for next time
		let ui_weak_capture = ui_weak.clone();
		let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
			if ui.get_mame_captured() {
				let _ = save_config(ui_weak_capture, false, None, None, None);
			}

			mamecapture::stop();
			ui.set_mame_captured(false);
		});

		if !process_has_error {
			let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
				let ui_mame = ui.global::<UIMAMEOptions>();

				if !(ui_mame.get_verbose_mode().into() || ui_mame.get_console_input().into() || ui_mame.get_debug_mode().into() || uses_debug_link(&ui_mame)) {
					let console_enabled = ui.get_mame_console_enabled();
					if console_enabled {
						ui.set_mame_console_enabled(false);

						let _ = save_config(ui_weak.clone(), true, None, None, None);
					}
				}
			});
		}
	});

	Ok(())
}

fn start_mame(ui_weak: slint::Weak<MainWindow>, drx: Receiver<String>) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();

	let mame_executable_path: String = Paths::resolve_mame_path(Some(ui.global::<UIPaths>().get_mame_path().into()));

	if mame_executable_path != "" && Path::new(&mame_executable_path).exists() {
		let mame_directory_path = LauncherConfig::get_parent(mame_executable_path.clone()).unwrap_or("".into());

		ui.set_mame_console_enabled(true);
		clear_console_text(&ui);

		let ui_mame = ui.global::<UIMAMEOptions>();

		let mut mame_command = Command::new(mame_executable_path.clone());

		#[cfg(target_os = "windows")]
		mame_command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		mame_command.current_dir(mame_directory_path.clone());

		let selected_box = ui_mame.get_selected_box();
		let running_machine_title = ui_mame.get_selectable_boxes()
			.iter()
			.find(|selectable_box| selectable_box.value == selected_box)
			.map(|selectable_box| selectable_box.hint)
			.filter(|hint| !hint.is_empty())
			.unwrap_or(selected_box.clone());
		ui.set_running_machine_title(running_machine_title);

		mame_command.arg(selected_box.to_string());

		mame_command.arg("-nomouse");
		mame_command.arg("-skip_gameinfo");

		// MAME draws inside the launcher above the console
		let windowing_mode = ui_mame.get_effective_windowing_mode();
		if windowing_mode == "captured" {
			ui.set_mame_captured(true);

			match mamecapture::start(&ui) {
				Some(host_window_id) => {
					mame_command.arg("-window");
					mame_command.arg("-parent_window").arg(host_window_id);

					// Use the window size from last time or make room for wtvmame (4:3 at the current width)
					let window_size = ui.window().size().to_logical(ui.window().scale_factor());
					let (width, height) = if ui_mame.get_captured_window_width() > 0.0 && ui_mame.get_captured_window_height() > 0.0 {
						(ui_mame.get_captured_window_width(), ui_mame.get_captured_window_height())
					} else {
						(window_size.width, (window_size.width * 3.0 / 4.0) + CAPTURED_DIVIDER_HEIGHT + ui_mame.get_captured_console_height())
					};
					resize_window_when_allowed(ui_weak.clone(), width, height, 20);
				},
				None => {
					ui.set_mame_captured(false);
					let _ = add_console_text(ui_weak.clone(), " \nWARNING: Couldn't make a place for MAME's window in the launcher. MAME will open in its own window.\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);
				}
			}
		}

		if !ui.get_mame_captured() && windowing_mode != "fullscreen" {
			mame_command.arg("-window");
			mame_command.arg("-nomaximize");
		}

		if ui_mame.get_verbose_mode().into() {
			mame_command.arg("-verbose");
		}

		if ui_mame.get_use_drc().into() {
			mame_command.arg("-drc");
		} else {
			mame_command.arg("-nodrc");
		}

		if ui_mame.get_debug_mode().into() {
			mame_command.arg("-debug");
		}

		if ui_mame.get_disable_sound().into() {
			mame_command.arg("-sound").arg("none");
		}

		let mut selected_bootrom = ui_mame.get_selected_bootrom().to_string();
		if selected_bootrom != "" && selected_bootrom != "None" && selected_bootrom != "WinCE" {
			selected_bootrom = Regex::new(r"\.o$").unwrap().replace_all(&selected_bootrom, "").to_string();

			mame_command.arg("-bios").arg(selected_bootrom);
		}

		let selected_box = ui_mame.get_selected_box().to_string();

		if ui_mame.get_can_use_wtvpointer() && ui_mame.get_enable_wtvpointer() {
			mame_command.arg("-wtvpointer");
		}

		let use_wtvnet = ui_mame.get_can_use_wtvnet() && ui_mame.get_enable_wtvnet();
		if use_wtvnet {
			mame_command.arg("-wtvnet");
		}

		let network_stack = if selected_box == "msntv2" || use_wtvnet {
			start_netstack(ui_weak.clone(), &mame_executable_path, &mame_directory_path, &mut mame_command, &selected_box)
		} else {
			None
		};

		let config_machines = LauncherConfig::get_mame_config(&mame_executable_path).ok().and_then(|config_mame| config_mame.machine).unwrap_or(vec![]);
		let selected_machine = config_machines.iter().find(|machine| machine.name.as_deref() == Some(selected_box.as_str())).cloned();

		let console_warning = |message: &str| {
			let _ = add_console_text(ui_weak.clone(), " \nWARNING: ".to_owned() + message + "\n", MAMEConsoleScrollMode::ForceScroll, 0);
		};

		let mut selected_slot_options: Vec<(String, String)> = vec![];
		let selected_wtvport_option = ui_mame.get_selected_wtvport_option().to_string();
		if ui_mame.get_can_use_wtvport() && ui_mame.get_enable_wtvport() && selected_wtvport_option != "" {
			selected_slot_options.push((WTVPORT_SLOT_NAME.to_string(), selected_wtvport_option));
		}
		let selected_parallel_option = ui_mame.get_selected_parallel_option().to_string();
		let use_parallel = ui_mame.get_can_use_parallel() && ui_mame.get_enable_parallel() && selected_parallel_option != "";
		if use_parallel {
			selected_slot_options.push((PARALLEL_SLOT_NAME.to_string(), selected_parallel_option.clone()));
		}
		for (slot_name, option_name) in selected_slot_options.iter() {
			mame_command.arg("-".to_owned() + slot_name).arg(option_name);
		}

		let (box_bitbangers, added_bitbangers) = match &selected_machine {
			Some(selected_machine) => get_bitbanger_brief_names(&config_machines, selected_machine, &selected_slot_options),
			None => (HashMap::new(), HashMap::new())
		};

		// Services that run alongside MAME and stop when it ends.
		let mut mame_services: Vec<Box<dyn Send>> = vec![];

		// The debug link brings its own console (the parallel port's), so the Console Input options don't apply.
		let mut debug_link_active = false;
		if use_parallel && selected_parallel_option == "socket" {
			let selected_parallel_socket_endpoint = ui_mame.get_selected_parallel_socket_endpoint().to_string();
			match added_bitbangers.get(&(":".to_owned() + PARALLEL_SLOT_NAME)).and_then(|brief_names| brief_names.first()) {
				Some(brief_name) if selected_parallel_socket_endpoint == DEBUG_LINK_SOCKET_ENDPOINT => {
					match start_debug_link(ui_weak.clone(), drx.clone()) {
						Ok((port, link)) => {
							mame_command.arg("-".to_owned() + brief_name).arg("socket.127.0.0.1:".to_owned() + &port.to_string());
							mame_services.push(link);
							debug_link_active = true;
						},
						Err(message) => console_warning(&message)
					}
				},
				Some(brief_name) if selected_parallel_socket_endpoint == WEBTV_BRIDGE_SOCKET_ENDPOINT => {
					match start_webtv_bridge(ui_weak.clone()) {
						Ok((port, bridge)) => {
							mame_command.arg("-".to_owned() + brief_name).arg("socket.127.0.0.1:".to_owned() + &port.to_string());
							mame_services.push(bridge);
						},
						Err(message) => console_warning(&message)
					}
				},
				Some(brief_name) if selected_parallel_socket_endpoint != "" => {
					mame_command.arg("-".to_owned() + brief_name).arg(to_mame_endpoint_path(&selected_parallel_socket_endpoint));
				},
				Some(_) => console_warning("No parallel port socket endpoint selected. The parallel port socket won't connect."),
				None => console_warning("Couldn't find the parallel port socket in MAME. The parallel port socket won't connect.")
			}
		}

		let selected_smartcard_endpoint = ui_mame.get_selected_smartcard_endpoint().to_string();
		if ui_mame.get_can_use_smartcard() && ui_mame.get_enable_smartcard() && selected_smartcard_endpoint != "" {
			match find_bitbanger_brief_name(&box_bitbangers, ":mainsc:bitb") {
				Some(brief_name) => {
					mame_command.arg("-".to_owned() + &brief_name).arg(to_mame_endpoint_path(&selected_smartcard_endpoint));
				},
				None => console_warning("This box doesn't have a SmartCard reader in MAME. The SmartCard won't be attached.")
			}
		}
		let selected_cacard_endpoint = ui_mame.get_selected_cacard_endpoint().to_string();
		if ui_mame.get_can_use_cacard() && ui_mame.get_enable_cacard() && selected_cacard_endpoint != "" {
			match find_bitbanger_brief_name(&box_bitbangers, ":utvsc:bitb") {
				Some(brief_name) => {
					mame_command.arg("-".to_owned() + &brief_name).arg(to_mame_endpoint_path(&selected_cacard_endpoint));
				},
				None => console_warning("This box doesn't have a CA card reader in MAME. The CA card won't be attached.")
			}
		}

		let selected_video_in_endpoint = ui_mame.get_selected_video_in_endpoint().to_string();
		if ui_mame.get_can_use_video_in() && ui_mame.get_enable_video_in() {
			match selected_machine.as_ref().and_then(|selected_machine| find_device_brief_name(selected_machine, "videocapture")) {
				Some(brief_name) => {
					let video_in_endpoint = if selected_video_in_endpoint != "" { selected_video_in_endpoint } else { DEFAULT_CAPTURE_DEVICE.to_string() };
					mame_command.arg("-".to_owned() + &brief_name).arg(video_in_endpoint);
				},
				None => console_warning("This box doesn't have video in in MAME. Video in won't be used.")
			}
		}

		let use_audio_in = ui_mame.get_can_use_audio_in() && ui_mame.get_enable_audio_in();
		let audio_input_tags = selected_machine.as_ref().map(|selected_machine| find_audio_input_tags(selected_machine)).unwrap_or(vec![]);
		match audio_input_tags.len() {
			count if count > 0 => {
				let selected_audio_in_endpoint = ui_mame.get_selected_audio_in_endpoint().to_string();
				let audio_in_node = if selected_audio_in_endpoint == DEFAULT_CAPTURE_DEVICE || selected_audio_in_endpoint == "" || ui_mame.get_audio_in_sound_module() == "portaudio" {
					"".to_string()
				} else if selected_audio_in_endpoint.starts_with(MAME_AUDIO_INPUT_NODE_PREFIX) || selected_audio_in_endpoint.starts_with(MAME_AUDIO_OUTPUT_NODE_PREFIX) {
					selected_audio_in_endpoint
				} else {
					MAME_AUDIO_INPUT_NODE_PREFIX.to_owned() + &selected_audio_in_endpoint
				};
				let node_name = if use_audio_in { Some(audio_in_node.as_str()) } else { None };

				for audio_input_tag in audio_input_tags.iter() {
					if let Err(e) = LauncherConfig::set_mame_audio_input(&mame_directory_path, &selected_box, audio_input_tag, node_name) {
						console_warning(&("Couldn't set the audio in device in MAME's config. Audio in may not work as expected.\n".to_owned() + &e.to_string()));
					}
				}

				let audio_in_sound_module = ui_mame.get_audio_in_sound_module().to_string();
				if use_audio_in && audio_in_sound_module != "" && !ui_mame.get_disable_sound() && !split_custom_options(&ui_mame.get_custom_options()).iter().any(|custom_option| custom_option == "-sound") {
					mame_command.arg("-sound").arg(&audio_in_sound_module);
				}
			},
			_ => {
				if use_audio_in {
					console_warning("This box doesn't have audio in in MAME. Audio in won't be used.");
				}
			}
		}

		if ui_mame.get_can_use_decoupled_sound() && ui_mame.get_use_decoupled_sound() {
			if LauncherConfig::mame_supports_option(&mame_executable_path, "decoupled_sound") {
				mame_command.arg("-decoupled_sound");
			} else {
				console_warning("This MAME doesn't support decoupled sound. Using the default audio timer.");
			}
		}

		if ui_mame.get_can_use_softmodem_record() && ui_mame.get_softmodem_record() {
			mame_command.arg("-sm_record");
		}

		let bitbanger_for_slot = |slot_select: &str, bitb_select: &str| -> String {
			let slot_prefix = ":".to_owned() + slot_select.trim_start_matches(':') + ":";
			box_bitbangers.iter()
				.find(|(tag, _)| tag.starts_with(&slot_prefix))
				.map(|(_, brief_name)| brief_name.clone())
				.unwrap_or(bitb_select.to_string())
		};

		// Custom options come after the launcher's options so they can override them.
		mame_command.args(split_custom_options(&ui_mame.get_custom_options()));

		let mut debug_bitb_port= 0;
		if ui_mame.get_console_input().into() && !debug_link_active {
			let found_debug_bitb = match Regex::new(r"^(?<slot_select>[^; ]+?)\; (?<bitb_select>.+?)$").unwrap().captures(ui_mame.get_selected_debug_bitb_startpoint().as_str()) {
				Some(matches) => {
					//mame_command.arg("-".to_owned() + &matches["slot_select"]).arg("null_modem");

					let selected_debug_bitb_endpoint = ui_mame.get_selected_debug_bitb_endpoint();
					let usable_debug_bitb_endpoint = match selected_debug_bitb_endpoint.as_str() {
						DEFAULT_DEBUG_ENDPOINT | "127.0.0.1" | "localhost" | "local" => match portpicker::pick_unused_port() {
							Some(found_port) => {
								debug_bitb_port = found_port;

								&("127.0.0.1:".to_owned() + &debug_bitb_port.to_string())
							},
							None => {
								let _ = add_console_text(ui_weak.clone(), "ERROR: Couldn't find an available port for debug console!\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);

								""
							}
						},
						_ => selected_debug_bitb_endpoint.as_str()
					};

					let debug_bitb_select = bitbanger_for_slot(&matches["slot_select"], &matches["bitb_select"]);
					if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(usable_debug_bitb_endpoint) {
						mame_command.arg("-".to_owned() + &debug_bitb_select).arg("socket.".to_owned() + &usable_debug_bitb_endpoint.to_string());
					} else {
						mame_command.arg("-".to_owned() + &debug_bitb_select).arg(usable_debug_bitb_endpoint);
					}

					true
				}
				None => false
			};

			if !found_debug_bitb {
				#[cfg(target_os = "windows")]
				mame_command.arg("-keyboardprovider").arg("win32");
				mame_command.arg("-background_input");

				#[cfg(target_os = "macos")]
				unsafe {
					let options = CFDictionaryCreateMutable(std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null());
					if !options.is_null() {
						CFDictionaryAddValue(options, kAXTrustedCheckOptionPrompt.as_void_ptr(), kCFBooleanTrue.as_void_ptr());
						if !AXIsProcessTrustedWithOptions(options) {
							let _ = add_console_text(ui_weak.clone(), " \n \nAccessibility permission not available. Console input wont be available because there's no permission to communicate with the MAME window. Please go into your settings, then select 'Privacy & Security', then select 'Accessibility', then give permission to this application.\n".to_string(), MAMEConsoleScrollMode::ForceScroll, 0);
						}
						CFRelease(options as *const _);
					}
				}
			}
		}

		// Special Pekoe2 device that gets attached to the WebTV Port. There's a UART, parallel port for download receieve and a PCMIA card (no idea how PCMIA works on there)
		// The parallel port ins't controlled like a full host-side parallel port. It's essentially just used to recieve builds and is a bitb stream device in wtvmame
		if ui_mame.get_can_use_wtvport() && ui_mame.get_enable_wtvport() && ui_mame.get_selected_wtvport_option() == PEKOE2_WTVPORT_OPTION {
			match added_bitbangers.get(&(":".to_owned() + WTVPORT_SLOT_NAME)) {
				Some(brief_names) if brief_names.len() >= 2 => {
					match portpicker::pick_unused_port() {
						Some(port) => {
							let keys = if !debug_link_active && debug_bitb_port == 0 {
								Some(drx.clone())
							} else {
								None
							};

							let _ = output_mame_debug(ui_weak.clone(), port, keys);

							mame_command.arg("-".to_owned() + &brief_names[0]).arg("socket.127.0.0.1:".to_owned() + &port.to_string());
						},
						None => console_warning("Couldn't find an available TCP port for the Pekoe2 UART!")
					}
					match start_pekoe2(ui_weak.clone()) {
						Ok((port, link)) => {
							mame_command.arg("-".to_owned() + &brief_names[1]).arg("socket.127.0.0.1:".to_owned() + &port.to_string());
							mame_services.push(link);
						},
						Err(message) => console_warning(&message)
					}
				},
				_ => console_warning("MAME didn't give me any Pekoe2 serial and parallel bitb ports!")
			}
		}

		// Boxes with a MoIP bridge use -moip for sip: modem endpoints and -bitb for ever other modem endpoint.
		let selected_modem_moip_startpoint: String = ui_mame.get_selected_modem_moip_startpoint().to_string();
		let selected_modem_bitb_startpoint: String = ui_mame.get_selected_modem_bitb_startpoint().to_string();
		let selected_modem_bitb_endpoint: String = if selected_modem_moip_startpoint != "" {
			ui_mame.get_selected_modem_moip_endpoint().to_string()
		} else {
			ui_mame.get_selected_modem_bitb_endpoint().to_string()
		};
		if selected_modem_bitb_endpoint.starts_with("sip:") {
			match Regex::new(r"^(?<moip_tag>[^; ]+?)\; (?<moip_select>.+?)$").unwrap().captures(selected_modem_moip_startpoint.as_str()) {
				Some(matches) => {
					mame_command.arg("-".to_owned() + &matches["moip_select"]).arg(&selected_modem_bitb_endpoint);
				}
				None => {
					let _ = add_console_text(ui_weak.clone(), " \n \nERROR: This box doesn't have a MoIP bridge so the sip: modem endpoint can't be used.\n".to_string(), MAMEConsoleScrollMode::ForceScroll, 0);
				}
			}
		} else if selected_modem_bitb_endpoint != "" && selected_modem_bitb_startpoint != "" {
			match Regex::new(r"^(?<slot_select>[^; ]+?)\; (?<bitb_select>.+?)$").unwrap().captures(selected_modem_bitb_startpoint.as_str()) {
				Some(matches) => {
					//mame_command.arg("-".to_owned() + &matches["slot_select"]).arg("null_modem");
		
					let modem_bitb_select = bitbanger_for_slot(&matches["slot_select"], &matches["bitb_select"]);
					if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(selected_modem_bitb_endpoint.as_str()) {
						mame_command.arg("-".to_owned() + &modem_bitb_select).arg(&("socket.".to_owned() + &selected_modem_bitb_endpoint));
					} else {
						mame_command.arg("-".to_owned() + &modem_bitb_select).arg(selected_modem_bitb_endpoint);
					}
				}
				None => {
				}
			}
		}

		let selected_lirc_startpoint: String = ui_mame.get_selected_lirc_startpoint().to_string();
		if ui_mame.get_enable_lirc().into() && selected_lirc_startpoint != "" {
			let selected_lirc_endpoint: String = ui_mame.get_selected_lirc_endpoint().to_string();
			if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(selected_lirc_endpoint.as_str()) {
				mame_command.arg("-lirc").arg(&("socket.".to_owned() + &selected_lirc_endpoint));
			} else {
				mame_command.arg("-lirc").arg(selected_lirc_endpoint);
			}
		}

		let selected_hdimg_path: String = ui_mame.get_selected_hdimg_path().to_string();
		let selected_hdimg_enabled = ui_mame.get_selected_hdimg_enabled();
		if selected_hdimg_enabled && selected_hdimg_path != "" {
			mame_command.arg("-hard").arg(selected_hdimg_path);
		}

		let _ = spawn_mame_from_command(ui_weak, debug_bitb_port, drx, mame_command, network_stack, mame_services);
	}

	Ok(())
}

// Whether MAME runs (or will run) with the debug link on the parallel port.
fn uses_debug_link(ui_mame: &UIMAMEOptions<'_>) -> bool {
	ui_mame.get_can_use_parallel() && ui_mame.get_enable_parallel()
		&& ui_mame.get_selected_parallel_option() == "socket"
		&& ui_mame.get_selected_parallel_socket_endpoint() == DEBUG_LINK_SOCKET_ENDPOINT
}

// The running debug link, for MAME's services. Stopping it gives the console its keys back.
struct DebugLinkService {
	_link: debuglink::DebugLink,
	ui_weak: slint::Weak<MainWindow>
}

impl Drop for DebugLinkService {
	fn drop(&mut self) {
		let _ = self.ui_weak.upgrade_in_event_loop(|ui| {
			ui.set_using_mame_debug(false);
			ui.set_link_transfer_active(false);
		});
	}
}

#[derive(Clone, Copy)]
enum DownloadTarget {
	DebugLink,
	Pekoe2
}

static DOWNLOAD_TARGET: Mutex<DownloadTarget> = Mutex::new(DownloadTarget::DebugLink);

// Where the debug link's file dialogs open: the last place one was used.
static DEBUG_LINK_LOCATION: Mutex<Option<PathBuf>> = Mutex::new(None);

fn debug_link_location(ui: &MainWindow) -> String {
	match DEBUG_LINK_LOCATION.lock().ok().and_then(|location| location.clone()) {
		Some(location) => get_choose_file_location(location.to_string_lossy().to_string()),
		None => get_choose_file_location(ui.global::<UIPaths>().get_last_opened_rom_path().into())
	}
}

fn remember_debug_link_location(path: &Path) {
	if let (Ok(mut location), Some(parent)) = (DEBUG_LINK_LOCATION.lock(), path.parent()) {
		*location = Some(parent.to_path_buf());
	}
}

fn set_link_transfer(ui: &MainWindow, title: &str, detail: &str, can_choose: bool, can_cancel: bool) {
	ui.set_link_transfer_active(true);
	ui.set_link_transfer_title(title.into());
	ui.set_link_transfer_detail(detail.into());
	ui.set_link_transfer_progress(0.0);
	ui.set_link_transfer_indeterminate(false);
	ui.set_link_transfer_can_choose(can_choose);
	ui.set_link_transfer_can_cancel(can_cancel);
}

fn format_bytes(bytes: u64) -> String {
	if bytes >= 1024 * 1024 {
		format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
	} else if bytes >= 1024 {
		format!("{:.1} KB", bytes as f64 / 1024.0)
	} else {
		format!("{} bytes", bytes)
	}
}

// Asks for the file the box waits for and sends it to the debug link. The box keeps waiting if none is picked.
fn choose_debug_link_download(ui_weak: slint::Weak<MainWindow>) {
	let ui = ui_weak.unwrap();
	let chooser = DialogBuilder::file()
		.set_title("Choose the file to download to the box")
		.set_location(&debug_link_location(&ui))
		.set_filename("");

	match chooser.open_single_file().show().unwrap_or(None) {
		Some(path) => {
			remember_debug_link_location(&path);
			let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or("download".into());
			ui.set_link_transfer_title(("Sending ".to_owned() + &name + " to the box").into());
			ui.set_link_transfer_detail("Reading the file...".into());
			ui.set_link_transfer_can_choose(false);

			let ui_weak = ui_weak.clone();
			let target = DOWNLOAD_TARGET.lock().map(|target| *target).unwrap_or(DownloadTarget::DebugLink);
			let _ = std::thread::spawn(move || {
				match fs::read(&path) {
					Ok(data) if data.is_empty() => {
						let message = "WARNING: ".to_owned() + &path.to_string_lossy() + " is empty!\n";
						let _ = add_console_text(ui_weak.clone(), message, MAMEConsoleScrollMode::ForceScroll, 0);

						let _ = ui_weak.upgrade_in_event_loop(|ui| {
							ui.set_link_transfer_detail("That file is empty. Choose another one.".into());
							ui.set_link_transfer_can_choose(true);
						});
					},
					Ok(data) => {
						let sent = match target {
							DownloadTarget::DebugLink => debuglink::send_command(debuglink::LinkCommand::Download { name, data }),
							DownloadTarget::Pekoe2 => pekoe2::send_command(pekoe2::Pekoe2Command::Download { name, data })
						};

						if !sent {
							let _ = add_console_text(ui_weak, "WARNING: The download path isn't running (Pekoe2 or debug-link)\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);
						}
					},
					Err(e) => {
						let message = "WARNING: Couldn't read ".to_owned() + &path.to_string_lossy() + ": " + &e.to_string() + "\n";
						let _ = add_console_text(ui_weak.clone(), message, MAMEConsoleScrollMode::ForceScroll, 0);
						let _ = ui_weak.upgrade_in_event_loop(|ui| {
							ui.set_link_transfer_detail("Couldn't read that file. Choose another one.".into());
							ui.set_link_transfer_can_choose(true);
						});
					}
				}
			});
		},
		None => {
			ui.set_link_transfer_detail("No file chosen yet. The box waits until one is.".into());
			ui.set_link_transfer_can_choose(true);
		}
	}
}

// Asks where the box's upload goes. Cancelling discards it.
fn choose_debug_link_upload_destination(ui_weak: slint::Weak<MainWindow>, name: String) {
	let ui = ui_weak.unwrap();
	let chooser = DialogBuilder::file()
		.set_title("Save the box's upload")
		.set_location(&debug_link_location(&ui))
		.set_filename(&name);

	let destination = chooser.save_single_file().show().unwrap_or(None);
	if let Some(path) = destination.as_ref() {
		remember_debug_link_location(path);
		if ui.get_link_transfer_active() {
			ui.set_link_transfer_title(("Receiving ".to_owned() + &name + " from the box").into());
		}
	}
	debuglink::send_command(debuglink::LinkCommand::UploadDestination(destination));
}

fn handle_debug_link_event(ui_weak: &slint::Weak<MainWindow>, filter: &Mutex<DebugConsoleFilter>, event: debuglink::LinkEvent) {
	use debuglink::LinkEvent;

	match event {
		LinkEvent::Log(line) => {
			let _ = add_console_text(ui_weak.clone(), line + "\n", MAMEConsoleScrollMode::ConditionalScroll, 0);
		},
		LinkEvent::Console(bytes) => {
			let filtered = filter.lock().ok().map(|mut filter| filter.filter(&bytes));
			if let Some((text, rshift)) = filtered {
				pekoe2::console_text(&text);
				let _ = add_console_text(ui_weak.clone(), text, MAMEConsoleScrollMode::ConditionalScroll, rshift);
			}
		},
		LinkEvent::Connected => {},
		LinkEvent::Disconnected => {
			let _ = ui_weak.upgrade_in_event_loop(|ui| ui.set_link_transfer_active(false));
		},
		LinkEvent::DownloadRequested { request } => {
			if let Ok(mut target) = DOWNLOAD_TARGET.lock() {
				*target = if request == debuglink::REQ_PEKOE2 {
					DownloadTarget::Pekoe2
				} else {
					DownloadTarget::DebugLink
				};
			}

			let ui_weak_cpy = ui_weak.clone();
			let _ = ui_weak.upgrade_in_event_loop(move |ui| {
				let title = "The box is waiting for a download (".to_owned() + debuglink::request_name(request) + ")";
				set_link_transfer(&ui, &title, "Choose the file to send.", false, false);
				choose_debug_link_download(ui_weak_cpy);
			});
		},
		LinkEvent::DownloadStarted { name, total } => {
			let _ = ui_weak.upgrade_in_event_loop(move |ui| {
				let title = "Downloading ".to_owned() + &name + " to the box";
				set_link_transfer(&ui, &title, &("0 of ".to_owned() + &format_bytes(total)), false, false);
			});
		},
		LinkEvent::UploadStarted => {
			let _ = ui_weak.upgrade_in_event_loop(|ui| {
				set_link_transfer(&ui, "Receiving an upload from the box", "Waiting for its name...", false, true);
				ui.set_link_transfer_indeterminate(true);
			});
		},
		LinkEvent::UploadNamed { name } => {
			let ui_weak_cpy = ui_weak.clone();
			let _ = ui_weak.upgrade_in_event_loop(move |ui| {
				ui.set_link_transfer_title(("Receiving ".to_owned() + &name + " from the box").into());
				choose_debug_link_upload_destination(ui_weak_cpy, name);
			});
		},
		LinkEvent::Progress { done, total } => {
			let _ = ui_weak.upgrade_in_event_loop(move |ui| {
				if !ui.get_link_transfer_active() {
					return;
				}
				match total {
					Some(total) if total > 0 => {
						ui.set_link_transfer_indeterminate(false);
						ui.set_link_transfer_progress((done as f64 / total as f64).min(1.0) as f32);
						ui.set_link_transfer_detail((format_bytes(done) + " of " + &format_bytes(total)).into());
					},
					_ => {
						ui.set_link_transfer_indeterminate(total.is_none());
						ui.set_link_transfer_detail(format_bytes(done).into());
					}
				}
			});
		},
		LinkEvent::TransferFinished { message } => {
			let _ = add_console_text(ui_weak.clone(), " \n".to_owned() + &message + "\n", MAMEConsoleScrollMode::ForceScroll, 0);
			let _ = ui_weak.upgrade_in_event_loop(|ui| ui.set_link_transfer_active(false));
		}
	}
}

// Starts the built-in debug link for the parallel port socket: the port MAME connects to, and the link (stops when dropped).
// It takes the console's keys while MAME runs.
fn start_debug_link(ui_weak: slint::Weak<MainWindow>, drx: Receiver<String>) -> Result<(u16, Box<dyn Send>), String> {
	let filter = Mutex::new(DebugConsoleFilter::default());
	let events_ui_weak = ui_weak.clone();
	let events: debuglink::EventFn = std::sync::Arc::new(move |event| handle_debug_link_event(&events_ui_weak, &filter, event));

	match debuglink::DebugLink::start(events, drx) {
		Ok(link) => {
			let port = link.port();
			if let Some(ui) = ui_weak.upgrade() {
				ui.set_using_mame_debug(true);
				ui.set_link_transfer_active(false);
			}
			let _ = add_console_text(ui_weak.clone(), " \nDebug Link started for the parallel port (127.0.0.1:".to_owned() + &port.to_string() + ")\n", MAMEConsoleScrollMode::ForceScroll, 0);
			Ok((port, Box::new(DebugLinkService { _link: link, ui_weak })))
		},
		Err(error) => Err("Couldn't start the Debug Link (".to_owned() + &error.to_string() + "). The parallel port socket won't connect.")
	}
}

struct Pekoe2Service {
	_link: pekoe2::Pekoe2Link,
	ui_weak: slint::Weak<MainWindow>
}

impl Drop for Pekoe2Service {
	fn drop(&mut self) {
		let _ = self.ui_weak.upgrade_in_event_loop(|ui| ui.set_link_transfer_active(false));
	}
}

fn start_pekoe2(ui_weak: slint::Weak<MainWindow>) -> Result<(u16, Box<dyn Send>), String> {
	let filter = Mutex::new(DebugConsoleFilter::default());
	let events_ui_weak = ui_weak.clone();
	let events: debuglink::EventFn = std::sync::Arc::new(move |event| handle_debug_link_event(&events_ui_weak, &filter, event));

	match pekoe2::Pekoe2Link::start(events) {
		Ok(link) => Ok((link.port(), Box::new(Pekoe2Service { _link: link, ui_weak }))),
		Err(error) => Err("Couldn't start the Pekoe2's parallel port (".to_owned() + &error.to_string() + "). Downloads won't work")
	}
}

// Starts the built-in WebTV Bridge for the parallel port socket: the port MAME connects to, and the bridge (stops when dropped).
#[cfg(webtv_bridge)]
fn start_webtv_bridge(ui_weak: slint::Weak<MainWindow>) -> Result<(u16, Box<dyn Send>), String> {
	let executable_dir = env::current_exe().ok().and_then(|path| LauncherConfig::get_parent_from_pathbuf(path).ok()).unwrap_or(".".into());
	let state_dir = Path::new(&executable_dir).join(WEBTV_BRIDGE_STATE_DIR);

	let log_ui_weak = ui_weak.clone();
	let log: webtvbridge::LogFn = std::sync::Arc::new(move |line: &str| {
		let _ = add_console_text(log_ui_weak.clone(), line.to_owned() + "\n", MAMEConsoleScrollMode::ConditionalScroll, 0);
	});

	match webtvbridge::WebTVBridge::start(webtvbridge::BridgeOptions::new(state_dir, log)) {
		Ok(bridge) => {
			let port = bridge.port();
			let _ = add_console_text(ui_weak, " \nWebTV Bridge started for the parallel port (127.0.0.1:".to_owned() + &port.to_string() + ")\n", MAMEConsoleScrollMode::ForceScroll, 0);
			Ok((port, Box::new(bridge)))
		},
		Err(error) => Err("Couldn't start the WebTV Bridge (".to_owned() + &error.to_string() + "). The parallel port socket won't connect.")
	}
}

#[cfg(not(webtv_bridge))]
fn start_webtv_bridge(_ui_weak: slint::Weak<MainWindow>) -> Result<(u16, Box<dyn Send>), String> {
	Err("This launcher was built without the WebTV Bridge (the \"webtv-bridge\" feature). The parallel port socket won't connect.".into())
}

fn start_netstack(ui_weak: slint::Weak<MainWindow>, mame_executable_path: &String, mame_directory_path: &String, mame_command: &mut Command, selected_box: &str) -> Option<NetworkStack> {
	let network_options = match LauncherConfig::get_persistent_config() {
		Ok(config) => config.network,
		_ => None
	}.map(NetworkOptions::with_defaults).unwrap_or(NetworkOptions::default_options());

	if !network_options.ethernet_enabled.unwrap_or(true) {
		return None;
	}

	if !LauncherConfig::mame_supports_option(mame_executable_path, "network_socket") {
		let _ = add_console_text(ui_weak.clone(), "WARNING: This MAME build doesn't support the launcher network so ethernet won't be available. Update MAME to use ethernet.\n".into(), MAMEConsoleScrollMode::ForceScroll, 0);

		return None;
	}

	let network_device = if selected_box == "msntv2" { MSNTV2_ETHERNET_DEVICE_TAG } else { ":wtvnet" };
	if let Err(e) = LauncherConfig::set_mame_network_interface(mame_directory_path, selected_box, network_device, 0) {
		let _ = add_console_text(ui_weak.clone(), "WARNING: Couldn't select the ethernet network device in MAME's config. Ethernet may not work.\n".to_owned() + &e.to_string() + "\n", MAMEConsoleScrollMode::ForceScroll, 0);
	}

	let dns0000_target = network_options.dns0000_target
		.map(|target| target.trim().to_string())
		.filter(|target| target != "")
		.unwrap_or(DEFAULT_DNS0000_TARGET.into());

	let dns_overrides: Vec<(String, String)> = network_options.dns_overrides.unwrap_or_default()
		.into_iter()
		.map(|(domain, target)| {
			if target.trim() == "0.0.0.0" {
				(domain, dns0000_target.clone())
			} else {
				(domain, target)
			}
		})
		.collect();

	let ip_overrides: Vec<(String, Vec<String>)> = network_options.ip_overrides.unwrap_or_default()
		.into_iter()
		.map(|(target, guest_ips)| {
			if target.trim() == "0.0.0.0" {
				(dns0000_target.clone(), guest_ips)
			} else {
				(target, guest_ips)
			}
		})
		.collect();

	let ui_weak_cpy = ui_weak.clone();
	let network_log = Box::new(move |text: String| {
		let _ = add_console_text(ui_weak_cpy.clone(), text, MAMEConsoleScrollMode::ConditionalScroll, 0);
	});

	match NetworkStack::start(dns_overrides, ip_overrides, network_log) {
		Ok(network_stack) => {
			mame_command.arg("-networkprovider").arg("socket");
			mame_command.arg("-network_socket").arg("127.0.0.1:".to_owned() + &network_stack.port().to_string());

			Some(network_stack)
		},
		Err(e) => {
			let _ = add_console_text(ui_weak.clone(), "ERROR: Couldn't start the launcher network! Ethernet won't be available.\n".to_owned() + &e.to_string() + "\n", MAMEConsoleScrollMode::ForceScroll, 0);

			None
		}
	}
}

fn end_mame(ui_weak: slint::Weak<MainWindow>) {
	let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);

	if mame_pid > 0 {
		let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
			ui.set_mame_console_enabled(false);
		});

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Closing MAME".into());
			
			if let Ok(sys) = System::new_all() {
				if let Some(process) = sys.process(Pid::from(mame_pid as usize)) {
					process.kill();
				}
			}

			let _ = save_config(ui_weak.clone(), true, None, None, None);

			disable_loading(&ui_weak);

			let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
				clear_console_text(&ui);
				let _= set_mame_pid(ui_weak.clone(), 0);
			});

		});
	} else {
		let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
			if ui.get_mame_console_enabled() {
				ui.set_mame_console_enabled(false);
				clear_console_text(&ui);
			}
		});
	}
}

#[cfg(target_os = "linux")]
fn send_keypess_linux(ui_weak: slint::Weak<MainWindow>, text: String, _shiftmod: bool) {
	let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);

	if mame_pid > 0 && text.len() > 0 {
		unsafe {
			let xdo: *mut libxdo_sys::xdo_t = libxdo_sys::xdo_new(std::ptr::null());

			// Store our (mame_launcher) window ID :: no longer needed with -background_input
			//let mut current_window: u64 = 0;
			//libxdo_sys::xdo_get_active_window(xdo, &mut current_window);

			// Search for MAME window ID via PID
			let search_query = libxdo_sys::Struct_xdo_search {
				title: std::ptr::null(),
				winclass: std::ptr::null(),
				winclassname: std::ptr::null(),
				winname: std::ptr::null(),
				pid: mame_pid as i32,
				max_depth: -1,
				only_visible: 0,
				screen: 0,
				require: 0,
				searchmask: (1 << 3) as u32, // SEARCH_PID
				desktop: 0,
				limit: 1,
			};
			// This would be an array but we will always try the first item since we're searching via PID.
			let mut mame_window: *mut u64 = std::ptr::null_mut();
			let mut search_result_count: u32 = 0;
			libxdo_sys::xdo_search_windows(xdo, &search_query,  &mut mame_window, &mut search_result_count);

			if search_result_count > 0 {
				let recv_bytes = text.as_bytes();

				// Bring MAME window into focus so it'll respond to keypresses. :: no longer needed with -background_input
				//libxdo_sys::xdo_activate_window(xdo, *mame_window);
				//libxdo_sys::xdo_wait_for_window_active(xdo, *mame_window, 1);

				match recv_bytes[0] {
					0x0a => {
						let send_bytes: [u8; 2] = [0x0d, 0x00];
						libxdo_sys::xdo_enter_text_window(xdo, *mame_window, send_bytes.as_ptr() as *const i8, CONSOLE_KEY_DELAY);
					},
					_ => {
						let send_bytes: [u8; 2] = [recv_bytes[0], 0x00];
						libxdo_sys::xdo_enter_text_window(xdo, *mame_window, send_bytes.as_ptr() as *const i8, CONSOLE_KEY_DELAY);
					}
				}

				// Go back to the launcher (this) window. :: no longer needed with -background_input
				//libxdo_sys::xdo_activate_window(xdo, current_window);
				//libxdo_sys::xdo_wait_for_window_active(xdo, current_window, 1);
			}

			libxdo_sys::xdo_free(xdo);
		}
	}
}

#[cfg(target_os = "windows")]
fn to_wstring(value: &str) -> Vec<u16> {
	use std::os::windows::ffi::OsStrExt;

	std::ffi::OsStr::new(value)
		.encode_wide()
		.chain(std::iter::once(0))
		.collect()
}
// Keeping this here in case we have problems when people have multiple MAME windows open. Need to convert lparam to a struct with the pid and key params.
/*#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_wnd_proc(hwnd: winapi::shared::windef::HWND, lparam: winapi::shared::minwindef::LPARAM) -> winapi::shared::minwindef::BOOL {
	GetWindowThreadProcessId(hwnd, &mut window_pid);

	let mame_pid: u32 = lparam as u32;

	if window_pid == mame_pid {

		return 0;
	}

	return 1;
}*/
#[cfg(target_os = "windows")]
fn send_keypess_windows(ui_weak: slint::Weak<MainWindow>, text: String, shiftmod: bool) {
	let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);

	if mame_pid > 0 && text.len() > 0 {
		unsafe {
			//EnumWindows(Some(enum_wnd_proc), mame_pid as isize);
			let hwnd = FindWindowW(to_wstring("MAME").as_ptr(), std::ptr::null_mut());
			if hwnd != std::ptr::null_mut() {
				let text_lc = text.to_lowercase();
				let recv_byteslc = text_lc.as_bytes();

				let vkey: i16 = VkKeyScanA(recv_byteslc[0] as i8);
				if vkey == 0x0250 || vkey == 0x0255 { // shift key
					return;
				}
				let scancode: u32 = match vkey {
					0x020d => 0x1c, // return key
					_ =>  MapVirtualKeyA(vkey as u32, 0x04 /* MAPVK_VK_TO_VSC_EX */)
				};

				let w_param: usize = vkey as usize;
				let mut l_param: isize = (scancode as isize) << 0x10 | 1;

				if shiftmod {
					PostMessageW(hwnd, winapi::um::winuser::WM_KEYDOWN, 0x10, 0x2a0001);
					std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));
				}
				PostMessageW(hwnd, winapi::um::winuser::WM_KEYDOWN, w_param, l_param);

				std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));

				if shiftmod {
					PostMessageW(hwnd, winapi::um::winuser::WM_KEYUP, 0x10, 0xc02a0001);
					std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));
				}
				l_param |= (1 << 0x1e) | (1 << 0x1f);
				PostMessageW(hwnd, winapi::um::winuser::WM_KEYUP, w_param, l_param);
			}
		}
	}
}

#[cfg(target_os = "macos")]
fn send_keypess_macos(ui_weak: slint::Weak<MainWindow>, text: String, shiftmod: bool) {
	let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);

	if mame_pid > 0 && text.len() > 0 {
		match CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
			Ok(event_source) => {
				let text_lc = text.to_lowercase();
				let recv_byteslc = text_lc.as_bytes();

				// Translate a character code from the console to a macOS keycode.
				// This assumes a standard US keyboard layout.
				let keycode = match recv_byteslc[0] {
					0x61 => 0x00, // a
					0x73 => 0x01, // s
					0x64 => 0x02, // d
					0x66 => 0x03, // f
					0x68 => 0x04, // h
					0x67 => 0x05, // g
					0x7a => 0x06, // z
					0x78 => 0x07, // x
					0x63 => 0x08, // c
					0x76 => 0x09, // v
					0x62 => 0x0b, // b
					0x71 => 0x0c, // q
					0x77 => 0x0d, // w
					0x65 => 0x0e, // e
					0x72 => 0x0f, // r
					0x79 => 0x10, // y
					0x74 => 0x11, // t
					0x31 => 0x12, // 1
					0x32 => 0x13, // 2
					0x33 => 0x14, // 3
					0x34 => 0x15, // 4
					0x36 => 0x16, // 6
					0x35 => 0x17, // 5
					0x3d => 0x18, // =
					0x39 => 0x19, // 9
					0x37 => 0x1a, // 7
					0x2d => 0x1b, // -
					0x38 => 0x1c, // 8
					0x30 => 0x1d, // 0
					0x5d => 0x1e, // ]
					0x6f => 0x1f, // o
					0x75 => 0x20, // u
					0x5b => 0x21, // [
					0x69 => 0x22, // i
					0x70 => 0x23, // p
					0x6c => 0x25, // l
					0x6a => 0x26, // j
					0x27 => 0x27, // '
					0x6b => 0x28, // k
					0x3b => 0x29, // ;
					0x5c => 0x2a, // \
					0x2c => 0x2b, // ,
					0x2f => 0x2c, // /
					0x6e => 0x2d, // n
					0x6d => 0x2e, // m
					0x2e => 0x2f, // .
					0x60 => 0x32, // `
					0x2a => 0x43, // *
					0x2b => 0x45, // +
					0x21 => 0x12, // ! (when shiftmod = true)
					0x40 => 0x13, // @ (when shiftmod = true)
					0x23 => 0x14, // # (when shiftmod = true)
					0x24 => 0x15, // $ (when shiftmod = true)
					0x25 => 0x17, // % (when shiftmod = true)
					0x5e => 0x16, // ^ (when shiftmod = true)
					0x26 => 0x1a, // & (when shiftmod = true)
					0x28 => 0x19, // ( (when shiftmod = true)
					0x29 => 0x1d, // ) (when shiftmod = true)
					0x5f => 0x1b, // _ (when shiftmod = true)
					0x7c => 0x2a, // | (when shiftmod = true)
					0x7d => 0x1e, // } (when shiftmod = true)
					0x7b => 0x21, // { (when shiftmod = true)
					0x3a => 0x29, // : (when shiftmod = true)
					0x22 => 0x27, // " (when shiftmod = true)
					0x3c => 0x2b, // < (when shiftmod = true)
					0x3e => 0x2f, // > (when shiftmod = true)
					0x3f => 0x2c, // ? (when shiftmod = true)
					0x7e => 0x32, // ~ (when shiftmod = true)
					0x09 => 0x30, // TAB
					0x20 => 0x31, // SPACE
					0x0a => 0x24, // RETURN
					0x08 => 0x33, // DELETE
					0xef => 0x47, // default to CLEAR (which will be ignored)
					_    => 0x47  // default to CLEAR (which will be ignored)
				};

				if keycode != 0x47 {
					// Press the key
					if shiftmod {
						match CGEvent::new_keyboard_event(event_source.clone(), KeyCode::SHIFT, true) {
							Ok(event) => {
								event.post_to_pid(mame_pid.try_into().unwrap_or(0));
								std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));
							}
							_ => {}
						}
					}
					match CGEvent::new_keyboard_event(event_source.clone(), keycode, true) {
						Ok(event) => {
							if shiftmod {
								event.set_flags(CGEventFlags::CGEventFlagShift | event.get_flags());
							}
							event.post_to_pid(mame_pid.try_into().unwrap_or(0));

							std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));

							// Release the key so it doesn't repeat
							if shiftmod {
								match CGEvent::new_keyboard_event(event_source.clone(), KeyCode::SHIFT, false) {
									Ok(event) => {
										std::thread::sleep(std::time::Duration::from_micros(CONSOLE_KEY_DELAY));
										event.post_to_pid(mame_pid.try_into().unwrap_or(0));
									}
									_ => {}
								}
							}
							match CGEvent::new_keyboard_event(event_source.clone(), keycode, false) {
								Ok(event) => {
									if shiftmod {
										event.set_flags(CGEventFlags::CGEventFlagShift | event.get_flags());
									}
									event.post_to_pid(mame_pid.try_into().unwrap_or(0));
								},
								_ => {}
							};
						},
						_ => {}
					};
				}
			},
			_ => {}
		};
	}
}

// Saving the config can block the UI thread long enough to drop frames, so wait for show/hide animations (200ms) to finish.
fn save_config_after_animation(ui_weak: slint::Weak<MainWindow>) {
	slint::Timer::single_shot(std::time::Duration::from_millis(300), move || {
		let _ = save_config(ui_weak, false, None, None, None);
	});
}

fn resize_window_when_allowed(ui_weak: slint::Weak<MainWindow>, width: f32, height: f32, tries: u32) {
	slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
		let Some(ui) = ui_weak.upgrade() else { return };

		let window_size = ui.window().size().to_logical(ui.window().scale_factor());
		if (window_size.width - width).abs() <= 1.0 && (window_size.height - height).abs() <= 1.0 {
			restore_window_size_limits(&ui);
			return;
		}

		ui.window().set_size(slint::LogicalSize::new(width, height));
		restore_window_size_limits(&ui);
		if tries > 0 {
			resize_window_when_allowed(ui_weak, width, height, tries - 1);
		}
	});
}

fn restore_window_size_limits(ui: &MainWindow) {
	use slint::winit_030::{winit, WinitWindowAccessor};

	// Too large a maximum = no maximum.
	let max_size = |width: f32, height: f32| -> Option<winit::dpi::Size> {
		if width >= 100000.0 && height >= 100000.0 {
			None
		} else {
			Some(winit::dpi::LogicalSize::new(width as f64, height as f64).into())
		}
	};

	let min_size: winit::dpi::Size = winit::dpi::LogicalSize::new(ui.get_window_min_width() as f64, ui.get_window_min_height() as f64).into();
	let max_size = max_size(ui.get_window_max_width(), ui.get_window_max_height());

	ui.window().with_winit_window(|winit_window| {
		winit_window.set_min_inner_size(Some(min_size));
		winit_window.set_max_inner_size(max_size);
	});
}

// Calls on_ready once the window is at least target_height tall (checked every 10ms).
// Gives up waiting after tries_left checks (the window manager may not allow the size) and calls on_ready anyway.
fn wait_for_window_height(ui_weak: slint::Weak<MainWindow>, target_height: f32, tries_left: u32, on_ready: impl FnOnce(MainWindow) + 'static) {
	slint::Timer::single_shot(std::time::Duration::from_millis(10), move || {
		let Some(ui) = ui_weak.upgrade() else { return };

		let window_height = ui.window().size().to_logical(ui.window().scale_factor()).height;
		if window_height + 1.0 >= target_height || tries_left == 0 {
			on_ready(ui);
		} else {
			wait_for_window_height(ui_weak, target_height, tries_left - 1, on_ready);
		}
	});
}

fn start_ui() -> Result<(), slint::PlatformError> {
	// The hook only changes the dropdown list window
	let _ = slint::BackendSelector::new()
		.with_winit_window_attributes_hook(externalpopup::window_attributes_hook)
		.select();

	let ui = MainWindow::new().unwrap();

	// Lets the selected box dropdown extend outside the window (but not on Wayland since it's broken on there)
	let _ = externalpopup::setup(&ui);

	mamecapture::setup(&ui);
	let ui_weak_capture = ui.as_weak();
	ui.on_mame_area_changed(move || {
		if let Some(ui) = ui_weak_capture.upgrade() {
			mamecapture::update_geometry(&ui);

			// Remember the captured window size for next time
			if ui.get_mame_captured() && ui.get_mame_console_enabled() {
				let ui_mame = ui.global::<UIMAMEOptions>();
				let window_size = ui.window().size().to_logical(ui.window().scale_factor());
				ui_mame.set_captured_window_width(window_size.width);
				ui_mame.set_captured_window_height(window_size.height);
			}
		}
	});

	let (dtx, drx) = bounded(10);

	let _ = load_config(ui.as_weak());

	let mut ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_box(move || {
		let ui = ui_weak.unwrap();
		let ui_mame = ui.global::<UIMAMEOptions>();

		ui_mame.set_selected_modem_bitb_startpoint("".into());
		ui_mame.set_selected_modem_moip_startpoint("".into());
		ui_mame.set_selected_debug_bitb_startpoint("".into());
		ui_mame.set_selected_lirc_startpoint("".into());

		let _ = save_config(ui_weak.clone(), true, None, None, None);
	});

	ui_weak = ui.as_weak();
	let advanced_options_opening = std::rc::Rc::new(std::cell::Cell::new(false));
	ui.global::<UIMAMEOptions>().on_toggle_advanced_options(move || {
		let ui = ui_weak.unwrap();
		let ui_mame = ui.global::<UIMAMEOptions>();

		// Ignore clicks while waiting for the window to grow.
		if advanced_options_opening.get() {
			return;
		}

		if ui_mame.get_show_advanced_options() {
			// (Closing) the window keeps its height while the options slide closed then shrinks once (see launcher-height).
			ui_mame.set_show_advanced_options(false);
			save_config_after_animation(ui_weak.clone());
			return;
		}

		// (Opening) grow the window first then slide the options open inside it.
		// Resizing and the first paint at the new size can take a while, starting the animation at the same time would skip most of it.
		let target_height = ui.get_advanced_expanded_content_height();
		ui.set_launcher_height_hold(target_height);
		advanced_options_opening.set(true);

		let advanced_options_opening = advanced_options_opening.clone();
		wait_for_window_height(ui_weak.clone(), target_height, 50, move |ui| {
			ui.global::<UIMAMEOptions>().set_show_advanced_options(true);
			// The window can't shrink while the options are animating so the hold isn't needed anymore.
			ui.set_launcher_height_hold(0.0);
			advanced_options_opening.set(false);

			save_config_after_animation(ui.as_weak());
		});
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_bootrom(move || {
		match LauncherConfig::get_persistent_config() {
			Ok(config) => {
				let ui = ui_weak.unwrap();
				let ui_mame = ui.global::<UIMAMEOptions>();

				let selected_box = ui_mame.get_selected_box().clone().to_string();
				let selected_bootrom = ui_mame.get_selected_bootrom().clone().to_string();

				let mut selected_bootroms = match config.mame_options.selected_bootroms {
					Some(bootroms) => bootroms,
					_ => HashMap::new()
				};
				selected_bootroms.insert(selected_box.clone(), selected_bootrom.clone());

				let _ = save_config(ui_weak.clone(), true, Some(selected_bootroms), None, None);
			},
			_ => {
				//
			}
		}
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_fart_options_changed(move || {
		let _ = save_config(ui_weak.clone(), false, None, None, None);
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_approm(move || {
		let _ = start_approm_select(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_revert_approm(move || {
		let _ = revert_approm(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_import_bootrom(move || {
		let _ = start_bootrom_import(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_import_approm(move || {
		let _ = start_approm_import(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_choose_hdimg(move || {
		let _ = choose_hdimg(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_unset_hdimg(move || {
		let _ = unset_hdimg(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_start_mame(move || {
		enable_loading(&ui_weak, "Starting MAME".into());

		let _ = check_custom_ssid(ui_weak.clone());

		let _ = start_mame(ui_weak.clone(), drx.clone());

		disable_loading(&ui_weak);
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_generate_ssid(move || {
		let ui = ui_weak.unwrap();
		let ui_mame = ui.global::<UIMAMEOptions>();

		let selected_box = ui_mame.get_selected_box().clone().to_string();

		if selected_box == "msntv2" {
			match MSNTV2BoxIdInfo::generate() {
				Ok(boxid_info) => {
					let _ = save_boxid(boxid_info.raw, ui_weak.clone(), false, false);
				},
				_ => {

				}
			}
		} else {
			let selected_ssid_manufacture = ui_weak.unwrap().global::<UIMAMEOptions>().get_selected_ssid_manufacture();

			match SSIDInfo::generate(
				SSIDBoxType::MAME, 
				SSIDManufacture::from_u16(u16::from_str_radix(&selected_ssid_manufacture.to_string().trim_start_matches("0x"), 16).unwrap_or(0x0000))
			) {
				Ok(ssid_info) => {
					let _ = save_ssid(ssid_info.raw, ui_weak.clone(), false, false);
				},
				_ => {

				}
			}
		}
	});

	ui_weak = ui.as_weak();
	ui.global::<UIPaths>().on_choose_mame(move || {
		match choose_executable_file(ui_weak.clone()) {
			Ok(executable_file_path) => {
				if executable_file_path != "" {
					let ui = ui_weak.unwrap();
					let ui_paths = ui.global::<UIPaths>();
				
					ui_paths.set_mame_path(executable_file_path.into());
	
					let _ = save_config(ui_weak.clone(), true, None, None, None);
				}
			},
			_ => { }
		}
	});

	ui_weak = ui.as_weak();
	ui.global::<UIPaths>().on_choose_python(move || {
		match choose_executable_file(ui_weak.clone()) {
			Ok(executable_file_path) => {
				if executable_file_path != "" {
					let ui = ui_weak.unwrap();
					let ui_paths = ui.global::<UIPaths>();
				
					ui_paths.set_python_path(executable_file_path.into());

					let _ = check_rommy(ui_weak.clone());

					let _ = save_config(ui_weak.clone(), false, None, None, None);
				}
			},
			_ => { }
		}
	});

	ui_weak = ui.as_weak();
	ui.global::<UIPaths>().on_choose_rommy(move || {
		match choose_executable_file(ui_weak.clone()) {
			Ok(executable_file_path) => {
				if executable_file_path != "" {
					let ui = ui_weak.unwrap();
					let ui_paths = ui.global::<UIPaths>();
				
					ui_paths.set_rommy_path(executable_file_path.into());

					let _ = check_rommy(ui_weak.clone());

					let _ = save_config(ui_weak.clone(), false, None, None, None);
				}
			},
			_ => { }
		}
	});

	ui.global::<UIPaths>().on_open_url_path(move |url| {
		let _ = open::that(url.to_string());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIAbout>().on_do_fart(move || {
		let _ = do_fart(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.on_send_key_to_mame(move |text, shiftmod| {
		let ui = ui_weak.unwrap();
		if ui.get_mame_console_enabled() {
			if ui.get_using_mame_debug() {
				let _ = dtx.send(text.to_string());
			} else {
				// bf0 versions of WebTV use the hardware keyboard for serial input and smartcard bigbang for serial output.
				// MAME printfs the smartcard data to the console, and this captures the keystrokes on the console to be sent to MAME.
				// When MAME supports solo-based boxes this will be changed a bit but this works for now.

				#[cfg(target_os = "linux")]
				send_keypess_linux(ui_weak.clone(), text.to_string(), shiftmod);
				#[cfg(target_os = "windows")]
				send_keypess_windows(ui_weak.clone(), text.to_string(), shiftmod);
				#[cfg(target_os = "macos")]
				send_keypess_macos(ui_weak.clone(), text.to_string(), shiftmod);
			}
		}
	});

	ui.set_mame_console_lines(slint::ModelRc::new(ConsoleLines::default()));

	ui_weak = ui.as_weak();
	let link_ui_weak = ui.as_weak();
	ui.on_link_transfer_choose(move || {
		choose_debug_link_download(link_ui_weak.clone());
	});

	ui.on_link_transfer_cancel(move || {
		debuglink::send_command(debuglink::LinkCommand::CancelUpload);
	});

	ui.on_copy_console_range(move |start_row, start_col, end_row, end_col| {
		let ui = ui_weak.unwrap();
		let lines = ui.get_mame_console_lines();
		let end_row = (end_row.max(0) as usize).min(lines.row_count().saturating_sub(1));

		let console_text = (start_row.max(0) as usize..=end_row)
			.filter_map(|row| lines.row_data(row).map(|line| (row, line)))
			.map(|(row, line)| {
				let line = line.text;
				let start = if row == start_row as usize { start_col.max(0) as usize } else { 0 };
				let end = if row == end_row { end_col.max(0) as usize } else { usize::MAX };
				line.chars().skip(start).take(end.saturating_sub(start)).collect::<String>()
			})
			.collect::<Vec<_>>()
			.join("\n");

		ui.set_mame_console_copy_text(console_text.into());
		ui.invoke_copy_console_text();
		ui.set_mame_console_copy_text("".into());
	});

	ui_weak = ui.as_weak();
	ui.on_console_word_span(move |row, col| {
		let ui = ui_weak.unwrap();
		let line: Vec<char> = ui.get_mame_console_lines().row_data(row.max(0) as usize).unwrap_or_default().text.chars().collect();
		let col = (col.max(0) as usize).min(line.len());

		let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
		let clicked = if col < line.len() { col } else if col > 0 { col - 1 } else { return ConsoleWordSpan { start: 0, end: 0 } };
		let clicked_class = class(line[clicked]);

		let mut start = clicked;
		let mut end = clicked + 1;
		if clicked_class != 2 {
			while start > 0 && class(line[start - 1]) == clicked_class {
				start -= 1;
			}
			while end < line.len() && class(line[end]) == clicked_class {
				end += 1;
			}
		}

		ConsoleWordSpan { start: start as i32, end: end as i32 }
	});

	ui_weak = ui.as_weak();
	ui.on_close_mame(move || {
		end_mame(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.window().on_close_requested(move || {
		// Stop MAME before the launcher exits, important if MAME was drawn inside the launcher.
		let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);
		if mame_pid > 0 {
			if let Ok(sys) = System::new_all() {
				if let Some(process) = sys.process(Pid::from(mame_pid as usize)) {
					process.kill();
				}
			}
		}
		mamecapture::stop();

		end_mame(ui_weak.clone());

		slint::CloseRequestResponse::HideWindow
	});

	ui.run()
}