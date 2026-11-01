use rand_core::{RngCore, OsRng};
use std::io::{Error, ErrorKind};

const BOXID_VER: u8 = 2; // If your BoxID different but the MAC is the same, then it's probably because of this
const BOXID_BIT_COUNT: u8 = 64;
const BOXID_CHUNK_BITS: u8 = 13;
const BOXID_LAST_CHUNK_BITS: u8 = 12;
const BOXID_CRC8_IDX: usize = 0;
const BOXID_TYPE_IDX: usize = 1;
const BOXID_MACX_IDX: usize = 2;
const BOXID_MACX_LEN: u8 = 6;
const BOXID_MACX_ORDER: [usize; 6] = [4, 1, 2, 5, 0, 3];
const BOXID_MACX_XOR: u8 = 0x4a;

const BOXID_BYTE_BITCNT: u8 = 8;

const BOXID_INTEL_CSUM_TARGET: u16 = 0xbaba;
const BOXID_EEPROM_SIZE: usize = 128; // Only a 128 byte EEPROM is emulated in wtvmame. May allow configurable sizes later.
const BOXID_EEPROM_READ_INTERVAL: usize = 2; // Read one WORD at a time

const BOXID_DEFAULT_EEPROM_BLOB: [u8; BOXID_EEPROM_SIZE] = [
	// 0x00       0x01       0x02       0x03       0x04       0x05       0x06       0x07
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x08       0x09       0x0a       0x0b       0x0c       0x0d       0x0e       0x0f
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x10       0x11       0x12       0x13       0x14       0x15       0x16       0x17
	0xE1,0xD5, 0xAD,0x1D, 0x13,0x39, 0xC1,0xAC, 0x43,0x88, 0x1B,0x10, 0x00,0x00, 0xF4,0xB0,
	// 0x18       0x19       0x1a       0x1b       0x1c       0x1d       0x1e       0x1f
	0x0B,0x4F, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x20       0x21       0x22       0x23       0x24       0x25       0x26       0x27
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x28       0x29       0x2a       0x2b       0x2c       0x2d       0x2e       0x2f
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x30       0x31       0x32       0x33       0x34       0x35       0x36       0x37
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00,
	// 0x38       0x39       0x3a       0x3b       0x3c       0x3d       0x3e       0x3f
	0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0x00,0x00, 0xFB,0x48 
];

//
// The MSNTV2 box id is a 64-bit (8-byte) bit sequence:
//   CCCCCCCC | LLLLLVVV | MMMMMMMM | MMMMMMMM | MMMMMMMM | MMMMMMMM | MMMMMMMM | MMMMMMMM
//       - CCCCCCCC: CRC8 of the entire bit sequence (with the C bits being 0)
//       - LLLLL: The length in bytes of the M bits. Always 6 since we're using the MAC address but allows up to 32 bytes.
//       - VVV: probably some version field. It's 0 for the MSNTV2
//       - MMMMMMMM: These are the modified MAC address bytes. These need to be unwound in a different order and XORd with 0x4a to return to the ethernet MAC sequence.
//
// The ID shown to the user takes this 64-bit sequence, chunks the ID into four 13-bit chunks then a 12-bit chunk at the end.
// The bit value of each chunk is converted into base-10 strings then concatinated and shown to the user
//

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MSNTV2BoxIdInfo {
	pub raw: [u8; 8],
	pub value: String,
	pub id_type: u8,
	pub id_version: u8,
	pub id_length: u8,
	pub crc: u8,
	pub calculated_crc: u8,
	pub mac_address_raw: [u8; 6],
	pub mac_address_str: String,
	pub mac_address_oui: u32,
	pub mac_address_nic_id: u32,
	pub mac_address_is_broadcast: bool,
	pub mac_address_is_local: bool,
	pub eeprom_blob: [u8; BOXID_EEPROM_SIZE],
	pub eeprom_crc: u16,
	pub calculated_eeprom_crc: u16,
}

