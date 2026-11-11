// By: Eric MacDonald (eMac)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml;
use quick_xml;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::reader::Reader;
use quick_xml::writer::Writer;
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::collections::HashMap;
use regex_lite::Regex;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

const CONFIG_FILE_NAME: &'static str = "mame_launcher.toml";
const DEFAULT_MAME_CFG_DIRECTORY: &'static str = "cfg";
const DEFAULT_DNS_OVERRIDE_ADDRESS: &'static str = "0.0.0.0";
// A DNS override set to 0.0.0.0 points to the dns0000_target host
pub const DEFAULT_DNS0000_TARGET: &'static str = "wtv.ooguy.com";
const DEFAULT_DNS_OVERRIDE_DOMAINS: [&'static str; 2] = [
	"sg2.trusted.msntv.msn.com",
	"headwaiter.trusted.msntv.msn.com"
];
// An IP override target set to 0.0.0.0 also points to dns0000_target
const DEFAULT_IP_OVERRIDE_ADDRESS: &'static str = "wtvsvr.mywire.org";
const DEFAULT_IP_OVERRIDE_IPS: [&'static str; 37] = [
	"10.0.0.1",        // Appears in many builds (including production)
	"10.0.0.2",        // ^ same, secondary server after first one fails
	"10.0.1.1",        // Appears in many internal (and export) builds
	"10.0.1.2",        // ^ same, secondary server after first one fails
	"10.0.1.3",        // Appears in many LC2 3426+ internal builds
	"10.0.1.4",        // ^ same, secondary server after first one fails
	"10.0.3.1",        // Only in internal UTV builds
	"10.0.128.1",      // Several internal builds
	"10.0.128.2",      // ^ same, secondary server after first one fails
	"10.0.129.1",      // Several internal builds
	"10.0.129.2",      // ^ same, secondary server after first one fails
	"10.0.130.1",      // No idea, referenced in community code. Probably just to cover the bases
	"10.0.130.2",      // ^ same
	"10.0.131.1",      // ^ same
	"10.0.131.2",      // ^ same
	"10.0.132.1",      // Several internal/weekly builds
	"10.0.132.2",      // ^ same, secondary server after first one fails
	"192.168.102.1",   // Internal builds
	"192.168.102.2",   // ^ same, secondary server after first one fails
	"204.254.74.126",  // Internal builds
	"207.76.180.1",    // No idea, referenced in community code.
	"207.76.180.2",    // No idea, referenced in community code.
	"207.76.180.12",   // No idea, referenced in community code.
	"207.76.180.13",   // No idea, referenced in community code.
	"207.76.180.96",   // Internal builds LC2 up to 7352. Not in 7658+ builds
	"210.150.22.37",   // Internal builds LC2 3426+
	"210.150.22.58",   // Internal builds LC2 3426+
	"210.150.22.73",   // No idea, referenced in community code.
	"210.150.22.74",   // No idea, referenced in community code.
	"209.240.194.40",  // No idea, referenced in community code.
	"209.240.194.41",  // Internal builds 3454+, bf0app 1228+
	"209.240.194.42",  // ^ same
	"209.240.194.43",  // Internal builds LC2/Echostar pre 7151. Also found in UTV internal builds
	"209.240.194.70",  // Internal builds. Replaced 209.240.194.43 LC2 7151+
	"209.240.194.71",  // ^ same
	"209.240.194.72",  // ^ same
	"209.240.194.73"   // ^ same
];

static MAME_CONFIG_CACHE: once_cell::sync::Lazy<Mutex<Option<(String, MAMEDocument)>>> = once_cell::sync::Lazy::new(|| {
	Mutex::new(None)
});

