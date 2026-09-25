use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

use crate::pci;

const PIIX3_USB_VENDOR: u16 = 0x8086;
const PIIX3_USB_DEVICE: u16 = 0x7020;

const REG_USBCMD: u16 = 0x00;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

const USBCMD_RUN: u16 = 1 << 0;
const USBCMD_HCRESET: u16 = 1 << 1;
const USBCMD_GRESET: u16 = 1 << 2;
const USBCMD_MAXP64: u16 = 1 << 7;

const PORTSC_CONNECT_STATUS: u16 = 1 << 0;
const PORTSC_PORT_ENABLE: u16 = 1 << 2;
const PORTSC_RESET: u16 = 1 << 9;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Td {
    link: u32,
    ctrl_status: u32,
    token: u32,
    buffer: u32,
}

const TD_LINK_TERMINATE: u32 = 1 << 0;
const TD_LINK_DEPTH_FIRST: u32 = 1 << 2;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_STATUS_LOW_SPEED: u32 = 1 << 26;
const TD_STATUS_CERR_ONE: u32 = 1 << 27;
const TD_STATUS_ERROR_MASK: u32 = 0x7 << 17;

const PID_IN: u32 = 0x69;
const PID_SETUP: u32 = 0x2D;

fn make_token(pid: u32, device: u8, endpoint: u8, data1: bool, len: usize) -> u32 {
    let max_len = if len == 0 { 0x7FF } else { (len - 1) as u32 };
    pid | (u32::from(device) << 8)
        | (u32::from(endpoint) << 15)
        | (u32::from(data1) << 19)
        | (max_len << 21)
}

#[repr(C, align(16))]
struct Qh {
    head_link: u32,
    element_link: u32,
    _reserved: [u32; 2],
}

const QH_LINK_TERMINATE: u32 = 1 << 0;
const QH_LINK_IS_QH: u32 = 1 << 1;

struct Uhci {
    qh: &'static mut Qh,
    int_td: &'static mut Td,
    report_buf: &'static mut [u8; 8],
    device_address: u8,
    toggle: bool,
}

static READY: AtomicBool = AtomicBool::new(false);
static mut UHCI: Option<Uhci> = None;

fn wait_a_little() {
    for _ in 0..100_000 {
        core::hint::black_box(());
    }
}

fn wait_ms(ms: u32) {
    for _ in 0..ms {
        wait_a_little();
    }
}

fn wait_for_td_chain(td_chain: &[&Td]) -> bool {
    for _ in 0..20_000 {
        let mut all_done = true;
        for td in td_chain {
            let status = td.ctrl_status;
            if status & TD_STATUS_ERROR_MASK != 0 {
                return false;
            }
            if status & TD_STATUS_ACTIVE != 0 {
                all_done = false;
                break;
            }
        }
        if all_done {
            return true;
        }
        wait_a_little();
    }
    false
}

fn alloc_dma<T>() -> &'static mut T {
    let (_, virt) = crate::memory::allocate_dma_frames(1);
    unsafe {
        core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, 4096);
        &mut *(virt.as_u64() as *mut T)
    }
}

fn phys_of<T>(reference: &T) -> u32 {
    let virt = reference as *const T as u64;
    let phys = virt - crate::memory::phys_to_virt(x86_64::PhysAddr::new(0)).as_u64();
    phys as u32
}

