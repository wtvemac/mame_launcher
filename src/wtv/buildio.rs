pub mod romio;
pub mod diskio;
pub mod flashdiskio;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BuildIODataCollation {
	// Used for data that hasn't been changed in any special way.
	Raw,
	// All bootroms and flash approms with 2 chips.
	StrippedROMs,
	// 16-bit little endien
	// All HD boxes prior to the UltimateTV. [1234=>2143]
	ByteSwapped16,
	// 16-bit little endien step with 32-bit little endien step
	// Data on the UltimateTV's HD (possibly other WinCE-based HD boxes). [1234=>3412]
	ByteSwapped1632
}
impl BuildIODataCollation {
	fn convert_raw_data(buf: &mut [u8], collation: BuildIODataCollation) -> Result<(), Box<dyn std::error::Error>> {
		if collation == BuildIODataCollation::ByteSwapped16 {
			buf.chunks_exact_mut(2).for_each(|chunk| {
				chunk.swap(0, 1);
			});
		} else if collation == BuildIODataCollation::ByteSwapped1632 {
			buf.chunks_exact_mut(4).for_each(|chunk| {
				chunk.swap(0, 2);
				chunk.swap(1, 3);
			});
		}

		Ok(())
	}
}

pub trait BuildIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>>;

	fn open(file_path: String, collation: Option<BuildIODataCollation>, size: Option<u32>) -> Result<Option<Self>, Box<dyn std::error::Error>> where Self: Sized;

	fn create(file_path: String, collation: Option<BuildIODataCollation>, size: Option<u32>) -> Result<Option<Self>, Box<dyn std::error::Error>> where Self: Sized;

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>;

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>>;

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>>;

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>>;

	fn collation(&mut self) -> Result<BuildIODataCollation, Box<dyn std::error::Error>>;
}