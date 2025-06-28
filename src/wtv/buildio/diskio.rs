use packbytes::{FromBytes, ToBytes};
use super::{BuildIO, BuildIODataCollation};
use std::{
	fs::{File, OpenOptions},
	path::Path
};
use std::io::{Read, Write, Seek, SeekFrom};
use chd::Chd;
use regex::Regex;

const CHD_HEADER_SIZE: u32 = 0x0000007c;
const CHD_METADATA_SIZE: u32 = 0x00000010;
const CHD_HEADER_VERSION: u32 = 5;
const CHD_METADATA_CHUNK_ID: u32 = 0x47444444; // GDDD
const CHD_METADATA_SECS: u32 = 0x0000003f;
const CHD_METADATA_HEADS: u32 = 0x00000010;

const CHD_MAGIC: [u8; 8] = [b'M', b'C', b'o', b'm', b'p', b'r', b'H', b'D'];

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(be)]
pub struct Sha1Hash {
	hash: [u8; 20]
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(be)]
pub struct CHDHeaderV5 {
	pub magic: [u8; 8],
	pub header_size: u32,
	pub header_version: u32,
	pub compressor: [u32; 4],
	pub uncompressed_size: u64,
	pub hunk_map_offset: u64,
	pub disk_metadata_offset: u64,
	pub hunk_size_bytes: u32,
	pub sector_size_bytes: u32,
	pub sha1: [Sha1Hash; 3],
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(be)]
pub struct DataU24 {
	ms: u8,
	ls: u16
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes, ToBytes)]
#[packbytes(be)]
pub struct CHDChunkMetadata {
	pub chunk_id: u32,
	pub flags: u8,
	pub size: DataU24,
	pub next_offset: u64,
}

#[allow(dead_code)]
pub struct HunkWriteInfo {
	pub hunk_index: usize,
	pub hunk_offset: usize,
	pub size: usize,
	pub data: Vec<u8>
}

#[allow(dead_code)]
pub struct CompressedHunkDiskIO {
	file_path: String,
	diff_path: String,
	collation: BuildIODataCollation,
	size: u64,
	created: bool,
	chd: Box<Chd<File>>,
	current_hunk_index: u32,
	current_hunk_offset: usize,
	current_hunk_read: bool,
	current_hunk: Vec<u8>,
	pending_hunk_writes: Vec<HunkWriteInfo>
}
impl CompressedHunkDiskIO {
	fn read_hunk(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let mut hunk = self.chd.hunk(self.current_hunk_index)?;
				
		hunk.read_hunk_in(&mut Vec::new(), &mut self.current_hunk)?;

		self.current_hunk_read = true;

		Ok(())
	}

