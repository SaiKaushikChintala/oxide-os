use std::path::PathBuf;
use std::{env, fs};

const SECTOR_SIZE: usize = 512;
const TABLE_START_SECTOR: u64 = 1;
const TABLE_SECTORS: u64 = 8;
const DATA_START_SECTOR: u64 = 1 + TABLE_SECTORS;
const IMAGE_TOTAL_SECTORS: u64 = 2048;

fn build_disk_image(path: &PathBuf) {
    let files: &[(&str, &[u8])] = &[
        ("motd.txt", b"Welcome to Oxide -- a from-scratch x86-64 kernel in Rust.\n" as &[u8]),
        (
            "about.txt",
            b"This file was read from a real virtio-blk disk through a real\n\
kernel block driver and a real (if minimal) on-disk filesystem,\n\
not baked into the kernel image like the initrd demo programs.\n",
        ),
        ("hello.txt", b"hello, disk!\n"),
    ];
    assert!(files.len() <= (TABLE_SECTORS as usize) * (SECTOR_SIZE / 64));

    let mut image = Vec::new();
    image.resize((DATA_START_SECTOR as usize) * SECTOR_SIZE, 0u8);

    let mut next_sector = DATA_START_SECTOR;
    let mut table_entries = Vec::new();
    for (name, contents) in files {
        assert!(name.len() <= 52, "file name too long for a 52-byte field");
        let start_sector = next_sector;
        let size_bytes = contents.len() as u32;
        let sectors_used = (contents.len()).div_ceil(SECTOR_SIZE).max(1) as u64;

        let mut padded = contents.to_vec();
        padded.resize(sectors_used as usize * SECTOR_SIZE, 0u8);
        image.extend_from_slice(&padded);

        table_entries.push((*name, start_sector, size_bytes));
        next_sector += sectors_used;
    }

    let mut superblock = [0u8; SECTOR_SIZE];
    superblock[0..4].copy_from_slice(b"OXFS");
    superblock[4..8].copy_from_slice(&1u32.to_le_bytes());
    superblock[8..12].copy_from_slice(&(table_entries.len() as u32).to_le_bytes());
    superblock[12..20].copy_from_slice(&next_sector.to_le_bytes());
    image[0..SECTOR_SIZE].copy_from_slice(&superblock);

    let table_bytes_start = (TABLE_START_SECTOR as usize) * SECTOR_SIZE;
    for (i, (name, start_sector, size_bytes)) in table_entries.iter().enumerate() {
        let entry_off = table_bytes_start + i * 64;
        let mut name_field = [0u8; 52];
        name_field[..name.len()].copy_from_slice(name.as_bytes());
        image[entry_off..entry_off + 52].copy_from_slice(&name_field);
        image[entry_off + 52..entry_off + 56].copy_from_slice(&start_sector.to_le_bytes()[..4]);
        image[entry_off + 56..entry_off + 60].copy_from_slice(&size_bytes.to_le_bytes());
    }

    image.resize((IMAGE_TOTAL_SECTORS as usize) * SECTOR_SIZE, 0u8);
    fs::write(path, &image).unwrap();
}

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let kernel = PathBuf::from(env::var_os("CARGO_BIN_FILE_KERNEL_kernel").unwrap());

    let uefi_path = out_dir.join("uefi.img");
    bootloader::UefiBoot::new(&kernel)
        .create_disk_image(&uefi_path)
        .unwrap();

    let bios_path = out_dir.join("bios.img");
    bootloader::BiosBoot::new(&kernel)
        .create_disk_image(&bios_path)
        .unwrap();

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let uefi_out = manifest_dir.join("uefi.img");
    let bios_out = manifest_dir.join("bios.img");
    fs::copy(&uefi_path, &uefi_out).unwrap();
    fs::copy(&bios_path, &bios_out).unwrap();

    println!("cargo:rustc-env=UEFI_PATH={}", uefi_out.display());
    println!("cargo:rustc-env=BIOS_PATH={}", bios_out.display());

    let disk_out = manifest_dir.join("disk.img");
    if !disk_out.exists() {
        build_disk_image(&disk_out);
    }
    println!("cargo:rustc-env=DISK_PATH={}", disk_out.display());
}
