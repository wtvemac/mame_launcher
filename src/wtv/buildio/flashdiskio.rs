use packbytes::FromBytes;

use super::{BuildIO, BuildIODataCollation};
use std::fs::File;
use std::io::{Read, Write, Seek, SeekFrom};

// These values can change depending on what MDOC chip is used.
// I've only seen these values used with the MDOC chips WebTV supports 
const USER_PAGE_SIZE: u64 = 0x00000200;
const SPARE_PAGE_SIZE: u64 = 0x00000010;
const PAGES_PER_UNIT: u64 = 0x00000020;

const WRITTEN_MARK: i16 = 0x5555;
//const FOLDED_MARK: i16 = 0x5555;
const ERASED_MARK: i16 = 0x3c69;

const EMPTY_PAGE: u64 = 0xffffffff;

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes)]
#[packbytes(le)]
pub struct UserControlInformation {
	pub page0_info: u64,
	pub usr_virtual_unit_number: i16,
	pub usr_replace_unit_number: i16,
	pub spr_virtual_unit_number: i16,
	pub spr_replace_unit_number: i16,

	pub page1_info: u64,
	pub wear_info: i32,
	pub usr_erase_status: i16,
	pub spr_erase_status: i16,

	pub page2_info: u64,
	pub usr_fold_status: i16,
	pub spr_fold_status: i16
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes)]
#[packbytes(le)]
pub struct PageInformation {
	pub user_ecc_data: [u8; 6],
	pub user_data_status: i16
}

#[allow(dead_code)]
pub struct FlashdiskIO {
	file_path: String,
	collation: BuildIODataCollation,
	size: u64,
	file: File,
	total_user_size: u64,
	total_spare_size: u64,
	total_units: u64,
	user_page_offsets: Vec<u64>,
	current_page_index: usize,
	current_page_offset: usize,
	current_page_read: bool,
	current_page: Vec<u8>

}

impl FlashdiskIO {
	// Size detection can easily be done since the page sizes remain the same across all of WebTV's MDOC chips
	// If there's anything different we should be able to use the device name from MAME's XML.
	fn detect_mdoc_config(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let file_size = self.file.metadata().unwrap().len();

		// user_size  = ( spare_size / spare_page_size ) * user_page_size
		// spare_size = ( user_size  / user_page_size  ) * spare_page_size
		// file_size  = user_size + spare_size

		let total_user_size = ((file_size as f64) / (1.0 + ((SPARE_PAGE_SIZE as f64) / (USER_PAGE_SIZE as f64)))) as u64;
		let total_spare_size = file_size - total_user_size;

		let _ = self.set_mdoc_config(total_user_size, total_spare_size);

		Ok(())
	}

	pub fn set_mdoc_config(&mut self, total_user_size: u64, total_spare_size: u64) -> Result<(), Box<dyn std::error::Error>> {
		self.total_user_size = total_user_size;
		self.total_spare_size = total_spare_size;

		let total_pages = self.total_user_size / USER_PAGE_SIZE;
		self.total_units = total_pages / PAGES_PER_UNIT;
		self.user_page_offsets = vec![EMPTY_PAGE; total_pages as usize];

		Ok(())
	}

	/*
	 *
	 * The M-Systems DiskOnChip splits the user's data into chunks called units and each unit has
	 * x number of pages. Each page has an entry in a spare data table. MAME creates a file with 
	 * the user data at the top and spare data appended to the bottom.
	 *
	 * The spare data is basically a table used by the wear leveling algorithm.
	 * Each page normaly has 16 bytes of spare data. The first 8 bytes stores ECC and status
	 * information for each page. The next 8 bytes for the first 3 pages in a unit are used for 
	 * NFTL data. This NFTL data is called the "Unit Control Information"
	 * 
	 * Unit Control Information 0 (first page):
	 * 
	 *     The user and spare state are separated but in practice the spare and user data share 
	 *     the same state so the status is just repeated.
	 * 
	 *     struct {
	 *         uint16_t unknown00; // Virtual address of this unit (user)
	 *         uint16_t unknown01; // If this isn't 0xffff then this indicates another unit that has pages that replace data in this unit.
	 *         uint16_t unknown02; // Repeat of unknown00
	 *         uint16_t unknown03; // Repeat of unknown01
	 *     }
	 * 
	 * Unit Control Information 1 (second page):
	 *     struct {
	 *         uint32_t unknown00; // The number of times this unit was erased (wear level)
	 *         uint16_t unknown01; // Usually "0x3c69" (erase mark) if this unit was erased at least once
	 *         uint16_t unknown02; // Usually "0x3c69" (erase mark) if this unit was erased at least once
	 *     }
	 * 
	 * Unit Control Information 2 (thid page):
	 * 
	 *     If data is being merged (folded or consolidated) into this unit then unknown00
	 *     and unknown01 is marked with 0x5555, otherwise it's 0xffff. This is part of the 
	 *     NFTL garbage collector.
	 * 
	 *     struct {
	 *         uint16_t unknown00;
	 *         uint16_t unknown01;
	 *         uint32_t unused;
	 *     }
	 * 
	 * The table contains how many times each unit has been erased, if data is currently written
	 * to it, where in the sequence the unit is at and other state information. If one byte needs
	 * to be updated in a page, you'd normally need to do a full unit erase then re-write each page.
	 * To reduce the amount of erases needed, we look for a unit that's fresh and re-write the page
	 * with the byte you want to update then mark both the previous unit (unit A) and updated unit (unit B)
	 * so we use data can be re-constructed. The pages that were updated in unit B are merged into unit A
	 * to form the data read from the OS. Sometimess two units are physically merged into each
	 * other on the flash in a "fold" operation if we need optimize for space. If a unit is erased
	 * the max amount of times then it's marked bad and that unit is ignored.
	 * 
	 * As you'd expect, data can become out-of-order with this algorithm. There's a translation
	 * layer ("NAND Flash Translation Layer") so the data is merged and appares in sequence 
	 * correctly to the OS even though it's out of order on the NAND flash.
	 *
	 * enumerate_pages reads in the spare area of the M-Systems DiskOnChip file and does the translation
	 * so page indexes point to the correct data.
	 * 
	 */

