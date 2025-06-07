use super::{BuildIO, BuildIODataCollation};
use std::fs::File;
use std::io::{Read, Write, Seek, SeekFrom};

#[allow(dead_code)]
pub struct FlashdiskIO {
	file_path: String,
	collation: BuildIODataCollation,
	rom_size: u64,
	f0: Option<File>,
	f1: Option<File>
}

impl BuildIO for FlashdiskIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>> {
		Ok(self.file_path.clone())
	}

	fn open(file_path: String, collation: Option<BuildIODataCollation>) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = FlashdiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			rom_size: 0,
			f0: None,
			f1: None
		};

		if io.collation == BuildIODataCollation::StrippedROMs {
			io.f0 = Some(File::open(file_path.clone() + "0")?);
			io.f1 = Some(File::open(file_path.clone() + "1")?);
		} else {
			io.f0 = Some(File::open(file_path.clone())?);
		}

		Ok(Box::new(io))
	}

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: u64) -> Result<Box<dyn BuildIO>, Box<dyn std::error::Error>> {
		let mut io = FlashdiskIO {
			file_path: file_path.clone(),
			collation: collation.unwrap_or(BuildIODataCollation::Raw),
			rom_size: size,
			f0: None,
			f1: None
		};

		if io.collation == BuildIODataCollation::StrippedROMs {
			io.f0 = Some(File::create(file_path.clone() + "0")?);
			io.f1 = Some(File::create(file_path.clone() + "1")?);
		} else {
			io.f0 = Some(File::create(file_path.clone())?);
		}

		Ok(Box::new(io))
	}

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>  {
		if self.collation == BuildIODataCollation::StrippedROMs {
			//let wanted_pos: i64 = pos.into();

			let f0_seek = self.f0.as_ref().unwrap().seek(SeekFrom::Start(pos / 2))?;
			let f1_seek = self.f1.as_ref().unwrap().seek(SeekFrom::Start(pos / 2))?;

			if f0_seek == f1_seek {
				Ok(f0_seek * 2)
			} else {
				Ok(0)
			}
		} else {
			Ok(self.f0.as_ref().unwrap().seek(SeekFrom::Start(pos))?)
		}
	}

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if self.collation == BuildIODataCollation::StrippedROMs {
			if buf.len() < 0x4 {
				panic!("Buffer needs to be greater than 4 bytes.");
			} else if (buf.len() % 2) == 1 {
				panic!("Buffer needs to be a multiple of 2.");
			} else {
				let mut rsize: usize = 0x0;

				for index in (0..buf.len()).step_by(4) {
					rsize += self.f0.as_ref().unwrap().read(&mut buf[(index + 0)..(index + 2)])?;

					// Stop reading if the buffer is a miltiple of 2 but not a multiple of 4. For example, like reading into a 62 byte buffer.
					if (index + 4) <= buf.len() {
						rsize += self.f1.as_ref().unwrap().read(&mut buf[(index + 2)..(index + 4)])?;
					}
				}

				Ok(rsize)
			}
		} else {
			Ok(self.f0.as_ref().unwrap().read(buf)?)
		}
	}

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>> {
		if self.collation == BuildIODataCollation::StrippedROMs {
			if buf.len() < 0x4 {
				panic!("Buffer needs to be greater than 4 bytes.");
			} else if (buf.len() % 2) == 1 {
				panic!("Buffer needs to be a multiple of 2.");
			} else {
				let mut rsize: usize = 0x0;

				for index in (0..buf.len()).step_by(4) {
					unsafe {
						let buf1: [u8; 2] = [*buf.get_unchecked_mut(index + 0), *buf.get_unchecked_mut(index + 1)];

						rsize += self.f0.as_ref().unwrap().write(&buf1)?;

						// Write null padding if the buffer is a miltiple of 2 but not a multiple of 4. For example, like writing from a 62 byte buffer.
						if (index + 4) <= buf.len() {
							let buf2: [u8; 2] = [*buf.get_unchecked_mut(index + 2), *buf.get_unchecked_mut(index + 3)];

							rsize += self.f1.as_ref().unwrap().write(&buf2)?;
						} else {
							let null_padding: [u8; 2] = [0x00; 0x02];
							rsize += self.f1.as_ref().unwrap().write(&null_padding)?;
						}
					}
				}

				Ok(rsize)
			}
		} else {
			Ok(self.f0.as_ref().unwrap().write(buf)?)
		}
	}

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>> {
		if self.collation == BuildIODataCollation::StrippedROMs {
			Ok(self.f0.as_ref().unwrap().metadata().unwrap().len() * 2)
		} else {
			Ok(self.f0.as_ref().unwrap().metadata().unwrap().len())
		}
	}

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>> {
		Ok(self.collation)
	}
}