	pub fn find_diff_file(chd_file_path: String) -> Result<String, Box<dyn std::error::Error>> {
		let path = Path::new(&chd_file_path);

		let chd_parent = match path.parent() {
			Some(parent) => parent.to_str().unwrap_or("".into()),
			_ => "".into()
		};

		if chd_parent != "" {
			let chd_stem = match path.file_stem() {
				Some(stem) => stem.to_str().unwrap_or("".into()),
				_ => "".into()
			};

			// Only using diff file if this is a CHD for a WebTV preset file inside MAME
			// Checking if the chd file is in the /roms/XXX/ folder to detect MAME
			if Path::new(&(chd_parent.to_owned() + "/../../roms")).exists() {
				// We return the path to where the diff file would exist.
				return Ok(chd_parent.to_owned() + "/../../diff/" + chd_stem + ".dif");
			}
		}

		Ok("".into())
	}
}
impl BuildIO for CompressedHunkDiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let diff_file_path = CompressedHunkDiskIO::find_diff_file(file_path.clone()).unwrap_or("".into());

		let chd;
		if diff_file_path != "" && Path::new(&diff_file_path).exists() {
			chd = Box::new(Chd::open(
					File::open(diff_file_path.clone())?, 
			Some(Box::new(Chd::open(
						File::open(file_path.clone())?, 
						None
					)?))
				)?);
		} else {
			chd = Box::new(Chd::open(
					File::open(file_path.clone())?, 
					None
				)?);
		}

		let mut io = CompressedHunkDiskIO {
			file_path: file_path.clone(),
			diff_path: diff_file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: 0,
			created: false,
			chd: chd,
			current_hunk_index: 0,
			current_hunk_offset: 0,
			current_hunk_read: false,
			current_hunk: vec![],
			pending_hunk_writes: vec![]
		};

		io.size = io.chd.header().logical_bytes();
		io.current_hunk = io.chd.get_hunksized_buffer();

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = CompressedHunkDiskIO {
			file_path: file_path.clone(),
			diff_path: CompressedHunkDiskIO::find_diff_file(file_path.clone()).unwrap_or("".into()).clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: size,
			created: true,
			chd: Box::new(Chd::open(File::open(file_path.clone())?, None)?),
			current_hunk_index: 0,
			current_hunk_offset: 0,
			current_hunk_read: false,
			current_hunk: vec![],
			pending_hunk_writes: vec![]
		};

		io.current_hunk = io.chd.get_hunksized_buffer();

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		let hunk_size = self.chd.header().hunk_size() as u64;

		self.current_hunk_index = (pos / hunk_size) as u32;

		if self.current_hunk_index > self.chd.header().hunk_count().into() {
			self.current_hunk_index = 0;
			self.current_hunk_offset = 0;
			self.current_hunk_read = false;

			Ok(0)
		} else {
			self.current_hunk_offset = pos as usize % hunk_size as usize;
			self.current_hunk_read = false;

			Ok(pos)
		}
	}

	fn stream_position(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		let hunk_size = self.chd.header().hunk_size() as u64;

		Ok((self.current_hunk_index as u64 * hunk_size) + self.current_hunk_offset as u64)
	}

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if buf.len() < 0x4 {
			Err("Buffer length needs to be 4 bytes or greater.".into())
		} else if (buf.len() & 1) == 1 {
			Err("Buffer length needs to be a multiple of 2.".into())
		} else {
			let hunk_size: usize = self.chd.header().hunk_size() as usize;
			let need_total_size = buf.len();
			let mut read_total_size = 0;
			let mut current_buf_index = 0;

			while read_total_size < need_total_size {
				if !self.current_hunk_read {
					match self.read_hunk() {
						Err(e) => {
							return Err(e);
						}
						_ => {
							//
						}
					};
				}
	
				let mut current_read_size = need_total_size - read_total_size;
				if current_read_size > (hunk_size - self.current_hunk_offset) {
					current_read_size = hunk_size - self.current_hunk_offset;
					self.current_hunk_read = false;
					self.current_hunk_index += 1;

					if self.current_hunk_index > self.chd.header().hunk_count().into() {
						self.current_hunk_index = 0;
					}
				}

				buf[current_buf_index as usize..(current_buf_index + current_read_size) as usize]
					.copy_from_slice(&self.current_hunk[self.current_hunk_offset as usize..(self.current_hunk_offset + current_read_size) as usize]);

				current_buf_index += current_read_size;
				read_total_size += current_read_size;
				self.current_hunk_offset += current_read_size;

				if !self.current_hunk_read {
					self.current_hunk_offset = 0;
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
			let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

			let hunk_size = self.chd.header().hunk_size() as usize;

			let need_total_size = buf.len();
			let mut write_total_size = 0;
			let mut current_buf_index = 0;

			while write_total_size < need_total_size {
				let mut current_write_size = need_total_size - write_total_size;
				let write_hunk_index = self.current_hunk_index as usize;
				let write_hunk_offset = self.current_hunk_offset as usize;

				if current_write_size > (hunk_size - self.current_hunk_offset) {
					current_write_size = hunk_size - self.current_hunk_offset;
					self.current_hunk_index += 1;
					self.current_hunk_offset = 0;
				} else {
					self.current_hunk_offset += current_write_size;
				}

				self.pending_hunk_writes.push(HunkWriteInfo {
					hunk_index: write_hunk_index,
					hunk_offset: write_hunk_offset,
					size: current_write_size,
					data: buf[current_buf_index as usize..(current_buf_index + current_write_size) as usize].to_vec()
				});

				current_buf_index += current_write_size;
				write_total_size += current_write_size;
			}

			Ok(write_total_size)
		}
	}

	fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		if self.pending_hunk_writes.len() > 0 {
			let hunk_size = self.chd.header().hunk_size() as usize;
			let hunk_count = self.chd.header().hunk_count() as usize;

			let mut hunk_map = vec![0x00000000 as u32; hunk_count];

			let metadata = "CYLS:".to_owned() + &(self.len().unwrap_or(0) / (CHD_METADATA_HEADS * CHD_METADATA_SECS * self.chd.header().unit_bytes()) as u64).to_string() + ","
				+ "HEADS:" + &CHD_METADATA_HEADS.to_string() + ","
				+ "SECS:"  + &CHD_METADATA_SECS.to_string() + ","
				+ "BPS:"   + &self.chd.header().unit_bytes().to_string();

			// The hunk data starts after the header, metadata and hunk map table.
			// This is the header size + metadata size + size of the hunk map for all hunks. Then align up so it's divisible by the hunk size.
			let metadata_offset = (CHD_HEADER_SIZE as usize) + (hunk_count * 4);
			let metadata_end_offset = metadata_offset + CHD_METADATA_SIZE as usize + metadata.len() + 1;
			let hunk_data_offset = (metadata_end_offset + (hunk_size - 1)) & !(hunk_size - 1);
			// The hunks are mapped to a hunk index that starts at the top of the file. So we figure out how many hunk-sized entries we skip before we get to the actual hunk data.
			let start_file_hunk_index = hunk_data_offset / hunk_size;

			// Create a backup
			let has_diff = Path::new(&self.diff_path).exists();
			if has_diff {
				let _ = std::fs::copy(&self.diff_path, self.diff_path.clone() + ".bak");
			}

			let mut current_file_hunk_index = start_file_hunk_index;
			match File::create(&self.diff_path) {
				Ok(mut dstf) => {
					let _ = dstf.seek(SeekFrom::Start(0));

					let parent_sha1 = match self.chd.header().has_parent() {
						true => self.chd.header().parent_sha1().unwrap_or([0x00; 20]),
						false => self.chd.header().sha1().unwrap_or([0x00; 20]),
					};

					let _ = dstf.write(&CHDHeaderV5 {
						magic: CHD_MAGIC,
						header_size: CHD_HEADER_SIZE,
						header_version: CHD_HEADER_VERSION,
						compressor: [0; 4],
						uncompressed_size: self.len().unwrap_or(0),
						hunk_map_offset: CHD_HEADER_SIZE as u64,
						disk_metadata_offset: metadata_offset as u64,
						hunk_size_bytes: hunk_size as u32,
						sector_size_bytes: self.chd.header().unit_bytes(),
						sha1: [
							Sha1Hash { hash: [0x00; 20] },
							Sha1Hash { hash: [0x00; 20] },
							Sha1Hash { hash: parent_sha1 }
						],
					}.to_be_bytes());

					let mut hunks = vec![];
					for hwi in self.pending_hunk_writes.iter() {
						let mut current_hunk = self.chd.get_hunksized_buffer();

						if hwi.hunk_offset > 0 || hwi.size < hunk_size {
							let mut hunk = self.chd.hunk(hwi.hunk_index as u32)?;

							hunk.read_hunk_in(&mut Vec::new(), &mut current_hunk)?;
						}

						current_hunk[hwi.hunk_offset as usize..(hwi.hunk_offset + hwi.size) as usize]
							.copy_from_slice(&hwi.data);

						hunk_map[hwi.hunk_index] = current_file_hunk_index as u32;

						current_file_hunk_index += 1;

						hunks.push(current_hunk);
					}

					let mut missing_hunk_offsets = vec![];
					// Write back hunks that weren't changed from the old diff.
					if has_diff {
						match File::open(&(self.diff_path.clone() + ".bak")) {
							Ok(mut old_diff) => {
								let _ = old_diff.seek(SeekFrom::Start(0))?;
								match CHDHeaderV5::read_packed(&mut old_diff) {
									Ok(old_dif_header) => {
										if old_dif_header.header_version == CHD_HEADER_VERSION {
											// Read in the hunk offsets from the original diff file that need to be moved.

											for hunk_map_index in 0..hunk_map.len() {
												// Only move hunks that don't have data in the new diff file
												if hunk_map[hunk_map_index] == 0x00000000 {
													let mut file_hunk_index_buff = [0x00 as u8; 4];
													let _ = old_diff.seek(SeekFrom::Start(old_dif_header.hunk_map_offset + (hunk_map_index * 4) as u64));
													let _ = old_diff.read(&mut file_hunk_index_buff);
													let file_hunk_index = u32::from_be_bytes(file_hunk_index_buff) as u64;

													// If the old diff has data then move it.
													if file_hunk_index != 0x00000000 {
														hunk_map[hunk_map_index] = current_file_hunk_index as u32;
														current_file_hunk_index += 1;

														missing_hunk_offsets.push(file_hunk_index * hunk_size as u64);
													}
												}
											}
										}
									},
									_ => {
										//
									}
								};
							},
							_ => {
								//
							}
						}
					}

					let mut hunk_map_block = vec![];
					for hunk_map_entry in hunk_map.iter() {
						hunk_map_block.extend_from_slice(&hunk_map_entry.to_be_bytes());
					}
					let _ = dstf.write(&hunk_map_block);

					let _ = dstf.write(&CHDChunkMetadata {
						chunk_id: CHD_METADATA_CHUNK_ID,
						flags: 1,
						size: DataU24 { ms: 0, ls: metadata.len() as u16 + 1 },
						next_offset: 0,
					}.to_be_bytes());

					let _ = dstf.write(&mut metadata.as_bytes());
					let _ = dstf.write(&[0x00; 1]);

					// Padding so the first hunk is correctly aligned.
					let _ = dstf.write(&vec![0x00 as u8; hunk_data_offset - metadata_end_offset]);

					for hunk in hunks.iter() {
						let _ = dstf.write(&hunk);
					}

					// Move the hunks from the old to the new diff file.

					if has_diff && missing_hunk_offsets.len() > 0 {
						match File::open(&(self.diff_path.clone() + ".bak")) {
							Ok(mut old_diff) => {
								for hunk_offset in missing_hunk_offsets.iter() {
									let mut missing_hunk = self.chd.get_hunksized_buffer();
									let _ = old_diff.seek(SeekFrom::Start(*hunk_offset))?;
									let _ = old_diff.read(&mut missing_hunk);

									let _ = dstf.write(&missing_hunk);
								}
							}
							_ => {
								//
							}
						}
					}
				},
				_ => {
					//
				}
			};

			self.pending_hunk_writes.clear();
		}

		Ok(())
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.size)
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(self.collation)
	}
}

