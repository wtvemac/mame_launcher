// By: Eric MacDonald (eMac)

#![windows_subsystem = "windows"]

mod config;
mod wtv;

use std::{
	collections::HashMap, 
	fs::File,
	io::{BufReader, Read, Write, Cursor}, 
	path::Path, process::{Command, Stdio},
	env
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

use config::{LauncherConfig, MAMEMachineNode, PersistentConfig, Paths, MAMEOptions};
use wtv::{
	buildio::BuildIO,
	buildmeta::{BuildMeta, BuildInfo},
	ssid::{SSIDInfo, SSIDBoxType, SSIDManufacture}
};

slint::include_modules!();

const STACK_SIZE: usize = 32 * 1024 * 1024;
const SSID_ROM_FILE: &'static str = "ds2401.bin";
const DEFAULT_BOOTORM_FILE_NAME: &'static str = "bootrom.o";
const BOOTROM_BASE_ADDRESS: u32 = 0x9fc00000;
const BOOTROM_FLASH_FILE_PREFIX: &'static str = "bootrom_flash";
// wtv2 (Plus) boxes will be detected and ran from this launcher but we assume (and can only verify) a flash-based approm 
const APPROM1_BASE_ADDRESS: u32 = 0x9f000000;
const APPROM2_BASE_ADDRESS: u32 = 0x9fe00000;
const APPROM1_FLASH_FILE_PREFIX: &'static str = "bank0_flash";
const APPROM2_FLASH_FILE_PREFIX: &'static str = "approm_flash";
const ALLOW_APPROM2_FILES: bool = false;
const PUBLIC_TOUCHPP_ADDRESS: &'static str = "wtv.ooguy.com:1122";
#[cfg(target_os = "linux")]
const CONSOLE_KEY_DELAY: u32 = 200 * 1000;
#[cfg(target_os = "windows")]
const CONSOLE_KEY_DELAY: u64 = 25 * 1000;

const FART_INTRO1: &'static [u8] = include_bytes!("../sounds/fart-intro1.mp3");
const FART_INTRO2: &'static [u8]  = include_bytes!("../sounds/fart-intro2.mp3");
const FART_INTRO3: &'static [u8]  = include_bytes!("../sounds/fart-intro3.mp3");
const FART1: &'static [u8]  = include_bytes!("../sounds/fart1.mp3");
const FART2: &'static [u8]  = include_bytes!("../sounds/fart2.mp3");
const FART3: &'static [u8]  = include_bytes!("../sounds/fart3.mp3");

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum BuildStorageType {
	UnknownStorageType,
	StrippedFlashBuild,
	MaskRomBuild
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
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
#[derive(Debug, Clone, PartialEq)]
enum SSIDStorageState {
	UnknownSSIDState,
	SSIDLooksGood,
	FileNotFound,
	ManufactureMismatch,
	CRCMismatch,
	BoxTypeMismatch,
	CantReadSSID
}

// Selectable build item with data to verify its integrity.
#[allow(dead_code)]
#[derive(Debug, Clone)]
struct VerifiableBuildItem {
	pub hint: slint::SharedString,
	pub value: slint::SharedString,
	pub description: slint::SharedString,
	pub status: String,
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

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
enum MAMEConsoleScrollMode {
	NoScrollCheck,
	ConditionalScroll,
	ForceScroll
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

	let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
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
					.unwrap_or(&"".to_string())
					.into()
			} else if rom_name != DEFAULT_BOOTORM_FILE_NAME {
				continue;
			}

			let mut bootrom = VerifiableBuildItem {
				hint: "".into(),
				value: rom_name.clone().into(),
				status: rom_status.clone().into(),
				description: description.into(),
				hash: "".into(),
				build_storage_type: BuildStorageType::MaskRomBuild,
				build_storage_state: BuildStorageState::UnknownBuildState,
				build_info: None
			};

			let bootrom_path = mame_directory_path.clone() + "/roms/" + &selected_box + "/" + &rom_name.clone().to_string();

			if Path::new(&bootrom_path).exists() {
				match BuildMeta::new(bootrom_path, None, None) {
					Ok(build_meta) => {
						bootrom.build_info = Some(build_meta.build_info.clone());
						bootrom.hint = build_meta.build_info.build_header.build_version.clone().to_string().into();
	
						if build_meta.build_info.build_header.code_checksum != build_meta.build_info.calculated_code_checksum {
							bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
						} else if build_meta.build_info.romfs_header.romfs_checksum != build_meta.build_info.calculated_romfs_checksum {
							bootrom.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
						} else if build_meta.build_info.build_header.build_base_address != BOOTROM_BASE_ADDRESS {
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
	if bootroms.iter().count() == 0 && Regex::new(r"^wtv\d+dev$").unwrap().is_match(selected_box.as_str()) {
		let mut bootrom = VerifiableBuildItem {
			hint: "".into(),
			value: BOOTROM_FLASH_FILE_PREFIX.into(),
			status: "".into(),
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
				match BuildMeta::new(bootrom_path_prefix, Some(true), None) {
					Ok(build_meta) => {
						bootrom.build_info = Some(build_meta.build_info.clone());
						bootrom.hint = build_meta.build_info.build_header.build_version.clone().to_string().into();
	
						if build_meta.build_info.build_header.code_checksum != build_meta.build_info.calculated_code_checksum {
							bootrom.build_storage_state = BuildStorageState::CodeChecksumMismatch;
						} else if build_meta.build_info.romfs_header.romfs_checksum != build_meta.build_info.calculated_romfs_checksum {
							bootrom.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
						} else if build_meta.build_info.build_header.build_base_address != BOOTROM_BASE_ADDRESS {
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
	}

	Ok(bootroms)
}

fn get_approms(config: &LauncherConfig, selected_machine: &MAMEMachineNode, selected_bootrom_index: usize) -> Result<Vec<VerifiableBuildItem>, Box<dyn std::error::Error>> {
	let mut approms: Vec<VerifiableBuildItem> = vec![];

	let selected_box = 
		selected_machine.name
		.clone()
		.unwrap_or("".into());

	let config_persistent_paths = config.persistent.paths.clone();

	let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
	let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());

	let mut approm = VerifiableBuildItem {
		hint: "".into(),
		value: APPROM1_FLASH_FILE_PREFIX.into(),
		status: "".into(),
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

	let mut approm_path_prefix = approm1_path_prefix.clone();
	
	// MAME doesn't provide details about the flash device used, so we must guess this. If approm2 files are there, then use approm2 paths otherwise use an approm1 path.
	if ALLOW_APPROM2_FILES && (Path::new(&(approm2_path_prefix.clone() + "0")).exists() || Path::new(&(approm2_path_prefix.clone() + "1")).exists()) {
		approm_path_prefix = approm2_path_prefix.clone();
	}

	let approm_path0 = approm_path_prefix.clone() + "0";
	let approm_path1 = approm_path_prefix.clone() + "1";

	let approm_path0_exists = Path::new(&approm_path0).exists();
	let approm_path1_exists = Path::new(&approm_path1).exists();

	if approm_path0_exists || approm_path1_exists {
		if approm_path0_exists && approm_path1_exists {
			match BuildMeta::new(approm_path_prefix, Some(true), None) {
				Ok(build_meta) => {
					approm.build_info = Some(build_meta.build_info.clone());
					approm.hint = build_meta.build_info.build_header.build_version.clone().to_string().into();

					if build_meta.build_info.build_header.code_checksum != build_meta.build_info.calculated_code_checksum {
						approm.build_storage_state = BuildStorageState::CodeChecksumMismatch;
					} else if build_meta.build_info.romfs_header.romfs_checksum != build_meta.build_info.calculated_romfs_checksum {
						approm.build_storage_state = BuildStorageState::RomfsChecksumMismatch;
					} else if build_meta.build_info.build_header.build_base_address != APPROM1_BASE_ADDRESS && build_meta.build_info.build_header.build_base_address != APPROM2_BASE_ADDRESS {
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
		} else {
			approm.build_storage_state = BuildStorageState::StrippedFlashCyclopsed;
		}
	} else {
		approm.build_storage_state = BuildStorageState::StrippedFlashMissing;
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

	let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
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

fn populate_selected_box_config(ui_weak: &slint::Weak<MainWindow>, config: &LauncherConfig, selected_box: &String) -> Result<(), Box<dyn std::error::Error>> {
	let config_mame: config::MAMEDocument = config.mame.clone();
	let config_persistent_mame = config.persistent.mame_options.clone();

	let mut available_bootroms: Vec<VerifiableBuildItem> = vec![];
	let mut selected_bootrom = VerifiableBuildItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		status: "".into(),
		hash: "".into(),
		build_storage_type: BuildStorageType::UnknownStorageType,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None,
	};
	let mut selected_bootrom_state = BuildStorageState::UnknownBuildState;
	let mut selected_bootrom_index: usize = 0;

	let mut available_approms: Vec<VerifiableBuildItem> = vec![];
	let mut selected_approm = VerifiableBuildItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		status: "".into(),
		hash: "".into(),
		build_storage_type: BuildStorageType::UnknownStorageType,
		build_storage_state: BuildStorageState::UnknownBuildState,
		build_info: None,
	};
	let mut selected_approm_state = BuildStorageState::UnknownBuildState;

	let mut available_ssids: Vec<VerifiableSSIDItem> = vec![];
	let mut selected_ssid = VerifiableSSIDItem {
		hint: "".into(),
		value: "".into(),
		description: "".into(),
		ssid_storage_state: SSIDStorageState::UnknownSSIDState,
		ssid_info: None
	};

	for machine in config_mame.machine.unwrap_or(vec![]).iter() {
		let machine_name = 
			machine.name
			.clone()
			.unwrap_or("".into());
		if machine_name == *selected_box {
			available_bootroms = match get_bootroms(config, &machine) {
				Ok(bootroms) => bootroms,
				Err(_e) => vec![]
			};
			for (index, bootrom) in available_bootroms.iter().enumerate() {
				if bootrom.value.to_string() == config_persistent_mame.selected_bootrom.clone().unwrap_or("".into()) {
					selected_bootrom = bootrom.clone();
					selected_bootrom_index = index;
				}
			}
			if available_bootroms.iter().count() > 0 && selected_bootrom.value == "" {
				selected_bootrom = available_bootroms[0].clone();
				selected_bootrom_index = 0;
			}

			available_approms = match get_approms(config, &machine, selected_bootrom_index) {
				Ok(approms) => approms,
				Err(_e) => vec![]
			};
			for approm in available_approms.iter() {
				if approm.value.to_string() == config_persistent_mame.selected_box.clone().unwrap_or("".into()) {
					selected_approm = approm.clone();
				}
			}
			
			available_ssids = match get_ssids(config, &machine) {
				Ok(ssids) => ssids,
				Err(_e) => vec![]
			};
			for ssid in available_ssids.iter() {
				selected_ssid = ssid.clone();
				break;
			}
		}
	}

	let selected_box = selected_box.clone();
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

			if selected_bootrom.value == "" {
				selected_bootrom = available_bootroms[0].clone();
			}

			ui_mame.set_selected_bootrom(selected_bootrom.value.clone().into());
			ui_mame.set_selected_bootrom_index(selected_bootrom_index.clone() as i32);

			selected_bootrom_state = selected_bootrom.build_storage_state.clone();

			match selected_bootrom.build_storage_state {
				BuildStorageState::UnknownBuildState => {
					ui.set_launcher_state_message("Unknown BootROM state. Please choose a new bootrom.o file if it doesn't run!".into());
				},
				BuildStorageState::BuildLooksGood => {
					// No need to show a warning message for this.
				},
				BuildStorageState::FileNotFound => {
					ui.set_launcher_state_message("The BootROM image doesn't exist. Please choose a bootrom.o file!".into());
				},
				BuildStorageState::RomSizeMismatch => {
					ui.set_launcher_state_message("BootROM size mismatch! MAME will probably reject this BootROM!".into());
				},
				BuildStorageState::RomHashMismatch => {
					ui.set_launcher_state_message("BootROM hash mismatch! MAME will probably reject this BootROM!".into());
				},
				BuildStorageState::StrippedFlashCyclopsed => {
					ui.set_launcher_state_message("Found one BootROM flash file but couldn't find the other. Choosing a new bootrom.o file may fix this.".into());
				},
				BuildStorageState::StrippedFlashMissing => {
					ui.set_launcher_state_message("Couldn't find a BootROM. The flash files may be missing? Choosing a new bootrom.o file may fix this.".into());
				},
				BuildStorageState::CantReadBuild => {
					ui.set_launcher_state_message("Error parsing BootROM image? Please choose a bootrom.o file!".into());
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
		} else {
			ui.set_mame_broken(true);
			ui.set_launcher_state_message("I asked MAME to list its usable BootROMs and it gave me nothing! Broken MAME executable?".into());
		}

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
			if selected_approm.value == "" {
				selected_approm = available_approms[0].clone();
			}

			ui_mame.set_selected_approm(selected_approm.value.clone().into());

			selected_approm_state = selected_approm.build_storage_state.clone();

			// Only one error can be displayed at a time so bootrom errors take precedence (if we have a bad bootrom, nothing will boot).
			if selected_bootrom_state == BuildStorageState::BuildLooksGood {
				match selected_approm.build_storage_state {
					BuildStorageState::UnknownBuildState => {
						ui.set_launcher_state_message("Unknown AppROM state. Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::BuildLooksGood => {
						// No need to show a warning message for this.
					},
					BuildStorageState::FileNotFound => {
						ui.set_launcher_state_message("The AppROM image doesn't exist. Please choose a approm.o file!".into());
					},
					BuildStorageState::RomSizeMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::RomHashMismatch => {
						// Not checking an AppROM as a verified MAME ROM.
					},
					BuildStorageState::StrippedFlashCyclopsed => {
						ui.set_launcher_state_message("Found one AppROM flash file but couldn't find the other. Choosing a new approm.o file may fix this.".into());
					},
					BuildStorageState::StrippedFlashMissing => {
						ui.set_launcher_state_message("Couldn't find an AppROM. The flash files may be missing? Choosing a new approm.o file may fix this.".into());
					},
					BuildStorageState::CantReadBuild => {
						ui.set_launcher_state_message("Error parsing AppROM image? Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::CodeChecksumMismatch => {
						ui.set_launcher_state_message("AppROM code checksum mismatch! Did you choose an image that's too large? Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::RomfsChecksumMismatch=> {
						ui.set_launcher_state_message("AppROM ROMFS checksum mismatch! Did you choose an image that's too large? Please choose a new approm.o file if it doesn't run!".into());
					},
					BuildStorageState::BadBaseAddress => {
						ui.set_launcher_state_message("AppROM base address incorrect! Did you choose an image for the wrong box? Please choose a new approm.o file if it doesn't run!".into());
						// The case where they select a bfe approm for a bf0 bootrom or a bf0 approm for a bfe bootrom wil still break. Check for this case?
					}
					}
			}
		} else if selected_bootrom_state == BuildStorageState::BuildLooksGood {
			ui.set_launcher_state_message("No AppROMs available. Please choose a new approm.o!".into());
		}

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
			if selected_ssid.value == "" {
				selected_ssid = available_ssids[0].clone();
			}

			ui_mame.set_ssid_in_file(selected_ssid.value.clone().into());
			ui_mame.set_selected_ssid(selected_ssid.value.clone().into());

			// Only show if the BootROM and AppROM states are good.
			if selected_bootrom_state == BuildStorageState::BuildLooksGood && selected_approm_state == BuildStorageState::BuildLooksGood {
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
		} else if selected_bootrom_state == BuildStorageState::BuildLooksGood && selected_approm_state == BuildStorageState::BuildLooksGood {
			ui.set_launcher_state_message("SSID not found. You can generate one here.".into());
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
			if Regex::new(r"^wtv\d+sony$").unwrap().is_match(selected_box.as_str())  && available_ssid_manufacture.manufacture != SSIDManufacture::Sony {
				continue;
			} else if Regex::new(r"^wtv\d+phil$").unwrap().is_match(selected_box.as_str()) && available_ssid_manufacture.manufacture != SSIDManufacture::Phillips {
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

	Ok(())
}

fn populate_config(ui_weak: &slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	enable_loading(&ui_weak, "Loading...".into());
	
	let config = LauncherConfig::new().unwrap();
	let config_mame = config.mame.clone();
	let config_persistent_paths = config.persistent.paths.clone();
	let config_persistent_mame = config.persistent.mame_options.clone();
	let mame_path = config_persistent_paths.mame_path.clone().unwrap_or("".into());

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
		ui_paths.set_last_opened_path(config_persistent_paths.last_opened_path.unwrap_or("".into()).into());

		////
		//
		// Start->Modem Endpoint
		//
		////

		let mut selectable_bitb_endpoints: Vec<HintedItem> = vec![
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

					selectable_bitb_endpoints.push(
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
		ui_mame.set_selectable_bitb_endpoints(slint::ModelRc::new(slint::VecModel::from(selectable_bitb_endpoints)));
		// Defailting to the public server to lean toward MAME working vs defaulting to a local server that may not be there.
		ui_mame.set_selected_bitb_endpoint(config_persistent_mame.selected_bitb_endpoint.unwrap_or(PUBLIC_TOUCHPP_ADDRESS.into()).into());


		////
		//
		// Start->Options
		//
		////

		ui_mame.set_verbose_mode(config_persistent_mame.verbose_mode.unwrap_or(true).into());
		ui_mame.set_windowed_mode(config_persistent_mame.windowed_mode.unwrap_or(true).into());
		ui_mame.set_low_latency(config_persistent_mame.low_latency.unwrap_or(false).into());
		ui_mame.set_debug_mode(config_persistent_mame.debug_mode.unwrap_or(false).into());
		ui_mame.set_skip_info_screen(config_persistent_mame.skip_info_screen.unwrap_or(true).into());
		ui_mame.set_disable_mouse_input(config_persistent_mame.disable_mouse_input.unwrap_or(true).into());
		ui_mame.set_console_input(config_persistent_mame.console_input.unwrap_or(true).into());
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
			if machine.biosset.iter().count() > 0 && machine.runnable.clone().unwrap_or("".into()) != "no" {
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
		let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
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
		let _ = save_config(ui_weak.clone(), true);
	});

	if is_blocking {
		let _ = save_thread.join();
	}

	Ok(())
}

fn save_bootrom(source_path: String, ui_weak: slint::Weak<MainWindow>, remove_source: bool) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
		let ui_weak = ui.as_weak();

		let ui_mame = ui.global::<UIMAMEOptions>();
		let selected_box = ui_mame.get_selected_box().to_string();
		let try_bootrom_file: String = ui_mame.get_selected_bootrom().into();

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Saving BootROM".into());

			let config = LauncherConfig::new().unwrap();

			let config_persistent_paths = config.persistent.paths.clone();
			let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
			let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());


			let mut bootrom_file: String = "".into();
			let mut bootrom_stripped = false;
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
				bootrom_stripped = true;
			} else if bootrom_file != "" {
				bootrom_directory_path = mame_directory_path.clone() + "/roms/" + &selected_box;
				bootrom_file_path = bootrom_directory_path.clone() + "/" + &bootrom_file.clone();
				bootrom_stripped = false;
			}

			if bootrom_directory_path != "" && bootrom_file_path != "" {
				match std::fs::create_dir_all(bootrom_directory_path) {
					Ok(_) => {
						match BuildIO::create(bootrom_file_path, Some(bootrom_stripped), Some(bootrom_rom_size)) {
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
			let _ = save_config(ui_weak.clone(), true);
		});
	});

	Ok(())
}

fn save_approm(source_path: String, ui_weak: slint::Weak<MainWindow>, remove_source: bool) -> Result<(), Box<dyn std::error::Error>> {
	let _ = ui_weak.upgrade_in_event_loop(move |ui: MainWindow| {
		let ui_weak = ui.as_weak();
		
		let correct_checksums = true;

		let ui_mame = ui.global::<UIMAMEOptions>();
		let selected_box = ui_mame.get_selected_box().to_string();
		let selected_bootrom_index: usize = ui_mame.get_selected_bootrom_index() as usize;

		let _ = std::thread::spawn(move || {
			enable_loading(&ui_weak, "Saving AppROM".into());

			let config = LauncherConfig::new().unwrap();

			let config_persistent_paths = config.persistent.paths.clone();
			let mame_executable_path = config_persistent_paths.mame_path.unwrap_or("".into());
			let mame_directory_path = LauncherConfig::get_parent(mame_executable_path).unwrap_or("".into());


			let approm_directory_path: String;
			if selected_bootrom_index > 0 {
				approm_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box + "_" + &selected_bootrom_index.to_string();
			} else {
				approm_directory_path = mame_directory_path.clone() + "/nvram/" + &selected_box;
			}

			let approm_file_path = approm_directory_path.clone() + "/" + APPROM1_FLASH_FILE_PREFIX;
			let approm_stripped = true;
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
						match BuildIO::create(approm_file_path, Some(approm_stripped), Some(approm_rom_size)) {
							Ok(mut destf) => {
								match File::open(source_path.clone()) {
									Ok(mut srcf) => {
										let mut buffer: Vec<u8> = vec![0x00; approm_rom_size as usize];

										let _ = srcf.read(&mut buffer);

										// This serves as a convience like it does in my WebTV Disk Editor.
										if correct_checksums {
											let mut correct_code_checksum = 0x00000000;
											let mut correct_romfs_checksum = 0x00000000;
											let mut romfs_offset: usize = 0x00;

											match BuildMeta::new(source_path.clone(), None, None) {
												Ok(build_meta) => {
													correct_code_checksum = build_meta.build_info.calculated_code_checksum;
													correct_romfs_checksum = build_meta.build_info.calculated_romfs_checksum;
													romfs_offset = build_meta.build_info.romfs_offset as usize;
												},
												_ => { }
											}

											if correct_code_checksum != 0x00000000 {
												buffer[0x08] = ((correct_code_checksum >> 0x18) & 0xff) as u8;
												buffer[0x09] = ((correct_code_checksum >> 0x10) & 0xff) as u8;
												buffer[0x0a] = ((correct_code_checksum >> 0x08) & 0xff) as u8;
												buffer[0x0b] = ((correct_code_checksum >> 0x00) & 0xff) as u8;
											}

											if correct_romfs_checksum != 0x00000000 && romfs_offset > 0x00 && romfs_offset <= (approm_rom_size as usize) {
												buffer[romfs_offset - 0x04] = ((correct_romfs_checksum >> 0x18) & 0xff) as u8;
												buffer[romfs_offset - 0x03] = ((correct_romfs_checksum >> 0x10) & 0xff) as u8;
												buffer[romfs_offset - 0x02] = ((correct_romfs_checksum >> 0x08) & 0xff) as u8;
												buffer[romfs_offset - 0x01] = ((correct_romfs_checksum >> 0x00) & 0xff) as u8;
											}
										}

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

			let _ = save_config(ui_weak.clone(), true);
		});
	});

	Ok(())
}

fn save_config(ui_weak: slint::Weak<MainWindow>, reload: bool) -> Result<(), Box<dyn std::error::Error>> {
	enable_loading(&ui_weak.clone(), "Saving Config".into());

	let _ = ui_weak.upgrade_in_event_loop(move |ui| {
		let ui_paths = ui.global::<UIPaths>();
		let ui_mame = ui.global::<UIMAMEOptions>();
		
		let config = PersistentConfig {
			paths: Paths {
				mame_path: Some(ui_paths.get_mame_path().into()),
				python_path: Some(ui_paths.get_python_path().into()),
				rommy_path: Some(ui_paths.get_rommy_path().into()),
				last_opened_path: Some(ui_paths.get_last_opened_path().into())
			},
			mame_options: MAMEOptions {
				selected_box: Some(ui_mame.get_selected_box().into()),
				selected_bootrom: Some(ui_mame.get_selected_bootrom().into()),
				selected_bitb_endpoint: Some(ui_mame.get_selected_bitb_endpoint().into()),
				verbose_mode: Some(ui_mame.get_verbose_mode().into()),
				windowed_mode: Some(ui_mame.get_windowed_mode().into()),
				low_latency: Some(ui_mame.get_low_latency().into()),
				debug_mode: Some(ui_mame.get_debug_mode().into()),
				skip_info_screen: Some(ui_mame.get_skip_info_screen().into()),
				disable_mouse_input: Some(ui_mame.get_disable_mouse_input().into()),
				console_input: Some(ui_mame.get_console_input().into()),
				disable_sound: Some(ui_mame.get_disable_sound().into()),
				custom_options: Some(ui_mame.get_custom_options().into())
			}
		};

		let _ = LauncherConfig::save_persistent_config(&config); // May freeze up UI

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

	let mut last_opened_path: String = ui_paths.get_last_opened_path().into();

	if last_opened_path == "" {
		last_opened_path = "~".into();
	}

	let chooser: FileDialog;

	chooser = 
		FileDialog::new()
		.set_location(&last_opened_path)
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
			ui_paths.set_last_opened_path(LauncherConfig::get_parent(selected_file_path.clone()).unwrap_or("".into()).into());

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

fn choose_bootrom(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
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
										let _ = save_bootrom(rommy_file_path.clone(), ui_weak_cpy, true);
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
						let _ = save_bootrom(selected_file_path.clone(), ui_weak, false);
					}
				});
			}
		},
		_ => { }
	}

	Ok(())
}

fn choose_approm(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
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
										let _ = save_approm(rommy_file_path.clone(), ui_weak_cpy, true);
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
						let _ = save_approm(selected_file_path.clone(), ui_weak, false);
					}
				});
			}
		},
		_ => { }
	}

	Ok(())
}

fn choose_build_file(ui_weak: slint::Weak<MainWindow>) -> Result<String, Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();
	let ui_paths = ui.global::<UIPaths>();

	let mut last_opened_path: String = ui_paths.get_last_opened_path().into();

	if last_opened_path == "" {
		last_opened_path = "~".into();
	}

	let rommy_enabled = ui_paths.get_rommy_enabled();

	let chooser: FileDialog;

	if rommy_enabled {
		chooser = 
			FileDialog::new()
			.set_location(&last_opened_path)
			.set_filename("".into())
			.add_filter("WebTV Build Files", &["o", "bin", "img", "rom", "brom", "json"])
			.add_filter("WebTV Build Image", &["o", "bin", "img"])
			.add_filter("WebTV partXXX File", &["rom", "brom"])
			.add_filter("Rommy dt.json File", &["json"]);
	} else {
		chooser = 
			FileDialog::new()
			.set_location(&last_opened_path)
			.set_filename("".into())
			.add_filter("WebTV Build Image", &["o", "bin", "img"]);
	}

	let selected_file_pathbuf = chooser.show_open_single_file().unwrap_or(None);

	let mut image_path: String = "".into();
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
			ui_paths.set_last_opened_path(LauncherConfig::get_parent(selected_file_path.clone()).unwrap_or("".into()).into());

			image_path = selected_file_path.clone();
		}
	}

	Ok(image_path)

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
			if scroll_mode == MAMEConsoleScrollMode::ForceScroll {
				ui.set_force_scroll(true);
			}

			let mut rng = rand::thread_rng();

			if scroll_mode == MAMEConsoleScrollMode::ConditionalScroll || scroll_mode == MAMEConsoleScrollMode::ForceScroll {
				ui.set_check_value(rng.gen::<i32>());
			}

			let previous_text = ui.get_mame_console_text().to_string();
			ui.set_mame_console_text((previous_text + &text).into());


			if scroll_mode == MAMEConsoleScrollMode::ConditionalScroll || scroll_mode == MAMEConsoleScrollMode::ForceScroll {
				ui.set_check_value(rng.gen::<i32>());
			}
		});
	}

	Ok(())
}

fn start_mame(ui_weak: slint::Weak<MainWindow>) -> Result<(), Box<dyn std::error::Error>> {
	let ui = ui_weak.unwrap();

	let mame_executable_path: String = ui.global::<UIPaths>().get_mame_path().into();

	if mame_executable_path != "" && Path::new(&mame_executable_path).exists() {
		let mame_directory_path = LauncherConfig::get_parent(mame_executable_path.clone()).unwrap_or("".into());

		ui.set_mame_console_enabled(true);
		ui.set_mame_console_text("".into());

		let ui_mame = ui.global::<UIMAMEOptions>();

		let mut mame_command = Command::new(mame_executable_path.clone());

		#[cfg(target_os = "windows")]
		mame_command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		mame_command.current_dir(mame_directory_path);

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
		
		if ui_mame.get_low_latency().into() {
			mame_command.arg("-autoframeskip");
			mame_command.arg("-lowlatency");
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
		}

		if ui_mame.get_disable_sound().into() {
			mame_command.arg("-sound").arg("none");
		}

		let mut selected_bootrom = ui_mame.get_selected_bootrom().to_string();
		if selected_bootrom != "" {
			selected_bootrom = Regex::new(r"\.o$").unwrap().replace_all(&selected_bootrom, "").to_string();

			mame_command.arg("-bios").arg(selected_bootrom);
		}

		let custom_options: String = ui_mame.get_custom_options().to_string();
		if custom_options != "" {
			// EMAC: should acocunt for quoted arguments but this is good "for now"
			mame_command.args(custom_options.split(" "));
		}

		mame_command.arg(ui_mame.get_selected_box().to_string());

		let selected_bitb_endpoint: String = ui_mame.get_selected_bitb_endpoint().to_string();
		if selected_bitb_endpoint != "" {
			mame_command.arg("-spot:modem").arg("null_modem");

			if Regex::new(r"^[^\:]+\:\d+$").unwrap().is_match(selected_bitb_endpoint.as_str()) {
				mame_command.arg("-bitb").arg(&("socket.".to_owned() + &selected_bitb_endpoint));
			} else {
				mame_command.arg("-bitb").arg(selected_bitb_endpoint);
			}
		}

		let _ = std::thread::spawn(move || {
			let mut full_mame_command_line: String;

			full_mame_command_line = mame_command.get_program().to_str().unwrap_or("".into()).to_string() + " ";
			full_mame_command_line += &mame_command.get_args().map(|arg_str| arg_str.to_str().unwrap_or("".into())).collect::<Vec<_>>().join(" ");

			let _ = add_console_text(ui_weak.clone(), " \n \nStarting MAME: '".to_owned() + &full_mame_command_line + "'\n", MAMEConsoleScrollMode::ForceScroll);

			mame_command.stderr(Stdio::piped());
			mame_command.stdout(Stdio::piped());

			match mame_command.spawn() {
				Ok(mame) => {
					let _= set_mame_pid(ui_weak.clone(), mame.id());

					#[cfg(target_os = "windows")]
					let mut last_byte: u8 = 0x00;
					match (mame.stdout, mame.stderr) {
						(Some(stdout), Some(stderr)) => {
							let mut stderr_buf: [u8; 1] = [0x00; 1];
							let mut stderr_reader = BufReader::new(stderr);
							let ui_weak_cpy = ui_weak.clone();
							let _ = std::thread::spawn(move || {
								loop {
									match stderr_reader.read(&mut stderr_buf) {
										Ok(stderr_bytes_read) => {
											if stderr_bytes_read == 0 {
												break;
											} else {
												// EMAC: stdout and stderr can get jumbled with this implementation...
												let _ = add_console_text(ui_weak_cpy.clone(), (stderr_buf[0] as char).to_string(), MAMEConsoleScrollMode::ForceScroll);
											}
										},
										_ => {
											break;
										}
									}
								}
							});

							let mut stdout_buf: [u8; 1] = [0x00; 1];
							let mut stdout_reader = BufReader::new(stdout);
							loop {
								match stdout_reader.read(&mut stdout_buf) {
									Ok(stdout_bytes_read) => {
										if stdout_bytes_read == 0 {
											break;
										} else {
												#[cfg(target_os = "windows")]
											// Don't repeat newline chars.
											{
												if stdout_buf[0] == 0x0a || stdout_buf[0] == 0x0d {
													if last_byte != stdout_buf[0] && (last_byte == 0x0a || last_byte == 0x0d) {
													    last_byte = stdout_buf[0].clone();
													    continue;
													}
												}

												last_byte = stdout_buf[0].clone();
											}
											
			
											let _ = add_console_text(ui_weak.clone(), (stdout_buf[0] as char).to_string(), MAMEConsoleScrollMode::ConditionalScroll);
										}
									},
									_ => {
										break;
									}
								}
							}
						},
						_ => { }
					}
				},
				Err(_) => { }
			};

			let _= set_mame_pid(ui_weak.clone(), 0);

			let _ = add_console_text(ui_weak.clone(), " \nMAME Ended\n".into(), MAMEConsoleScrollMode::ForceScroll);
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
				if vkey == 0x0250 || vkey == 0x0255 { // hift key
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

fn start_ui() -> Result<(), slint::PlatformError> {
	let ui = MainWindow::new().unwrap();

	let _ = load_config(ui.as_weak());

	let mut ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_box(move || {
		let _ = save_config(ui_weak.clone(), true);
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_select_bootrom(move || {
		let _ = save_config(ui_weak.clone(), true);
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_choose_bootrom(move || {
		let _ = choose_bootrom(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_choose_approm(move || {
		let _ = choose_approm(ui_weak.clone());
	});

	ui_weak = ui.as_weak();
	ui.global::<UIMAMEOptions>().on_start_mame(move || {
		enable_loading(&ui_weak, "Starting MAME".into());

		let _ = save_config(ui_weak.clone(), true);

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
	
					let _ = save_config(ui_weak.clone(), true);
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
					let _ = save_config(ui_weak.clone(), false);
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
					let _ = save_config(ui_weak.clone(), false);
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
		#[cfg(target_os = "linux")]
		send_keypess_linux(ui_weak.clone(), text.to_string(), shiftmod);
		#[cfg(target_os = "windows")]
		send_keypess_windows(ui_weak.clone(), text.to_string(), shiftmod);

		// MacOS can be supported but in this tool you need to keep the MAME window activated as you type (more consistent that way anyway even though it might be awkward)
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