impl MSNTV2BoxIdInfo {
	#[allow(dead_code)]
	pub fn generate() -> Result<MSNTV2BoxIdInfo, Box<dyn std::error::Error>> {
		let mut box_id = MSNTV2BoxIdInfo {
			raw: [0x00; 8],
			value: "".to_string(),
			id_type: 0,
			id_version: 0,
			id_length: 0,
			crc: 0,
			calculated_crc: 0,
			mac_address_raw: [0x00; 6],
			mac_address_str: "".to_string(),
			mac_address_oui: 0,
			mac_address_nic_id: 0,
			mac_address_is_broadcast: false,
			mac_address_is_local: false,
			eeprom_blob: BOXID_DEFAULT_EEPROM_BLOB,
			eeprom_crc: 0,
			calculated_eeprom_crc: 0,
		};

		let mut mac_address: [u8; 6] = [0x00; 6];
		OsRng.fill_bytes(&mut mac_address);

		// Remove is_broadcast/multicast and is_local bits
		mac_address[0] &= !0x3;

		let _ = box_id.set_mac_address(mac_address, true);

		Ok(box_id)
	}

	#[allow(dead_code)]
	pub fn set_box_id(&mut self, raw_box_id: [u8; 8], update_crcs: bool) -> Result<(), Box<dyn std::error::Error>> {
		self.raw = raw_box_id.clone();

		self.crc = self.raw[BOXID_CRC8_IDX];
		self.calculated_crc = self.calculate_crc().unwrap_or(0x00);

		self.id_type = self.raw[BOXID_TYPE_IDX];
		self.id_version = self.raw[BOXID_TYPE_IDX] & 0x07;
		self.id_length = (self.raw[BOXID_TYPE_IDX] >> 0x03) & 0x1f;

		self.value = self.serialize_boxid_value().unwrap_or("".to_string());

		// The MSNTV2 takes the MAC address bytes, XORs them with 0x4a then scrambles the byte order in a predictable way.
		// This takes the ID bytes and returns it back into a usable MAC address.
		let mut cur_idx = 0;
		for &swap_idx in &BOXID_MACX_ORDER {
			self.mac_address_raw[cur_idx] = self.raw[BOXID_MACX_IDX + swap_idx] ^ BOXID_MACX_XOR;
			cur_idx += 1;
		}
		let _ = self.unpack_mac_address();

		if update_crcs {
			let crc_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

			self.eeprom_crc = u16::from_le_bytes(self.eeprom_blob[crc_idx..(crc_idx + BOXID_EEPROM_READ_INTERVAL)].try_into().unwrap_or([0x00; 2]));
			self.calculated_eeprom_crc = self.eeprom_crc;
		}

		Ok(())
	}

	pub fn set_mac_address(&mut self, mac_address: [u8; 6], update_crcs: bool) -> Result<(), Box<dyn std::error::Error>> {
		self.mac_address_raw = mac_address.clone();
		let _ = self.unpack_mac_address();

		self.raw[BOXID_TYPE_IDX] = ((BOXID_MACX_LEN & 0x1f) << 3) | ((BOXID_VER & 0x07) << 0);
		self.id_type = self.raw[BOXID_TYPE_IDX];
		self.id_version = self.raw[BOXID_TYPE_IDX] & 0x07;
		self.id_length = (self.raw[BOXID_TYPE_IDX] >> 0x03) & 0x1f;

		let mut cur_idx = 0;
		for &swap_idx in &BOXID_MACX_ORDER {
			self.raw[BOXID_MACX_IDX + cur_idx] = self.mac_address_raw[swap_idx] ^ BOXID_MACX_XOR;
			cur_idx += 1;
		}

		self.raw[BOXID_CRC8_IDX] = self.calculate_crc().unwrap_or(0x00);
		self.crc = self.raw[BOXID_CRC8_IDX];
		self.calculated_crc = self.raw[BOXID_CRC8_IDX];

		self.value = self.serialize_boxid_value().unwrap_or("".to_string());

		if update_crcs {
			let crc_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

			self.eeprom_crc = u16::from_le_bytes(self.eeprom_blob[crc_idx..(crc_idx + BOXID_EEPROM_READ_INTERVAL)].try_into().unwrap_or([0x00; 2]));
			self.calculated_eeprom_crc = self.eeprom_crc;
		}

		Ok(())
	}

