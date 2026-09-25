use core::fmt::Write as _;
use x86_64::VirtAddr;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

use crate::initrd::{FileSystem, Initrd};
use crate::memory::AddressSpace;
use crate::scheduler;
use crate::{elf, serial, usermode};

pub const INIT_STACK_TOP: u64 = 0x7aaa_aaaa_0000;
pub const PRODUCER_STACK_TOP: u64 = 0x1aaa_aaaa_0000;
pub const CONSUMER_STACK_TOP: u64 = 0x1bbb_bbbb_0000;
pub const FORKER_STACK_TOP: u64 = 0x4aaa_aaaa_0000;
pub const HELLO_STACK_TOP: u64 = 0x6aaa_aaaa_0000;
pub const PIE_DEMO_STACK_TOP: u64 = 0x3aaa_aaaa_0000;
pub const PIE_DEMO_LOAD_BASE: u64 = 0x3000_0000_0000;

pub fn run_init() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("init").expect("initrd missing 'init'");
    load_and_exec(&bytes, INIT_STACK_TOP, 0);
}

fn run_producer() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("producer").expect("initrd missing 'producer'");
    load_and_exec(&bytes, PRODUCER_STACK_TOP, 0);
}

fn run_consumer() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("consumer").expect("initrd missing 'consumer'");
    load_and_exec(&bytes, CONSUMER_STACK_TOP, 0);
}

pub fn run_forker() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("forker").expect("initrd missing 'forker'");
    load_and_exec(&bytes, FORKER_STACK_TOP, 0);
}

pub fn run_hello() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("hello").expect("initrd missing 'hello'");
    load_and_exec(&bytes, HELLO_STACK_TOP, 0);
}

pub fn run_pie_demo() -> ! {
    let initrd = Initrd::build();
    let bytes = initrd.read("pie_demo").expect("initrd missing 'pie_demo'");
    load_and_exec(&bytes, PIE_DEMO_STACK_TOP, PIE_DEMO_LOAD_BASE);
}

pub fn run_pipeline() -> ! {
    let producer = scheduler::spawn_process(run_producer, AddressSpace::new());
    let code = scheduler::wait_for(producer);
    writeln!(serial(), "[pipeline] producer (pid {producer}) exited with code {code}").unwrap();

    let consumer = scheduler::spawn_process(run_consumer, AddressSpace::new());
    let code = scheduler::wait_for(consumer);
    writeln!(serial(), "[pipeline] consumer (pid {consumer}) exited with code {code}").unwrap();

    loop {
        x86_64::instructions::hlt();
    }
}

pub fn load_and_exec(elf_bytes: &[u8], user_stack_top: u64, base: u64) -> ! {
    let elf = elf::ElfFile::parse(elf_bytes).expect("invalid ELF file");
    let bias = if elf.e_type() == elf::ET_DYN { base } else { 0 };
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;

    for seg in elf.load_segments() {
        let vaddr = seg.vaddr + bias;
        let start_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(vaddr));
        let end_addr = vaddr + seg.memsz.max(1) - 1;
        let end_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(end_addr));

        scheduler::with_current_address_space(|space| {
            for page in Page::range_inclusive(start_page, end_page) {
                match space.map_page(page, flags) {
                    Ok(_) | Err(MapToError::PageAlreadyMapped(_)) => {}
                    Err(e) => panic!("failed to map ELF segment page: {e:?}"),
                }
            }
            space.register_region(vaddr, vaddr + seg.memsz);
        })
        .expect("load_and_exec must run as a process with its own address space");

        let data = elf.segment_data(seg);
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), vaddr as *mut u8, data.len());
            if seg.memsz > seg.filesz {
                let bss_start = (vaddr + seg.filesz) as *mut u8;
                core::ptr::write_bytes(bss_start, 0, (seg.memsz - seg.filesz) as usize);
            }
        }
    }

    if let Some(dynamic_vaddr) = elf.dynamic_vaddr() {
        unsafe {
            elf::apply_relative_relocations(dynamic_vaddr + bias, bias);
        }
    }

    let stack_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(user_stack_top - 1));
    scheduler::with_current_address_space(|space| {
        space
            .map_page(stack_page, flags)
            .expect("failed to map process stack page");
        space.register_region(user_stack_top - 4096, user_stack_top);
    })
    .expect("load_and_exec must run as a process with its own address space");

    usermode::jump_to_ring3(elf.entry_point() + bias, user_stack_top & !0xf, 0);
}
