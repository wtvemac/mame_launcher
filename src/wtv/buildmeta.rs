use std::io::{Read, Seek, SeekFrom};
use std::fs;
use std::path::{Path, PathBuf};

use packbytes::FromBytes;

use super::buildio::{
	BuildIO,
	BuildIODataCollation,
	romio::ROMIO,
	diskio::DiskIO,
	flashdiskio::FlashdiskIO,
	stream::BuildIOStream
};

use regex::Regex;

use mbrman::{
	MBR,
	MBRPartitionEntry
};
use fatfs::{
	Dir
};

use md5::{
	Md5,
	Digest
};

use rsa::{
	RsaPublicKey,
	pkcs8::DecodePublicKey
};
use sha1::{
	Sha1
};

const WEBTV_BLOCK_SIZE: u64 = 0x00000200;

const RAW_LAYOUT_CHECK_OFFSET: u64 = 0x00000000;
const RAW_LAYOUT_CHECK_MASK: u32 = 0xfffff000;
const RAW_LAYOUT_CHECK_VALUE: u32 = 0x10000000;
const RAW_BUILD_OFFSET0: u64 = 0x00000000;

const PARTITION_TABLE_MAGIC: u32 = 0x74696d6e; // timn
const PARTITION_TABLE_MAGIC_OFFSET: u64 = 0x00000008;

const FLASHDISK_ADMININFO_OFFSET: u64 = 0x00000600;
const FLASHDISK_NVRAM_SIZE_CHECK_VALUE: u32 = 0x00004000;
const FLASHDISK_BUILD_HEADER_OFFSET: u64 = 0x00000800;
const FLASHDISK_DEFAULT_BUILD_OFFSET: u64 = 0x00020800;
const FLASHDISK_DEFAULT_BLOCK0: [u8; WEBTV_BLOCK_SIZE as usize] = [
	0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x01,
    0x01, 0x00, 0x04, 0x0F, 0xC2, 0xE8, 0x02, 0x00, 0x00, 0x00, 0x1E, 0x7D, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA 
];
const FLASHDISK_DEFAULT_BLOCK2: [u8; WEBTV_BLOCK_SIZE as usize] = [
    0xE9, 0x54, 0x76, 0x57, 0x65, 0x62, 0x54, 0x56, 0x00, 0x00, 0x00, 0x00, 0x02, 0x04, 0x01, 0x00,
    0x01, 0xF0, 0x00, 0x1E, 0x7D, 0xF8, 0x20, 0x00, 0x02, 0x00, 0x10, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x1E, 0x7D, 0x00, 0x00, 0x80, 0x00, 0x29, 0x00, 0x00, 0x00, 0x00, 0x20, 0x20, 0x20, 0x20, 0x20,
    0x20, 0x20, 0x20, 0x20, 0x20, 0x20, 0x46, 0x41, 0x54, 0x31, 0x36, 0x20, 0x20, 0x20, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x55, 0xAA 
];
const FLASHDISK_DEFAULT_BLOCK3: [u8; WEBTV_BLOCK_SIZE as usize] = [ // this is the admin info block
    0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x68, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00,
    0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x20, 0x00, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x20,
    0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x01, 0x04, 0x00, 0x00, 0x00, 0x44, 0x00, 0x00, 0x00, 0x40,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00, 0x40,
    0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x44,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x11, 0xF7, 0x83, 0x82, 0x54, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00 
];


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

const MSNTV2_BOOT_SECTOR_OFFSET: u64 = 0x4000;
const MSNTV2_OEM_NAME_BSOFFSET: u64 = 0x03;
const MSNTV2_OEM_NAME_VALUE: u64 = 0x4d534e5456000000; // MSNTV\x00\x00\x00 as BE int
const MSNTV2_PROD_SIG_PUBKEY: &'static [u8]  = include_bytes!("../../keys/msntv2-mainkey.ber");
const MSNTV2_SIG_FILE_NAME: &'static str = "BOOT.SIG";