	fn enumerate_pages(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let mut logical_unit_index = 0;
		while logical_unit_index < self.total_units {
			let mut physical_unit_index = logical_unit_index;


			let mut chain_index = 0;
			while chain_index < self.total_units {
				let unit_offset = physical_unit_index * (SPARE_PAGE_SIZE * PAGES_PER_UNIT);
				let _ = self.file.seek(SeekFrom::Start(self.total_user_size + unit_offset))?;
				match UserControlInformation::read_packed(&mut self.file) {
					Ok(uci) => {
						if uci.usr_erase_status == ERASED_MARK && uci.usr_virtual_unit_number > -1 {
							let mut page_index = 0;
							while page_index < PAGES_PER_UNIT {
								let page_offset = unit_offset + (page_index * SPARE_PAGE_SIZE);
								let _ = self.file.seek(SeekFrom::Start(self.total_user_size + page_offset))?;
								match PageInformation::read_packed(&mut self.file) {
									Ok(pi) => {
										if pi.user_data_status == WRITTEN_MARK {
											let virtual_page_index = ((uci.usr_virtual_unit_number as u64 * PAGES_PER_UNIT) + page_index) as usize;
											let user_page_offset = ((physical_unit_index * PAGES_PER_UNIT) + page_index) * USER_PAGE_SIZE;

											if virtual_page_index < self.user_page_offsets.iter().len() {
												self.user_page_offsets[virtual_page_index] = user_page_offset;
											}
										}
									},
									_ => {
										//
									}
								};
								page_index += 1;
							}
							if uci.usr_replace_unit_number != -1 {
								physical_unit_index = uci.usr_replace_unit_number as u64;
							} else {
								break;
							}
						} else {
							break;
						}
					}
					_ => {
						//
					}
				};
				chain_index += 1;
			}
			logical_unit_index += 1;
		}

		Ok(())
	}
}
impl BuildIO for FlashdiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = FlashdiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: 0,
			file: File::open(file_path.clone())?,
			total_user_size: 0,
			total_spare_size: 0,
			total_units: 0,
			user_page_offsets: vec![],
			current_page_index: 0,
			current_page_offset: 0,
			current_page_read: false,
			current_page: vec![0xff; USER_PAGE_SIZE as usize]
		};

		io.size = io.file.metadata().unwrap().len();

		let _= io.detect_mdoc_config();

		let _ = io.enumerate_pages();

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = FlashdiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: size,
			file: File::create(file_path.clone())?,
			total_user_size: 0,
			total_spare_size: 0,
			total_units: 0,
			user_page_offsets: vec![],
			current_page_index: 0,
			current_page_offset: 0,
			current_page_read: false,
			current_page: vec![0xff; USER_PAGE_SIZE as usize]
		};

		let _= io.detect_mdoc_config();

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		self.current_page_index = (pos / USER_PAGE_SIZE) as usize;

		if self.current_page_index < self.user_page_offsets.iter().len() {
			self.current_page_offset = (pos % USER_PAGE_SIZE) as usize;
			self.current_page_read = false;

			Ok(pos)
		} else {
			self.current_page_index = 0;
			self.current_page_offset = 0;
			self.current_page_read = false;

			Ok(0)
		}
	}

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if buf.len() < 0x4 {
			return Err("Buffer length needs to be 4 bytes or greater.".into());
		} else if (buf.len() & 1) == 1 {
			return Err("Buffer length needs to be a multiple of 2.".into());
		} else {
			let need_total_size = buf.len();
			let mut read_total_size = 0;
			let mut current_buf_index = 0;

			while read_total_size < need_total_size {
				if !self.current_page_read && self.current_page_index < self.user_page_offsets.iter().len(){
					let page_offset = self.user_page_offsets[self.current_page_index];
					if page_offset == EMPTY_PAGE {
						self.current_page.fill(0xff);
					} else {
						self.file.seek(SeekFrom::Start(page_offset))?;

						match self.file.read(&mut self.current_page) {
							Err(e) => {
								return Err(Box::new(e));
							}
							_ => {
								//
							}
						};
					}
				}
	
				let mut current_read_size = need_total_size - read_total_size;
				if current_read_size > (USER_PAGE_SIZE as usize - self.current_page_offset) {
					current_read_size = USER_PAGE_SIZE as usize - self.current_page_offset;
					self.current_page_read = false;
					self.current_page_index += 1;

					if self.current_page_index < self.user_page_offsets.iter().len() {
						self.current_page_index = 0;
					}
				}

				buf[current_buf_index as usize..(current_buf_index + current_read_size) as usize]
					.copy_from_slice(&self.current_page[self.current_page_offset as usize..(self.current_page_offset + current_read_size) as usize]);

				current_buf_index += current_read_size;
				read_total_size += current_read_size;
				self.current_page_offset += current_read_size;

				if !self.current_page_read {
					self.current_page_offset = 0;
				}
			}

			let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

			Ok(read_total_size as usize)
		}
	}

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		Ok(self.file.write(buf)?)
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.total_user_size)
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(self.collation)
	}
}