	pub fn set_string(&mut self, id_str: String, update_crcs: bool) -> Result<(), Box<dyn std::error::Error>> {
		let in_id: String = id_str
			.chars()
			.filter(|&c| ![' ', ':', '-', '.', ','].contains(&c))
			.collect();

		if in_id.len() == 20 && in_id.chars().all(|c| c.is_ascii_digit()) {
			let mut boxid_chunks = [0u32; 5];
			for idx in 0..5 {
				let start = idx * 4;
				let end = start + 4;

				boxid_chunks[idx] = in_id[start..end].parse::<u32>()?;
			}

			let raw_box_id = MSNTV2BoxIdInfo::extract_boxid_bytes(&boxid_chunks).unwrap_or([0x00; 8]);
			let _ = self.set_box_id(raw_box_id, update_crcs);
		} else if in_id.len() == 12 && u64::from_str_radix(&in_id, 16).is_ok() {
			let mut mac_address = [0u8; 6];
			for i in 0..6 {
				let start = i * 2;
				let end = start + 2;
				mac_address[i] = u8::from_str_radix(&in_id[start..end], 16).unwrap_or(0x00);
			}
			let _ = self.set_mac_address(mac_address, update_crcs);
		}

		Ok(())
	}

	#[allow(dead_code)]
	pub fn new_from_eeprom(eeprom_blob: [u8; BOXID_EEPROM_SIZE]) -> Result<MSNTV2BoxIdInfo, Box<dyn std::error::Error>> {
		let crc_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

		let mut box_id = MSNTV2BoxIdInfo {
			raw: [0x00; 8],
			value: "".to_string(),
			id_type: 0,
			id_version: 0,
			id_length: 0,
			crc: 0,
			calculated_crc: 0,
			mac_address_raw: [0x00; 6],
			mac_address_str: "".to_string(),
			mac_address_oui: 0,
			mac_address_nic_id: 0,
			mac_address_is_broadcast: false,
			mac_address_is_local: false,
			eeprom_blob: eeprom_blob.clone(),
			eeprom_crc: u16::from_le_bytes(eeprom_blob[crc_idx..(crc_idx + BOXID_EEPROM_READ_INTERVAL)].try_into().unwrap_or([0x00; 2])),
			calculated_eeprom_crc: MSNTV2BoxIdInfo::calculate_raw_eeprom_csum(&eeprom_blob).unwrap_or(0x0000),
		};

		let box_max_addr: [u8; 6] = eeprom_blob[..6].try_into().unwrap_or([0x00; 6]);
		let _ = box_id.set_mac_address(box_max_addr, false);

		Ok(box_id)
	}

	#[allow(dead_code)]
	pub fn new_from_string(id_str: String) -> Result<MSNTV2BoxIdInfo, Box<dyn std::error::Error>>  {
		let crc_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

		let mut box_id = MSNTV2BoxIdInfo {
			raw: [0x00; 8],
			value: "".to_string(),
			id_type: 0,
			id_version: 0,
			id_length: 0,
			crc: 0,
			calculated_crc: 0,
			mac_address_raw: [0x00; 6],
			mac_address_str: "".to_string(),
			mac_address_oui: 0,
			mac_address_nic_id: 0,
			mac_address_is_broadcast: false,
			mac_address_is_local: false,
			eeprom_blob: BOXID_DEFAULT_EEPROM_BLOB,
			eeprom_crc: 0,
			calculated_eeprom_crc: 0,
		};

		let _ = box_id.set_string(id_str, true);

		box_id.eeprom_crc = u16::from_le_bytes(box_id.eeprom_blob[crc_idx..(crc_idx + BOXID_EEPROM_READ_INTERVAL)].try_into().unwrap_or([0x00; 2]));
		box_id.calculated_eeprom_crc = box_id.eeprom_crc;

		Ok(box_id)
	}

	#[allow(dead_code)]
	pub fn new_from_mac_address(mac_address: [u8; 6]) -> Result<MSNTV2BoxIdInfo, Box<dyn std::error::Error>>  {
		let mut box_id = MSNTV2BoxIdInfo {
			raw: [0x00; 8],
			value: "".to_string(),
			id_type: 0,
			id_version: 0,
			id_length: 0,
			crc: 0,
			calculated_crc: 0,
			mac_address_raw: [0x00; 6],
			mac_address_str: "".to_string(),
			mac_address_oui: 0,
			mac_address_nic_id: 0,
			mac_address_is_broadcast: false,
			mac_address_is_local: false,
			eeprom_blob: BOXID_DEFAULT_EEPROM_BLOB,
			eeprom_crc: 0,
			calculated_eeprom_crc: 0,
		};

		let _ = box_id.set_mac_address(mac_address, true);


		Ok(box_id)
	}	