const NO_ROMFS_FLAG: u32 = 0x4e6f4653; // NoFS

#[allow(dead_code)]
#[derive(Debug, Copy, Clone, FromBytes)]
#[packbytes(be)]
pub struct DiskAdminInfo {
	pub primary_nv_alloc_bytes: u32,
	pub secondary_nv_alloc_bytes: u32,
	pub browser_alloc_bytes: u32,
	pub select_alloc_bytes: u32,
	pub diag_alloc_bytes: u32,
	pub tier3_alloc_bytes: u32,
	pub unknown1: u32,
	pub unknown2: u32,
	pub primary_nv_size: u32,
	pub secondary_nv_size: u32,
	pub browser_size: u32,
	pub unknown3: u32,
	pub unknown4: u32,
	pub diag_size: u32,
	pub tier3_size: u32,
	pub unknown5: u32,
	pub unknown6: u32,
	pub primary_nv0_block: u32,
	pub unknown7: u32,
	pub secondary_nv0_block: u32,
	pub diag_block: u32,
	pub browser0_block: u32,
	pub browser1_block: u32,
	pub browser_select_block: u32,
	pub primary_nv1_block: u32,
	pub unknown8: u32,
	pub secondary_nv1_block: u32,
	pub primary_nv_select_block: u32,
	pub secondary_nv_select_block: u32,
	pub tier3_block: u32,
	pub unknown9: u32,
	pub unknown10: u32,
	pub unknown11: u32,
	pub checksum: u32,
	pub unknown_marker: u32 // No idea what this is. Possibly a phone number when converted to decimal. Always 0xf7838254
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BuildMetaLayout {
	UnknownLayout,
	RawLayout,
	FlashdiskLayout,
	LC2DiskLayout,
	WebstarDiskLayout,
	UTVDiskLayout,
	MSNTV2DiskLayout
}

#[allow(non_snake_case, dead_code)]
pub mod BuildFlag {
	#[allow(non_snake_case, non_upper_case_globals)]
	pub const CompressedData: u32 = 0x01;
	#[allow(non_snake_case, non_upper_case_globals)]
	pub const Internal:       u32 = 0x02;
	#[allow(non_snake_case, non_upper_case_globals)]
	pub const Debug:          u32 = 0x04;
	#[allow(non_snake_case, non_upper_case_globals)]
	pub const WinCE:          u32 = 0x10;
	#[allow(non_snake_case, non_upper_case_globals)]
	pub const Satellite:      u32 = 0x20;
}


#[allow(dead_code)]
pub struct BuildMeta {
	pub file_path: String,
	pub collation: BuildIODataCollation,
	pub layout: BuildMetaLayout,
	pub build_count: u8,
	pub selected_build_index: u8,
	pub admin_info: DiskAdminInfo,
	pub build_info: [BuildInfo; 2],
	pub msntv2_info: MSNTV2Info,
	io: Box<dyn BuildIO>
}

#[allow(dead_code)]
#[derive(Debug, Copy, Clone)]
pub struct BuildInfo {
	pub available: bool,
	pub build_header: BuildHeader,
	pub romfs_header: ROMFSHeader,
	pub build_offset: u64, // Relative to the start of the file
	pub romfs_offset: u64, // Relative to the the build offset
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

#[allow(dead_code)]
#[derive(Debug)]
pub struct MSNTV2FileInfo {
	pub file_name: String,
	pub file_flags: String,
	pub file_size: u32,
	pub full_md5: [u8; 16],
	pub calculated_full_md5: [u8; 16],
	pub partial_md5: [u8; 16],
	pub calculated_partial_md5: [u8; 16],
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct MSNTV2Info {
	pub rsa_signature: Vec<u8>,
	pub signature_validates: Option<bool>,
	pub build_flags: String,
	pub build_contact: String,
	pub build_date: String,
	pub build_number: u32,
	pub build_version: String,
	pub build_signature_contact: String,
	pub build_files: Vec<MSNTV2FileInfo>
}

impl BuildMeta {
	pub fn open_rom(file_path: String, collation: Option<BuildIODataCollation>, calculate_checksums: bool) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match ROMIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf, None, calculate_checksums, false)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn open_disk(file_path: String, collation: Option<BuildIODataCollation>, calculate_checksums: bool, calculate_signatures: bool) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match DiskIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf, None, calculate_checksums, calculate_signatures)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn open_flashdisk(file_path: String, collation: Option<BuildIODataCollation>, calculate_checksums: bool) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		match FlashdiskIO::open(file_path.clone(), collation) {
			Ok(srcf) => {
				BuildMeta::new(srcf, Some(BuildMetaLayout::FlashdiskLayout), calculate_checksums, false)
			},
			Err(e) => {
				Err(e)
			}
		}
	}

