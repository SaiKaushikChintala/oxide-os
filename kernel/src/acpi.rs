use alloc::vec::Vec;
use spin::Mutex;
use x86_64::PhysAddr;
use x86_64::instructions::port::Port;

use crate::memory;

#[repr(C, packed)]
struct Rsdp {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
}

#[repr(C, packed)]
struct SdtHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: u32,
    creator_revision: u32,
}

static mut PM1A_CNT_BLK: u16 = 0;
static mut FOUND: bool = false;
static mut LOCAL_APIC_PHYS: u64 = 0;
static CPU_APIC_IDS: Mutex<Vec<u8>> = Mutex::new(Vec::new());

pub fn init(rsdp_phys: Option<u64>) {
    let Some(rsdp_phys) = rsdp_phys else { return };
    let rsdp_virt = memory::phys_to_virt(PhysAddr::new(rsdp_phys));
    let rsdp = unsafe { &*(rsdp_virt.as_u64() as *const Rsdp) };
    if &rsdp.signature != b"RSD PTR " {
        return;
    }

    let rsdt_phys = PhysAddr::new(u64::from(rsdt_address_le(rsdp)));
    let rsdt_virt = memory::phys_to_virt(rsdt_phys);
    let rsdt_header = unsafe { &*(rsdt_virt.as_u64() as *const SdtHeader) };
    if &rsdt_header.signature != b"RSDT" {
        return;
    }

    let entries_len = header_length_le(rsdt_header) as usize;
    let entries_bytes = entries_len.saturating_sub(core::mem::size_of::<SdtHeader>());
    let entry_count = entries_bytes / 4;
    let entries_ptr = (rsdt_virt.as_u64() as usize + core::mem::size_of::<SdtHeader>()) as *const u32;

    for i in 0..entry_count {
        let table_phys = unsafe { entries_ptr.add(i).read_unaligned() };
        let table_virt = memory::phys_to_virt(PhysAddr::new(u64::from(table_phys)));
        let table_header = unsafe { &*(table_virt.as_u64() as *const SdtHeader) };

        if &table_header.signature == b"FACP" {
            let fadt_bytes = table_virt.as_u64() as *const u8;
            let pm1a_cnt_blk = unsafe { (fadt_bytes.add(64) as *const u32).read_unaligned() };
            unsafe {
                PM1A_CNT_BLK = pm1a_cnt_blk as u16;
                FOUND = true;
            }
        } else if &table_header.signature == b"APIC" {
            parse_madt(table_virt, header_length_le(table_header) as usize);
        }
    }
}

fn parse_madt(table_virt: x86_64::VirtAddr, table_len: usize) {
    let base = table_virt.as_u64() as *const u8;
    let local_apic_phys = unsafe { (base.add(36) as *const u32).read_unaligned() };
    unsafe { LOCAL_APIC_PHYS = u64::from(local_apic_phys) };

    let mut offset = 44usize;
    let mut ids = CPU_APIC_IDS.lock();
    while offset + 2 <= table_len {
        let entry_type = unsafe { base.add(offset).read() };
        let entry_len = unsafe { base.add(offset + 1).read() } as usize;
        if entry_len < 2 || offset + entry_len > table_len {
            break;
        }

        if entry_type == 0 && entry_len >= 8 {
            let apic_id = unsafe { base.add(offset + 3).read() };
            let flags = unsafe { (base.add(offset + 4) as *const u32).read_unaligned() };
            let enabled = flags & 0x1 != 0;
            if enabled {
                ids.push(apic_id);
            }
        }

        offset += entry_len;
    }
}

pub fn cpu_apic_ids() -> Vec<u8> {
    CPU_APIC_IDS.lock().clone()
}

pub fn local_apic_phys() -> Option<u64> {
    let addr = unsafe { LOCAL_APIC_PHYS };
    (addr != 0).then_some(addr)
}

fn rsdt_address_le(rsdp: &Rsdp) -> u32 {
    rsdp.rsdt_address
}
fn header_length_le(header: &SdtHeader) -> u32 {
    header.length
}

pub fn available() -> bool {
    unsafe { FOUND }
}

const SLP_EN: u16 = 1 << 13;
const SLP_TYPA_S5: u16 = 0;

pub fn shutdown() -> ! {
    if unsafe { FOUND } {
        let mut port: Port<u16> = Port::new(unsafe { PM1A_CNT_BLK });
        unsafe { port.write(SLP_EN | (SLP_TYPA_S5 << 10)) };
    }
    loop {
        x86_64::instructions::hlt();
    }
}

pub fn reboot() -> ! {
    let mut status_port: Port<u8> = Port::new(0x64);
    let mut data_port: Port<u8> = Port::new(0x64);
    unsafe {
        while status_port.read() & 0x02 != 0 {
            core::hint::spin_loop();
        }
        data_port.write(0xFE);
    }
    loop {
        x86_64::instructions::hlt();
    }
}