static MAME_USAGE_CACHE: once_cell::sync::Lazy<Mutex<Option<(String, String)>>> = once_cell::sync::Lazy::new(|| {
	Mutex::new(None)
});

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
    pub devname: Option<String>,
	#[serde(rename = "@default")]
    pub default: Option<String>
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
    pub name: Option<String>,
	#[serde(rename = "@tag")]
    pub tag: Option<String>
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
    pub network: Option<NetworkOptions>,
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
		if mame_path.len() > 0 && &mame_path[0..1] == "." {
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
    pub selected_modem_moip_endpoint: Option<String>,
    pub selected_debug_bitb_endpoint: Option<String>,
    pub selected_lirc_endpoint: Option<String>,
	pub selected_hdimg_paths: Option<HashMap<String, String>>,
	pub selected_hdimg_enabled: Option<HashMap<String, bool>>,
    pub verbose_mode: Option<bool>,
    pub use_drc: Option<bool>,
    pub debug_mode: Option<bool>,
    pub validate_build_image: Option<bool>,
    pub console_input: Option<bool>,
    pub disable_sound: Option<bool>,
    pub windowing_mode: Option<String>,
    pub captured_console_height: Option<f32>,
    pub captured_window_width: Option<f32>,
    pub captured_window_height: Option<f32>,
    pub use_decoupled_sound: Option<bool>,
    pub softmodem_record: Option<bool>,
    pub enable_wtvpointer: Option<bool>,
    pub enable_wtvnet: Option<bool>,
    pub enable_lirc: Option<bool>,
    pub selected_audio_in_endpoint: Option<String>,
    pub enable_audio_in: Option<bool>,
    pub selected_video_in_endpoint: Option<String>,
    pub enable_video_in: Option<bool>,
    pub selected_smartcard_endpoint: Option<String>,
    pub enable_smartcard: Option<bool>,
    pub selected_cacard_endpoint: Option<String>,
    pub enable_cacard: Option<bool>,
    pub selected_wtvport_option: Option<String>,
    pub enable_wtvport: Option<bool>,
    pub selected_parallel_option: Option<String>,
    pub selected_parallel_socket_endpoint: Option<String>,
    pub selected_parallel_hostlpt_device: Option<String>,
    pub enable_parallel: Option<bool>,
    pub custom_options: Option<String>,
    pub show_advanced_options: Option<bool>
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NetworkOptions {
	pub ethernet_enabled: Option<bool>,
	// When the DNS override is mapped to 0.0.0.0 then resolve this host/IP to get the IP
	pub dns0000_target: Option<String>,
	// For the MSNTV2 we need this so we can redirect some names to a reachable server. A host => 0.0.0.0 will use the resolved IP from dns0000_target (above)
	pub dns_overrides: Option<HashMap<String, String>>,
	// The WebTV OS tries to connect to some unreachable IPs, this allows you to map IPs to a reachable server IP. Same rules with the resolve target as dns_overrides.
	pub ip_overrides: Option<HashMap<String, Vec<String>>>
}

impl NetworkOptions {
	pub fn default_options() -> NetworkOptions {
		NetworkOptions {
			ethernet_enabled: Some(true),
			dns0000_target: Some(DEFAULT_DNS0000_TARGET.into()),
			dns_overrides: Some(
				DEFAULT_DNS_OVERRIDE_DOMAINS
					.iter()
					.map(|domain| (domain.to_string(), DEFAULT_DNS_OVERRIDE_ADDRESS.to_string()))
					.collect()
			),
			ip_overrides: Some(HashMap::from([(
				DEFAULT_IP_OVERRIDE_ADDRESS.to_string(),
				DEFAULT_IP_OVERRIDE_IPS
					.iter()
					.map(|ip| ip.to_string())
					.collect()
			)]))
		}
	}

	pub fn with_defaults(self) -> NetworkOptions {
		let default_network = NetworkOptions::default_options();

		NetworkOptions {
			ethernet_enabled: self.ethernet_enabled.or(default_network.ethernet_enabled),
			dns0000_target: self.dns0000_target.or(default_network.dns0000_target),
			dns_overrides: self.dns_overrides.or(default_network.dns_overrides),
			ip_overrides: self.ip_overrides.or(default_network.ip_overrides)
		}
	}
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

	fn get_file_cache_key(path: &String) -> Result<String, Box<dyn std::error::Error>> {
		let canonical_path = fs::canonicalize(path)?;
		let metadata = fs::metadata(&canonical_path)?;
		let mtime_ns = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();

		Ok(format!("{}|{}|{}", metadata.len(), mtime_ns, canonical_path.to_string_lossy()))
	}

	fn default_mame_config() -> MAMEDocument {
		MAMEDocument {
			build: Some("".into()),
			debug: Some("".into()),
			mameconfig: Some("".into()),
			machine: Some([].into())
		}
	}

	fn run_mame_listxml(mame_path: &String) -> Result<String, Box<dyn std::error::Error>> {
		let mut command = Command::new(mame_path);

		command.arg("-listxml");

		#[cfg(target_os = "windows")]
		command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		let mame_xml = command.output()?;

		if !mame_xml.status.success() {
			return Err(format!("MAME -listxml exited with {}", mame_xml.status).into());
		}

		Ok(String::from_utf8(mame_xml.stdout)?)
	}

	pub fn get_mame_config(mame_path: &String) -> Result<MAMEDocument, Box<dyn std::error::Error>> {
		let cache_key = LauncherConfig::get_file_cache_key(mame_path)?;

		if let Ok(memory_cache) = MAME_CONFIG_CACHE.lock() {
			if let Some((memory_cache_key, mame_config)) = memory_cache.as_ref() {
				if *memory_cache_key == cache_key {
					return Ok(mame_config.clone());
				}
			}
		}

		let xml_string = LauncherConfig::run_mame_listxml(mame_path)?;

		let mame_config: MAMEDocument =
			quick_xml::de::from_str(&xml_string)?;

		if let Ok(mut memory_cache) = MAME_CONFIG_CACHE.lock() {
			*memory_cache = Some((cache_key, mame_config.clone()));
		}

		Ok(mame_config)
	}

	fn run_mame_showusage(mame_path: &String) -> Result<String, Box<dyn std::error::Error>> {
		let mut command = Command::new(mame_path);

		command.arg("-showusage");

		#[cfg(target_os = "windows")]
		command.creation_flags(0x08000000); // CREATE_NO_WINDOW

		let mame_usage = command.output()?;

		Ok(String::from_utf8_lossy(&mame_usage.stdout).to_string())
	}

	fn get_mame_usage(mame_path: &String) -> Option<String> {
		let cache_key = LauncherConfig::get_file_cache_key(mame_path).ok()?;

		if let Ok(memory_cache) = MAME_USAGE_CACHE.lock() {
			if let Some((memory_cache_key, cached_usage)) = memory_cache.as_ref() {
				if *memory_cache_key == cache_key {
					return Some(cached_usage.clone());
				}
			}
		}

		let mame_usage = LauncherConfig::run_mame_showusage(mame_path).unwrap_or("".into());

		if let Ok(mut memory_cache) = MAME_USAGE_CACHE.lock() {
			*memory_cache = Some((cache_key, mame_usage.clone()));
		}

		Some(mame_usage)
	}

	pub fn mame_supports_option(mame_path: &String, option_name: &str) -> bool {
		let Some(mame_usage) = LauncherConfig::get_mame_usage(mame_path) else {
			return false;
		};

		let option_flag = "-".to_owned() + option_name;

		mame_usage.lines().any(|line| line.split_whitespace().next() == Some(option_flag.as_str()))
	}

	pub fn mame_sound_modules(mame_path: &String) -> Vec<String> {
		let Some(mame_usage) = LauncherConfig::get_mame_usage(mame_path) else {
			return vec![];
		};

		mame_usage.lines()
			.find(|line| line.split_whitespace().next() == Some("-sound"))
			.and_then(|line| line.split(':').nth(1))
			.map(|modules| {
				modules
					.replace(" or ", ",")
					.split(',')
					.map(|module| module.trim().to_string())
					.filter(|module| module != "")
					.collect()
			})
			.unwrap_or(vec![])
	}

	fn get_mame_cfg_directory(mame_directory: &String) -> PathBuf {
		let mut cfg_directory: String = DEFAULT_MAME_CFG_DIRECTORY.into();

		if let Ok(mame_ini_text) = fs::read_to_string(Path::new(mame_directory).join("mame.ini")) {
			if let Some(matches) = Regex::new(r"(?m)^\s*cfg_directory\s+(?<value>.+?)\s*$").unwrap().captures(&mame_ini_text) {
				let value = matches["value"].split(';').next().unwrap_or("").trim().trim_matches('"').to_string();

				if value != "" {
					cfg_directory = value;
				}
			}
		}

		if Path::new(&cfg_directory).is_absolute() {
			PathBuf::from(cfg_directory)
		} else {
			Path::new(mame_directory).join(cfg_directory)
		}
	}

	fn xml_attribute_is(element: &BytesStart, name: &str, value: &str) -> bool {
		element.attributes().flatten().any(|attribute| {
			attribute.key.as_ref() == name && attribute.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|attribute_value| attribute_value == value).unwrap_or(false)
		})
	}

	fn xml_with_attribute(element: &BytesStart, name: &str, value: &str) -> BytesStart<'static> {
		let mut new_element = BytesStart::new(element.name().as_ref().to_owned());
		let mut replaced = false;

		for attribute in element.attributes().flatten() {
			if attribute.key.as_ref() == name {
				new_element.push_attribute((name, value));
				replaced = true;
			} else {
				new_element.push_attribute(attribute);
			}
		}

		if !replaced {
			new_element.push_attribute((name, value));
		}

		new_element
	}

	fn xml_write_network_device<W: std::io::Write>(writer: &mut Writer<W>, device_element: &BytesStart, wrap_in_network: bool) -> quick_xml::Result<()> {
		if wrap_in_network {
			writer.write_event(Event::Text(BytesText::new("\n        ")))?;
			writer.write_event(Event::Start(BytesStart::new("network")))?;
		}

		writer.write_event(Event::Text(BytesText::new("\n            ")))?;
		writer.write_event(Event::Empty(device_element.borrow()))?;

		if wrap_in_network {
			writer.write_event(Event::Text(BytesText::new("\n        ")))?;
			writer.write_event(Event::End(BytesEnd::new("network")))?;
		}

		Ok(())
	}

	pub fn set_mame_network_interface(mame_directory: &String, machine_name: &str, device_tag: &str, interface: i32) -> Result<(), Box<dyn std::error::Error>> {
		let cfg_directory = LauncherConfig::get_mame_cfg_directory(mame_directory);
		let cfg_file_path = cfg_directory.join(machine_name.to_owned() + ".cfg");

		let interface_value = interface.to_string();
		let mut device_element = BytesStart::new("device");
		device_element.push_attribute(("tag", device_tag));
		device_element.push_attribute(("interface", interface_value.as_str()));

		if !cfg_file_path.exists() {
			fs::create_dir_all(&cfg_directory)?;

			let mut system_element = BytesStart::new("system");
			system_element.push_attribute(("name", machine_name));

			let mut writer = Writer::new(Vec::new());
			writer.write_event(Event::Decl(BytesDecl::new("1.0", None, None)))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::Comment(BytesText::new(" This file is autogenerated; comments and unknown tags will be stripped ")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::Start(BytesStart::new("mameconfig").with_attributes([("version", "10")])))?;
			writer.write_event(Event::Text(BytesText::new("\n    ")))?;
			writer.write_event(Event::Start(system_element))?;
			LauncherConfig::xml_write_network_device(&mut writer, &device_element, true)?;
			writer.write_event(Event::Text(BytesText::new("\n    ")))?;
			writer.write_event(Event::End(BytesEnd::new("system")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::End(BytesEnd::new("mameconfig")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;

			fs::write(cfg_file_path, writer.into_inner())?;

			return Ok(());
		}

		let cfg_text = fs::read_to_string(&cfg_file_path)?;

		let (cfg_bom, cfg_xml) = match cfg_text.strip_prefix('\u{feff}') {
			Some(cfg_xml) => ("\u{feff}", cfg_xml),
			None => ("", cfg_text.as_str())
		};

		let mut has_system = false;
		let mut has_network = false;
		let mut has_device = false;
		{
			let mut reader = Reader::from_str(cfg_xml);
			let mut in_system = false;
			let mut in_network = false;

			loop {
				match reader.read_event()? {
					Event::Start(element) if element.name().as_ref() == "system" => {
						in_system = LauncherConfig::xml_attribute_is(&element, "name", machine_name);
						has_system |= in_system;
					},
					Event::End(element) if element.name().as_ref() == "system" => in_system = false,
					Event::Start(element) if in_system && element.name().as_ref() == "network" => {
						in_network = true;
						has_network = true;
					},
					Event::Empty(element) if in_system && element.name().as_ref() == "network" => has_network = true,
					Event::End(element) if element.name().as_ref() == "network" => in_network = false,
					Event::Start(element) | Event::Empty(element) if in_network && element.name().as_ref() == "device" => {
						has_device |= LauncherConfig::xml_attribute_is(&element, "tag", device_tag);
					},
					Event::Eof => break,
					_ => {}
				}
			}
		}

		if !has_system {
			return Err("Couldn't find where to add the network device in the MAME cfg file.".into());
		}

		let mut reader = Reader::from_str(cfg_xml);
		let mut writer = Writer::new(Vec::new());
		let mut in_system = false;
		let mut in_network = false;

		loop {
			let event = reader.read_event()?;

			match &event {
				Event::Start(element) if element.name().as_ref() == "system" => {
					in_system = LauncherConfig::xml_attribute_is(element, "name", machine_name);

					writer.write_event(event.borrow())?;
					if in_system && !has_network {
						LauncherConfig::xml_write_network_device(&mut writer, &device_element, true)?;
					}

					continue;
				},
				Event::End(element) if element.name().as_ref() == "system" => in_system = false,
				Event::Start(element) if in_system && element.name().as_ref() == "network" => {
					in_network = true;

					writer.write_event(event.borrow())?;
					if !has_device {
						LauncherConfig::xml_write_network_device(&mut writer, &device_element, false)?;
					}

					continue;
				},
				Event::Empty(element) if in_system && element.name().as_ref() == "network" && !has_device => {
					writer.write_event(Event::Start(BytesStart::new("network").with_attributes(element.attributes().flatten())))?;
					LauncherConfig::xml_write_network_device(&mut writer, &device_element, false)?;
					writer.write_event(Event::Text(BytesText::new("\n        ")))?;
					writer.write_event(Event::End(BytesEnd::new("network")))?;

					continue;
				},
				Event::End(element) if element.name().as_ref() == "network" => in_network = false,
				Event::Start(element) | Event::Empty(element) if in_network && element.name().as_ref() == "device" && LauncherConfig::xml_attribute_is(element, "tag", device_tag) => {
					if !LauncherConfig::xml_attribute_is(element, "interface", &interface_value) {
						let new_element = LauncherConfig::xml_with_attribute(element, "interface", &interface_value);

						if let Event::Start(_) = event {
							writer.write_event(Event::Start(new_element))?;
						} else {
							writer.write_event(Event::Empty(new_element))?;
						}

						continue;
					}
				},
				Event::Eof => break,
				_ => {}
			}

			writer.write_event(event.borrow())?;
		}

		let new_cfg_xml = String::from_utf8(writer.into_inner())?;

		if new_cfg_xml != cfg_xml {
			fs::write(cfg_file_path, cfg_bom.to_owned() + &new_cfg_xml)?;
		}

		Ok(())
	}

	pub fn set_mame_audio_input(mame_directory: &String, machine_name: &str, device_tag: &str, node_name: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
		let cfg_directory = LauncherConfig::get_mame_cfg_directory(mame_directory);
		let cfg_file_path = cfg_directory.join(machine_name.to_owned() + ".cfg");

		if !cfg_file_path.exists() {
			fs::create_dir_all(&cfg_directory)?;

			let mut system_element = BytesStart::new("system");
			system_element.push_attribute(("name", machine_name));

			let mut writer = Writer::new(Vec::new());
			writer.write_event(Event::Decl(BytesDecl::new("1.0", None, None)))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::Comment(BytesText::new(" This file is autogenerated; comments and unknown tags will be stripped ")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::Start(BytesStart::new("mameconfig").with_attributes([("version", "10")])))?;
			writer.write_event(Event::Text(BytesText::new("\n    ")))?;
			writer.write_event(Event::Start(system_element))?;
			LauncherConfig::xml_write_sound_map(&mut writer, device_tag, node_name, true)?;
			writer.write_event(Event::Text(BytesText::new("\n    ")))?;
			writer.write_event(Event::End(BytesEnd::new("system")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;
			writer.write_event(Event::End(BytesEnd::new("mameconfig")))?;
			writer.write_event(Event::Text(BytesText::new("\n")))?;

			fs::write(cfg_file_path, writer.into_inner())?;

			return Ok(());
		}

		let cfg_text = fs::read_to_string(&cfg_file_path)?;

		let (cfg_bom, cfg_xml) = match cfg_text.strip_prefix('\u{feff}') {
			Some(cfg_xml) => ("\u{feff}", cfg_xml),
			None => ("", cfg_text.as_str())
		};

		let mut has_system = false;
		{
			let mut reader = Reader::from_str(cfg_xml);

			loop {
				match reader.read_event()? {
					Event::Start(element) | Event::Empty(element) if element.name().as_ref() == "system" => {
						has_system |= LauncherConfig::xml_attribute_is(&element, "name", machine_name);
					},
					Event::Eof => break,
					_ => {}
				}
			}
		}

		if !has_system {
			return Err("Couldn't find where to add the audio input in the MAME cfg file.".into());
		}

		let mut reader = Reader::from_str(cfg_xml);
		let mut writer = Writer::new(Vec::new());
		let mut in_system = false;
		let mut in_mixer = false;
		let mut has_mixer = false;
		let mut wrote_sound_map = false;
		let mut skip_depth = 0;

		loop {
			let event = reader.read_event()?;

			if skip_depth > 0 {
				match &event {
					Event::Start(_) => skip_depth += 1,
					Event::End(_) => skip_depth -= 1,
					Event::Eof => break,
					_ => {}
				}

				continue;
			}

			match &event {
				Event::Start(element) if element.name().as_ref() == "system" => {
					in_system = LauncherConfig::xml_attribute_is(element, "name", machine_name);
				},
				Event::End(element) if element.name().as_ref() == "system" => {
					if in_system && !has_mixer {
						// The whitespace before </system> was already written so add the indent difference.
						writer.write_event(Event::Text(BytesText::new("    ")))?;
						writer.write_event(Event::Start(BytesStart::new("mixer")))?;
						writer.write_event(Event::Text(BytesText::new("\n            ")))?;
						LauncherConfig::xml_write_sound_map_element(&mut writer, device_tag, node_name, "            ")?;
						writer.write_event(Event::Text(BytesText::new("\n        ")))?;
						writer.write_event(Event::End(BytesEnd::new("mixer")))?;
						writer.write_event(Event::Text(BytesText::new("\n    ")))?;
					}

					in_system = false;
				},
				Event::Start(element) if in_system && element.name().as_ref() == "mixer" => {
					in_mixer = true;
					has_mixer = true;
				},
				Event::Empty(element) if in_system && element.name().as_ref() == "mixer" => {
					has_mixer = true;
					writer.write_event(Event::Start(BytesStart::new("mixer").with_attributes(element.attributes().flatten())))?;
					LauncherConfig::xml_write_sound_map(&mut writer, device_tag, node_name, false)?;
					writer.write_event(Event::Text(BytesText::new("\n        ")))?;
					writer.write_event(Event::End(BytesEnd::new("mixer")))?;

					continue;
				},
				Event::End(element) if in_mixer && element.name().as_ref() == "mixer" => {
					if !wrote_sound_map {
						// The whitespace before </mixer> was already written so add the indent difference.
						writer.write_event(Event::Text(BytesText::new("    ")))?;
						LauncherConfig::xml_write_sound_map_element(&mut writer, device_tag, node_name, "            ")?;
						writer.write_event(Event::Text(BytesText::new("\n        ")))?;
						wrote_sound_map = true;
					}

					in_mixer = false;
				},
				Event::Start(element) if in_mixer && element.name().as_ref() == "sound_map" && LauncherConfig::xml_attribute_is(element, "tag", device_tag) => {
					LauncherConfig::xml_write_sound_map_element(&mut writer, device_tag, node_name, "            ")?;
					wrote_sound_map = true;
					skip_depth = 1;

					continue;
				},
				Event::Empty(element) if in_mixer && element.name().as_ref() == "sound_map" && LauncherConfig::xml_attribute_is(element, "tag", device_tag) => {
					LauncherConfig::xml_write_sound_map_element(&mut writer, device_tag, node_name, "            ")?;
					wrote_sound_map = true;

					continue;
				},
				Event::Eof => break,
				_ => {}
			}

			writer.write_event(event.borrow())?;
		}

		let new_cfg_xml = String::from_utf8(writer.into_inner())?;

		if new_cfg_xml != cfg_xml {
			fs::write(cfg_file_path, cfg_bom.to_owned() + &new_cfg_xml)?;
		}

		Ok(())
	}

	fn xml_write_sound_map<W: std::io::Write>(writer: &mut Writer<W>, device_tag: &str, node_name: Option<&str>, wrap_in_mixer: bool) -> quick_xml::Result<()> {
		if wrap_in_mixer {
			writer.write_event(Event::Text(BytesText::new("\n        ")))?;
			writer.write_event(Event::Start(BytesStart::new("mixer")))?;
		}

		writer.write_event(Event::Text(BytesText::new("\n            ")))?;
		LauncherConfig::xml_write_sound_map_element(writer, device_tag, node_name, "            ")?;

		if wrap_in_mixer {
			writer.write_event(Event::Text(BytesText::new("\n        ")))?;
			writer.write_event(Event::End(BytesEnd::new("mixer")))?;
		}

		Ok(())
	}

	// <sound_map tag="device_tag"><node_mapping node="node_name" db="0.000000" /></sound_map> or an empty sound_map when there's no node (unmapped).
	fn xml_write_sound_map_element<W: std::io::Write>(writer: &mut Writer<W>, device_tag: &str, node_name: Option<&str>, indent: &str) -> quick_xml::Result<()> {
		let sound_map_element = BytesStart::new("sound_map").with_attributes([("tag", device_tag)]);

		match node_name {
			Some(node_name) => {
				writer.write_event(Event::Start(sound_map_element))?;
				writer.write_event(Event::Text(BytesText::new(&("\n".to_owned() + indent + "    "))))?;
				writer.write_event(Event::Empty(BytesStart::new("node_mapping").with_attributes([("node", node_name), ("db", "0.000000")])))?;
				writer.write_event(Event::Text(BytesText::new(&("\n".to_owned() + indent))))?;
				writer.write_event(Event::End(BytesEnd::new("sound_map")))?;
			},
			None => {
				writer.write_event(Event::Empty(sound_map_element))?;
			}
		}

		Ok(())
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
				selected_modem_moip_endpoint: None,
				selected_debug_bitb_endpoint: None,
				selected_lirc_endpoint: None,
				selected_hdimg_paths: None,
				selected_hdimg_enabled: None,
				verbose_mode: Some(false),
				use_drc: Some(true),
				debug_mode: Some(false),
				validate_build_image: Some(true),
				console_input: Some(false),
				disable_sound: Some(false),
				windowing_mode: Some("captured".into()),
				captured_console_height: None,
				captured_window_width: None,
				captured_window_height: None,
				use_decoupled_sound: Some(false),
				softmodem_record: Some(false),
				enable_wtvpointer: Some(true),
				enable_wtvnet: Some(true),
				enable_lirc: Some(false),
				selected_audio_in_endpoint: None,
				enable_audio_in: Some(false),
				selected_video_in_endpoint: None,
				enable_video_in: Some(false),
				selected_smartcard_endpoint: None,
				enable_smartcard: Some(false),
				selected_cacard_endpoint: None,
				enable_cacard: Some(false),
				selected_wtvport_option: None,
				enable_wtvport: Some(false),
				selected_parallel_option: None,
				selected_parallel_socket_endpoint: None,
				selected_parallel_hostlpt_device: None,
				enable_parallel: Some(false),
				custom_options: Some("".into()),
				show_advanced_options: Some(false)
			},
			network: Some(NetworkOptions::default_options())
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
