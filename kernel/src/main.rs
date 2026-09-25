#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::{entry_point, BootInfo};
use core::fmt::Write;
use uart_16550::backend::PioBackend;
use uart_16550::{Config, Uart16550Tty};
use x86_64::VirtAddr;
use x86_64::structures::paging::Page;

mod acpi;
mod allocator;
mod devfs;
mod diskfs;
mod elf;
mod framebuffer;
mod gdt;
mod initrd;
mod interrupts;
mod keyboard;
mod loader;
mod memory;
mod net;
mod pci;
mod percpu;
mod pipe;
mod scheduler;
mod shell;
mod smp;
mod syscall;
mod task;
mod tcp;
mod timer;
mod usb;
mod usermode;
mod virtio_blk;
mod virtio_net;

pub static BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use x86_64::instructions::{nop, port::Port};

    unsafe {
        let mut port = Port::new(0xf4);
        port.write(exit_code as u32);
    }

    loop {
        nop();
    }
}

pub struct SerialGuard {
    guard: Option<spin::MutexGuard<'static, Option<Uart16550Tty<PioBackend>>>>,
    interrupts_were_enabled: bool,
}

impl Drop for SerialGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        if self.interrupts_were_enabled {
            x86_64::instructions::interrupts::enable();
        }
    }
}

impl Write for SerialGuard {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let guard = self.guard.as_mut().expect("held until dropped");
        guard.as_mut().expect("initialized just above in serial()").write_str(s)
    }
}

pub fn serial() -> SerialGuard {
    let interrupts_were_enabled = x86_64::instructions::interrupts::are_enabled();
    if interrupts_were_enabled {
        x86_64::instructions::interrupts::disable();
    }

    static SERIAL: spin::Mutex<Option<Uart16550Tty<PioBackend>>> = spin::Mutex::new(None);
    let mut guard = SERIAL.lock();
    if guard.is_none() {
        *guard = Some(
            unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) }
                .expect("should initialize serial device from valid config and valid port"),
        );
    }
    SerialGuard { guard: Some(guard), interrupts_were_enabled }
}

entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    let physical_memory_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader did not map physical memory");
    let rsdp_addr = boot_info.rsdp_addr.into_option();

    let fb = boot_info
        .framebuffer
        .take()
        .expect("bootloader did not provide a framebuffer");
    let fb_info = fb.info();
    framebuffer::init(fb.into_buffer(), fb_info);

    let bsp_tss_ptr = gdt::init();
    interrupts::init_idt();

    println!("Oxide kernel booted.");
    writeln!(serial(), "Oxide kernel booted.").unwrap();

    x86_64::instructions::interrupts::int3();
    println!("Survived a breakpoint exception, IDT is working.");
    writeln!(serial(), "Survived a breakpoint exception, IDT is working.").unwrap();

    unsafe {
        memory::init_globals(VirtAddr::new(physical_memory_offset), &boot_info.memory_regions);
    }

    allocator::init_heap().expect("heap initialization failed");
    println!("Heap initialized.");
    writeln!(serial(), "Heap initialized.").unwrap();

    percpu::init_this_cpu(smp::initial_apic_id(), bsp_tss_ptr);

    let boxed = Box::new(41);
    let mut vec = Vec::new();
    for i in 0..10 {
        vec.push(i);
    }
    let s = String::from("hello from the heap");
    println!("boxed={boxed}, vec={vec:?}, s={s}");
    writeln!(serial(), "boxed={boxed}, vec={vec:?}, s={s}").unwrap();

    let stack_bottom = VirtAddr::new(boot_info.kernel_stack_bottom);
    let guard_page: Page<x86_64::structures::paging::Size4KiB> =
        Page::containing_address(stack_bottom - 1u64);
    if memory::unmap_page(guard_page) {
        println!("Stack guard page installed at {guard_page:?}");
        writeln!(serial(), "Stack guard page installed at {guard_page:?}").unwrap();
    } else {
        println!("Stack guard page already unmapped by bootloader");
        writeln!(serial(), "Stack guard page already unmapped by bootloader").unwrap();
    }

    acpi::init(rsdp_addr);
    let acpi_msg = if acpi::available() {
        "PM1a control block found (shutdown/reboot enabled)"
    } else {
        "no usable tables found (reboot still available)"
    };
    println!("ACPI: {acpi_msg}");
    writeln!(serial(), "ACPI: {acpi_msg}").unwrap();

    let aps_started = smp::start_aps();
    println!("SMP: {} application processor(s) started", aps_started);
    writeln!(serial(), "SMP: {aps_started} application processor(s) started").unwrap();

    match usb::init() {
        Some(connected) => {
            let kb = if usb::keyboard_ready() { "HID keyboard bound" } else { "no HID keyboard bound" };
            println!("USB: UHCI controller found, {connected} port(s) connected, {kb}");
            writeln!(serial(), "USB: UHCI controller found, {connected} port(s) connected, {kb}").unwrap();
        }
        None => {
            println!("USB: no UHCI controller found");
            writeln!(serial(), "USB: no UHCI controller found").unwrap();
        }
    }

    if virtio_blk::init() {
        let sectors = virtio_blk::capacity_sectors().unwrap_or(0);
        println!("Disk: virtio-blk found, {sectors} sectors ({} KiB)", sectors * 512 / 1024);
        writeln!(serial(), "Disk: virtio-blk found, {sectors} sectors").unwrap();

        match diskfs::list() {
            Some(files) => {
                writeln!(serial(), "Disk: OXFS found, {} file(s)", files.len()).unwrap();
                for f in &files {
                    writeln!(serial(), "Disk:   {} ({} bytes)", f.name, f.size_bytes).unwrap();
                }
                if let Some(bytes) = diskfs::read("hello.txt") {
                    let text = core::str::from_utf8(&bytes).unwrap_or("<invalid utf8>");
                    writeln!(serial(), "Disk: read hello.txt: {text:?}").unwrap();
                }

                if diskfs::write("boot.log", b"Oxide booted and wrote this file to disk.\n") {
                    if let Some(bytes) = diskfs::read("boot.log") {
                        let text = core::str::from_utf8(&bytes).unwrap_or("<invalid utf8>");
                        writeln!(serial(), "Disk: wrote + read back boot.log: {text:?}").unwrap();
                    }
                } else {
                    writeln!(serial(), "Disk: write self-test failed").unwrap();
                }
            }
            None => writeln!(serial(), "Disk: no OXFS filesystem found").unwrap(),
        }
    } else {
        println!("Disk: no virtio-blk device found");
        writeln!(serial(), "Disk: no virtio-blk device found").unwrap();
    }

    scheduler::init();
    scheduler::spawn(counter_thread_a);
    scheduler::spawn(counter_thread_b);
    syscall::init();
    scheduler::spawn_process(usermode::launch, memory::AddressSpace::new());
    scheduler::spawn_process(loader::run_init, memory::AddressSpace::new());
    scheduler::spawn_process(loader::run_forker, memory::AddressSpace::new());
    scheduler::spawn_process(loader::run_hello, memory::AddressSpace::new());
    scheduler::spawn_process(loader::run_pie_demo, memory::AddressSpace::new());
    scheduler::spawn(loader::run_pipeline);
    scheduler::spawn(net::run);
    scheduler::spawn(usb::poll_keyboard);

    unsafe {
        interrupts::init_pics();
    }
    timer::init();
    x86_64::instructions::interrupts::enable();
    println!("Interrupts enabled: timer + keyboard.");
    writeln!(serial(), "Interrupts enabled: timer + keyboard.").unwrap();
    println!("Scheduler running two demo kernel threads (see serial log).");
    println!("A ring-3 user process is also running via real syscalls.");

    println!("Type 'help' for a list of commands.");
    shell::run();
}

fn counter_thread_a() -> ! {
    let mut n: u64 = 0;
    loop {
        writeln!(serial(), "[thread A] tick {n}").unwrap();
        n += 1;
        scheduler::sleep_ticks(50);
    }
}

fn counter_thread_b() -> ! {
    let mut n: u64 = 0;
    loop {
        writeln!(serial(), "[thread B] tick {n}").unwrap();
        n += 1;
        scheduler::sleep_ticks(70);
    }
}

#[alloc_error_handler]
fn alloc_error_handler(layout: core::alloc::Layout) -> ! {
    panic!("allocation error: {layout:?}")
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    smp::halt_other_cores();
    println!("KERNEL PANIC: {info}");
    let _ = writeln!(serial(), "KERNEL PANIC: {info}");
    loop {
        x86_64::instructions::hlt();
    }
}
