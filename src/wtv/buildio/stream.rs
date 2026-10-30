use super::{BuildIO, BuildIODataCollation};
use std::io::{self, Read, Write, Seek, SeekFrom};

fn to_io_error(e: Box<dyn std::error::Error>) -> io::Error {
	io::Error::other(e.to_string())
}

pub struct BuildIOStream<'a> {
	io: &'a mut dyn BuildIO,
	alignment: u64,
	base: u64,
	len: u64,
	pos: u64
}
impl<'a> BuildIOStream<'a> {
	pub fn new(io: &'a mut dyn BuildIO) -> io::Result<BuildIOStream<'a>> {
		let len = io.len().map_err(to_io_error)?;

		Ok(BuildIOStream::slice(io, 0, len))
	}

	pub fn slice(io: &'a mut dyn BuildIO, base: u64, len: u64) -> BuildIOStream<'a> {
		let alignment = match io.collation().unwrap_or(BuildIODataCollation::Raw) {
			BuildIODataCollation::Raw => 1,
			_ => 4
		};

		BuildIOStream {
			io: io,
			alignment: alignment,
			base: base,
			len: len,
			pos: 0
		}
	}

	fn aligned_range(&self, abs: u64, n: usize) -> (u64, u64) {
		let mask = self.alignment - 1;

		(abs & !mask, (abs + n as u64 + mask) & !mask)
	}

	fn read_exact_at(&mut self, pos: u64, buf: &mut [u8]) -> io::Result<()> {
		self.io.seek(pos).map_err(to_io_error)?;

		match self.io.read(buf).map_err(to_io_error)? {
			n if n == buf.len() => Ok(()),
			_ => Err(io::ErrorKind::UnexpectedEof.into())
		}
	}
}

impl Read for BuildIOStream<'_> {
	fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
		let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
		if n == 0 {
			return Ok(0);
		}

		let abs = self.base + self.pos;
		let (start, end) = self.aligned_range(abs, n);

		if start == abs && end == abs + n as u64 {
			self.read_exact_at(abs, &mut buf[..n])?;
		} else {
			let mut data = vec![0x00; (end - start) as usize];
			self.read_exact_at(start, &mut data)?;

			let offset = (abs - start) as usize;
			buf[..n].copy_from_slice(&data[offset..(offset + n)]);
		}

		self.pos += n as u64;

		Ok(n)
	}
}
impl Write for BuildIOStream<'_> {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
		if n == 0 {
			return Ok(0);
		}

		let abs = self.base + self.pos;
		let (start, end) = self.aligned_range(abs, n);

		let mut data;
		if start == abs && end == abs + n as u64 {
			data = buf[..n].to_vec();
		} else {
			data = vec![0x00; (end - start) as usize];
			self.read_exact_at(start, &mut data)?;

			let offset = (abs - start) as usize;
			data[offset..(offset + n)].copy_from_slice(&buf[..n]);
		}

		self.io.seek(start).map_err(to_io_error)?;
		if self.io.write(&mut data).map_err(to_io_error)? != data.len() {
			return Err(io::ErrorKind::WriteZero.into());
		}

		self.pos += n as u64;

		Ok(n)
	}

	fn flush(&mut self) -> io::Result<()> {
		self.io.commit().map_err(to_io_error)
	}
}

impl Seek for BuildIOStream<'_> {
	fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
		let new_pos = match pos {
			SeekFrom::Start(p) => p as i64,
			SeekFrom::Current(d) => self.pos as i64 + d,
			SeekFrom::End(d) => self.len as i64 + d
		};

		if new_pos < 0 {
			return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek before start."));
		}

		self.pos = new_pos as u64;

		Ok(self.pos)
	}
}
