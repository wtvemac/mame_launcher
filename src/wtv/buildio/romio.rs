use super::{BuildIO, BuildIODataCollation};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write, Seek, SeekFrom};

#[allow(dead_code)]
pub struct ROMIO {
	file_path: String,
	collation: BuildIODataCollation,
	size: u64,
	created: bool,
	f0: File,
	f1: Option<File>
}

impl BuildIO for ROMIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io;

		if collation.unwrap_or(BuildIODataCollation::Raw) == BuildIODataCollation::StrippedROMs {
			io = ROMIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: 0,
				created: false,
				f0: OpenOptions::new().read(true).write(true).open(file_path.clone() + "0")?,
				f1: Some(OpenOptions::new().read(true).write(true).open(file_path.clone() + "1")?)
			};

			io.size = io.f0.metadata().unwrap().len() * 2;
		} else {
			io = ROMIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: 0,
				created: false,
				f0: OpenOptions::new().read(true).write(true).open(file_path.clone())?,
				f1: None
			};

			io.size = io.f0.metadata().unwrap().len();
		}

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let io;

		if collation.unwrap_or(BuildIODataCollation::Raw) == BuildIODataCollation::StrippedROMs {
			io = ROMIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: size,
				created: true,
				f0: OpenOptions::new().read(true).write(true).create(true).open(file_path.clone() + "0")?,
				f1: Some(OpenOptions::new().read(true).write(true).create(true).open(file_path.clone() + "1")?)
			};
		} else {
			io = ROMIO {
				file_path: file_path.clone(),
				collation: collation.unwrap_or(BuildIODataCollation::Raw),
				size: size,
				created: true,
				f0: OpenOptions::new().read(true).write(true).create(true).open(file_path.clone())?,
				f1: None
			};
		}

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		if self.collation == BuildIODataCollation::StrippedROMs {
			//let wanted_pos: i64 = pos.into();

			let f0_seek = self.f0.seek(SeekFrom::Start(pos / 2))?;
			let f1_seek = self.f1.as_ref().unwrap().seek(SeekFrom::Start(pos / 2))?;

			if f0_seek == f1_seek {
				Ok(f0_seek * 2)
			} else {
				Ok(0)
			}
		} else {
			Ok(self.f0.seek(SeekFrom::Start(pos))?)
		}
	}

	fn stream_position(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		Ok(self.f0.stream_position()?)
	}

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		let corrected_size = buf.len().min((self.len().unwrap_or(0) - self.stream_position().unwrap_or(0)) as usize);

		if self.collation == BuildIODataCollation::StrippedROMs {
			if buf.len() < 0x4 {
				Err("Buffer needs to be greater than 4 bytes.".into())
			} else if (buf.len() & 1) == 1 {
				Err("Buffer needs to be a multiple of 2.".into())
			} else {
				let mut rsize: usize = 0x0;

				if self.collation == BuildIODataCollation::StrippedROMs {
					for index in (0..corrected_size).step_by(4) {
						rsize += self.f0.read(&mut buf[(index + 0)..(index + 2)])?;

						// Stop reading if the buffer is a miltiple of 2 but not a multiple of 4. For example, like reading into a 62 byte buffer.
						if (index + 4) <= corrected_size {
							rsize += self.f1.as_ref().unwrap().read(&mut buf[(index + 2)..(index + 4)])?;
						}
					}
				} else {
					rsize += self.f0.read(buf)?;
				}

				Ok(rsize)
			}
		} else {
			Ok(self.f0.read(&mut buf[0..corrected_size])?)
		}
	}

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		let corrected_size = buf.len().min((self.len().unwrap_or(0) - self.stream_position().unwrap_or(0)) as usize);

		if self.collation == BuildIODataCollation::StrippedROMs {
			if buf.len() < 0x4 {
				Err("Buffer needs to be greater than 4 bytes.".into())
			} else if (buf.len() & 1) == 1 {
				Err("Buffer needs to be a multiple of 2.".into())
			} else {
				let mut rsize: usize = 0x0;

				let mut buf0 = vec![0x00 as u8; corrected_size/2];
				let mut buf1 = vec![0x00 as u8; corrected_size/2];

				let mut stripped_bufindex = 0;
				for whole_bufindex in (0..corrected_size).step_by(4) {
					buf0[stripped_bufindex + 0] = buf[whole_bufindex + 0];
					buf0[stripped_bufindex + 1] = buf[whole_bufindex + 1];

					buf1[stripped_bufindex + 0] = buf[whole_bufindex + 2];
					buf1[stripped_bufindex + 1] = buf[whole_bufindex + 3];
					
					stripped_bufindex += 2;
				}

				rsize += self.f0.write(&buf0)?;
				rsize += self.f1.as_ref().unwrap().write(&buf1)?;

				Ok(rsize)
			}
		} else {
			Ok(self.f0.write(&buf[0..corrected_size])?)
		}
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