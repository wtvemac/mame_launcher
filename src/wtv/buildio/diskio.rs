use super::{BuildIO, BuildIODataCollation};
use std::{
	fs::{File, OpenOptions},
	path::Path
};
use std::io::{Read, Write, Seek, SeekFrom};
use chd::Chd;
use regex::Regex;

#[allow(dead_code)]
struct CompressedHunkDiskIO {
	file_path: String,
	collation: BuildIODataCollation,
	size: u64,
	created: bool,
	chd: Box<Chd<File>>,
	current_hunk_index: u32,
	current_hunk_offset: usize,
	current_hunk_read: bool,
	current_hunk: Vec<u8>
}
impl CompressedHunkDiskIO {
	fn read_hunk(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let mut hunk = self.chd.hunk(self.current_hunk_index)?;
				
		let mut temp_buf = Vec::new();

		hunk.read_hunk_in(&mut temp_buf, &mut self.current_hunk)?;

		self.current_hunk_read = true;

		Ok(())
	}
}
impl BuildIO for CompressedHunkDiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let path = Path::new(&file_path);

		let diff_file_path = match path.parent() {
			Some(parent) => {
				match parent.to_str() {
					Some(parent_str) => {
						let file_path_stem = match path.file_stem() {
							Some(file_stem) => {
								file_stem.to_str().unwrap_or("".into())
							},
							_ => {
								//panic!("Couldn't get file stem from file path.");
								"".into()
							}
						};
	
						parent_str.to_owned() + "/../../diff/" + file_path_stem + ".dif"
					},
					_ => {
						//panic!("Couldn't get parent directory string from file path");
						"".into()
					}
				}
			},
			_ => {
				//panic!("Couldn't find parent directory from file path.");
				"".into()
			}
		};
		
		let mut io: CompressedHunkDiskIO;
		if Path::new(&diff_file_path).exists() {
			io = CompressedHunkDiskIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: 0,
				created: false,
				chd: 
					Box::new(
						Chd::open(
							File::open(diff_file_path.clone())?, 
					Some(
								Box::new(
									Chd::open(
										File::open(file_path.clone())?, 
										None
									)?
								)
							)
						)?
					),
				current_hunk_index: 0,
				current_hunk_offset: 0,
				current_hunk_read: false,
				current_hunk: vec![]
			};
		} else {
			io = CompressedHunkDiskIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: 0,
				created: false,
				chd:
					Box::new(
						Chd::open(
							File::open(file_path.clone())?, 
							None
						)?
					),
				current_hunk_index: 0,
				current_hunk_offset: 0,
				current_hunk_read: false,
				current_hunk: vec![]
			};
		}

		io.size = io.chd.header().logical_bytes();
		io.current_hunk = io.chd.get_hunksized_buffer();

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = CompressedHunkDiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			size: size,
			created: true,
			chd: Box::new(Chd::open(File::open(file_path.clone())?, None)?),
			current_hunk_index: 0,
			current_hunk_offset: 0,
			current_hunk_read: false,
			current_hunk: vec![]
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

	fn write(&mut self, _buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		//let _ = BuildIODataCollation::convert_raw_data(buf, self.collation);

		Ok(0)
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