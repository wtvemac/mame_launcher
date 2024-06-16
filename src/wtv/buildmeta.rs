use packbytes::FromBytes;

use super::buildio::BuildIO;

#[allow(dead_code)]
pub struct BuildMeta {
	pub build_path: String,
	pub stripped: bool,
	pub rom_size: u32,
	pub file: BuildIO,
	pub build_info: BuildInfo,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BuildInfo {
	pub build_header: BuildHeader,
	pub romfs_header: ROMFSHeader,
	pub romfs_offset: u32,
	pub calculated_code_checksum: u32,
	pub calculated_romfs_checksum: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, FromBytes)]
#[packbytes(be)]
pub struct BuildHeader {
	pub branch_and_delay_instructions: u64,
	pub code_checksum: u32,
	pub build_dword_length: u32,
	pub code_dword_length: u32,
	pub build_version: u32,
	pub data_section_address: u32,
	pub data_section_length: u32,
	pub bss_section_length: u32,
	pub romfs_address: u32,
	pub lzj_data_version: u32,
	pub lzj_data_length: u32,

	// These will be incorrect for builds that don't include these. We'd run into instructions or a blank area for those builds.
	pub build_base_address: u32,
	pub build_flags: u32,
	pub data_section_compressed_length: u32,
	pub compressed_bootrom_address: u32
}

#[allow(dead_code)]
#[derive(Debug, Clone, FromBytes)]
#[packbytes(be)]
pub struct ROMFSHeader {
	pub romfs_dword_length: u32,
	pub romfs_checksum: u32,
}


impl BuildMeta {
	pub fn new(build_path: String, stripped: Option<bool>, rom_size: Option<u32>) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		let build_stripped = stripped.unwrap_or(false);
		let build_rom_size = rom_size.unwrap_or(0x000000);

		let mut wtv_buildmeta = BuildMeta {
			build_path: build_path.clone(),
			stripped: build_stripped,
			rom_size: build_rom_size,
			file: BuildIO::open(build_path.clone(), stripped, rom_size)?,
			build_info: BuildInfo {
				build_header: BuildHeader::from_bytes([0x00; 0x40]),
				romfs_header: ROMFSHeader::from_bytes([0x00; 0x08]),
				romfs_offset: 0x00,
				calculated_code_checksum: 0x00000000,
				calculated_romfs_checksum: 0x00000000
			}
		};

		wtv_buildmeta.build_info.build_header = wtv_buildmeta.get_build_header().unwrap_or(wtv_buildmeta.build_info.build_header);
		wtv_buildmeta.build_info.calculated_code_checksum = wtv_buildmeta.calculate_dword_checksum(0, wtv_buildmeta.build_info.build_header.code_dword_length, Some(0x02)).unwrap_or(0);
	
		// Classic bootrom or approm
		if wtv_buildmeta.build_info.build_header.branch_and_delay_instructions == 0x1000000900000000 || wtv_buildmeta.build_info.build_header.branch_and_delay_instructions == 0x1000000E00000000 || wtv_buildmeta.build_info.build_header.branch_and_delay_instructions == 0x1000000F00000000 {
			// Classic bfe or bf0 bootrom
			if wtv_buildmeta.build_info.build_header.romfs_address == 0x9fe00000 {
				wtv_buildmeta.build_info.build_header.build_base_address = 0x9fc00000;
			// Classic bfe approm
			} else if wtv_buildmeta.build_info.build_header.romfs_address > 0x9fe00000 {
				wtv_buildmeta.build_info.build_header.build_base_address = 0x9fe00000;
			// Classic bf0 approm
			} else {
				wtv_buildmeta.build_info.build_header.build_base_address = 0x9f000000;
			}
		// Classic bfe bootrom
		} else if wtv_buildmeta.build_info.build_header.branch_and_delay_instructions == 0x1000011600000000 {
			wtv_buildmeta.build_info.build_header.build_base_address = 0x9fc00000;
		}

		if wtv_buildmeta.build_info.build_header.romfs_address != 0x4e6f4653 { // != NoFS
			wtv_buildmeta.build_info.romfs_offset = wtv_buildmeta.build_info.build_header.romfs_address.wrapping_sub(wtv_buildmeta.build_info.build_header.build_base_address);
			wtv_buildmeta.build_info.romfs_header = wtv_buildmeta.get_romfs_header().unwrap_or(wtv_buildmeta.build_info.romfs_header);
			let romfs_end_offset = wtv_buildmeta.build_info.romfs_offset.wrapping_sub(wtv_buildmeta.build_info.romfs_header.romfs_dword_length * 0x04).wrapping_sub(0x08);
			let data_length;
			match wtv_buildmeta.file.len() {
				Ok(len) => {
					data_length = len as u32;
				},
				_ => {
					data_length = 0x00;
				}
			}
			if romfs_end_offset >= data_length {
				wtv_buildmeta.build_info.calculated_romfs_checksum = wtv_buildmeta.calculate_dword_checksum(romfs_end_offset, wtv_buildmeta.build_info.romfs_header.romfs_dword_length, None).unwrap_or(0);
			}
		}

		Ok(wtv_buildmeta)
	}

	pub fn get_build_header(&mut self) -> Result<BuildHeader, Box<dyn std::error::Error>>  {
		let _ = self.file.seek(0x00)?;

		let mut build_header = [0x00; 0x40];
		let _ = self.file.read(&mut build_header).unwrap_or(0);

		Ok(BuildHeader::from_bytes(build_header))
	}

	pub fn get_romfs_header(&mut self) -> Result<ROMFSHeader, Box<dyn std::error::Error>>  {
		let mut romfs_header = [0x00; 0x08];

		if self.build_info.build_header.romfs_address != 0x4e6f4653 { // != NoFS
			let romfs_header_offset = self.build_info.romfs_offset.wrapping_sub(0x08);
			let data_length;
			match self.file.len() {
				Ok(len) => {
					data_length = len as u32;
				},
				_ => {
					data_length = 0;
				}
			}
			if romfs_header_offset >= data_length {
				let _ = self.file.seek(romfs_header_offset.into())?;

				let _ = self.file.read(&mut romfs_header).unwrap_or(0);
			}
		}

		Ok(ROMFSHeader::from_bytes(romfs_header))
	}

	pub fn calculate_dword_checksum(&mut self, start: u32, length: u32, skip: Option<u32>) -> Result<u32, Box<dyn std::error::Error>> {
		let mut checksum: u32 = 0x00;

		let _ = self.file.seek(start.into())?;

		if length <= 0x2000000 { // If we're trying to checksum data larger than 32MB then something bad's probably happened.
			for dword_index in 0..length {
				let mut code_chunk = [0x00; 0x04];
				let _ = self.file.read(&mut code_chunk)?;

				if skip == None || dword_index != skip.unwrap_or(0) {
					checksum = checksum.wrapping_add(u32::from_be_bytes(code_chunk));
				}
			}
		}

		Ok(checksum)
	}
}