	#[allow(dead_code)]
	pub fn new(raw_box_id: [u8; 8]) -> Result<MSNTV2BoxIdInfo, Box<dyn std::error::Error>>  {
		let crc_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

		let mut box_id = MSNTV2BoxIdInfo {
			raw: [0x00; 8],
			value: "".to_string(),
			id_type: 0,
			id_version: 0,
			id_length: 0,
			crc: 0,
			calculated_crc: 0,
			mac_address_raw: [0x00; 6],
			mac_address_str: "".to_string(),
			mac_address_oui: 0,
			mac_address_nic_id: 0,
			mac_address_is_broadcast: false,
			mac_address_is_local: false,
			eeprom_blob: BOXID_DEFAULT_EEPROM_BLOB,
			eeprom_crc: 0,
			calculated_eeprom_crc: 0,
		};

		let _ = box_id.set_box_id(raw_box_id, true);

		box_id.eeprom_crc = u16::from_le_bytes(box_id.eeprom_blob[crc_idx..(crc_idx + BOXID_EEPROM_READ_INTERVAL)].try_into().unwrap_or([0x00; 2]));
		box_id.calculated_eeprom_crc = box_id.eeprom_crc;

		Ok(box_id)
	}

	#[allow(dead_code)]
	fn update_eeprom_csum(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		MSNTV2BoxIdInfo::update_raw_eeprom_csum(&mut self.eeprom_blob)
	}

	#[allow(dead_code)]
	fn update_raw_eeprom_csum(eeprom_blob: &mut [u8; BOXID_EEPROM_SIZE]) -> Result<(), Box<dyn std::error::Error>> {
		let start_idx = BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL;

		let eeprom_csum_diff = MSNTV2BoxIdInfo::calculate_raw_eeprom_csum(eeprom_blob).unwrap_or(0x0000);

		eeprom_blob[start_idx..start_idx + BOXID_EEPROM_READ_INTERVAL].copy_from_slice(&eeprom_csum_diff.to_le_bytes());

		Ok(())
	}	

	#[allow(dead_code)]
	fn calculate_eeprom_csum(&mut self) -> Result<u16, Box<dyn std::error::Error>> {
		MSNTV2BoxIdInfo::calculate_raw_eeprom_csum(&self.eeprom_blob)
	}

	#[allow(dead_code)]
	fn calculate_raw_eeprom_csum(eeprom_blob: &[u8; BOXID_EEPROM_SIZE]) -> Result<u16, Box<dyn std::error::Error>> {
		let mut csum_value: u32 = 0;
		let limit = (BOXID_EEPROM_SIZE - BOXID_EEPROM_READ_INTERVAL) as u8;

		for eeprom_idx in (0..limit).step_by(BOXID_EEPROM_READ_INTERVAL) {
			let word_value = u16::from_le_bytes([
				eeprom_blob[eeprom_idx as usize],
				eeprom_blob[(eeprom_idx as usize) + 1],
			]);

			csum_value += word_value as u32;
		}

		Ok(BOXID_INTEL_CSUM_TARGET.wrapping_sub(csum_value as u16))
	}

	fn unpack_mac_address(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		self.mac_address_str = self.mac_address_raw.clone()
			.iter()
			.map(|b| format!("{:02X}", b))
			.collect::<Vec<String>>()
			.join(":");

		self.mac_address_oui = ((self.mac_address_raw[0] as u32) << 16) | ((self.mac_address_raw[1] as u32) << 8) | (self.mac_address_raw[2] as u32);
		self.mac_address_nic_id = ((self.mac_address_raw[3] as u32) << 16) | ((self.mac_address_raw[4] as u32) << 8) | (self.mac_address_raw[5] as u32);
		self.mac_address_is_broadcast = (self.mac_address_raw[0] & 0x1) != 0;
		self.mac_address_is_local = (self.mac_address_raw[0] & 0x2) != 0;

		self.eeprom_blob[..6].copy_from_slice(&self.mac_address_raw);
		let _ = self.update_eeprom_csum();

		Ok(())
	}

