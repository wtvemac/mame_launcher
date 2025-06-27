use packbytes::{FromBytes, ToBytes};

use super::{BuildIO, BuildIODataCollation};
use std::{
	fs::{File, OpenOptions},
	io::{Read, Write, Seek, SeekFrom},
	path::Path
};

// These values can change depending on what MDOC chip is used.
// I've only seen these values used with the MDOC chips WebTV supports 
const USR_PAGE_SIZE: u64 = 0x00000200;
const SPR_PAGE_SIZE: u64 = 0x00000010;
const PAGES_PER_UNIT: u64 = 0x00000010; // 8MB flashdisk, 16MB = 0x20
const DISKINFO_UNITS: u64 = 0x00000002;

const DISK_MAGIC: [u8; 6] = [b'A', b'N', b'A', b'N', b'D', 0x00];

const WRITTEN_MARK: i16 = 0x5555;
//const FOLDED_MARK: i16 = 0x5555;
const ERASED_MARK: i16 = 0x3c69;

const EMPTY_PAGE: u64 = 0xffffffff;

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct DiskInformation {
	pub magic: [u8; 6],
	pub total_usable_units: i16,
	pub frist_usable_unit: i16,
	pub usable_size: i32,
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct PageInformation {
	pub usr_ecc_data: [u8; 6],
	pub usr_data_status: i16
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct UnitOrderInformation {
	pub usr_virtual_unit_number: i16,
	pub usr_replace_unit_number: i16,
	pub spr_virtual_unit_number: i16,
	pub spr_replace_unit_number: i16,
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct UnitEraseInformation {
	pub wear_info: i32,
	pub usr_erase_status: i16,
	pub spr_erase_status: i16,
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct UnitFoldInformation {
	pub usr_fold_status: i16,
	pub spr_fold_status: i16,
	pub unused: i32
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(le)]
pub struct UnitBlankInformation {
	pub data: [u8; 8]
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes)]
#[packbytes(le)]
pub struct UserControlInformation {
	pub page0_info: PageInformation,
	pub order: UnitOrderInformation,

	pub page1_info: PageInformation,
	pub erase: UnitEraseInformation,

	pub page2_info: PageInformation,
	pub fold: UnitFoldInformation,
}

#[allow(dead_code)]
pub struct PageWriteInfo {
	pub page_index: usize,
	pub page_offset: usize,
	pub size: usize,
	pub data: Vec<u8>
}

#[allow(dead_code)]
pub struct FlashdiskIO {
	file_path: String,
	collation: BuildIODataCollation,
	size: u64,
	created: bool,
	file: File,
	total_usr_size: u64,
	total_spr_size: u64,
	total_units: u64,
	usr_page_offsets: Vec<u64>,
	current_page_index: usize,
	current_page_offset: usize,
	current_page_read: bool,
	current_page: Vec<u8>,
	pending_page_writes: Vec<PageWriteInfo>

}

impl FlashdiskIO {
	// Size detection can easily be done since the page sizes remain the same across all of WebTV's MDOC chips
	// If there's anything different we should be able to use the device name from MAME's XML.
	fn detect_mdoc_config(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let file_size = self.file.metadata().unwrap().len();

		// usr_size  = (spr_size / SPR_PAGE_SIZE) * USR_PAGE_SIZE
		// spr_size  = (usr_size / USR_PAGE_SIZE) * SPR_PAGE_SIZE
		// file_size = usr_size + spr_size

		let total_usr_size = ((file_size as f64) / (1.0 + ((SPR_PAGE_SIZE as f64) / (USR_PAGE_SIZE as f64)))) as u64;
		let total_spr_size = file_size - total_usr_size;

		let _ = self.set_mdoc_config(total_usr_size, total_spr_size);

		Ok(())
	}

	pub fn set_mdoc_config(&mut self, total_usr_size: u64, total_spr_size: u64) -> Result<(), Box<dyn std::error::Error>> {
		self.total_usr_size = total_usr_size;
		self.total_spr_size = total_spr_size;

		let total_pages = self.total_usr_size / USR_PAGE_SIZE;
		self.total_units = total_pages / PAGES_PER_UNIT;
		self.usr_page_offsets = vec![EMPTY_PAGE; total_pages as usize];

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
				let unit_offset = physical_unit_index * (SPR_PAGE_SIZE * PAGES_PER_UNIT);
				let _ = self.file.seek(SeekFrom::Start(self.total_usr_size + unit_offset))?;
				match UserControlInformation::read_packed(&mut self.file) {
					Ok(uci) => {
						if uci.erase.usr_erase_status == ERASED_MARK && uci.order.usr_virtual_unit_number > -1 {
							let mut page_index = 0;
							while page_index < PAGES_PER_UNIT {
								let page_offset = unit_offset + (page_index * SPR_PAGE_SIZE);
								let _ = self.file.seek(SeekFrom::Start(self.total_usr_size + page_offset))?;
								match PageInformation::read_packed(&mut self.file) {
									Ok(pi) => {
										if pi.usr_data_status == WRITTEN_MARK {
											let virtual_page_index = ((uci.order.usr_virtual_unit_number as u64 * PAGES_PER_UNIT) + page_index) as usize;
											let usr_page_offset = ((physical_unit_index * PAGES_PER_UNIT) + page_index) * USR_PAGE_SIZE;

											if virtual_page_index < self.usr_page_offsets.iter().len() {
												self.usr_page_offsets[virtual_page_index] = usr_page_offset;
											}
										}
									},
									_ => {
										//
									}
								};
								page_index += 1;
							}
							if uci.order.usr_replace_unit_number != -1 {
								physical_unit_index = uci.order.usr_replace_unit_number as u64;
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
			created: false,
			file: OpenOptions::new().read(true).write(true).open(file_path.clone())?,
			total_usr_size: 0,
			total_spr_size: 0,
			total_units: 0,
			usr_page_offsets: vec![],
			current_page_index: 0,
			current_page_offset: 0,
			current_page_read: false,
			current_page: vec![0xff; USR_PAGE_SIZE as usize],
			pending_page_writes: vec![]
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
			created: true,
			file: OpenOptions::new().read(true).write(true).create(true).open(file_path.clone())?,
			total_usr_size: 0,
			total_spr_size: 0,
			total_units: 0,
			usr_page_offsets: vec![],
			current_page_index: 0,
			current_page_offset: 0,
			current_page_read: false,
			current_page: vec![0xff; USR_PAGE_SIZE as usize],
			pending_page_writes: vec![]
		};

		let _ = io.set_mdoc_config(size, (size / USR_PAGE_SIZE) * SPR_PAGE_SIZE);

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		self.current_page_index = (pos / USR_PAGE_SIZE) as usize;

		if self.current_page_index < self.usr_page_offsets.iter().len() {
			self.current_page_offset = (pos % USR_PAGE_SIZE) as usize;
			self.current_page_read = false;

			Ok(pos)
		} else {
			self.current_page_index = 0;
			self.current_page_offset = 0;
			self.current_page_read = false;

			Ok(0)
		}
	}

	fn stream_position(&mut self) -> Result<u64, Box<dyn std::error::Error>>  {
		Ok((self.current_page_index as u64 * USR_PAGE_SIZE) + self.current_page_offset as u64)
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
				if !self.current_page_read && self.current_page_index < self.usr_page_offsets.len() {
					let page_offset = self.usr_page_offsets[self.current_page_index];
					if page_offset == EMPTY_PAGE {
						self.current_page.fill(0xff);
					} else {
						self.file.seek(SeekFrom::Start(page_offset))?;

						match self.file.read(&mut self.current_page) {
							Ok(_) => {
								self.current_page_read = true;
							},
							Err(e) => {
								return Err(Box::new(e));
							}
						};
					}
				}
	
				let mut current_read_size = need_total_size - read_total_size;
				if current_read_size > (USR_PAGE_SIZE as usize - self.current_page_offset) {
					current_read_size = USR_PAGE_SIZE as usize - self.current_page_offset;
					self.current_page_read = false;
					self.current_page_index += 1;

					if self.current_page_index >= self.usr_page_offsets.len() {
						self.current_page_index = 0;
					}
				}

				buf[current_buf_index as usize..(current_buf_index + current_read_size) as usize]
					.copy_from_slice(&self.current_page[self.current_page_offset as usize..(self.current_page_offset + current_read_size) as usize]);

				current_buf_index += current_read_size;
				read_total_size += current_read_size;

				if self.current_page_read {
					self.current_page_offset += current_read_size;
				} else {
					self.current_page_offset = 0;
				}
			}

			let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

			Ok(read_total_size as usize)
		}
	}

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if buf.len() < 0x4 {
			return Err("Buffer length needs to be 4 bytes or greater.".into());
		} else if (buf.len() & 1) == 1 {
			return Err("Buffer length needs to be a multiple of 2.".into());
		} else {
			let need_total_size = buf.len();
			let mut write_total_size = 0;
			let mut current_buf_index = 0;

			while write_total_size < need_total_size {
				let mut current_write_size = need_total_size - write_total_size;
				let write_page_index = self.current_page_index;
				let write_page_offset = self.current_page_offset;

				if current_write_size > (USR_PAGE_SIZE as usize - self.current_page_offset) {
					current_write_size = USR_PAGE_SIZE as usize - self.current_page_offset;
					self.current_page_index += 1;
					self.current_page_offset = 0;
				} else {
					self.current_page_offset += current_write_size;
				}

				self.pending_page_writes.push(PageWriteInfo {
					page_index: write_page_index,
					page_offset: write_page_offset,
					size: current_write_size,
					data: buf[current_buf_index as usize..(current_buf_index + current_write_size) as usize].to_vec()
				});

				current_buf_index += current_write_size;
				write_total_size += current_write_size;
			}

			Ok(write_total_size)
		}

		//Ok(self.file.write(buf)?)
	}

	// We read in the entire disk, recreate the spare table then write the disk back to a file (saving a backup)
	// This is less complex than trying to write to disk normally since we don't need to keep track of replace units, wear leveling etc...
	// and is possible since this isn't real hardware where keeping track of those things matter.
	fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		if self.pending_page_writes.len() > 0 {
			let mut usr_data = vec![0xff as u8; self.total_usr_size as usize];
			let mut spr_data = vec![0xff as u8; self.total_spr_size as usize];

			if usr_data.len() > 0 {
				let usr_start_index = (DISKINFO_UNITS * (USR_PAGE_SIZE * PAGES_PER_UNIT)) as usize;

				let _ = self.seek(0);
				let _ = self.read(&mut usr_data[usr_start_index..]);

				for pri in self.pending_page_writes.iter() {
					let usr_index = usr_start_index + (pri.page_index * USR_PAGE_SIZE as usize) + pri.page_offset;

					usr_data[usr_index..(usr_index + pri.size)]
						.copy_from_slice(&pri.data[0..pri.size]);
				}

				self.pending_page_writes.clear();

				for header_index in 0..DISKINFO_UNITS {
					let usr_index = (header_index * (USR_PAGE_SIZE * PAGES_PER_UNIT)) as usize;

					let disk_information = DiskInformation {
						magic: DISK_MAGIC,
						total_usable_units: (self.total_usr_size / (USR_PAGE_SIZE * PAGES_PER_UNIT)) as i16,
						frist_usable_unit: 0,
						usable_size: (self.total_usr_size as usize - usr_start_index) as i32
					}.to_le_bytes();

					usr_data[usr_index..(usr_index + disk_information.len()) as usize]
						.copy_from_slice(&disk_information);
				}

				if spr_data.len() > 0 {
					let mut unit_index = 0;
					let mut usable_unit_index = 0;
					let mut page_index = 0;
					let mut unit_spr_index = 0;
					let mut page_spr_index = 0;
					let mut unit_written_to = false;
					for start_index in (0..usr_data.len()).step_by(USR_PAGE_SIZE as usize) {
						let end_index = (start_index + USR_PAGE_SIZE as usize).min(usr_data.len());

						let page_written_to = match usr_data[start_index..end_index].iter().find(|&&b| b != 0xff) {
							Some(_) => true,
							_ => false
						};

						if page_written_to {
							unit_written_to = true;

							let page_info = PageInformation {
								usr_ecc_data: [0x00; 6],
								usr_data_status: WRITTEN_MARK
							}.to_le_bytes();


							spr_data[page_spr_index as usize..(page_spr_index + (SPR_PAGE_SIZE / 2)) as usize]
								.copy_from_slice(&page_info);
						}

						page_spr_index += SPR_PAGE_SIZE;

						page_index += 1;
						if page_index == PAGES_PER_UNIT {
							if unit_index >= DISKINFO_UNITS {
								if unit_written_to {
									spr_data[(unit_spr_index + (SPR_PAGE_SIZE / 2)) as usize..(unit_spr_index + SPR_PAGE_SIZE) as usize]
									.copy_from_slice(&UnitOrderInformation {
										usr_virtual_unit_number: usable_unit_index as i16,
										usr_replace_unit_number: -1,
										spr_virtual_unit_number: usable_unit_index as i16,
										spr_replace_unit_number: -1
									}.to_le_bytes());
								}

								usable_unit_index += 1;
							}

							unit_spr_index += SPR_PAGE_SIZE;

							// All units should be at least erased even if nothing was written to them.

							spr_data[(unit_spr_index + (SPR_PAGE_SIZE / 2)) as usize..(unit_spr_index + SPR_PAGE_SIZE) as usize]
							.copy_from_slice(&UnitEraseInformation {
								wear_info: 1,
								usr_erase_status: ERASED_MARK,
								spr_erase_status: ERASED_MARK
							}.to_le_bytes());

							unit_index += 1;
							page_index = 0;
							unit_spr_index = page_spr_index;
							unit_written_to = false;
						}
					}
				}

				// Create a backup
				if Path::new(&self.file_path).exists() {
					let _ = std::fs::copy(&self.file_path, self.file_path.clone() + ".bak");
				}

				match File::create(&self.file_path) {
					Ok(mut dstf) => {
						let _ = dstf.seek(SeekFrom::Start(0));
						let _ = dstf.write(&usr_data);
						let _ = dstf.write(&spr_data);
					},
					_ => {
						//
					}
				};
			}
		}

		Ok(())
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.total_usr_size)
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(self.collation)
	}
}
