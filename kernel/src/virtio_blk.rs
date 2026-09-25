use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::memory;
use crate::pci;

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_BLK_DEVICE_ID: u16 = 0x1001;

const REG_GUEST_FEATURES: u16 = 0x04;
const REG_QUEUE_ADDRESS: u16 = 0x08;
const REG_QUEUE_SIZE: u16 = 0x0C;
const REG_QUEUE_SELECT: u16 = 0x0E;
const REG_QUEUE_NOTIFY: u16 = 0x10;
const REG_DEVICE_STATUS: u16 = 0x12;
const REG_DEVICE_CONFIG: u16 = 0x14;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;

const QUEUE_REQUEST: u16 = 0;

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;

const VIRTIO_BLK_T_IN: u32 = 0;
const VIRTIO_BLK_T_OUT: u32 = 1;

pub const SECTOR_SIZE: usize = 512;

#[derive(Clone, Copy)]
struct VirtAddrPtr(u64);

impl VirtAddrPtr {
    unsafe fn write_u16(self, offset: usize, value: u16) {
        unsafe { ((self.0 as usize + offset) as *mut u16).write_volatile(value) };
    }
    unsafe fn read_u16(self, offset: usize) -> u16 {
        unsafe { ((self.0 as usize + offset) as *const u16).read_volatile() }
    }
    unsafe fn write_u32(self, offset: usize, value: u32) {
        unsafe { ((self.0 as usize + offset) as *mut u32).write_volatile(value) };
    }
    unsafe fn write_u64(self, offset: usize, value: u64) {
        unsafe { ((self.0 as usize + offset) as *mut u64).write_volatile(value) };
    }
}

struct VirtQueue {
    io_base: u16,
    index: u16,
    size: u16,
    desc: VirtAddrPtr,
    avail: VirtAddrPtr,
    used: VirtAddrPtr,
    used_seen: u16,
}

impl VirtQueue {
    fn setup(io_base: u16, index: u16) -> Self {
        let mut select_port: Port<u16> = Port::new(io_base + REG_QUEUE_SELECT);
        unsafe { select_port.write(index) };

        let mut size_port: Port<u16> = Port::new(io_base + REG_QUEUE_SIZE);
        let size = unsafe { size_port.read() };
        assert!(size > 0, "virtio queue {index} reported size 0");

        let desc_bytes = usize::from(size) * 16;
        let avail_bytes = 4 + usize::from(size) * 2;
        let used_offset = (desc_bytes + avail_bytes).div_ceil(4096) * 4096;
        let used_bytes = 4 + usize::from(size) * 8;
        let total_bytes = used_offset + used_bytes;
        let pages = total_bytes.div_ceil(4096);

        let (phys, virt) = memory::allocate_dma_frames(pages);
        unsafe {
            core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, pages * 4096);
        }

        let mut addr_port: Port<u32> = Port::new(io_base + REG_QUEUE_ADDRESS);
        unsafe { addr_port.write((phys.as_u64() / 4096) as u32) };

        let queue = VirtQueue {
            io_base,
            index,
            size,
            desc: VirtAddrPtr(virt.as_u64()),
            avail: VirtAddrPtr(virt.as_u64() + desc_bytes as u64),
            used: VirtAddrPtr(virt.as_u64() + used_offset as u64),
            used_seen: 0,
        };
        unsafe { queue.avail.write_u16(0, 1) };
        queue
    }

    fn set_desc(&self, idx: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let base = usize::from(idx) * 16;
        unsafe {
            self.desc.write_u64(base, addr);
            self.desc.write_u32(base + 8, len);
            self.desc.write_u16(base + 12, flags);
            self.desc.write_u16(base + 14, next);
        }
    }

    fn submit(&mut self, idx: u16) {
        unsafe {
            let avail_idx = self.avail.read_u16(2);
            let ring_slot = 4 + (avail_idx % self.size) as usize * 2;
            self.avail.write_u16(ring_slot, idx);
            self.avail.write_u16(2, avail_idx.wrapping_add(1));
        }
        let mut notify_port: Port<u16> = Port::new(self.io_base + REG_QUEUE_NOTIFY);
        unsafe { notify_port.write(self.index) };
    }

    fn poll_used(&mut self) -> Option<u16> {
        let used_idx = unsafe { self.used.read_u16(2) };
        if used_idx == self.used_seen {
            return None;
        }
        let slot = 4 + (self.used_seen % self.size) as usize * 8;
        let id = unsafe { ((self.used.0 as usize + slot) as *const u32).read_volatile() as u16 };
        self.used_seen = self.used_seen.wrapping_add(1);
        Some(id)
    }
}

