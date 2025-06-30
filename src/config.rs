// By: Eric MacDonald (eMac)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml;
use quick_xml;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::collections::HashMap;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const CONFIG_FILE_NAME: &'static str = "mame_launcher.toml";

////
//
// MAME config obtained after running `mame -listxml`
//
////

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
#[serde(rename = "mame")]
pub struct MAMEDocument {
	#[serde(rename = "@build")]
    pub build: Option<String>,
	#[serde(rename = "@debug")]
    pub debug: Option<String>,
	#[serde(rename = "@mameconfig")]
    pub mameconfig: Option<String>,
	pub machine: Option<Vec<MAMEMachineNode>>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@sourcefile")]
    pub sourcefile: Option<String>,
	#[serde(rename = "@runnable")]
    pub runnable: Option<String>,
	pub description: Option<String>,
	pub year: Option<String>,
	pub manufacturer: Option<String>,
	pub biosset: Option<Vec<MAMEMachineBIOSSetNode>>,
	pub rom: Option<Vec<MAMEMachineROMNode>>,
	pub device_ref: Option<Vec<MAMEMachineDeviceRefNode>>,
	pub chip: Option<Vec<MAMEMachineChipNode>>,
	pub slot: Option<Vec<MAMEMachineSlotNode>>,
	pub device: Option<Vec<MAMEMachineDeviceNode>>,
	pub disk: Option<Vec<MAMEMachineDiskNode>>,
	pub feature: Option<Vec<MAMEMachineFeatureNode>>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineBIOSSetNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@description")]
    pub description: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineROMNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@bios")]
	pub bios: Option<String>,
	#[serde(rename = "@size")]
	pub size: Option<u32>,
	#[serde(rename = "@status")]
	pub status: Option<String>,
	#[serde(rename = "@region")]
	pub region: Option<String>,
	#[serde(rename = "@offset")]
	pub offset: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineDeviceNode {
	#[serde(rename = "@type")]
    pub dtype: Option<String>,
	#[serde(rename = "@tag")]
    pub tag: Option<String>,
	pub instance: Option<Vec<MAMEMachineDeviceInstanceNode>>,
	pub extension: Option<Vec<MAMEMachineDeviceExcensionNode>>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineDiskNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@region")]
    pub region: Option<String>,
	#[serde(rename = "@index")]
    pub index: Option<String>,
	#[serde(rename = "@writable")]
    pub writable: Option<String>,
	#[serde(rename = "@modifiable")]
    pub modifiable: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineFeatureNode {
	#[serde(rename = "@type")]
    pub ftype: Option<String>,
	#[serde(rename = "@status")]
    pub status: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineSlotNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	pub slotoption: Option<Vec<MAMEMachineSlotOptionNode>>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineSlotOptionNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@devname")]
    pub devname: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineDeviceInstanceNode {
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@briefname")]
    pub briefname: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineDeviceExcensionNode {
	#[serde(rename = "@name")]
    pub name: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineDeviceRefNode {
	#[serde(rename = "@name")]
    pub name: Option<String>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct MAMEMachineChipNode {
	#[serde(rename = "@type")]
    pub ctype: Option<String>,
	#[serde(rename = "@tag")]
    pub tag: Option<String>,
	#[serde(rename = "@name")]
    pub name: Option<String>,
	#[serde(rename = "@clock")]
    pub clock: Option<u64>
}

////
//
// Main TOML config
//
////

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct LauncherConfig {
    pub persistent: PersistentConfig,
	pub mame: MAMEDocument
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PersistentConfig {
    pub paths: Paths,
    pub mame_options: MAMEOptions,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Paths {
    pub mame_path: Option<String>,
    pub python_path: Option<String>,
    pub rommy_path: Option<String>,
    pub last_opened_exe_path: Option<String>,
    pub last_opened_rom_path: Option<String>,
    pub last_opened_img_path: Option<String>
}

impl Paths {
	#[allow(dead_code)]
	pub fn resolve_mame_path(mame_path: Option<String>) -> String {
		#[allow(unused_mut)]
		let mut mame_path = mame_path.unwrap_or("".into());

		#[cfg(target_os = "macos")]
		if &mame_path[0..1] == "." {
			let executable_dir = 
			LauncherConfig::get_parent_from_pathbuf(env::current_exe().unwrap_or("".into()))
			.unwrap_or("".into());

			mame_path = executable_dir + "/" + &mame_path;
		}

		return mame_path;
	}
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MAMEOptions {
    pub selected_box: Option<String>,
	pub selected_bootroms: Option<HashMap<String, String>>,
    pub selected_modem_bitb_endpoint: Option<String>,
    pub selected_debug_bitb_endpoint: Option<String>,
	pub selected_hdimg_paths: Option<HashMap<String, String>>,
	pub selected_hdimg_enabled: Option<HashMap<String, bool>>,
    pub verbose_mode: Option<bool>,
    pub windowed_mode: Option<bool>,
    pub use_drc: Option<bool>,
    pub debug_mode: Option<bool>,
    pub skip_info_screen: Option<bool>,
    pub disable_mouse_input: Option<bool>,
    pub console_input: Option<bool>,
    pub disable_sound: Option<bool>,
    pub custom_options: Option<String>
}

impl LauncherConfig {
	pub fn new() -> Result<LauncherConfig, Box<dyn std::error::Error>> {
		let persistent_config = 
		LauncherConfig::get_persistent_config()
			.unwrap_or(LauncherConfig::default_persistent_config());

		let mame_path = Paths::resolve_mame_path(persistent_config.paths.mame_path.clone());

		let mame_config;

		if Path::new(&mame_path).exists() {
			mame_config =
			LauncherConfig::get_mame_config(&mame_path)
				.unwrap_or(LauncherConfig::default_mame_config());
		} else {
			mame_config = LauncherConfig::default_mame_config();
		}

		let config = LauncherConfig {
			persistent: persistent_config,
			mame: mame_config,
		};

		Ok(config)
	}

	pub fn save_persistent_config(persistent_config: &PersistentConfig) -> Result<(), Box<dyn std::error::Error>> {
		let toml_config_text: String =
			toml::to_string(persistent_config)?;

		let executable_dir = 
			LauncherConfig::get_parent_from_pathbuf(env::current_exe()?)
			.unwrap_or("".into());

		let config_file_path = executable_dir + "/" + CONFIG_FILE_NAME;

		fs::write(config_file_path, toml_config_text)?;

		Ok(())
	}

	pub fn get_parent_from_pathbuf(path: std::path::PathBuf) -> Result<String, Box<dyn std::error::Error>> {
		let executable_parent =
			path
			.parent()
			.ok_or("Couldn't find parent directory.")?;

		let executable_dir =
			executable_parent
			.to_str()
			.expect("Couldn't stringify parent directory.");

		Ok(executable_dir.to_string())
	}

	pub fn get_parent(path: String) -> Result<String, Box<dyn std::error::Error>> {
		LauncherConfig::get_parent_from_pathbuf(PathBuf::from(path))
	}

	fn default_mame_config() -> MAMEDocument {
		MAMEDocument {
			build: Some("".into()),
			debug: Some("".into()),
			mameconfig: Some("".into()),
			machine: Some([].into())
		}
	}

	pub fn get_mame_config(mame_path: &String) -> Result<MAMEDocument, Box<dyn std::error::Error>> {
		let mut command = Command::new(mame_path);

		command.arg("-listxml");

		#[cfg(target_os = "windows")]
		command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		let mame_xml = command.output()?;

		let xml_string =
			std::str::from_utf8(&mame_xml.stdout)?;

		let mame_config: MAMEDocument =
			quick_xml::de::from_str(xml_string)?;

		Ok(mame_config)
	}

	fn default_persistent_config() -> PersistentConfig {
		PersistentConfig {
			paths: Paths {
				mame_path: Some("".into()),
				python_path: Some("".into()),
				rommy_path: Some("".into()),
				last_opened_exe_path: Some("".into()),
				last_opened_rom_path: Some("".into()),
				last_opened_img_path: Some("".into())
			},
			mame_options: MAMEOptions {
				selected_box: Some("wtv1sony".into()),
				selected_bootroms: None,
				selected_modem_bitb_endpoint: None,
				selected_debug_bitb_endpoint: None,
				selected_hdimg_paths: None,
				selected_hdimg_enabled: None,
				verbose_mode: Some(false),
				windowed_mode: Some(true),
				use_drc: Some(true),
				debug_mode: Some(false),
				skip_info_screen: Some(true),
				disable_mouse_input: Some(true),
				console_input: Some(false),
				disable_sound: Some(false),
				custom_options: Some("".into())
			}
		}
	}
	
	pub fn get_persistent_config() -> Result<PersistentConfig, Box<dyn std::error::Error>> {
		let executable_dir = 
			LauncherConfig::get_parent_from_pathbuf(env::current_exe()?)
			.unwrap_or("".into());

		let config_file_path = executable_dir + "/" + CONFIG_FILE_NAME;

		let toml_config_text: String = 
			fs::read_to_string(config_file_path)?;

		let toml_config: PersistentConfig = 
			toml::from_str(&toml_config_text)?;

		Ok(toml_config)
	}
}
