use x86_64::VirtAddr;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

use crate::{gdt, scheduler};

const USER_CODE_ADDR: u64 = 0x5555_5555_0000;
const USER_STACK_TOP: u64 = 0x6666_6666_0000;

#[rustfmt::skip]
static USER_PROGRAM: [u8; 74] = [
    0xBB, 0x03, 0x00, 0x00, 0x00,
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x48, 0x8D, 0x35, 0x21, 0x00, 0x00, 0x00,
    0xBA, 0x13, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x04, 0x00, 0x00, 0x00,
    0xBF, 0x64, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xFF, 0xCB,
    0x75, 0xD8,
    0xFA,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x31, 0xFF,
    0x0F, 0x05,
    b'H', b'e', b'l', b'l', b'o', b' ', b'f', b'r', b'o', b'm', b' ',
    b'r', b'i', b'n', b'g', b' ', b'3', b'!', b'\n',
];

pub fn user_region_contains(start: u64, end: u64) -> bool {
    scheduler::with_current_address_space(|space| space.contains(start, end)).unwrap_or(false)
}

fn map_and_register(page: Page<Size4KiB>, flags: PageTableFlags, region: (u64, u64)) {
    scheduler::with_current_address_space(|space| {
        space.map_page(page, flags).expect("failed to map process page");
        space.register_region(region.0, region.1);
    })
    .expect("this must run as a process with its own address space");
}

fn setup() -> (u64, u64) {
    let code_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(USER_CODE_ADDR));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::USER_ACCESSIBLE;
    map_and_register(code_page, flags, (USER_CODE_ADDR, USER_STACK_TOP));

    let stack_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(USER_STACK_TOP - 1));
    map_and_register(stack_page, flags, (USER_STACK_TOP - 4096, USER_STACK_TOP));

    unsafe {
        core::ptr::copy_nonoverlapping(
            USER_PROGRAM.as_ptr(),
            USER_CODE_ADDR as *mut u8,
            USER_PROGRAM.len(),
        );
    }

    (USER_CODE_ADDR, USER_STACK_TOP & !0xf)
}

pub fn launch() -> ! {
    let (entry, stack) = setup();
    jump_to_ring3(entry, stack, 0);
}

pub fn fork_child_entry() -> ! {
    let (rip, rsp) = scheduler::take_current_fork_resume()
        .expect("fork_child_entry must run as a task with a pending fork_resume");
    jump_to_ring3(rip, rsp, 0);
}

pub fn jump_to_ring3(entry: u64, user_stack: u64, initial_rax: u64) -> ! {
    let s = gdt::selectors();
    let user_cs = u64::from(s.user_code_selector.0);
    let user_ss = u64::from(s.user_data_selector.0);
    unsafe {
        enter_usermode(entry, user_stack, user_cs, user_ss, initial_rax);
    }
}

#[unsafe(naked)]
unsafe extern "C" fn enter_usermode(
    entry: u64,
    user_stack: u64,
    user_cs: u64,
    user_ss: u64,
    initial_rax: u64,
) -> ! {
    core::arch::naked_asm!(
        "cli",
        "mov rax, r8",
        "push rcx",
        "push rsi",
        "push {rflags}",
        "push rdx",
        "push rdi",
        "iretq",
        rflags = const 0x202u64,
    )
}
