use packbytes::FromBytes;

use super::buildio::{
	BuildIO,
	BuildIODataCollation,
	romio::ROMIO,
	diskio::DiskIO,
	flashdiskio::FlashdiskIO
};

const RAW_LAYOUT_CHECK_OFFSET: u64 = 0x00000000;
const RAW_LAYOUT_CHECK_MASK: u32 = 0xffffff00;
const RAW_LAYOUT_CHECK_VALUE: u32 = 0x10000000;
const RAW_BUILD_OFFSET0: u64 = 0x00000000;

const PARTITION_TABLE_MAGIC: u32 = 0x74696d6e; // timn
const PARTITION_TABLE_MAGIC_OFFSET: u64 = 0x00000008;

const LC2_PARTITION_TABLE_OFFSET: u64 = 0x014c1000;
const LC2_BUILD_SELECT_OFFSET: u64 = 0x01080600;
const LC2_BUILD_OFFSET0: u64 = 0x00080600;
const LC2_BUILD_OFFSET1: u64 = 0x00880600;

const WEBSTART_PART_COUNT_CHECK_OFFSET: u64 = 0x00000004;
const WEBSTART_PART_TYPE_CHECK_OFFSET: u64 = 0x00000068;
const WEBSTART_PART_TYPE_CHECK_VALUE: u32 = 0x00000004;
const WEBSTAR_BUILD_OFFSET0: u64 = 0x00080600;

const UTV_PARTITION_TABLE_OFFSET: u64 = 0x178c1000;
const UTV_BUILD_SELECT_OFFSET: u64 = 0x17480600;
const UTV_BUILD_OFFSET0: u64 = 0x13480600;
const UTV_BUILD_OFFSET1: u64 = 0x15480600;

const NO_ROMFS_FLAG: u32 = 0x4e6f4653; // NoFS

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BuildMetaLayout {
	UnknownLayout,
	RawLayout,
	FlashdiskLayout,
	LC2DiskLayout,
	WebstarDiskLayout,
	UTVDiskLayout
}

#[allow(dead_code)]
pub struct BuildMeta {
	pub file_path: String,
	pub collation: BuildIODataCollation,
	pub layout: BuildMetaLayout,
	pub build_count: u8,
	pub selected_build_index: u8,
	pub build_info: [BuildInfo; 2],
	io: Box<dyn BuildIO>
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone)]
pub struct BuildInfo {
	pub available: bool,
	pub build_header: BuildHeader,
	pub romfs_header: ROMFSHeader,
	pub build_offset: u64,
	pub romfs_offset: u64,
	pub calculated_code_checksum: u32,
	pub calculated_romfs_checksum: u32,
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes)]
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
#[derive(Debug, Copy, Clone, FromBytes)]
#[packbytes(be)]
pub struct ROMFSHeader {
	pub romfs_dword_length: u32,
	pub romfs_checksum: u32,
}

impl BuildMeta {
	pub fn open_rom(file_path: String, collation: Option<BuildIODataCollation>) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match ROMIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn open_disk(file_path: String, collation: Option<BuildIODataCollation>) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match DiskIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn open_flashdisk(file_path: String, collation: Option<BuildIODataCollation>) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match FlashdiskIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn new(build_io: Box<dyn BuildIO>) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		let mut wtv_buildmeta = BuildMeta::default_buildmeta(build_io);

		wtv_buildmeta.file_path = wtv_buildmeta.io.file_path().unwrap_or("".into()).clone();
		wtv_buildmeta.collation = wtv_buildmeta.io.collation().unwrap_or(BuildIODataCollation::Raw);
		wtv_buildmeta.layout = wtv_buildmeta.get_layout().unwrap_or(BuildMetaLayout::UnknownLayout);

		let _ = wtv_buildmeta.load_buildinfo();

