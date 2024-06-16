use rand_core::{RngCore, OsRng};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

#[allow(dead_code)]
#[derive(Debug, Clone, EnumIter, PartialEq)]
pub enum SSIDBoxType {
	Generic  = 0xff,
	None     = 0x00,
	Internal = 0x01,
	MattMan  = 0x69,
	MAME     = 0x71,
	Retail   = 0x81,
	Viewer   = 0x91
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BoxTypeItem {
	pub box_type: SSIDBoxType,
	pub name: String,
	pub value: i32,
	pub hex_value: String,
	pub description: String
}


impl SSIDBoxType {
	#[allow(dead_code)]
	pub fn from_u8(value: u8) -> SSIDBoxType {
		match value {
			0xff => SSIDBoxType::Generic,
			0x00 => SSIDBoxType::None,
			0x01 => SSIDBoxType::Internal,
			0x69 => SSIDBoxType::MattMan,
			0x71 => SSIDBoxType::MAME,
			0x81 => SSIDBoxType::Retail,
			0x91 => SSIDBoxType::Viewer,
			_    => SSIDBoxType::Generic 
		}
	}

	#[allow(dead_code)]
	pub fn to_u8(box_type: &SSIDBoxType) -> u8 {
		match box_type {
			SSIDBoxType::Generic  => 0xff,
			SSIDBoxType::None     => 0x00,
			SSIDBoxType::Internal => 0x01,
			SSIDBoxType::MattMan  => 0x69,
			SSIDBoxType::MAME     => 0x71,
			SSIDBoxType::Retail   => 0x81,
			SSIDBoxType::Viewer   => 0x91,
		}
	}

	#[allow(dead_code)]
	pub fn to_string(box_type: &SSIDBoxType) -> String {
		match box_type {
			SSIDBoxType::Generic  => "Generic".into(),
			SSIDBoxType::None     => "None".into(),
			SSIDBoxType::Internal => "Internal".into(),
			SSIDBoxType::MattMan  => "MattMan".into(),
			SSIDBoxType::MAME     => "MAME".into(),
			SSIDBoxType::Retail   => "Retail".into(),
			SSIDBoxType::Viewer   => "Viewer".into(),
		}
	}

	#[allow(dead_code)]
	pub fn to_item(box_type: SSIDBoxType) -> BoxTypeItem {
		let mut description: String = "".into();

		if box_type == SSIDBoxType::MattMan {
			description = "MattMan's cool SSID".into();
		} else if box_type == SSIDBoxType::Internal {
			description = "Used for WNI's internal testing".into();
		}

		BoxTypeItem {
			box_type: box_type.clone(),
			name: SSIDBoxType::to_string(&box_type),
			value: box_type.clone() as i32,
			hex_value: format!("0x{:02x}", (box_type.clone() as i32)).into(),
			description: description,
		}
	}

	#[allow(dead_code)]
	pub fn to_list() -> Vec<BoxTypeItem> {
		let mut list: Vec<BoxTypeItem> = vec![];

		for box_type in SSIDBoxType::iter() {
			list.push(SSIDBoxType::to_item(box_type));
		}

		list
	}
}

#[derive(Debug, Clone, EnumIter, PartialEq)]
pub enum SSIDManufacture {
	Generic    = 0xffff,
	Sony       = 0x0000,
	Phillips   = 0x1000,
	WebTVOEM   = 0x2000,
	Pace       = 0x3000,
	Mitsubishi = 0x4000,
	Unknown1   = 0x5000,
	Fujitsu    = 0x6000,
	Samsung    = 0x7000,
	Echostar   = 0x8000,
	RCA        = 0x9000,
	Sharp      = 0xa000,
	Unknown2   = 0xb000,
	Unknown3   = 0xc000,
	Unknown4   = 0xd000,
	Unknown5   = 0xe000,
	Unknown6   = 0xf000,
	Matsushita = 0x0001
}

#[derive(Debug, Clone)]
pub struct ManufactureItem {
	pub manufacture: SSIDManufacture,
	pub name: String,
	pub value: u16,
	pub hex_value: String,
	pub description: String
}

impl SSIDManufacture {
	#[allow(dead_code)]
	pub fn from_u16(value: u16) -> SSIDManufacture {
		match value {
			0xffff => SSIDManufacture::Generic,
			0x0000 => SSIDManufacture::Sony,
			0x1000 => SSIDManufacture::Phillips,
			0x2000 => SSIDManufacture::WebTVOEM,
			0x3000 => SSIDManufacture::Pace,
			0x4000 => SSIDManufacture::Mitsubishi,
			0x5000 => SSIDManufacture::Unknown1,
			0x6000 => SSIDManufacture::Fujitsu,
			0x7000 => SSIDManufacture::Samsung,
			0x8000 => SSIDManufacture::Echostar,
			0x9000 => SSIDManufacture::RCA,
			0xa000 => SSIDManufacture::Sharp,
			0xb000 => SSIDManufacture::Unknown2,
			0xc000 => SSIDManufacture::Unknown3,
			0xd000 => SSIDManufacture::Unknown4,
			0xe000 => SSIDManufacture::Unknown5,
			0xf000 => SSIDManufacture::Unknown6,
			0x0001 => SSIDManufacture::Matsushita,
			_      => SSIDManufacture::Generic 
		}
	}