fn control_no_data(uhci: &mut Uhci, device: u8, setup: [u8; 8]) -> bool {
    let setup_buf: &'static mut [u8; 8] = alloc_dma();
    *setup_buf = setup;
    let setup_td: &'static mut Td = alloc_dma();
    let status_td: &'static mut Td = alloc_dma();

    let low_speed = if device == 0 { TD_STATUS_LOW_SPEED } else { 0 };
    *status_td = Td {
        link: TD_LINK_TERMINATE,
        ctrl_status: TD_STATUS_ACTIVE | TD_STATUS_CERR_ONE | low_speed,
        token: make_token(PID_IN, device, 0, true, 0),
        buffer: 0,
    };
    *setup_td = Td {
        link: phys_of(&*status_td) | TD_LINK_DEPTH_FIRST,
        ctrl_status: TD_STATUS_ACTIVE | TD_STATUS_CERR_ONE | low_speed,
        token: make_token(PID_SETUP, device, 0, false, 8),
        buffer: phys_of(&*setup_buf),
    };

    uhci.qh.element_link = phys_of(&*setup_td);
    let ok = wait_for_td_chain(&[setup_td, status_td]);
    uhci.qh.element_link = QH_LINK_TERMINATE;
    ok
}

pub fn init() -> Option<usize> {
    let dev = pci::find_device(PIIX3_USB_VENDOR, PIIX3_USB_DEVICE)?;
    dev.enable_bus_mastering_and_io();
    let io_base = dev.io_bar(4)?;

    let mut cmd_port: Port<u16> = Port::new(io_base + REG_USBCMD);
    unsafe { cmd_port.write(USBCMD_GRESET) };
    wait_ms(10);
    unsafe { cmd_port.write(0) };

    unsafe { cmd_port.write(USBCMD_HCRESET) };
    for _ in 0..1000 {
        if unsafe { cmd_port.read() } & USBCMD_HCRESET == 0 {
            break;
        }
        wait_a_little();
    }

    let frame_list: &'static mut [u32; 1024] = alloc_dma();
    let qh: &'static mut Qh = alloc_dma();
    qh.head_link = QH_LINK_TERMINATE;
    qh.element_link = QH_LINK_TERMINATE;

    let qh_phys = phys_of(&*qh) | QH_LINK_IS_QH;
    for slot in frame_list.iter_mut() {
        *slot = qh_phys;
    }

    let mut frbase_port: Port<u32> = Port::new(io_base + REG_FRBASEADD);
    let mut frnum_port: Port<u16> = Port::new(io_base + REG_FRNUM);
    unsafe {
        frbase_port.write(phys_of(&*frame_list));
        frnum_port.write(0);
        cmd_port.write(USBCMD_RUN | USBCMD_MAXP64);
    }

    let mut connected = 0;
    let mut kb_port_reg = None;
    for reg in [REG_PORTSC1, REG_PORTSC2] {
        let mut port: Port<u16> = Port::new(io_base + reg);
        if unsafe { port.read() } & PORTSC_CONNECT_STATUS != 0 {
            connected += 1;
            if kb_port_reg.is_none() {
                kb_port_reg = Some(reg);
            }
        }
    }

    let int_td: &'static mut Td = alloc_dma();
    let report_buf: &'static mut [u8; 8] = alloc_dma();
    int_td.ctrl_status = 0;

    let mut uhci = Uhci {
        qh,
        int_td,
        report_buf,
        device_address: 0,
        toggle: false,
    };

    if let Some(reg) = kb_port_reg {
        bring_up_keyboard(&mut uhci, io_base + reg);
    }

    unsafe {
        UHCI = Some(uhci);
    }

    Some(connected)
}

fn bring_up_keyboard(uhci: &mut Uhci, portsc_addr: u16) {
    let mut port: Port<u16> = Port::new(portsc_addr);
    unsafe {
        let status = port.read();
        port.write(status | PORTSC_RESET);
    }
    wait_ms(50);
    unsafe {
        let status = port.read();
        port.write(status & !PORTSC_RESET);
    }
    wait_ms(10);
    unsafe {
        let status = port.read();
        port.write(status | PORTSC_PORT_ENABLE);
    }
    wait_ms(10);

    if unsafe { port.read() } & PORTSC_CONNECT_STATUS == 0 {
        return;
    }

    if !control_no_data(uhci, 0, [0x00, 0x05, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]) {
        return;
    }
    wait_ms(2);
    uhci.device_address = 1;

    if !control_no_data(uhci, 1, [0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00]) {
        return;
    }

    let _ = control_no_data(uhci, 1, [0x21, 0x0B, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]);

    arm_interrupt_transfer(uhci);
    READY.store(true, Ordering::SeqCst);
}

