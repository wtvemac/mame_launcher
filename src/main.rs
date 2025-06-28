// By: Eric MacDonald (eMac)

#![windows_subsystem = "windows"]

mod config;
mod wtv;

use std::{
	collections::HashMap, 
	fs::File,
	io::{BufReader, Read, Write, Cursor, Seek, SeekFrom}, 
	path::Path, process::{Command, Stdio},
	env,
	process::{ChildStdout, ChildStderr},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering::Relaxed}
	}
};

use rand::{
	Rng,
	distributions::{Alphanumeric, DistString}
};
use regex::Regex;
use native_dialog::FileDialog;
use sysinfo::{Pid, System};
use rodio;
use serialport;
use hex;
use which::which;
use open;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

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

use config::{LauncherConfig, MAMEMachineNode, MAMEOptions, Paths, PersistentConfig};
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
		BuildInfo
	},
	ssid::{SSIDInfo, SSIDBoxType, SSIDManufacture}
};

slint::include_modules!();

const STACK_SIZE: usize = 32 * 1024 * 1024;
const SSID_ROM_FILE: &'static str = "ds2401.bin";
const DEFAULT_BOOTORM_FILE_NAME: &'static str = "bootrom.o";
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
const DEFAULT_FLASHDISK_SIZE: u64 = 8 * 1024 * 1024;
const PUBLIC_TOUCHPP_ADDRESS: &'static str = "wtv.ooguy.com:1122";
const CONSOLE_READ_BUFFER_SIZE: usize = 1024;
const CONSOLE_SCROLLBACK_LINES: usize = 9000;
#[cfg(target_os = "linux")]
const CONSOLE_KEY_DELAY: u32 = 200 * 1000;
#[cfg(target_os = "windows")]
const CONSOLE_KEY_DELAY: u64 = 25 * 1000;
#[cfg(target_os = "macos")]
const CONSOLE_KEY_DELAY: u64 = 55 * 1000;

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
	FlashdiskBuild
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
	BadBaseAddress
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
enum SSIDStorageState {
	UnknownSSIDState,
	SSIDLooksGood,
	FileNotFound,
	ManufactureMismatch,
	CRCMismatch,
	BoxTypeMismatch,
	CantReadSSID
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum SlotType {
	ModemSerial,
	DebugSerial,
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
	pub ssid_info: Option<SSIDInfo>
}

// Slot item that uses the null_modem bitbanger.
// Keeping track of these so we can select a slot to use for the modem or debug serial port.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct MachineSlotItem {
	pub hint: slint::SharedString,
	pub value: slint::SharedString,
	pub description: slint::SharedString,
	pub bitbanger_name: String,
	pub slot_name: String,
	pub slot_type: SlotType
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