	fn extract_boxid_bytes(boxid_chunks: &[u32; 5]) -> Result<[u8; 8], Box<dyn std::error::Error>> {
		let mut raw_box_id = [0u8; 8];
		let mut byte_idx = 0;
		
		let mut leftover_bitcnt = 0;
		let mut leftover_bits = 0u32;
		let mut boxid_bitcnt = BOXID_BIT_COUNT;

		for &chunk in boxid_chunks {
			let chunk_bitcnt = std::cmp::min(boxid_bitcnt, BOXID_CHUNK_BITS);
			
			let unseen_bits = chunk | (leftover_bits << chunk_bitcnt);
			let mut unseen_bitcnt = leftover_bitcnt + chunk_bitcnt;

			while unseen_bitcnt >= BOXID_BYTE_BITCNT && byte_idx < 8 {
				unseen_bitcnt -= BOXID_BYTE_BITCNT;
				raw_box_id[byte_idx] = ((unseen_bits >> unseen_bitcnt) & 0xff) as u8;
				byte_idx += 1;
			}

			leftover_bitcnt = unseen_bitcnt;
			leftover_bits = unseen_bits & ((1 << leftover_bitcnt) - 1);

			if boxid_bitcnt >= BOXID_CHUNK_BITS {
				boxid_bitcnt -= BOXID_CHUNK_BITS;
			} else {
				break;
			}
		}

		Ok(raw_box_id)
	}

	fn serialize_boxid_value(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		MSNTV2BoxIdInfo::serialize_raw_boxid_value(&self.raw)
	}

	fn serialize_raw_boxid_value(raw_box_id: &[u8; 8]) -> Result<String, Box<dyn std::error::Error>> {
		let mut boxid_chunks: [u32; 5] = [0, 0, 0, 0, 0];
		let boxid_chunk_len = boxid_chunks.len();
		let boxid_last_idx = boxid_chunk_len - 1;
		let mut boxid_bit_idx = 0;
		for idx in 0..boxid_chunk_len {
			let chunk_bitcnt = if idx >= boxid_last_idx {
				BOXID_LAST_CHUNK_BITS
			} else {
				BOXID_CHUNK_BITS
			};

			boxid_chunks[idx] = MSNTV2BoxIdInfo::extract_raw_boxid_bit_value(raw_box_id, boxid_bit_idx, chunk_bitcnt)?;
			
			boxid_bit_idx += chunk_bitcnt;
		}

		Ok(boxid_chunks
			.iter()
			.map(|&chunk| format!("{:04}", chunk))
			.collect())
	}

	#[allow(dead_code)]
	fn extract_bit_value(&mut self, bit_index: u8, bit_length: u8) -> Result<u32, Box<dyn std::error::Error>> {
		MSNTV2BoxIdInfo::extract_raw_boxid_bit_value(&self.raw, bit_index, bit_length)
	}

	fn extract_raw_boxid_bit_value(raw_box_id: &[u8; 8], bit_index: u8, bit_length: u8) -> Result<u32, Box<dyn std::error::Error>> {
		let mut bit_value: u32 = 0;

		for collected_bitcnt in 0..bit_length {
			let cur_bit_idx = bit_index + collected_bitcnt;
			
			let byte_idx = (cur_bit_idx / BOXID_BYTE_BITCNT) as usize;
			let byte_bit_idx = (BOXID_BYTE_BITCNT - 1) - (cur_bit_idx % BOXID_BYTE_BITCNT);

			if byte_idx >= 8 {
				let msg = format!("Bit index '{}' goes beyond the BoxID array!", cur_bit_idx);
				return Err(Box::new(Error::new(ErrorKind::UnexpectedEof, msg)));				
			}

			let bit = ((raw_box_id[byte_idx] >> byte_bit_idx) & 1) as u32;
			bit_value = (bit_value << 1) | bit;
		}

		Ok(bit_value)
	}

	fn calculate_crc(&mut self) -> Result<u8, Box<dyn std::error::Error>> {
		MSNTV2BoxIdInfo::calculate_raw_boxid_crc8(&self.raw)
	}

	fn calculate_raw_boxid_crc8(raw_box_id: &[u8; 8]) -> Result<u8, Box<dyn std::error::Error>> {
		let mut crc8_val = 0x00;
		for &boxid_byte_val in &raw_box_id[1..] {
			let crc8_rol = ((crc8_val << 0x01) | (crc8_val >> 0x07)) & 0xff;

			crc8_val = crc8_rol ^ boxid_byte_val;
		}

		Ok(crc8_val)
	}
}