	#[allow(dead_code)]
	pub fn to_u16(manufacture: &SSIDManufacture) -> u16 {
		match manufacture {
			SSIDManufacture::Generic    => 0xffff,
			SSIDManufacture::Sony       => 0x0000,
			SSIDManufacture::Phillips   => 0x1000,
			SSIDManufacture::WebTVOEM   => 0x2000,
			SSIDManufacture::Pace       => 0x3000,
			SSIDManufacture::Mitsubishi => 0x4000,
			SSIDManufacture::Unknown1   => 0x5000,
			SSIDManufacture::Fujitsu    => 0x6000,
			SSIDManufacture::Samsung    => 0x7000,
			SSIDManufacture::Echostar   => 0x8000,
			SSIDManufacture::RCA        => 0x9000,
			SSIDManufacture::Sharp      => 0xa000,
			SSIDManufacture::Unknown2   => 0xb000,
			SSIDManufacture::Unknown3   => 0xc000,
			SSIDManufacture::Unknown4   => 0xd000,
			SSIDManufacture::Unknown5   => 0xe000,
			SSIDManufacture::Unknown6   => 0xf000,
			SSIDManufacture::Matsushita => 0x0001
		}
	}

	#[allow(dead_code)]
	pub fn to_string(manufacture: &SSIDManufacture) -> String {
		match manufacture {
			SSIDManufacture::Generic    => "Generic".into(),
			SSIDManufacture::Sony       => "Sony".into(),
			SSIDManufacture::Phillips   => "Phillips".into(),
			SSIDManufacture::WebTVOEM   => "WebTV OEM".into(),
			SSIDManufacture::Pace       => "PACE".into(),
			SSIDManufacture::Mitsubishi => "Mitsubishi".into(),
			SSIDManufacture::Unknown1   => "Unknown1".into(),
			SSIDManufacture::Fujitsu    => "Fujitsu".into(),
			SSIDManufacture::Samsung    => "Samsung".into(),
			SSIDManufacture::Echostar   => "Echostar".into(),
			SSIDManufacture::RCA        => "RCA".into(),
			SSIDManufacture::Sharp      => "Sharp".into(),
			SSIDManufacture::Unknown2   => "Unknown2".into(),
			SSIDManufacture::Unknown3   => "Unknown3".into(),
			SSIDManufacture::Unknown4   => "Unknown4".into(),
			SSIDManufacture::Unknown5   => "Unknown5".into(),
			SSIDManufacture::Unknown6   => "Unknown6".into(),
			SSIDManufacture::Matsushita => "Mitsushita".into()
		}
	}

	#[allow(dead_code)]
	pub fn to_item(manufacture: SSIDManufacture) -> ManufactureItem {
		let mut description: String = "".into();

		if manufacture == SSIDManufacture::Phillips {
			description = "Phillips Magnavox".into();
		} else if manufacture == SSIDManufacture::Pace {
			description = "PACE Phillips".into();
		} else if manufacture == SSIDManufacture::Sharp {
			description = "Sharp Electronics".into();
		}

		ManufactureItem {
			manufacture: manufacture.clone(),
			name: SSIDManufacture::to_string(&manufacture),
			value: manufacture.clone() as u16,
			hex_value: format!("0x{:04x}", (manufacture.clone() as u16)).into(),
			description: description,
		}
	}

