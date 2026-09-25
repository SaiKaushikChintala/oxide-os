use alloc::string::String;
use alloc::vec::Vec;

use crate::virtio_blk;

const MAGIC: &[u8; 4] = b"OXFS";
const TABLE_START_SECTOR: u64 = 1;
const ENTRIES_PER_SECTOR: usize = virtio_blk::SECTOR_SIZE / 64;
const MAX_ENTRIES_SECTORS: u64 = 8;
const MAX_ENTRIES: usize = ENTRIES_PER_SECTOR * MAX_ENTRIES_SECTORS as usize;

pub struct DiskFile {
    pub name: String,
    pub start_sector: u64,
    pub size_bytes: u32,
}

struct Superblock {
    file_count: u32,
    next_free_sector: u64,
}

fn read_superblock() -> Option<Superblock> {
    let mut sector0 = [0u8; virtio_blk::SECTOR_SIZE];
    if !virtio_blk::read_sector(0, &mut sector0) {
        return None;
    }
    if &sector0[0..4] != MAGIC {
        return None;
    }
    Some(Superblock {
        file_count: u32::from_le_bytes(sector0[8..12].try_into().unwrap()),
        next_free_sector: u64::from_le_bytes(sector0[12..20].try_into().unwrap()),
    })
}

fn write_superblock(sb: &Superblock) -> bool {
    let mut sector0 = [0u8; virtio_blk::SECTOR_SIZE];
    if !virtio_blk::read_sector(0, &mut sector0) {
        return false;
    }
    sector0[8..12].copy_from_slice(&sb.file_count.to_le_bytes());
    sector0[12..20].copy_from_slice(&sb.next_free_sector.to_le_bytes());
    virtio_blk::write_sector(0, &sector0)
}

fn entry_location(index: usize) -> (u64, usize) {
    let sector = TABLE_START_SECTOR + (index / ENTRIES_PER_SECTOR) as u64;
    let offset_in_sector = (index % ENTRIES_PER_SECTOR) * 64;
    (sector, offset_in_sector)
}

fn read_entry_at(index: usize) -> Option<DiskFile> {
    let (sector, off) = entry_location(index);
    let mut buf = [0u8; virtio_blk::SECTOR_SIZE];
    if !virtio_blk::read_sector(sector, &mut buf) {
        return None;
    }
    let name_bytes = &buf[off..off + 52];
    let name_len = name_bytes.iter().position(|&b| b == 0).unwrap_or(52);
    let name = String::from_utf8_lossy(&name_bytes[..name_len]).into_owned();
    let start_sector = u32::from_le_bytes(buf[off + 52..off + 56].try_into().unwrap()) as u64;
    let size_bytes = u32::from_le_bytes(buf[off + 56..off + 60].try_into().unwrap());
    Some(DiskFile { name, start_sector, size_bytes })
}

fn write_entry_at(index: usize, name: &str, start_sector: u64, size_bytes: u32) -> bool {
    let (sector, off) = entry_location(index);
    let mut buf = [0u8; virtio_blk::SECTOR_SIZE];
    if !virtio_blk::read_sector(sector, &mut buf) {
        return false;
    }
    let mut name_field = [0u8; 52];
    name_field[..name.len()].copy_from_slice(name.as_bytes());
    buf[off..off + 52].copy_from_slice(&name_field);
    buf[off + 52..off + 56].copy_from_slice(&(start_sector as u32).to_le_bytes());
    buf[off + 56..off + 60].copy_from_slice(&size_bytes.to_le_bytes());
    virtio_blk::write_sector(sector, &buf)
}

pub fn list() -> Option<Vec<DiskFile>> {
    let sb = read_superblock()?;
    let mut files = Vec::with_capacity(sb.file_count as usize);
    for index in 0..(sb.file_count as usize).min(MAX_ENTRIES) {
        files.push(read_entry_at(index)?);
    }
    Some(files)
}

pub fn read(name: &str) -> Option<Vec<u8>> {
    let files = list()?;
    let file = files.into_iter().find(|f| f.name == name)?;

    let sectors_needed = (file.size_bytes as usize).div_ceil(virtio_blk::SECTOR_SIZE).max(1);
    let mut data = Vec::with_capacity(sectors_needed * virtio_blk::SECTOR_SIZE);
    let mut buf = [0u8; virtio_blk::SECTOR_SIZE];
    for i in 0..sectors_needed {
        if !virtio_blk::read_sector(file.start_sector + i as u64, &mut buf) {
            return None;
        }
        data.extend_from_slice(&buf);
    }
    data.truncate(file.size_bytes as usize);
    Some(data)
}

pub fn write(name: &str, data: &[u8]) -> bool {
    if name.len() > 52 {
        return false;
    }
    let Some(mut sb) = read_superblock() else {
        return false;
    };
    let capacity_sectors = virtio_blk::capacity_sectors().unwrap_or(0);

    let mut existing_index = None;
    for index in 0..(sb.file_count as usize).min(MAX_ENTRIES) {
        let Some(entry) = read_entry_at(index) else { return false };
        if entry.name == name {
            existing_index = Some((index, entry));
            break;
        }
    }

    let sectors_needed = data.len().div_ceil(virtio_blk::SECTOR_SIZE).max(1) as u64;

    let (index, start_sector, growing_file_count) = match existing_index {
        Some((index, entry)) => {
            let old_sectors = (entry.size_bytes as usize).div_ceil(virtio_blk::SECTOR_SIZE).max(1) as u64;
            if sectors_needed <= old_sectors {
                (index, entry.start_sector, false)
            } else {
                (index, sb.next_free_sector, false)
            }
        }
        None => {
            if sb.file_count as usize >= MAX_ENTRIES {
                return false;
            }
            (sb.file_count as usize, sb.next_free_sector, true)
        }
    };

    if start_sector + sectors_needed > capacity_sectors {
        return false;
    }

    for i in 0..sectors_needed as usize {
        let chunk_start = i * virtio_blk::SECTOR_SIZE;
        let chunk_end = (chunk_start + virtio_blk::SECTOR_SIZE).min(data.len());
        let mut buf = [0u8; virtio_blk::SECTOR_SIZE];
        if chunk_start < data.len() {
            buf[..chunk_end - chunk_start].copy_from_slice(&data[chunk_start..chunk_end]);
        }
        if !virtio_blk::write_sector(start_sector + i as u64, &buf) {
            return false;
        }
    }

    if !write_entry_at(index, name, start_sector, data.len() as u32) {
        return false;
    }

    if start_sector == sb.next_free_sector {
        sb.next_free_sector += sectors_needed;
    }
    if growing_file_count {
        sb.file_count += 1;
    }
    write_superblock(&sb)
}
