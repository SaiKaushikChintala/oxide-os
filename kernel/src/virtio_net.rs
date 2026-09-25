use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::memory;
use crate::pci;

const VIRTIO_VENDOR_ID: u16 = 0x1AF4;
const VIRTIO_NET_DEVICE_ID: u16 = 0x1000;

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

const QUEUE_RX: u16 = 0;
const QUEUE_TX: u16 = 1;

const RX_BUFFERS: usize = 8;
const VIRTIO_NET_HDR_LEN: usize = 10;
const FRAME_BUF_LEN: usize = VIRTIO_NET_HDR_LEN + 1514;

const DESC_F_WRITE: u16 = 2;

struct VirtQueue {
    io_base: u16,
    index: u16,
    size: u16,
    desc: VirtAddrPtr,
    avail: VirtAddrPtr,
    used: VirtAddrPtr,
    used_seen: u16,
}

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

    fn poll_used(&mut self) -> Option<(u16, u32)> {
        let used_idx = unsafe { self.used.read_u16(2) };
        if used_idx == self.used_seen {
            return None;
        }
        let slot = 4 + (self.used_seen % self.size) as usize * 8;
        let id = unsafe { self.desc_used_id(slot) };
        let len = unsafe { self.desc_used_len(slot) };
        self.used_seen = self.used_seen.wrapping_add(1);
        Some((id, len))
    }

    unsafe fn desc_used_id(&self, slot: usize) -> u16 {
        unsafe { ((self.used.0 as usize + slot) as *const u32).read_volatile() as u16 }
    }
    unsafe fn desc_used_len(&self, slot: usize) -> u32 {
        unsafe { ((self.used.0 as usize + slot + 4) as *const u32).read_volatile() }
    }
}

#[derive(Clone, Copy)]
struct DmaBuffer {
    phys: u64,
    virt: VirtAddrPtr,
}

pub struct VirtioNet {
    mac: [u8; 6],
    rx: VirtQueue,
    tx: VirtQueue,
    rx_buffers: [DmaBuffer; RX_BUFFERS],
    tx_buffer: DmaBuffer,
}

static DRIVER: Mutex<Option<VirtioNet>> = Mutex::new(None);

pub fn init() -> bool {
    let Some(dev) = pci::find_device(VIRTIO_VENDOR_ID, VIRTIO_NET_DEVICE_ID) else {
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

    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        let mut mac_port: Port<u8> = Port::new(io_base + REG_DEVICE_CONFIG + i as u16);
        *byte = unsafe { mac_port.read() };
    }

    let mut rx = VirtQueue::setup(io_base, QUEUE_RX);
    let tx = VirtQueue::setup(io_base, QUEUE_TX);

    let mut rx_buffers = [DmaBuffer { phys: 0, virt: VirtAddrPtr(0) }; RX_BUFFERS];
    for (i, slot) in rx_buffers.iter_mut().enumerate() {
        let (phys, virt) = memory::allocate_dma_frames(1);
        *slot = DmaBuffer { phys: phys.as_u64(), virt: VirtAddrPtr(virt.as_u64()) };
        rx.set_desc(i as u16, phys.as_u64(), FRAME_BUF_LEN as u32, DESC_F_WRITE, 0);
        rx.submit(i as u16);
    }

    let (tx_phys, tx_virt) = memory::allocate_dma_frames(1);
    let tx_buffer = DmaBuffer {
        phys: tx_phys.as_u64(),
        virt: VirtAddrPtr(tx_virt.as_u64()),
    };

    unsafe {
        status_port.write(STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);
    }

    *DRIVER.lock() = Some(VirtioNet {
        mac,
        rx,
        tx,
        rx_buffers,
        tx_buffer,
    });
    true
}

pub fn mac_address() -> Option<[u8; 6]> {
    DRIVER.lock().as_ref().map(|d| d.mac)
}

pub fn send(frame: &[u8]) {
    let mut guard = DRIVER.lock();
    let Some(driver) = guard.as_mut() else {
        return;
    };

    while driver.tx.poll_used().is_some() {}

    unsafe {
        core::ptr::write_bytes(driver.tx_buffer.virt.0 as *mut u8, 0, VIRTIO_NET_HDR_LEN);
        core::ptr::copy_nonoverlapping(
            frame.as_ptr(),
            (driver.tx_buffer.virt.0 as usize + VIRTIO_NET_HDR_LEN) as *mut u8,
            frame.len(),
        );
    }
    driver.tx.set_desc(
        0,
        driver.tx_buffer.phys,
        (VIRTIO_NET_HDR_LEN + frame.len()) as u32,
        0,
        0,
    );
    driver.tx.submit(0);

    while driver.tx.poll_used().is_none() {
        core::hint::spin_loop();
    }
}

pub fn try_receive(out: &mut [u8]) -> Option<usize> {
    let mut guard = DRIVER.lock();
    let driver = guard.as_mut()?;

    let (id, len) = driver.rx.poll_used()?;
    let buf = driver.rx_buffers[id as usize % RX_BUFFERS];
    let payload_len = (len as usize).saturating_sub(VIRTIO_NET_HDR_LEN);
    let copy_len = payload_len.min(out.len());
    unsafe {
        core::ptr::copy_nonoverlapping(
            (buf.virt.0 as usize + VIRTIO_NET_HDR_LEN) as *const u8,
            out.as_mut_ptr(),
            copy_len,
        );
    }

    driver
        .rx
        .set_desc(id, buf.phys, FRAME_BUF_LEN as u32, DESC_F_WRITE, 0);
    driver.rx.submit(id);

    Some(copy_len)
}