#[allow(dead_code)]
struct RawDiskIO {
	file_path: String,
	collation: BuildIODataCollation,
	size: u64,
	created: bool,
	file: File,
}
impl BuildIO for RawDiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = RawDiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: 0,
			created: false,
			file: OpenOptions::new().read(true).write(true).open(file_path.clone())?
		};

		io.size = io.file.metadata().unwrap().len();

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let io = RawDiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: size,
			created: true,
			file: OpenOptions::new().read(true).write(true).create(true).open(file_path.clone())?,
		};

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		Ok(self.file.seek(SeekFrom::Start(pos))?)
	}

	fn stream_position(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.file.stream_position()?)
	}

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if buf.len() < 0x4 {
			Err("Buffer length needs to be 4 bytes or greater.".into())
		} else if (buf.len() & 1) == 1 {
			Err("Buffer length needs to be a multiple of 2.".into())
		} else {
			let result = self.file.read(buf)?;

			let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

			Ok(result)
		}
	}

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

		Ok(self.file.write(buf)?)
	}

	fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		Ok(())
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.size)
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(self.collation)
	}
}

#[allow(dead_code)]
pub struct DiskIO;
impl BuildIO for DiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok("".into())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		if Regex::new(r"\.(chd|dif)$")?.is_match(file_path.as_str()) {
			CompressedHunkDiskIO::open(file_path.clone(), collation)
		} else {
			RawDiskIO::open(file_path.clone(), collation)
		}
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		if Regex::new(r"\.(chd|dif)$")?.is_match(file_path.as_str()) {
			CompressedHunkDiskIO::create(file_path.clone(), collation, size)
		} else {
			RawDiskIO::create(file_path.clone(), collation, size)
		}
	}

	fn seek(&mut self, _pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		Ok(0)
	}

	fn stream_position(&mut self) -> Result<u64, Box<dyn std::error::Error>>  {
		Ok(0)
	}

	fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		Ok(0)
	}

	fn write(&mut self, _buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		Ok(0)
	}

	fn commit(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		Ok(())
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(0)
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(BuildIODataCollation::Raw)
	}
}