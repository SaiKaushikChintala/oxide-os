use core::fmt::Write as _;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

use crate::{gdt, percpu, print, println, scheduler, serial};

pub const SYS_WRITE: u64 = 1;
pub const SYS_EXIT: u64 = 2;
pub const SYS_GETPID: u64 = 3;
pub const SYS_SLEEP: u64 = 4;
pub const SYS_READ: u64 = 5;
pub const SYS_MMAP: u64 = 6;
pub const SYS_WAIT: u64 = 7;
pub const SYS_KILL: u64 = 8;
pub const SYS_FORK: u64 = 9;
pub const SYS_EXEC: u64 = 10;

pub const ENOSYS: u64 = u64::MAX;
pub const EFAULT: u64 = u64::MAX - 1;

pub const FD_STDIN: u64 = 0;
pub const FD_STDOUT: u64 = 1;
pub const FD_PIPE_READ: u64 = 3;
pub const FD_PIPE_WRITE: u64 = 4;

pub fn init() {
    let s = gdt::selectors();
    Star::write(
        s.user_code_selector,
        s.user_data_selector,
        s.kernel_code_selector,
        s.kernel_data_selector,
    )
    .expect("GDT layout must satisfy SYSCALL/SYSRET selector offsets");

    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG);
    unsafe {
        Efer::update(|flags| *flags |= EferFlags::SYSTEM_CALL_EXTENSIONS);
    }
}

#[unsafe(naked)]
unsafe extern "C" fn syscall_entry() {
    core::arch::naked_asm!(
        "cli",
        "mov gs:[{user_rsp_off}], rsp",
        "mov rsp, gs:[{stack_top_off}]",
        "push rcx",
        "push r11",
        "mov gs:[{user_rip_off}], rcx",
        "mov rcx, rdx",
        "mov rdx, rsi",
        "mov rsi, rdi",
        "mov rdi, rax",
        "call {dispatch}",
        "pop r11",
        "pop rcx",
        "mov rsp, gs:[{user_rsp_off}]",
        "sysretq",
        user_rsp_off = const percpu::USER_RSP_OFFSET,
        user_rip_off = const percpu::USER_RIP_OFFSET,
        stack_top_off = const percpu::SYSCALL_STACK_TOP_OFFSET,
        dispatch = sym syscall_dispatch,
    )
}

extern "C" fn syscall_dispatch(nr: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    match nr {
        SYS_WRITE => sys_write(arg1, arg2, arg3),
        SYS_EXIT => sys_exit(arg1),
        SYS_GETPID => sys_getpid(),
        SYS_SLEEP => sys_sleep(arg1),
        SYS_READ => sys_read(arg1, arg2, arg3),
        SYS_MMAP => ENOSYS,
        SYS_WAIT => scheduler::wait_for(arg1),
        SYS_KILL => u64::from(scheduler::kill(arg1)),
        SYS_FORK => sys_fork(),
        SYS_EXEC => sys_exec(arg1),
        _ => ENOSYS,
    }
}

fn user_range_is_valid(ptr: u64, len: u64) -> bool {
    let Some(end) = ptr.checked_add(len) else {
        return false;
    };
    crate::usermode::user_region_contains(ptr, end)
}

fn sys_write(fd: u64, ptr: u64, len: u64) -> u64 {
    if !user_range_is_valid(ptr, len) {
        let _ = writeln!(serial(), "[syscall] rejected write() outside user region");
        return EFAULT;
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };

    match fd {
        FD_STDOUT => {
            let s = core::str::from_utf8(bytes).unwrap_or("<invalid utf8>");
            print!("{s}");
            let _ = write!(serial(), "{s}");
            len
        }
        FD_PIPE_WRITE => {
            crate::pipe::write(bytes);
            len
        }
        _ => ENOSYS,
    }
}

fn sys_read(fd: u64, ptr: u64, len: u64) -> u64 {
    if !user_range_is_valid(ptr, len) {
        let _ = writeln!(serial(), "[syscall] rejected read() outside user region");
        return EFAULT;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) };

    match fd {
        FD_STDIN => {
            let mut n = 0;
            while n < buf.len() {
                match crate::keyboard::try_pop_char() {
                    Some(c) if c.is_ascii() => {
                        buf[n] = c as u8;
                        n += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                    Some(_) => {}
                    None if n > 0 => break,
                    None => scheduler::schedule(),
                }
            }
            n as u64
        }
        FD_PIPE_READ => crate::pipe::read(buf) as u64,
        _ => ENOSYS,
    }
}

fn sys_exit(code: u64) -> ! {
    println!("[process] exited with code {code}");
    let _ = writeln!(serial(), "[process] exited with code {code}");
    scheduler::exit_current_task(code)
}

fn sys_getpid() -> u64 {
    scheduler::current_task_id()
}

fn sys_sleep(ticks: u64) -> u64 {
    scheduler::sleep_ticks(ticks);
    0
}

fn sys_fork() -> u64 {
    let parent_rip = percpu::user_rip_scratch();
    let parent_rsp = percpu::user_rsp_scratch();

    let child_space = scheduler::with_current_address_space(|space| space.fork());
    let Some(child_space) = child_space else {
        return ENOSYS;
    };

    scheduler::spawn_forked(
        crate::usermode::fork_child_entry,
        child_space,
        (parent_rip, parent_rsp),
    )
}

fn sys_exec(program_id: u64) -> u64 {
    let (name, stack_top) = match program_id {
        0 => ("init", crate::loader::INIT_STACK_TOP),
        1 => ("producer", crate::loader::PRODUCER_STACK_TOP),
        2 => ("consumer", crate::loader::CONSUMER_STACK_TOP),
        _ => return ENOSYS,
    };
    let initrd = crate::initrd::Initrd::build();
    let Some(bytes) = crate::initrd::FileSystem::read(&initrd, name) else {
        return ENOSYS;
    };
    crate::loader::load_and_exec(&bytes, stack_top, 0)
}