struct VirtioBlk {
    capacity_sectors: u64,
    queue: VirtQueue,
    header_phys: u64,
    header_virt: VirtAddrPtr,
    data_phys: u64,
    data_virt: VirtAddrPtr,
    status_phys: u64,
    status_virt: VirtAddrPtr,
}

static DRIVER: Mutex<Option<VirtioBlk>> = Mutex::new(None);

pub fn init() -> bool {
    let Some(dev) = pci::find_device(VIRTIO_VENDOR_ID, VIRTIO_BLK_DEVICE_ID) else {
        return false;
    };
    dev.enable_bus_mastering_and_io();
    let Some(io_base) = dev.io_bar(0) else {
        return false;
    };

    let mut status_port: Port<u8> = Port::new(io_base + REG_DEVICE_STATUS);
    unsafe {
        status_port.write(0);
        status_port.write(STATUS_ACKNOWLEDGE);
        status_port.write(STATUS_ACKNOWLEDGE | STATUS_DRIVER);
    }

    let mut features_port: Port<u32> = Port::new(io_base + REG_GUEST_FEATURES);
    unsafe { features_port.write(0) };

    let mut capacity_bytes = [0u8; 8];
    for (i, byte) in capacity_bytes.iter_mut().enumerate() {
        let mut cap_port: Port<u8> = Port::new(io_base + REG_DEVICE_CONFIG + i as u16);
        *byte = unsafe { cap_port.read() };
    }
    let capacity_sectors = u64::from_le_bytes(capacity_bytes);

    let queue = VirtQueue::setup(io_base, QUEUE_REQUEST);

    let (header_phys, header_virt) = memory::allocate_dma_frames(1);
    let data_phys = header_phys.as_u64() + 512;
    let data_virt = VirtAddrPtr(header_virt.as_u64() + 512);
    let status_phys = header_phys.as_u64() + 512 + 512;
    let status_virt = VirtAddrPtr(header_virt.as_u64() + 512 + 512);

    unsafe {
        status_port.write(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
    }

    *DRIVER.lock() = Some(VirtioBlk {
        capacity_sectors,
        queue,
        header_phys: header_phys.as_u64(),
        header_virt: VirtAddrPtr(header_virt.as_u64()),
        data_phys,
        data_virt,
        status_phys,
        status_virt,
    });
    true
}

pub fn capacity_sectors() -> Option<u64> {
    DRIVER.lock().as_ref().map(|d| d.capacity_sectors)
}

fn do_request(sector: u64, buf_ptr: *mut u8, req_type: u32) -> bool {
    let mut guard = DRIVER.lock();
    let Some(driver) = guard.as_mut() else {
        return false;
    };

    while driver.queue.poll_used().is_some() {}

    unsafe {
        driver.header_virt.write_u32(0, req_type);
        driver.header_virt.write_u32(4, 0);
        driver.header_virt.write_u64(8, sector);
    }

    if req_type == VIRTIO_BLK_T_OUT {
        unsafe {
            core::ptr::copy_nonoverlapping(buf_ptr, driver.data_virt.0 as *mut u8, SECTOR_SIZE);
        }
    }

    let data_flags = if req_type == VIRTIO_BLK_T_IN {
        DESC_F_NEXT | DESC_F_WRITE
    } else {
        DESC_F_NEXT
    };

    driver.queue.set_desc(0, driver.header_phys, 16, DESC_F_NEXT, 1);
    driver
        .queue
        .set_desc(1, driver.data_phys, SECTOR_SIZE as u32, data_flags, 2);
    driver.queue.set_desc(2, driver.status_phys, 1, DESC_F_WRITE, 0);
    driver.queue.submit(0);

    while driver.queue.poll_used().is_none() {
        core::hint::spin_loop();
    }

    let status = unsafe { (driver.status_virt.0 as *const u8).read_volatile() };
    if status != 0 {
        return false;
    }

    if req_type == VIRTIO_BLK_T_IN {
        unsafe {
            core::ptr::copy_nonoverlapping(driver.data_virt.0 as *const u8, buf_ptr, SECTOR_SIZE);
        }
    }
    true
}

pub fn read_sector(sector: u64, buf: &mut [u8; SECTOR_SIZE]) -> bool {
    do_request(sector, buf.as_mut_ptr(), VIRTIO_BLK_T_IN)
}

#[allow(dead_code)]
pub fn write_sector(sector: u64, buf: &[u8; SECTOR_SIZE]) -> bool {
    do_request(sector, buf.as_ptr() as *mut u8, VIRTIO_BLK_T_OUT)
}