	pub fn to_list(include_unknown: bool, include_generic: bool) -> Vec<ManufactureItem> {
		let mut list: Vec<ManufactureItem> = vec![];

		let unknown_manufactures = [
			SSIDManufacture::Unknown1,
			SSIDManufacture::Unknown2,
			SSIDManufacture::Unknown3,
			SSIDManufacture::Unknown4,
			SSIDManufacture::Unknown5,
			SSIDManufacture::Unknown6,
		];

		for manufacture in SSIDManufacture::iter() {
			if !include_unknown && unknown_manufactures.contains(&manufacture) {
				continue;
			} else if !include_generic && manufacture == SSIDManufacture::Generic {
				continue;
			}

			list.push(SSIDManufacture::to_item(manufacture));
		}

		list
	}
}

//  BOX ID  | MANUFACTURER ID | CRC
// TTRRRRRR | M_SMSS          | CC
//
//	TT     = Box type
//	RRRRRR = Random ID. Allocating approx 16.7 million boxes per manufacturer
//	M      = Manufacturer company. Allocating approx 255 manufacturers, 16 if just the first M is used
//	_      = Unused/random?
//	S      = Manufacturer signature? Needs to be bM02 before the manufacturer is checked, otherwise WebTV Generic is assumed.
//	CC     = SSID CRC

#[derive(Debug, Clone)]
pub struct SSIDInfo {
	pub box_type: SSIDBoxType,
	pub box_id: u32,
	pub manufacture: SSIDManufacture,
	pub manufacture_unknown1: u8,
	pub manufacture_signature: u16,
	pub crc: u8,
	pub calculated_crc: u8,
	pub raw: [u8; 0x08],
	pub value: String,
}

impl SSIDInfo {
	pub fn generate(box_type: SSIDBoxType, manufacture: SSIDManufacture) -> Result<SSIDInfo, Box<dyn std::error::Error>> {
		let mut raw_ssid = [0x00; 0x08];

		OsRng.fill_bytes(&mut raw_ssid);
		raw_ssid[0] = SSIDBoxType::to_u8(&box_type);

		let u_manufacture: u16 = SSIDManufacture::to_u16(&manufacture);
		let u_manufacture_signature: u16 = 0xb002;
		let u_manufacture_unknown1: u8 = 0x00;

		raw_ssid[4] = (((u_manufacture >> 8) & 0xf0) as u8) | (u_manufacture_unknown1 & 0x0f);
		raw_ssid[5] = (((u_manufacture_signature >> 8) & 0xf0) as u8) | (((u_manufacture >> 0) & 0x0f) as u8);
		raw_ssid[6] = (u_manufacture_signature & 0xff) as u8;
		raw_ssid[7] = match SSIDInfo::calculate_raw_crc(raw_ssid) {
			Ok(crc) => crc,
			_ => 0x00
		};

		SSIDInfo::new(raw_ssid)
	}

	pub fn new(ssid: [u8; 0x08]) -> Result<SSIDInfo, Box<dyn std::error::Error>>  {
		let u_box_type = ssid[0];
		let u_box_id = u32::from_be_bytes(ssid[0..4].try_into().unwrap_or([0x00, 0x00, 0x00, 0x00])) & 0xffffff;
		let u_manufacture = u16::from_be_bytes(ssid[4..6].try_into().unwrap_or([0x00, 0x00])) & 0xf00f;
		let u_manufacture_unknown1 = ssid[4] & 0x0f;
		let u_manufacture_signature = u16::from_be_bytes(ssid[5..7].try_into().unwrap_or([0x00, 0x00])) & 0xf0ff;
		let u_crc = ssid[7];


		let mut wtv_ssid = SSIDInfo {
			box_type: SSIDBoxType::from_u8(u_box_type),
			box_id: u_box_id,
			manufacture: SSIDManufacture::from_u16(u_manufacture),
			manufacture_unknown1: u_manufacture_unknown1,
			manufacture_signature: u_manufacture_signature,
			crc: u_crc,
			calculated_crc: 0,
			raw: ssid.clone(),
			value: ssid.iter()
				.map(|b| format!("{:02x}", b).to_string())
				.collect::<Vec<String>>()
				.join("")
				.into()
		};

		wtv_ssid.calculated_crc = wtv_ssid.calculate_crc().unwrap_or(0x00);

		Ok(wtv_ssid)
	}

	fn calculate_crc(&mut self) -> Result<u8, Box<dyn std::error::Error>> {
		SSIDInfo::calculate_raw_crc(self.raw)
	}

	fn calculate_raw_crc(raw_ssid: [u8; 0x08]) -> Result<u8, Box<dyn std::error::Error>> {
		let mut ssid_crc = 0;

		for index in 0..7 {
			let mut byte = raw_ssid[index];

			for _ in 0..8 {
				let mix = (ssid_crc ^ byte) & 1;

				ssid_crc >>= 1;

				if mix == 1 {
					ssid_crc ^= 0x8C;
				}

				byte >>= 1;
			}
		}

		Ok(ssid_crc)
	}
}