	pub fn new(build_io: Box<dyn BuildIO>, layout: Option<BuildMetaLayout>, calculate_checksums: bool, calculate_signatures: bool) -> Result<BuildMeta, Box<dyn std::error::Error>>  {
		let mut wtv_buildmeta = BuildMeta::default_buildmeta(build_io);

		wtv_buildmeta.file_path = wtv_buildmeta.io.file_path().unwrap_or("".into()).clone();
		wtv_buildmeta.collation = wtv_buildmeta.io.collation().unwrap_or(BuildIODataCollation::Raw);

		wtv_buildmeta.layout = match layout {
			Some(forced_layout) => {
				if forced_layout == BuildMetaLayout::FlashdiskLayout {
					let _ = wtv_buildmeta.get_admininfo();
				}

				forced_layout
			},
			_ => wtv_buildmeta.get_layout().unwrap_or(BuildMetaLayout::UnknownLayout)
		};

		let _ = wtv_buildmeta.load_buildinfo(calculate_checksums, calculate_signatures);

		Ok(wtv_buildmeta)
	}

	pub fn write_build(&mut self, buf: &mut Vec<u8>) -> Result<usize, Box<dyn std::error::Error>> {
		let build_offset = match self.layout {
			BuildMetaLayout::FlashdiskLayout => {
				if self.build_count == 0 {
					// Write the default data so a build can load.
					let _ = self.io.seek(0x00000000);
					let _ = self.io.write(&mut FLASHDISK_DEFAULT_BLOCK0.clone())?;
					let _ = self.io.seek(0x00000400);
					let _ = self.io.write(&mut FLASHDISK_DEFAULT_BLOCK2.clone())?;
					let _ = self.io.seek(0x00000600);
					let _ = self.io.write(&mut FLASHDISK_DEFAULT_BLOCK3.clone())?;

					(FLASHDISK_DEFAULT_BUILD_OFFSET + FLASHDISK_BUILD_HEADER_OFFSET) as u64
				} else {
					self.build_info[self.selected_build_index as usize].build_offset
				}
			},
			_ => self.build_info[self.selected_build_index as usize].build_offset
		};

		let _ = self.io.seek(build_offset);

		let result = self.io.write(buf)?;

		let _ = self.io.commit();

		Ok(result)
	}

	pub fn set_selected_build_index(&mut self, selected_index: u8) -> Result<(), Box<dyn std::error::Error>> {
		if self.layout == BuildMetaLayout::LC2DiskLayout {
			let _ = self.io.seek(LC2_BUILD_SELECT_OFFSET)?;
			let _ = self.io.write(&mut [selected_index, 0, 0, 0]);
			let _ = self.io.commit();
		} else if self.layout == BuildMetaLayout::UTVDiskLayout {
			let _ = self.io.seek(UTV_BUILD_SELECT_OFFSET)?;
			let _ = self.io.write(&mut [selected_index, 0, 0, 0]);
			let _ = self.io.commit();
		}

		Ok(())
	}

