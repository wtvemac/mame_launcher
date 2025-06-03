pub mod romio;
pub mod diskio;
pub mod flashdiskio;

pub trait BuildIO {
	fn file_path(&mut self) -> Result<String, Box<dyn std::error::Error>>;

	fn open(file_path: String, stripped: Option<bool>, rom_size: Option<u32>) -> Result<Option<Self>, Box<dyn std::error::Error>> where Self: Sized;

	fn create(file_path: String, stripped: Option<bool>, rom_size: Option<u32>) -> Result<Option<Self>, Box<dyn std::error::Error>> where Self: Sized;

	fn seek(&mut self, pos: u64) -> Result<u64, Box<dyn std::error::Error>>;

	fn read(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>>;

	fn write(&mut self, buf: &mut [u8]) -> Result<usize, Box<dyn std::error::Error>>;

	fn len(&mut self) -> Result<u64, Box<dyn std::error::Error>>;
}