		Ok(wtv_buildmeta)
	}

	fn default_buildmeta(build_io: Box<dyn BuildIO>) -> BuildMeta {
		BuildMeta {
			file_path: "".into(),
			collation: BuildIODataCollation::Raw,
			layout: BuildMetaLayout::UnknownLayout,
			build_count: 0,
			selected_build_index: 0,
			build_info: [BuildMeta::default_buildinfo(); 2],
			io: build_io,
		}
	}

	fn default_buildinfo() -> BuildInfo {
		BuildInfo {
			available: false,
			build_header: BuildMeta::default_build_header(),
			romfs_header: BuildMeta::default_romfs_header(),
			build_offset: 0x00,
			romfs_offset: 0x00,
			calculated_code_checksum: 0x00000000,
			calculated_romfs_checksum: 0x00000000
		}
	}

	fn default_build_header() -> BuildHeader {
		BuildHeader::from_bytes([0x00; 0x40])
	}

	fn default_romfs_header() -> ROMFSHeader {
		ROMFSHeader::from_bytes([0x00; 0x08])
	}

	fn get_layout(&mut self) -> Result<BuildMetaLayout, Box<dyn std::error::Error>> {
		let file_size = self.io.len().unwrap_or(0);

		if file_size > UTV_PARTITION_TABLE_OFFSET {
			let _ = self.io.seek(UTV_PARTITION_TABLE_OFFSET + PARTITION_TABLE_MAGIC_OFFSET)?;
			let mut partition_table_check = [0x00; 0x04];
			let _ = self.io.read(&mut partition_table_check).unwrap_or(0);

			if u32::from_be_bytes(partition_table_check) == PARTITION_TABLE_MAGIC {
				return Ok(BuildMetaLayout::UTVDiskLayout);
			}
		}

		if file_size > LC2_PARTITION_TABLE_OFFSET {

			let _ = self.io.seek(LC2_PARTITION_TABLE_OFFSET + PARTITION_TABLE_MAGIC_OFFSET)?;
			let mut partition_table_check = [0x00; 0x04];
			let _ = self.io.read(&mut partition_table_check).unwrap_or(0);

			if u32::from_be_bytes(partition_table_check) == PARTITION_TABLE_MAGIC {

				let _ = self.io.seek(LC2_PARTITION_TABLE_OFFSET + WEBSTART_PART_COUNT_CHECK_OFFSET)?;
				let mut partition_count_check = [0x00; 0x04];
				let _ = self.io.read(&mut partition_count_check).unwrap_or(0);

				if u32::from_be_bytes(partition_count_check) >= 2 {

					let _ = self.io.seek(LC2_PARTITION_TABLE_OFFSET + WEBSTART_PART_TYPE_CHECK_OFFSET)?;
					let mut partition_type_check = [0x00; 0x04];
					let _ = self.io.read(&mut partition_type_check).unwrap_or(0);

					if u32::from_be_bytes(partition_type_check) == WEBSTART_PART_TYPE_CHECK_VALUE {
						return Ok(BuildMetaLayout::WebstarDiskLayout);
					}
				}
	
				return Ok(BuildMetaLayout::LC2DiskLayout);
			}
		}

		let _ = self.io.seek(RAW_LAYOUT_CHECK_OFFSET)?;
		let mut raw_check_data = [0x00; 0x04];
		let _ = self.io.read(&mut raw_check_data).unwrap_or(0);

		if u32::from_be_bytes(raw_check_data) & RAW_LAYOUT_CHECK_MASK == RAW_LAYOUT_CHECK_VALUE {
			return Ok(BuildMetaLayout::RawLayout);
		}

		Ok(BuildMetaLayout::UnknownLayout)
	}

	fn load_buildinfo(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		if self.layout == BuildMetaLayout::LC2DiskLayout {
			self.build_count = 2;
			self.selected_build_index = self.get_selected_build_index().unwrap_or(1);
			self.build_info[0] = self.get_buildinfo(LC2_BUILD_OFFSET0).unwrap_or(BuildMeta::default_buildinfo());
			self.build_info[1] = self.get_buildinfo(LC2_BUILD_OFFSET1).unwrap_or(BuildMeta::default_buildinfo());
		} else if self.layout == BuildMetaLayout::WebstarDiskLayout {
			self.build_count = 1;
			self.selected_build_index = 0;
			self.build_info[0] = self.get_buildinfo(WEBSTAR_BUILD_OFFSET0).unwrap_or(BuildMeta::default_buildinfo());
		} else if self.layout == BuildMetaLayout::UTVDiskLayout {
			self.build_count = 2;
			self.selected_build_index = self.get_selected_build_index().unwrap_or(1);
			self.build_info[0] = self.get_buildinfo(UTV_BUILD_OFFSET0).unwrap_or(BuildMeta::default_buildinfo());
			self.build_info[1] = self.get_buildinfo(UTV_BUILD_OFFSET1).unwrap_or(BuildMeta::default_buildinfo());
		} else {
			self.build_count = 1;
			self.selected_build_index = 0;
			self.build_info[0] = self.get_buildinfo(RAW_BUILD_OFFSET0).unwrap_or(BuildMeta::default_buildinfo());
		}

		Ok(())
	}

	fn get_selected_build_index(&mut self) -> Result<u8, Box<dyn std::error::Error>> {
		if self.layout == BuildMetaLayout::LC2DiskLayout {
			let _ = self.io.seek(LC2_BUILD_SELECT_OFFSET)?;
			let mut partition_count_check = [0x00; 0x04];
			let _ = self.io.read(&mut partition_count_check).unwrap_or(1);
			if partition_count_check[0] == 0x00 {
				return Ok(0);
			} else {
				return Ok(1);
			}
		} else if self.layout == BuildMetaLayout::UTVDiskLayout {
			let _ = self.io.seek(UTV_BUILD_SELECT_OFFSET)?;
			let mut partition_count_check = [0x00; 0x04];
			let _ = self.io.read(&mut partition_count_check).unwrap_or(1);
			if partition_count_check[0] == 0x00 {
				return Ok(0);
			} else {
				return Ok(1);
			}
		} else {
			return Ok(0);
		}
	}

	fn get_buildinfo(&mut self, build_offset: u64) -> Result<BuildInfo, Box<dyn std::error::Error>> {
		let mut buildinfo = BuildMeta::default_buildinfo();

		buildinfo.build_header = self.get_build_header(build_offset).unwrap_or(BuildMeta::default_build_header());

		buildinfo.calculated_code_checksum = self.calculate_dword_checksum(build_offset, buildinfo.build_header.code_dword_length, Some(0x02)).unwrap_or(0);

		if self.layout == BuildMetaLayout::RawLayout {
			let valid_classic_bd_instructions: [u64; 3] = [
				0x1000000900000000,
				0x1000000E00000000,
				0x1000000F00000000
			];
			if valid_classic_bd_instructions.contains(&buildinfo.build_header.branch_and_delay_instructions) {
				// Classic bfe or bf0 bootrom
				if buildinfo.build_header.romfs_address == 0x9fe00000 {
					buildinfo.build_header.build_base_address = 0x9fc00000;
				// Classic bfe approm
				} else if buildinfo.build_header.romfs_address > 0x9fe00000 {
					buildinfo.build_header.build_base_address = 0x9fe00000;
				// Classic bf0 approm
				} else {
					buildinfo.build_header.build_base_address = 0x9f000000;
				}
			// Classic bfe bootrom
			} else if buildinfo.build_header.branch_and_delay_instructions == 0x1000011600000000 {
				buildinfo.build_header.build_base_address = 0x9fc00000;
			}
		}

		if buildinfo.build_header.romfs_address != NO_ROMFS_FLAG { // != NoFS
			buildinfo.romfs_offset = buildinfo.build_header.romfs_address.wrapping_sub(buildinfo.build_header.build_base_address) as u64;
			buildinfo.romfs_header = self.get_romfs_header(build_offset, buildinfo.romfs_offset).unwrap_or(buildinfo.romfs_header);

			let romfs_dword_length = buildinfo.romfs_header.romfs_dword_length.wrapping_mul(0x04) as u64;
			let romfs_end_offset = buildinfo.romfs_offset.wrapping_sub(romfs_dword_length).wrapping_sub(0x08);
			let data_length = match self.io.len() {
				Ok(len) => len,
				_ => 0
			};
			let abs_romfs_end_offset = build_offset.wrapping_add(romfs_end_offset);

			if romfs_dword_length > 0 && abs_romfs_end_offset <= data_length {
				buildinfo.calculated_romfs_checksum = self.calculate_dword_checksum(build_offset + romfs_end_offset, buildinfo.romfs_header.romfs_dword_length, None).unwrap_or(0);
			}
		}

		Ok(buildinfo)
	}

	fn get_build_header(&mut self, build_offset: u64) -> Result<BuildHeader, Box<dyn std::error::Error>> {
		let _ = self.io.seek(build_offset)?;

		let mut build_header = [0x00; 0x40];
		let _ = self.io.read(&mut build_header).unwrap_or(0);

		Ok(BuildHeader::from_bytes(build_header))
	}

	fn get_romfs_header(&mut self, build_offset: u64, romfs_offset: u64) -> Result<ROMFSHeader, Box<dyn std::error::Error>>  {
		let mut romfs_header = [0x00; 0x08];

		if self.build_info[0].build_header.romfs_address != 0x4e6f4653 { // != NoFS
			let romfs_header_offset = romfs_offset.wrapping_sub(0x08);
			let abs_romfs_header_offset = build_offset.wrapping_add(romfs_header_offset);
			let data_length = match self.io.len() {
				Ok(len) => len,
				_ => 0
			};

			if abs_romfs_header_offset <= data_length {
				let _ = self.io.seek(abs_romfs_header_offset.into())?;

				let _ = self.io.read(&mut romfs_header).unwrap_or(0);
			}
		}

		Ok(ROMFSHeader::from_bytes(romfs_header))
	}

	fn calculate_dword_checksum(&mut self, start: u64, length: u32, skip: Option<u32>) -> Result<u32, Box<dyn std::error::Error>> {
		let mut checksum: u32 = 0x00;

		let _ = self.io.seek(start)?;

		if length <= 0x4000000 { // If we're trying to checksum data larger than 64MB then something bad's probably happened.
			for dword_index in 0..length {
				let mut code_chunk = [0x00; 0x04];
				let _ = self.io.read(&mut code_chunk)?;

				if skip == None || dword_index != skip.unwrap_or(0) {
					checksum = checksum.wrapping_add(u32::from_be_bytes(code_chunk));
				}
			}
		}

		Ok(checksum)
	}
}