	fn default_buildmeta(build_io: Box<dyn BuildIO>) -> BuildMeta {
		BuildMeta {
			file_path: "".into(),
			collation: BuildIODataCollation::Raw,
			layout: BuildMetaLayout::UnknownLayout,
			build_count: 0,
			selected_build_index: 0,
			admin_info: BuildMeta::default_admin_info(),
			build_info: [BuildMeta::default_buildinfo(); 2],
			msntv2_info: BuildMeta::default_msntv2_info(),
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

	fn default_admin_info() -> DiskAdminInfo {
		DiskAdminInfo::from_bytes([0x00; 0x8c])
	}

	fn default_msntv2_info() -> MSNTV2Info {
		MSNTV2Info {
			rsa_signature: vec![],
			signature_validates: None,
			build_flags: "".to_string(),
			build_contact: "".to_string(),
			build_date: "".to_string(),
			build_number: 0,
			build_version: "".to_string(),
			build_signature_contact: "".to_string(),
			build_files: vec![]
		}
	}

	fn get_admininfo(&mut self) -> Result<(), Box<dyn std::error::Error>> {
		let _ = self.io.seek(FLASHDISK_ADMININFO_OFFSET)?;
		let mut flashdisk_check = [0x00; 0x8c];
		let _ = self.io.read(&mut flashdisk_check).unwrap_or(0);
		self.admin_info = DiskAdminInfo::from_bytes(flashdisk_check);

		Ok(())
	}

	fn get_layout(&mut self) -> Result<BuildMetaLayout, Box<dyn std::error::Error>> {
		let file_size = self.io.len().unwrap_or(0);
		
		
		// Check flashdisk admin info block
		if file_size > FLASHDISK_ADMININFO_OFFSET {
			let _ = self.get_admininfo();

			if self.admin_info.primary_nv_alloc_bytes == FLASHDISK_NVRAM_SIZE_CHECK_VALUE
			&& self.admin_info.secondary_nv_alloc_bytes == FLASHDISK_NVRAM_SIZE_CHECK_VALUE {
				return Ok(BuildMetaLayout::FlashdiskLayout);
			}
		}

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

		if file_size > MSNTV2_BOOT_SECTOR_OFFSET {
			let _ = self.io.seek(MSNTV2_BOOT_SECTOR_OFFSET + MSNTV2_OEM_NAME_BSOFFSET)?;

			let mut boot_sector_check = [0x00; 0x08];
			let _ = self.io.read(&mut boot_sector_check).unwrap_or(0);

			if u64::from_be_bytes(boot_sector_check) == MSNTV2_OEM_NAME_VALUE {
				return Ok(BuildMetaLayout::MSNTV2DiskLayout);
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


	fn read_msntv2info(bootsig_blob: String) -> Result<MSNTV2Info, Box<dyn std::error::Error>> {
		let mut msntv2_info = BuildMeta::default_msntv2_info();

		for (line_idx, line_str) in bootsig_blob.lines().enumerate() {
			if line_idx == 0 {
				if Regex::new(r"^[0-9A-Fa-f]{2}+$").unwrap().is_match(line_str) {
					msntv2_info.rsa_signature = match hex::decode(line_str) {
						Ok(buf) => {
							buf
						},
						Err(_) => {
							vec![]
						},
					}
				}
			} else if line_idx == 1 {
				match Regex::new(r"^(?<build_flags>\w+)\s+(?<build_contact>\S+)\s+(?<build_date>\w+ \d+\/\d+\/\d+ \d+:\d+:\d+.?\d*)$").unwrap().captures(line_str) {
					Some(matches) => {
						msntv2_info.build_flags = matches["build_flags"].to_string();
						msntv2_info.build_contact = matches["build_contact"].to_string();
						msntv2_info.build_date = matches["build_date"].to_string();
					}
					None => {
					}
				}
			} else if line_idx == 2 {
				match Regex::new(r"^(?<build_number>[0-9A-Fa-f]{8})\s+(?<build_version>[0-9A-Fa-f]{8})\s+SIGNSUB:\s*(?<build_signature_contact>\S+)$").unwrap().captures(line_str) {
					Some(matches) => {
						msntv2_info.build_number = match u32::from_str_radix(&matches["build_number"], 16) {
							Ok(number) => {
								number
							},
							Err(_) => {
								0
							}
						};
						match u32::from_str_radix(&matches["build_version"], 16) {
							Ok(number) => {
								msntv2_info.build_version = format!("{}.{}", ((number >> 16) & 0xffff), (number & 0xffff));
							},
							Err(_) => {
								msntv2_info.build_version = "".to_string();
							}
						};
						msntv2_info.build_signature_contact = matches["build_signature_contact"].to_string();
					}
					None => {
					}
				}
			} else if line_idx > 2 {
				match Regex::new(r"^(?<file_name>[A-Z0-9]{1,8}(\.[A-Z0-9]{0,3})?)\s+(?<file_size>[0-9A-Fa-f]+)\s+(?<full_md5>[0-9A-Fa-f]{8} [0-9A-Fa-f]{8} [0-9A-Fa-f]{8} [0-9A-Fa-f]{8})\s+(?<file_flags>\w+)\s+#\s(?<partial_md5>[0-9A-Fa-f]{8} [0-9A-Fa-f]{8} [0-9A-Fa-f]{8} [0-9A-Fa-f]{8})+$").unwrap().captures(line_str) {
					Some(matches) => {
						let msntv2_file_info = MSNTV2FileInfo {
							file_name: matches["file_name"].to_string(),
							file_flags: matches["file_flags"].to_string(),
							file_size: match u32::from_str_radix(&matches["file_size"], 16) {
								Ok(number) => {
									number
								},
								Err(_) => {
									0
								}
							},
							full_md5: match hex::decode(matches["full_md5"].replace(" ", "")) {
								Ok(buf) => {
									match buf.try_into() {
										Ok(buf) => {
											buf
										},
										Err(_) => {
											[0x00; 16]
										}
									}
								},
								Err(_) => {
									[0x00; 16]
								},
							},
							calculated_full_md5: [0x00; 16],
							partial_md5: match hex::decode(matches["partial_md5"].replace(" ", "")) {
								Ok(buf) => {
									match buf.try_into() {
										Ok(buf) => {
											buf
										},
										Err(_) => {
											[0x00; 16]
										}
									}
								},
								Err(_) => {
									[0x00; 16]
								},
							},
							calculated_partial_md5: [0x00; 16]
						};

						msntv2_info.build_files.push(msntv2_file_info);
					}
					None => {
					}
				}
			}
		}

		Ok(msntv2_info)
	}

	fn verify_msntv2_signature(bootsig_blob: String, rsa_signature: &Vec<u8>) -> Result<bool, Box<dyn std::error::Error>> {
		// The signature verifies the data after the first line in BOOT.SIG
		let verify_data = bootsig_blob.find('\n').map(|idx| &bootsig_blob.as_bytes()[idx + 1..]).unwrap_or(&[]);

		let hashed_data = <Sha1 as sha1::Digest>::digest(verify_data);
		let public_key: RsaPublicKey = RsaPublicKey::from_public_key_der(MSNTV2_PROD_SIG_PUBKEY)?;

		// Split out the first byte since the MSNTV2 signature has a 0x01 prefix (probably a version)
		Ok(match rsa_signature.split_first() {
			Some((0x01, signature)) => {
				public_key.verify(
					rsa::pkcs1v15::Pkcs1v15Sign::new_unprefixed(),
					&hashed_data,
					signature
				).is_ok()
			},
			_ => {
				false
			}
		})
	}

	fn calculate_msntv2_file_md5s<F: Read + Seek>(file: &mut F, build_file: &mut MSNTV2FileInfo, calculate_short_md5s: bool, calculate_full_md5s: bool) {
		if calculate_full_md5s {
			let mut buffer = Vec::new();
			match file.read_to_end(&mut buffer) {
				Ok(_) => {
					let mut full_md5: [u8; 16] = Md5::digest(&buffer).into();
					for full_md5_chunk in full_md5.chunks_mut(4) {
						full_md5_chunk.reverse();
					}
					build_file.calculated_full_md5 = full_md5.clone();

					if calculate_short_md5s && build_file.file_size > 512 {
						let mut hasher = Md5::new();

						let mid_point = (build_file.file_size / 2) as usize;

						hasher.update(&buffer[..256]);
						hasher.update(&buffer[mid_point..mid_point + 256]);
						hasher.update(&buffer[(build_file.file_size - 256) as usize..]);

						let mut partial_md5: [u8; 16] = hasher.finalize().into();
						for partial_md5_chunk in partial_md5.chunks_mut(4) {
							partial_md5_chunk.reverse();
						}
						build_file.calculated_partial_md5 = partial_md5.clone();
					}
				},
				Err(_) => {
					//
				}
			}
		} else if calculate_short_md5s && build_file.file_size > 512 {
			let mut hasher = Md5::new();

			let mid_point = (build_file.file_size / 2) as u64;

			let mut buffer = [0x00 as u8; 256];

			match file.seek(SeekFrom::Start(0)) {
				Ok(_) => {
					match file.read_exact(&mut buffer) {
						Ok(_) => {
							hasher.update(&buffer);
						},
						Err(_) => {
						}
					};
				},
				Err(_) => {
					//
				}
			};

			match file.seek(SeekFrom::Start(mid_point)) {
				Ok(_) => {
					match file.read_exact(&mut buffer) {
						Ok(_) => {
							hasher.update(&buffer);
						},
						Err(_) => {
						}
					};
				},
				Err(_) => {
					//
				}
			};

			match file.seek(SeekFrom::End(-256)) {
				Ok(_) => {
					match file.read_exact(&mut buffer) {
						Ok(_) => {
							hasher.update(&buffer);
						},
						Err(_) => {
						}
					};
				},
				Err(_) => {
					//
				}
			};

			let mut partial_md5: [u8; 16] = hasher.finalize().into();
			for partial_md5_chunk in partial_md5.chunks_mut(4) {
				partial_md5_chunk.reverse();
			}
			build_file.calculated_partial_md5 = partial_md5.clone();
		}
	}

	fn find_local_file(dir_path: &Path, file_name: &str) -> Option<PathBuf> {
		let file_path = dir_path.join(file_name);

		if file_path.is_file() {
			Some(file_path)
		} else {
			// Case insensitive file lookup for macOS and Linux.
			fs::read_dir(dir_path).ok()?
				.filter_map(|entry| entry.ok())
				.map(|entry| entry.path())
				.find(|path| path.is_file() && path.file_name().map_or(false, |name| name.to_string_lossy().eq_ignore_ascii_case(file_name)))
		}
	}

	fn read_local_msntv2info(dir_path: String, calculate_short_md5s: bool, calculate_full_md5s: bool, calculate_signatures: bool) -> Result<MSNTV2Info, Box<dyn std::error::Error>> {
		let dir_path = Path::new(&dir_path);

		let sig_file_path =
			Self::find_local_file(dir_path, MSNTV2_SIG_FILE_NAME)
			.ok_or(format!("{} not found in {}", MSNTV2_SIG_FILE_NAME, dir_path.display()))?;
		let buffer = fs::read_to_string(sig_file_path)?;
		let mut msntv2_info = Self::read_msntv2info(buffer.clone())?;

		if calculate_signatures {
			msntv2_info.signature_validates = Some(match Self::verify_msntv2_signature(buffer, &msntv2_info.rsa_signature) {
				Ok(signature_validated) => {
					signature_validated
				},
				Err(_) => {
					false
				}
			});
		}

		if calculate_short_md5s || calculate_full_md5s {
			for build_file in msntv2_info.build_files.iter_mut() {
				match Self::find_local_file(dir_path, &build_file.file_name).map(fs::File::open) {
					Some(Ok(mut file)) => {
						Self::calculate_msntv2_file_md5s(&mut file, build_file, calculate_short_md5s, calculate_full_md5s);
					},
					_ => {
						//
					}
				}
			}
		}

		Ok(msntv2_info)
	}

	fn read_fatfs_msntv2info(fatfs_root: &Dir<'_, BuildIOStream<'_>>, calculate_short_md5s: bool, calculate_full_md5s: bool, calculate_signatures: bool) -> Result<MSNTV2Info, Box<dyn std::error::Error>> {
		let mut file = fatfs_root.open_file(MSNTV2_SIG_FILE_NAME)?;
		let mut buffer = String::new();
		let _ = file.read_to_string(&mut buffer)?;
		let mut msntv2_info = Self::read_msntv2info(buffer.clone())?;

		if calculate_signatures {
			msntv2_info.signature_validates = Some(match Self::verify_msntv2_signature(buffer, &msntv2_info.rsa_signature) {
				Ok(signature_validated) => {
					signature_validated
				},
				Err(_) => {
					false
				}
			});
		}

		if calculate_short_md5s || calculate_full_md5s {
			for build_file in msntv2_info.build_files.iter_mut() {
				match fatfs_root.open_file(&build_file.file_name) {
					Ok(mut file) => {
						Self::calculate_msntv2_file_md5s(&mut file, build_file, calculate_short_md5s, calculate_full_md5s);
					},
					Err(_) => {
					}
				}
			}

		}

		Ok(msntv2_info)
	}

	fn load_msntv2info(&mut self, calculate_checksums: bool, calculate_signatures: bool) -> Result<(), Box<dyn std::error::Error>> {
		self.build_count = 1;
		self.selected_build_index = 0;

		let mbr = MBR::read_from(&mut BuildIOStream::new(self.io.as_mut())?, WEBTV_BLOCK_SIZE as u32)?;

		let boot_partition = mbr.iter().fold(None, |found_partition, (_, current_partition)| {
			// Usable partition in the MBR
			if current_partition.is_used() {
				// Set the first partition found
				if found_partition.is_none() {
					Some(current_partition)
				// If we find a boot partition further along, then use that
				} else if current_partition.boot == mbrman::BOOT_ACTIVE {
					Some(current_partition)
				} else {
					// No match, use whatever is found (or None if not found)
					found_partition
				}
			} else {
				// No match, use whatever is found (or None if not found)
				found_partition
			}
		});

		if !boot_partition.is_none() {
			let start = boot_partition.unwrap().starting_lba as u64 * WEBTV_BLOCK_SIZE;
			let len = boot_partition.unwrap().sectors as u64 * WEBTV_BLOCK_SIZE;

			let msntv2_info: MSNTV2Info = {
				let fat_io = BuildIOStream::slice(self.io.as_mut(), start, len);
				let fat = fatfs::FileSystem::new(fat_io, fatfs::FsOptions::new())?;

				let root_dir = fat.root_dir();

				Self::read_fatfs_msntv2info(&root_dir, calculate_checksums, false, calculate_signatures)?
			};

			self.msntv2_info = msntv2_info;
		}

		Ok(())
	}

	fn load_buildinfo(&mut self, calculate_checksums: bool, calculate_signatures: bool) -> Result<(), Box<dyn std::error::Error>> {
		if self.layout == BuildMetaLayout::LC2DiskLayout {
			self.build_count = 2;
			self.selected_build_index = self.get_selected_build_index().unwrap_or(1);
			self.build_info[0] = self.get_buildinfo(LC2_BUILD_OFFSET0, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
			self.build_info[1] = self.get_buildinfo(LC2_BUILD_OFFSET1, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
		} else if self.layout == BuildMetaLayout::WebstarDiskLayout {
			self.build_count = 1;
			self.selected_build_index = 0;
			self.build_info[0] = self.get_buildinfo(WEBSTAR_BUILD_OFFSET0, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
		} else if self.layout == BuildMetaLayout::UTVDiskLayout {
			self.build_count = 2;
			self.selected_build_index = self.get_selected_build_index().unwrap_or(1);
			self.build_info[0] = self.get_buildinfo(UTV_BUILD_OFFSET0, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
			self.build_info[1] = self.get_buildinfo(UTV_BUILD_OFFSET1, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
		} else if self.layout == BuildMetaLayout::FlashdiskLayout {
			if self.admin_info.browser_alloc_bytes > 0 && self.admin_info.browser_size > 0 && self.admin_info.browser_alloc_bytes < self.io.len().unwrap_or(0) as u32 {
				let build0_offset = (self.admin_info.browser0_block as u64 * WEBTV_BLOCK_SIZE) + FLASHDISK_BUILD_HEADER_OFFSET;

				self.build_count = 1;
				self.selected_build_index = 0;
				self.build_info[0] = self.get_buildinfo(build0_offset, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
			} else {
				self.build_count = 0;
			}
		} else if self.layout == BuildMetaLayout::MSNTV2DiskLayout {
			let _ = self.load_msntv2info(calculate_checksums, calculate_signatures);
		} else {
			self.build_count = 1;
			self.selected_build_index = 0;
			self.build_info[0] = self.get_buildinfo(RAW_BUILD_OFFSET0, calculate_checksums).unwrap_or(BuildMeta::default_buildinfo());
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
		} else if self.layout == BuildMetaLayout::FlashdiskLayout {
			let browser_select_offset = self.admin_info.browser_select_block as u64 * WEBTV_BLOCK_SIZE;

			let _ = self.io.seek(browser_select_offset)?;
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

	fn get_buildinfo(&mut self, build_offset: u64, calculate_checksums: bool) -> Result<BuildInfo, Box<dyn std::error::Error>> {
		let mut buildinfo = BuildMeta::default_buildinfo();

		buildinfo.build_offset = build_offset;

		buildinfo.build_header = self.get_build_header(buildinfo.build_offset).unwrap_or(BuildMeta::default_build_header());

		// Can't check code checksum on flashdisk builds without decompressing
		if calculate_checksums && self.layout != BuildMetaLayout::FlashdiskLayout {
			buildinfo.calculated_code_checksum = self.calculate_dword_checksum(buildinfo.build_offset, buildinfo.build_header.code_dword_length, Some(0x02)).unwrap_or(0);
		}

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

		// Can't check ROMFS on flashdisk builds without decompressing or if the ROMFS address is NoFS
		if self.layout != BuildMetaLayout::FlashdiskLayout && buildinfo.build_header.romfs_address != NO_ROMFS_FLAG {
			buildinfo.romfs_offset = buildinfo.build_header.romfs_address.wrapping_sub(buildinfo.build_header.build_base_address) as u64;
			buildinfo.romfs_header = self.get_romfs_header(buildinfo.build_offset, buildinfo.romfs_offset).unwrap_or(buildinfo.romfs_header);

			let romfs_dword_length = buildinfo.romfs_header.romfs_dword_length.wrapping_mul(0x04) as u64;
			let romfs_end_offset = buildinfo.romfs_offset.wrapping_sub(romfs_dword_length).wrapping_sub(0x08);
			let data_length = match self.io.len() {
				Ok(len) => len,
				_ => 0
			};

			if calculate_checksums && romfs_dword_length > 0 && romfs_end_offset.wrapping_add(buildinfo.build_offset) <= data_length {
				buildinfo.calculated_romfs_checksum = self.calculate_dword_checksum(buildinfo.build_offset + romfs_end_offset, buildinfo.romfs_header.romfs_dword_length, None).unwrap_or(0);
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