		biossets.insert(biosset_name + ".o", biosset_description);
	}

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
					.into()
			} else if rom_name != DEFAULT_BOOTORM_FILE_NAME {
				continue;
			}

			let mut bootrom = VerifiableBuildItem {
				hint: "".into(),
				value: rom_name.clone().into(),
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
				match BuildMeta::open_rom(bootrom_path, None) {
					Ok(build_meta) => {
						bootrom.build_info = Some(build_meta.build_info[0].clone());
						bootrom.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
	
						if build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
							bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
						} else if build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
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
					match BuildMeta::open_rom(bootrom_path_prefix, Some(BuildIODataCollation::StrippedROMs)) {
						Ok(build_meta) => {
							bootrom.build_info = Some(build_meta.build_info[0].clone());
							bootrom.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();
		
							if build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
								bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
							} else if build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
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
				match BuildMeta::open_rom(approm_path_prefix.clone(), Some(BuildIODataCollation::StrippedROMs)) {
					Ok(build_meta) => {
						approm.build_info = Some(build_meta.build_info[0].clone());
						approm.hint = build_meta.build_info[0].build_header.build_version.clone().to_string().into();

						if build_meta.build_info[0].build_header.code_checksum != build_meta.build_info[0].calculated_code_checksum {
							approm.build_storage_state = BuildStorageState::CodeChecksumMismatch;
						} else if build_meta.build_info[0].romfs_header.romfs_checksum != build_meta.build_info[0].calculated_romfs_checksum {
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

fn populate_approms_from_disk_file(approms: &mut Vec<VerifiableBuildItem>, file_path: String, collation: Option<BuildIODataCollation>, prefix: String, discription: String, is_preset_disk: bool)  -> Result<(), Box<dyn std::error::Error>> {
	let can_revert = match is_preset_disk {
		true => {
			let diff_file_path = CompressedHunkDiskIO::find_diff_file(file_path.clone()).unwrap_or("".into());

			diff_file_path != "" && Path::new(&diff_file_path).exists()
		},
		_ => false
	};

	match BuildMeta::open_disk(file_path.clone(), collation) {
		Ok(build_meta) => {
			let mut build_index = 0;
			for buildinfo in build_meta.build_info.iter() {
				let mut approm = VerifiableBuildItem {
					hint: "".into(),
					value: (prefix.clone() + "[" + &build_index.to_string() + "]").into(),
					status: "".into(),
					can_revert: can_revert,
					description: discription.clone().into(),
					hash: "".into(),
					build_storage_type: BuildStorageType::DiskBuild,
					build_storage_state: BuildStorageState::UnknownBuildState,
					build_info: None
				};

				if build_index == build_meta.selected_build_index {
					approm.status = "selected".to_string();
				}

				approm.build_info = Some(buildinfo.clone());
				approm.hint = buildinfo.build_header.build_version.clone().to_string().into();

				if build_meta.build_count == 0 {
					approm.build_storage_state = BuildStorageState::CantReadBuild;
				} else if buildinfo.build_header.code_checksum != buildinfo.calculated_code_checksum {
					approm.build_storage_state = BuildStorageState::CodeChecksumMismatch;
				} else if buildinfo.romfs_header.romfs_checksum != buildinfo.calculated_romfs_checksum {
					approm.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
				} else if buildinfo.build_header.build_base_address < APPROM3_DISK_BASE_ADDRESS_MIN || buildinfo.build_header.build_base_address > APPROM3_DISK_BASE_ADDRESS_MAX {
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

	let disk_collation = match Regex::new(r"^wtv\d+utv").unwrap().is_match(selected_box.as_str()) {
		true => BuildIODataCollation::ByteSwapped1632,
		false => BuildIODataCollation::ByteSwapped16,
	};

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
					true
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
			false
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
		build_storage_type: BuildStorageType::DiskBuild,
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

	if Path::new(&file_path).exists() {
		if flashdisk_size > Path::new(&file_path).metadata().unwrap().len() {
			// The file is borked so remove it. There are some cases where this isn't intended but most times this will correct some issues.
			let _ = std::fs::remove_file(&file_path);
			approm.build_storage_state = BuildStorageState::FileNotFound;
		} else {
			match BuildMeta::open_flashdisk(file_path, Some(BuildIODataCollation::Raw)) {
				Ok(build_meta) => {
					let mut build_index = 0;
					for buildinfo in build_meta.build_info.iter() {

						if build_index == build_meta.selected_build_index {
							approm.status = "selected".to_string();
						}

						approm.build_info = Some(buildinfo.clone());
						approm.hint = buildinfo.build_header.build_version.clone().to_string().into();
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

fn get_ssids(config: &LauncherConfig, selected_machine: &MAMEMachineNode) -> Result<Vec<VerifiableSSIDItem>, Box<dyn std::error::Error>> {
	let mut ssids: Vec<VerifiableSSIDItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

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

	let mut ssid = VerifiableSSIDItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		ssid_storage_state: SSIDStorageState::UnknownSSIDState,
		ssid_info: None
	};

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
		if device_type == "bitbanger" {
			let mut bitbanger_briefname = "".to_string();
			for instance in device.instance.clone().unwrap_or(vec![]).iter() {
				bitbanger_briefname =
					instance.briefname
					.clone()
					.unwrap_or("".into());
				if bitbanger_briefname != "" {
					break;
				}
			}
			if bitbanger_briefname != "" {
				let device_tag = 
					device.tag
					.clone()
					.unwrap_or("".into());
				match Regex::new(r"^(?<slot_name>.+?)\:null_modem\:stream$").unwrap().captures(device_tag.as_str()) {
					Some(matches) => {
						bitbangers.insert(matches["slot_name"].to_string(), bitbanger_briefname);
					}
					None => {
					}
				}
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
					bitbanger_name: bitbangers[&slot_name.clone()].clone().into(),
					slot_name: slot_name.clone().into(),
					slot_type: SlotType::Unknown
				};
				if Regex::new(r"(modem|mdm)").unwrap().is_match(slot_name.clone().as_str()) {
					slot.slot_type = SlotType::ModemSerial;
				} else if Regex::new(r"(debug|dbg|pekoe)").unwrap().is_match(slot_name.clone().as_str()) {
					slot.slot_type = SlotType::DebugSerial;
				}

				slots.push(slot);
			}
		}
	}

	Ok(slots)
}

fn populate_selected_box_bootroms(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool) -> Result<(usize, BuildStorageState), Box<dyn std::error::Error>> {
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
					hint: available_bootrom.hint.clone(),
					tooltip: available_bootrom.description.clone(),
					value: available_bootrom.value.clone()
				}
			);
		}

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
						ui.set_launcher_state_message("Unknown BootROM state. Please choose a new bootrom.o file if it doesn't run!".into());
					},
					BuildStorageState::BuildLooksGood => {
						// No need to show a warning message for this.
					},
					BuildStorageState::FileNotFound => {
						ui.set_launcher_state_message("The BootROM image doesn't exist. Please choose a bootrom.o file!".into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::RomSizeMismatch => {
						ui.set_launcher_state_message("BootROM size mismatch! MAME will probably reject this BootROM!".into());
					},
					BuildStorageState::RomHashMismatch => {
						ui.set_launcher_state_message("BootROM hash mismatch! MAME will probably reject this BootROM!".into());
					},
					BuildStorageState::StrippedFlashCyclopsed => {
						ui.set_launcher_state_message("Found one BootROM flash file but couldn't find the other. Choosing a new bootrom.o file may fix this.".into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::StrippedFlashMissing => {
						ui.set_launcher_state_message("Couldn't find a BootROM. The flash files may be missing? Choosing a new bootrom.o file may fix this.".into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CantReadBuild => {
						ui.set_launcher_state_message("Error parsing BootROM image? Please choose a bootrom.o file!".into());
						ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CodeChecksumMismatch => {
						ui.set_launcher_state_message("BootROM code checksum mismatch! Did you choose an image that's too large? Please choose a new bootrom.o file if it doesn't run!".into());
					},
					BuildStorageState::RomfsChecksumMismatch => {
						ui.set_launcher_state_message("BootROM ROMFS checksum mismatch! Did you choose an image that's too large? Please choose a new bootrom.o file if it doesn't run!".into());
					}
					BuildStorageState::BadBaseAddress => {
						ui.set_launcher_state_message("BootROM base address incorrect! Did you choose an AppROM image? Please choose a new bootrom.o file if it doesn't run!".into());
					}
				}
			}
		} else {
			ui_mame.set_selected_bootrom("".into());
			ui_mame.set_selected_bootrom_index(0);
			ui_mame.set_bootrom_import_state(BuildImportState::WillCreate);

			if !supress_warnings {
				ui.set_launcher_state_message("I asked MAME to list its usable BootROMs for this box and it gave me nothing! Broken MAME executable?".into());
			}
		}
	});

	Ok((selected_bootrom_index, selected_bootrom.build_storage_state.clone()))
}

fn populate_selected_box_approms(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool, selected_bootrom_index: usize) -> Result<BuildStorageState, Box<dyn std::error::Error>> {
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
					hint: available_approm.hint.clone(),
					tooltip: available_approm.description.clone(),
					value: available_approm.value.clone()
				}
			);
		}

		ui_mame.set_selectable_approms(slint::ModelRc::new(slint::VecModel::from(selectable_approms)));
		if available_approms.iter().count() > 0 {
			ui_mame.set_selected_approm(selected_approm.value.clone().into());
			if selected_approm.value == "WinCE" {
				ui_mame.set_approm_import_state(BuildImportState::ImportUnavailable);
			} else {
				ui_mame.set_approm_import_state(BuildImportState::WillReplace);
			}

			if !supress_warnings {
				match selected_approm.build_storage_state {
					BuildStorageState::UnknownBuildState => {
						ui.set_launcher_state_message("Unknown AppROM state. Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::BuildLooksGood => {
						// No need to show a warning message for this.
					},
					BuildStorageState::FileNotFound => {
						ui.set_launcher_state_message("The AppROM image doesn't exist. Please choose a approm.o file!".into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::RomSizeMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::RomHashMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::StrippedFlashCyclopsed => {
						ui.set_launcher_state_message("Found one AppROM flash file but couldn't find the other. Choosing a new approm.o file may fix this.".into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::StrippedFlashMissing => {
						ui.set_launcher_state_message("Couldn't find an AppROM. The flash files may be missing? Choosing a new approm.o file may fix this.".into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CantReadBuild => {
						ui.set_launcher_state_message("Error parsing AppROM image? Please choose a new approm.o file if it doesn't run!".into());
						ui_mame.set_approm_import_state(BuildImportState::WillCreate);
					},
					BuildStorageState::CodeChecksumMismatch => {
						ui.set_launcher_state_message("AppROM code checksum mismatch! Did you choose an image that's too large? Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::RomfsChecksumMismatch => {
						ui.set_launcher_state_message("AppROM ROMFS checksum mismatch! Did you choose an image that's too large? Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::BadBaseAddress => {
						ui.set_launcher_state_message("AppROM base address incorrect! Did you choose an image for the wrong box? Please choose a new approm.o file if it doesn't run!".into());
						// The case where they select a bfe approm for a bf0 bootrom or a bf0 approm for a bfe bootrom wil still break. Check for this case?
					}
					}
			}
		} else {
			ui_mame.set_selected_approm("".into());

			if uses_disk_approms {
				ui_mame.set_approm_import_state(BuildImportState::ImportUnavailable);

				if !supress_warnings {
					if can_choose_hdimg {
						ui.set_launcher_state_message("No AppROMs available. Please choose a hard disk image!".into());
					} else {
						ui.set_launcher_state_message("No AppROMs available. Unable to find a disk!".into());
					}
				}
			} else {
				ui_mame.set_approm_import_state(BuildImportState::WillCreate);

				if !supress_warnings {
					ui.set_launcher_state_message("No AppROMs available. Please choose a new approm.o!".into());
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

fn populate_selected_box_ssids(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool) -> Result<SSIDStorageState, Box<dyn std::error::Error>> {
	let mut selected_ssid = VerifiableSSIDItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		ssid_storage_state: SSIDStorageState::UnknownSSIDState,
		ssid_info: None
	};

	let available_ssids = match get_ssids(config, &selected_machine) {
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
					hint: available_ssid.hint.clone(),
					tooltip: available_ssid.description.clone(),
					value: available_ssid.value.clone()
				}
			);
			break;
		}

		ui_mame.set_selectable_ssids(slint::ModelRc::new(slint::VecModel::from(selectable_ssids)));
		if available_ssids.iter().count() > 0 {
			ui_mame.set_ssid_in_file(selected_ssid.value.clone().into());
			ui_mame.set_selected_ssid(selected_ssid.value.clone().into());

			if !supress_warnings {
				match selected_ssid.ssid_storage_state {
					SSIDStorageState::UnknownSSIDState => {
						ui.set_launcher_state_message("Unknown SSID state. You can generate a new one below!".into());
					},
					SSIDStorageState::SSIDLooksGood => {
						// No need to show a warning message for this.
					},
					SSIDStorageState::FileNotFound => {
						ui.set_launcher_state_message("SSID file missing. You can generate a new one below!".into());
					},
					SSIDStorageState::ManufactureMismatch => {
						ui.set_launcher_state_message("SSID manufacture mismatch. You can generate a new one below!".into());
					},
					SSIDStorageState::CRCMismatch => {
						match selected_ssid.ssid_info.clone() {
							Some(ssid_info) => {
								let crc_hex: String = format!("{:02x}", (ssid_info.calculated_crc.clone() as i32)).into();
								ui.set_launcher_state_message(("SSID CRC mismatch (need ".to_owned() + &crc_hex + "). You can generate a new one below!").into());
							},
							_ => {
								ui.set_launcher_state_message("SSID CRC mismatch. You can generate a new one below!".into());
							}
						}
					},
					SSIDStorageState::BoxTypeMismatch => {
						ui.set_launcher_state_message("SSID not a MAME SSID. You can generate a new one below!".into());
					},
					SSIDStorageState::CantReadSSID => {
						ui.set_launcher_state_message("Error parsing SSID? You can generate a new one below!".into());
					},
				}
			}
		} else {
			ui_mame.set_ssid_in_file("".into());
			ui_mame.set_selected_ssid("".into());

			if !supress_warnings {
				ui.set_launcher_state_message("SSID not found. You can generate a new one below!".into());
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
					hint: available_ssid_manufacture.name.clone().into(),
					tooltip: available_ssid_manufacture.description.clone().into(),
					value: available_ssid_manufacture.hex_value.clone().into()
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

fn populate_selected_box_slots(ui_weak: &slint::Weak<MainWindow>, _config: &LauncherConfig, selected_machine: &MAMEMachineNode, supress_warnings: bool) -> Result<bool, Box<dyn std::error::Error>> {
	let mut found_modem_slot = false;
	let mut selected_modem_startpoint: MachineSlotItem  = MachineSlotItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		bitbanger_name: "".into(),
		slot_name: "".into(),
		slot_type: SlotType::Unknown
	};
	let mut selected_debug_startpoint = MachineSlotItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		bitbanger_name: "".into(),
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

	for slot in available_slots.iter() {
		if slot.slot_type == SlotType::ModemSerial && selected_modem_startpoint.slot_type == SlotType::Unknown {
			selected_modem_startpoint = slot.clone();
		} else if slot.slot_type == SlotType::DebugSerial && selected_debug_startpoint.slot_type == SlotType::Unknown {
			selected_debug_startpoint = slot.clone();
		}
	}

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_mame = ui.global::<UIMAMEOptions>();

		let mut selectable_modem_bitb_startpoints: Vec<HintedItem> = vec![];
		let mut selectable_debug_bitb_startpoints: Vec<HintedItem> = vec![];
		for available_slot in available_slots.iter() {
			if available_slot.slot_type == SlotType::ModemSerial {
				found_modem_slot = true;
				selectable_modem_bitb_startpoints.push(
					HintedItem {
						hint: available_slot.hint.clone().into(),
						tooltip: "".into(),
						value: available_slot.value.clone().into()
					}
				);
			} else if available_slot.slot_type == SlotType::DebugSerial {
				selectable_debug_bitb_startpoints.push(
					HintedItem {
						hint: available_slot.hint.clone().into(),
						tooltip: "".into(),
						value: available_slot.value.clone().into()
					}
				);
			}
		}
		ui_mame.set_selectable_modem_bitb_startpoints(slint::ModelRc::new(slint::VecModel::from(selectable_modem_bitb_startpoints)));
		if selected_modem_startpoint.slot_type == SlotType::ModemSerial {
			ui_mame.set_selected_modem_bitb_startpoint(selected_modem_startpoint.value.clone().into());
		}
		ui_mame.set_selectable_debug_bitb_startpoints(slint::ModelRc::new(slint::VecModel::from(selectable_debug_bitb_startpoints)));
		if selected_debug_startpoint.slot_type == SlotType::DebugSerial {
			ui_mame.set_selected_debug_bitb_startpoint(selected_debug_startpoint.value.clone().into());
		}
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
			let (selected_bootrom_index, selected_bootrom_state) = match populate_selected_box_bootroms(ui_weak, config, machine, supress_bootrom_warnings) {
				Ok((selected_bootrom_index, selected_bootrom_state)) => (selected_bootrom_index, selected_bootrom_state),
				Err(_e) => (0, BuildStorageState::UnknownBuildState)
			};

			//
			// Populate UI with approms for the selected box
			//
			// Only one warning can be displayed at a time so bootrom warnings take precedence (if we have a bad bootrom, nothing will boot).
			let supress_approm_warnings = supress_bootrom_warnings || selected_bootrom_state != BuildStorageState::BuildLooksGood;
			let selected_approm_state = match populate_selected_box_approms(ui_weak, config, machine, supress_approm_warnings, selected_bootrom_index) {
				Ok(selected_approm_state) => selected_approm_state,
				Err(_e) => BuildStorageState::UnknownBuildState
			};

			//
			// Populate UI with SSIDs for the selected box
			//
			// Only show if the BootROM and AppROM states are good.
			let supress_ssid_warnings = supress_approm_warnings || selected_approm_state != BuildStorageState::BuildLooksGood;
			let selected_ssid_state = match populate_selected_box_ssids(ui_weak, config, machine, supress_ssid_warnings) {
				Ok(selected_ssid_state) => selected_ssid_state,
				Err(_e) => SSIDStorageState::UnknownSSIDState
			};

			//
			// Populate UI with slots (modem and debug serial endpoints) for the selected box
			//
			// Only show warnings if the BootROM, AppROM and SSID states are good.
			let supress_slot_warnings = supress_ssid_warnings || selected_ssid_state != SSIDStorageState::SSIDLooksGood;
			let _ = populate_selected_box_slots(ui_weak, config, machine, supress_slot_warnings);
		}
	}

	Ok(())
}

fn populate_config(ui_weak: &slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	enable_loading(&ui_weak, "Loading...".into());
	
	let config = LauncherConfig::new().unwrap();
	let config_mame = config.mame.clone();
	let config_persistent_paths = config.persistent.paths.clone();
	let config_persistent_mame = config.persistent.mame_options.clone();
	let mame_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());

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
				hint: "Public TouchPPP Server".into(),
				tooltip: "".into(),
				value: PUBLIC_TOUCHPP_ADDRESS.into()
			},
			HintedItem {
				hint: "Local TouchPPP Server".into(),
				tooltip: "".into(),
				value: "127.0.0.1:1122".into()
			}
		];
		match serialport::available_ports() {
			Ok(ports) => {
				for (index, port) in ports.iter().enumerate() {
					let serial_port_type: String = match port.port_type {
						serialport::SerialPortType::BluetoothPort => "Bluetooth".into(),
						serialport::SerialPortType::PciPort => "PCI".into(),
						serialport::SerialPortType::UsbPort(_) => "USB".into(),
						_ => "Unknown".into()
					};

					selectable_modem_bitb_endpoints.push(
						HintedItem {
							hint: ("[".to_owned() + &serial_port_type + "] Serial Port " + &index.to_string()).into(),
							tooltip: ("Serial Port Type: ".to_owned() + &serial_port_type).into(),
							value: port.port_name.clone().into()
						}
					);
				}
			},
			_ => { }
		}
		ui_mame.set_selectable_modem_bitb_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_modem_bitb_endpoints)));
		// Defailting to the public server to lean toward MAME working vs defaulting to a local server that may not be there.
		ui_mame.set_selected_modem_bitb_endpoint(config_persistent_mame.selected_modem_bitb_endpoint.unwrap_or(PUBLIC_TOUCHPP_ADDRESS.into()).into());

		////
		//
		// Start->Options
		//
		////

		ui_mame.set_verbose_mode(config_persistent_mame.verbose_mode.unwrap_or(false).into());
		ui_mame.set_windowed_mode(config_persistent_mame.windowed_mode.unwrap_or(true).into());
		ui_mame.set_use_drc(config_persistent_mame.use_drc.unwrap_or(true).into());
		ui_mame.set_debug_mode(config_persistent_mame.debug_mode.unwrap_or(false).into());
		ui_mame.set_skip_info_screen(config_persistent_mame.skip_info_screen.unwrap_or(true).into());
		ui_mame.set_disable_mouse_input(config_persistent_mame.disable_mouse_input.unwrap_or(true).into());
		ui_mame.set_console_input(config_persistent_mame.console_input.unwrap_or(false).into());
		ui_mame.set_disable_sound(config_persistent_mame.disable_sound.unwrap_or(false).into());

		ui_mame.set_custom_options(config_persistent_mame.custom_options.unwrap_or("".into()).into());
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

				if Regex::new(r"^wtv\d+").unwrap().is_match(machine_name.as_str()) {
					let sort_re: Regex = Regex::new(r"^wtv(?<box_iteration>\d+)(?<box_name>.+)").unwrap();
					let mut sort_value: String = String::new();

					match sort_re.captures(machine_name.as_str()) {
						Some(matches) => {
							// This sets up sorting so the wtv1 boxes are first, wtv2 boxes are second and sony and phillips boxes come before any other box type.

							sort_value.push_str("0");
							sort_value.push_str(&matches["box_iteration"]);

							if matches["box_name"] == *"sony" {
								sort_value.push_str("0");
							} else if matches["box_name"] == *"phil" {
								sort_value.push_str("1");
							} else {
								sort_value.push_str("2");
								sort_value.push_str(&matches["box_name"]);
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
							sort_value: sort_value.clone().into()
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
				if cmp_a.sort_value == cmp_b.sort_value {
					cmp_a.hint.partial_cmp(&cmp_b.hint).unwrap()
				} else {
					cmp_a.sort_value.partial_cmp(&cmp_b.sort_value).unwrap()
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
						hint: selectable_box.hint.clone(),
						tooltip: selectable_box.tooltip.clone(),
						value: selectable_box.value.clone()
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

		if stored_ssid != requested_ssid {
			match hex::decode(requested_ssid) {
				Ok(raw_data) => {
					let mut raw_ssid: [u8; 0x08] = [0x00; 0x08];

					for (index, byte) in raw_data.iter().enumerate() {
						raw_ssid[index] = *byte;

						if index >= 7 {
							break;
						}
					}

					let _ = save_ssid(raw_ssid, ui_weak.clone(), true);
				}
				_ => { }
			}
		}
	});

	Ok(())
}

fn save_ssid(raw_ssid: [u8; 0x08], ui_weak: slint::Weak<MainWindow>, is_blocking: bool) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_mame = ui.global::<UIMAMEOptions>();
	let selected_box = ui_mame.get_selected_box().to_string();

	let save_thread = std::thread::spawn(move || {
		enable_loading(&ui_weak, "Saving SSID".into());

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

		disable_loading(&ui_weak);

		let _ = save_config(ui_weak.clone(), true, None, None, None);
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
			enable_loading(&ui_weak, "Saving BootROM".into());

			let config = LauncherConfig::new().unwrap();

			let config_persistent_paths = config.persistent.paths.clone();
			let mame_executable_path = Paths::resolve_mame_path(config_persistent_paths.mame_path.clone());
			let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());


			let mut bootrom_file: String = "".into();
			let mut bootrom_collation = BuildIODataCollation::Raw;
			let bootrom_rom_size = 0x200000;

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
				
						biossets.insert(biosset_name + ".o", biosset_description);
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
							if !biossets.contains_key(&rom_name) && rom_name != DEFAULT_BOOTORM_FILE_NAME {
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

fn set_disk_selected_approm(selected_box: &String, file_path: &String, selected_index: u8) -> Result<(), Box<dyn std::error::Error>> {
	let disk_collation = match Regex::new(r"^wtv\d+utv").unwrap().is_match(selected_box.as_str()) {
		true => BuildIODataCollation::ByteSwapped1632,
		false => BuildIODataCollation::ByteSwapped16,
	};

	match BuildMeta::open_disk(file_path.to_string(), Some(disk_collation)) {
		Ok(mut buildmeta) => {
			if buildmeta.selected_build_index != selected_index {
				let _ = buildmeta.set_selected_build_index(selected_index);
			}
		},
		_ => {
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
		approm_rom_size = 0x400000;
	} else {
		approm_rom_size = 0x200000;
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

fn import_disk_approm(selected_box: &String, file_path: &String, source_data: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
	let disk_collation = match Regex::new(r"^wtv\d+utv").unwrap().is_match(selected_box.as_str()) {
		true => BuildIODataCollation::ByteSwapped1632,
		false => BuildIODataCollation::ByteSwapped16,
	};

	match BuildMeta::open_disk(file_path.to_string(), Some(disk_collation)) {
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

	if Path::new(&disk_file_path).exists() {
		match BuildMeta::open_flashdisk(disk_file_path, Some(BuildIODataCollation::Raw)) {
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
						match BuildMeta::new(io, Some(BuildMetaLayout::FlashdiskLayout)) {
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
			enable_loading(&ui_weak, "Saving AppROM".into());

			let config = LauncherConfig::new().unwrap();

			match File::open(source_path.clone()) {
				Ok(mut srcf) => {
					let source_size = srcf.metadata().unwrap().len();

					if source_size > 0 {
						// Don't read more than 64MB
						let mut source_data: Vec<u8> = vec![0x00; source_size.max(4000000) as usize];

						let _ = srcf.seek(SeekFrom::Start(0));
						let _ = srcf.read(&mut source_data);

						// This serves as a convience like it does in my WebTV Disk Editor.
						if correct_checksums {
							match BuildMeta::open_rom(source_path.clone(), None) {
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
								let _ = import_disk_approm(&selected_box, &selected_hdimg_path, &mut source_data);
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

													let _ = import_disk_approm(&selected_box, &preset_img_path, &mut source_data);
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
				selected_hdimg_paths: selected_hdimg_paths,
				selected_hdimg_enabled: selected_hdimg_enabled,
				verbose_mode: Some(ui_mame.get_verbose_mode().into()),
				windowed_mode: Some(ui_mame.get_windowed_mode().into()),
				use_drc: Some(ui_mame.get_use_drc().into()),
				debug_mode: Some(ui_mame.get_debug_mode().into()),
				skip_info_screen: Some(ui_mame.get_skip_info_screen().into()),
				disable_mouse_input: Some(ui_mame.get_disable_mouse_input().into()),
				console_input: Some(ui_mame.get_console_input().into()),
				disable_sound: Some(ui_mame.get_disable_sound().into()),
				custom_options: Some(ui_mame.get_custom_options().into())
			}
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

	let mut last_opened_exe_path: String = ui_paths.get_last_opened_exe_path().into();

	if last_opened_exe_path == "" {
		last_opened_exe_path = "~".into();
	}

	let chooser: FileDialog;

	chooser = 
		FileDialog::new()
		.set_location(&last_opened_exe_path)
		.set_filename("".into());


	let selected_file_pathbuf = chooser.show_open_single_file().unwrap_or(None);

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

			let random_file_name = Alphanumeric.sample_string(&mut rand::thread_rng(), 16) + ".o";
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

					let rommy_enabled = ui_paths.get_rommy_enabled();
					let python_path: String = ui_paths.get_python_path().into();
					let rommy_path: String = ui_paths.get_rommy_path().into();

					if rommy_enabled && !Regex::new(r"\.(o|bin|img)$").unwrap().is_match(&selected_file_path) {
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

				if hdimg_enabled {
					let selected_hdimg_path = match config.persistent.mame_options.selected_hdimg_paths {
						Some(hdimg_paths) => hdimg_paths[&selected_box].clone(),
						_ => "".into()
					};

					if selected_hdimg_path != "" {
						let _ = set_disk_selected_approm(&selected_box.clone(), &selected_hdimg_path, selected_index);
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

										let _ = set_disk_selected_approm(&selected_box.clone(), &preset_img_path, selected_index);
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

					let rommy_enabled = ui_paths.get_rommy_enabled();
					let python_path: String = ui_paths.get_python_path().into();
					let rommy_path: String = ui_paths.get_rommy_path().into();

					if rommy_enabled && !Regex::new(r"\.(o|bin|img)$").unwrap().is_match(&selected_file_path) {
						let ui_weak_cpy = ui_weak.clone();
						let _ = std::thread::spawn(move || {
							enable_loading(&ui_weak, "Running Rommy".into());

							match get_rommy_file(selected_file_path, python_path, rommy_path) {
								Ok(rommy_file_path) => {
									if rommy_file_path != "" {
										let _ = import_approm(rommy_file_path.clone(), ui_weak_cpy, true, true);
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
						let _ = import_approm(selected_file_path.clone(), ui_weak, false, true);
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

	let mut last_opened_img_path: String = ui_paths.get_last_opened_img_path().into();

	if last_opened_img_path == "" {
		last_opened_img_path = "~".into();
	}

	let chooser = 
		FileDialog::new()
		.set_location(&last_opened_img_path)
		.set_filename("".into())
		.add_filter("WebTV HD Image", &["img", "dd", "bin"]);

	let selected_file_pathbuf = chooser.show_open_single_file().unwrap_or(None);

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

fn choose_build_file(ui_weak: slint::Weak<MainWindow>) -> Result<String, Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_paths = ui.global::<UIPaths>();

	let mut last_opened_rom_path: String = ui_paths.get_last_opened_rom_path().into();

	if last_opened_rom_path == "" {
		last_opened_rom_path = "~".into();
	}

	let rommy_enabled = ui_paths.get_rommy_enabled();

	let chooser: FileDialog;

	if rommy_enabled {
		chooser = 
			FileDialog::new()
			.set_location(&last_opened_rom_path)
			.set_filename("".into())
			.add_filter("WebTV Build Files", &["o", "bin", "img", "rom", "brom", "json"])
			.add_filter("WebTV Build Image", &["o", "bin", "img"])
			.add_filter("WebTV partXXX File", &["rom", "brom"])
			.add_filter("Rommy dt.json File", &["json"]);
	} else {
		chooser = 
			FileDialog::new()
			.set_location(&last_opened_rom_path)
			.set_filename("".into())
			.add_filter("WebTV Build Image", &["o", "bin", "img"]);
	}

	let selected_file_pathbuf = chooser.show_open_single_file().unwrap_or(None);

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

		let (_stream, shandle) = rodio::OutputStream::try_default().unwrap();

		let asink = rodio::Sink::try_new(&shandle).unwrap();

		let rand_intro_index = (rand::thread_rng().gen_range(1..1000) % 3) + 1;
		let fart_intro = match rand_intro_index {
			1 => FART_INTRO1,
			2 => FART_INTRO2,
			3 => FART_INTRO3,
			_ => FART_INTRO1
		};
		asink.append(rodio::Decoder::new(BufReader::new(Cursor::new(fart_intro))).unwrap());

		let rand_fart_index = (rand::thread_rng().gen_range(1..1000) % 3) + 1;
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
	if pid > 0 {
		let _ = ui_weak.upgrade_in_event_loop(move |ui| {
			ui.set_mame_pid(pid as i32);
		});
	}

	Ok(())
}

fn add_console_text(ui_weak: slint::Weak<MainWindow>, text: String, scroll_mode: MAMEConsoleScrollMode) -> Result<(), Box<dyn std::error::Error>> {
	if text != "" {
		let _ = ui_weak.upgrade_in_event_loop(move |ui| {
			let previous_text = ui.get_mame_console_text().to_string();

			let mut text_lines: Vec<_> = previous_text.split("\n").collect();
			if text_lines.len() > CONSOLE_SCROLLBACK_LINES {
				text_lines = text_lines[(text_lines.len() - CONSOLE_SCROLLBACK_LINES)..text_lines.len()].to_vec();
			}

			ui.set_scroll_mode(scroll_mode);
			ui.set_mame_console_text((text_lines.join("\n") + &text).into());
		});
	}

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
								let _ = add_console_text(ui_weak_cpy.clone(), console_text, MAMEConsoleScrollMode::ForceScroll);

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

							let _ = add_console_text(ui_weak.clone(), console_text, MAMEConsoleScrollMode::ConditionalScroll);
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

fn start_mame(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();

	let mame_executable_path: String = Paths::resolve_mame_path(Some(ui.global::<UIPaths>().get_mame_path().into()));

	if mame_executable_path != "" && Path::new(&mame_executable_path).exists() {
		let mame_directory_path = LauncherConfig::get_parent(mame_executable_path.clone()).unwrap_or("".into());

		ui.set_mame_console_enabled(true);
		ui.set_mame_console_text("".into());

		let ui_mame = ui.global::<UIMAMEOptions>();

		let mut mame_command = Command::new(mame_executable_path.clone());

		#[cfg(target_os = "windows")]
		mame_command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		mame_command.current_dir(mame_directory_path);

		mame_command.arg(ui_mame.get_selected_box().to_string());

		if ui_mame.get_verbose_mode().into() {
			mame_command.arg("-verbose");
		}

		if ui_mame.get_windowed_mode().into() {
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

		if ui_mame.get_skip_info_screen().into() {
			mame_command.arg("-skip_gameinfo");
		}

		if ui_mame.get_disable_mouse_input().into() {
			mame_command.arg("-nomouse");
		}

		if ui_mame.get_console_input().into() {
			#[cfg(target_os = "windows")]
			mame_command.arg("-keyboardprovider").arg("win32");
			mame_command.arg("-background_input");

			#[cfg(target_os = "macos")]
			unsafe {
				let options = CFDictionaryCreateMutable(std::ptr::null_mut(), 0, std::ptr::null(), std::ptr::null());
				if !options.is_null() {
					CFDictionaryAddValue(options, kAXTrustedCheckOptionPrompt.as_void_ptr(), kCFBooleanTrue.as_void_ptr());
					if !AXIsProcessTrustedWithOptions(options) {
						let _ = add_console_text(ui_weak.clone(), " \n \nAccessibility permission not available. Console input wont be available because there's no permission to communicate with the MAME window. Please go into your settings, then select 'Privacy & Security', then select 'Accessibility', then give permission to this application.\n".to_string(), MAMEConsoleScrollMode::ForceScroll);
					}
					CFRelease(options as *const _);
				}
			}
		}

		if ui_mame.get_disable_sound().into() {
			mame_command.arg("-sound").arg("none");
		}

		let mut selected_bootrom = ui_mame.get_selected_bootrom().to_string();
		if selected_bootrom != "" && selected_bootrom != "None" && selected_bootrom != "WinCE" {
			selected_bootrom = Regex::new(r"\.o$").unwrap().replace_all(&selected_bootrom, "").to_string();

			mame_command.arg("-bios").arg(selected_bootrom);
		}

		let custom_options: String = ui_mame.get_custom_options().to_string();
		if custom_options != "" {
			// EMAC: should acocunt for quoted arguments but this is good "for now"
			mame_command.args(custom_options.split(" "));
		}


		let selected_modem_bitb_startpoint: String = ui_mame.get_selected_modem_bitb_startpoint().to_string();
		let selected_modem_bitb_endpoint: String = ui_mame.get_selected_modem_bitb_endpoint().to_string();
		if selected_modem_bitb_endpoint != "" && selected_modem_bitb_startpoint != "" {
			match Regex::new(r"^(?<slot_select>[^; ]+?)\; (?<bitb_select>.+?)$").unwrap().captures(selected_modem_bitb_startpoint.as_str()) {
				Some(matches) => {
					mame_command.arg("-".to_owned() + &matches["slot_select"]).arg("null_modem");
		
					if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(selected_modem_bitb_endpoint.as_str()) {
						mame_command.arg("-".to_owned() + &matches["bitb_select"]).arg(&("socket.".to_owned() + &selected_modem_bitb_endpoint));
					} else {
						mame_command.arg("-".to_owned() + &matches["bitb_select"]).arg(selected_modem_bitb_endpoint);
					}
				}
				None => {
				}
			}
		}

		let selected_hdimg_path: String = ui_mame.get_selected_hdimg_path().to_string();
		let selected_hdimg_enabled = ui_mame.get_selected_hdimg_enabled();
		if selected_hdimg_enabled && selected_hdimg_path != "" {
			mame_command.arg("-hard").arg(selected_hdimg_path);
		}

		let _ = std::thread::spawn(move || {
			let mut full_mame_command_line: String;

			full_mame_command_line = mame_command.get_program().to_str().unwrap_or("".into()).to_string() + " ";
			full_mame_command_line += &mame_command.get_args().map(|arg_str| arg_str.to_str().unwrap_or("".into())).collect::<Vec<_>>().join(" ");

			let _ = add_console_text(ui_weak.clone(), " \n \nStarting MAME: '".to_owned() + &full_mame_command_line + "'\n", MAMEConsoleScrollMode::ForceScroll);

			mame_command.stderr(Stdio::piped());
			mame_command.stdout(Stdio::piped());

			let process_has_error = match mame_command.spawn() {
				Ok(mame) => {
					let _= set_mame_pid(ui_weak.clone(), mame.id());

					output_mame_stdio(ui_weak.clone(), mame.stdout, mame.stderr).unwrap_or(false)
				},
				Err(_) => false
			};

			let _= set_mame_pid(ui_weak.clone(), 0);

			let _ = add_console_text(ui_weak.clone(), " \nMAME Ended\n".into(), MAMEConsoleScrollMode::ForceScroll);

			if !process_has_error {
				let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
					let ui_mame = ui.global::<UIMAMEOptions>();

					if !(ui_mame.get_verbose_mode().into() || ui_mame.get_console_input().into() || ui_mame.get_debug_mode().into()) {
						let console_enabled = ui.get_mame_console_enabled();
						if console_enabled {
							ui.set_mame_console_enabled(false);
							let _ = load_config(ui_weak.clone());
						}
					}
				});
			}
		});
	}

	Ok(())
}

fn end_mame(ui_weak: slint::Weak<MainWindow>) {
	let mame_pid = get_mame_pid(ui_weak.clone()).unwrap_or(0);

	if mame_pid > 0 {
		let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
			ui.set_mame_console_enabled(false);
		});

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Closing MAME".into());
			
			let sys = System::new_all();
			if let Some(process) = sys.process(Pid::from(mame_pid as usize)) {
				process.kill();
			}

			let _ = load_config(ui_weak.clone());

			disable_loading(&ui_weak);

			let _ = ui_weak.clone().upgrade_in_event_loop(move |ui| {
				ui.set_mame_console_text("".into());
				let _= set_mame_pid(ui_weak.clone(), 0);
			});

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

fn start_ui() -> Result<(), slint::PlatformError> {
	let ui = MainWindow::new().unwrap();

	let _ = load_config(ui.as_weak());

	let mut ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_box(move || {
		let _ = save_config(ui_weak.clone(), true, None, None, None);
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

		let _ = save_config(ui_weak.clone(), true, None, None, None);

		let _ = check_custom_ssid(ui_weak.clone());

		let _ = match start_mame(ui_weak.clone()) {
			Ok(_) => {},
			_ => {},
		};

		disable_loading(&ui_weak);
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_generate_ssid(move || {
		let selected_ssid_manufacture = ui_weak.unwrap().global::<UIMAMEOptions>().get_selected_ssid_manufacture();

		match SSIDInfo::generate(
			SSIDBoxType::MAME, 
			SSIDManufacture::from_u16(u16::from_str_radix(&selected_ssid_manufacture.to_string().trim_start_matches("0x"), 16).unwrap_or(0x0000))
		) {
			Ok(ssid_info) => {
				let _ = save_ssid(ssid_info.raw, ui_weak.clone(), false);
			},
			_ => {

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
		// bf0 versions of WebTV use the hardware keyboard for serial input and smartcard bigbang for serial output.
		// MAME printfs the smartcard data to the console, and this captures the keystrokes on the console to be sent to MAME.
		// When MAME supports solo-based boxes this will be changed a bit but this works for now.
		
		#[cfg(target_os = "linux")]
		send_keypess_linux(ui_weak.clone(), text.to_string(), shiftmod);
		#[cfg(target_os = "windows")]
		send_keypess_windows(ui_weak.clone(), text.to_string(), shiftmod);
		#[cfg(target_os = "macos")]
		send_keypess_macos(ui_weak.clone(), text.to_string(), shiftmod);
	});

	ui_weak = ui.as_weak();
	ui.on_close_mame(move || {
		end_mame(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.window().on_close_requested(move || {
		end_mame(ui_weak.clone());

		slint::CloseRequestResponse::HideWindow
	});

	ui.run()
}