fn arm_interrupt_transfer(uhci: &mut Uhci) {
    let buf_phys = phys_of(&*uhci.report_buf);
    *uhci.int_td = Td {
        link: TD_LINK_TERMINATE,
        ctrl_status: TD_STATUS_ACTIVE | TD_STATUS_CERR_ONE | TD_STATUS_LOW_SPEED,
        token: make_token(PID_IN, uhci.device_address, 1, uhci.toggle, 8),
        buffer: buf_phys,
    };
    uhci.qh.element_link = phys_of(&*uhci.int_td);
}

fn usage_to_char(usage: u8, shift: bool) -> Option<char> {
    const DIGITS: [char; 10] = ['1', '2', '3', '4', '5', '6', '7', '8', '9', '0'];
    const DIGITS_SHIFTED: [char; 10] = ['!', '@', '#', '$', '%', '^', '&', '*', '(', ')'];
    let pick = |plain: char, shifted: char| Some(if shift { shifted } else { plain });
    match usage {
        0x04..=0x1D => {
            let letter = b'a' + (usage - 0x04);
            let letter = if shift { letter.to_ascii_uppercase() } else { letter };
            Some(letter as char)
        }
        0x1E..=0x27 => {
            let i = usize::from(usage - 0x1E);
            Some(if shift { DIGITS_SHIFTED[i] } else { DIGITS[i] })
        }
        0x28 => Some('\n'),
        0x2A => Some('\u{8}'),
        0x2B => Some('\t'),
        0x2C => Some(' '),
        0x2D => pick('-', '_'),
        0x2E => pick('=', '+'),
        0x2F => pick('[', '{'),
        0x30 => pick(']', '}'),
        0x31 => pick('\\', '|'),
        0x33 => pick(';', ':'),
        0x34 => pick('\'', '"'),
        0x35 => pick('`', '~'),
        0x36 => pick(',', '<'),
        0x37 => pick('.', '>'),
        0x38 => pick('/', '?'),
        _ => None,
    }
}

pub fn poll_keyboard() -> ! {
    let mut last_report = [0u8; 8];
    loop {
        crate::scheduler::sleep_ticks(1);
        if !READY.load(Ordering::SeqCst) {
            continue;
        }
        let Some(uhci) = (unsafe { (*&raw mut UHCI).as_mut() }) else {
            continue;
        };

        let active = uhci.int_td.ctrl_status & TD_STATUS_ACTIVE != 0;
        let errored = uhci.int_td.ctrl_status & TD_STATUS_ERROR_MASK != 0;
        if active {
            continue;
        }
        if errored {
            uhci.toggle = !uhci.toggle;
            arm_interrupt_transfer(uhci);
            continue;
        }

        let report = *uhci.report_buf;
        if report != last_report {
            let shift = report[0] & 0x22 != 0;
            for &usage in &report[2..8] {
                if usage == 0 {
                    continue;
                }
                if last_report[2..8].contains(&usage) {
                    continue;
                }
                if let Some(c) = usage_to_char(usage, shift) {
                    crate::keyboard::push_char(c);
                }
            }
            last_report = report;
        }

        uhci.toggle = !uhci.toggle;
        arm_interrupt_transfer(uhci);
    }
}

pub fn port_status() -> Option<[u16; 2]> {
    let dev = pci::find_device(PIIX3_USB_VENDOR, PIIX3_USB_DEVICE)?;
    let io_base = dev.io_bar(4)?;
    let mut p1: Port<u16> = Port::new(io_base + REG_PORTSC1);
    let mut p2: Port<u16> = Port::new(io_base + REG_PORTSC2);
    Some([unsafe { p1.read() }, unsafe { p2.read() }])
}

pub fn keyboard_ready() -> bool {
    READY.load(Ordering::SeqCst)
}
