#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

const SYS_WRITE: u64 = 1;
const SYS_EXIT: u64 = 2;
const SYS_GETPID: u64 = 3;
const SYS_WAIT: u64 = 7;
const SYS_FORK: u64 = 9;

unsafe fn syscall3(num: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let ret: u64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") num => ret,
            in("rdi") arg1,
            in("rsi") arg2,
            in("rdx") arg3,
            clobber_abi("C"),
            options(nostack),
        );
    }
    ret
}

fn write(fd: u64, bytes: &[u8]) {
    unsafe { syscall3(SYS_WRITE, fd, bytes.as_ptr() as u64, bytes.len() as u64) };
}

fn getpid() -> u64 {
    unsafe { syscall3(SYS_GETPID, 0, 0, 0) }
}

fn exit(code: u64) -> ! {
    unsafe { syscall3(SYS_EXIT, code, 0, 0) };
    unreachable!("SYS_EXIT never returns")
}

fn fork() -> u64 {
    unsafe { syscall3(SYS_FORK, 0, 0, 0) }
}

fn wait(pid: u64) -> u64 {
    unsafe { syscall3(SYS_WAIT, pid, 0, 0) }
}

fn write_decimal(fd: u64, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    write(fd, &buf[i..]);
}

fn write_hex(fd: u64, n: u64) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 16];
    for (i, slot) in buf.iter_mut().enumerate() {
        let shift = (15 - i) * 4;
        *slot = DIGITS[((n >> shift) & 0xf) as usize];
    }
    write(fd, &buf);
}

const STDOUT: u64 = 1;

static mut SHARED: u64 = 0xAAAA_AAAA_AAAA_AAAA;

#[unsafe(naked)]
#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    core::arch::naked_asm!("call {main}", main = sym rust_main)
}

extern "C" fn rust_main() -> ! {
    write(STDOUT, b"hello from a REAL compiled Rust userspace program!\n");
    let pid = getpid();
    write(STDOUT, b"my pid is ");
    write_decimal(STDOUT, pid);
    write(STDOUT, b"\n");

    let child_pid = fork();
    if child_pid == 0 {
        unsafe { core::ptr::write_volatile(&raw mut SHARED, 0xC0C0_C0C0_C0C0_C0C0) };
        let v = unsafe { core::ptr::read_volatile(&raw const SHARED) };
        write(STDOUT, b"child: SHARED after my write = 0x");
        write_hex(STDOUT, v);
        write(STDOUT, b"\n");
        exit(0);
    }

    wait(child_pid);
    let before = unsafe { core::ptr::read_volatile(&raw const SHARED) };
    write(STDOUT, b"parent: SHARED before my write  = 0x");
    write_hex(STDOUT, before);
    write(STDOUT, b"\n");
    unsafe { core::ptr::write_volatile(&raw mut SHARED, 0xF00D_F00D_F00D_F00D) };
    let after = unsafe { core::ptr::read_volatile(&raw const SHARED) };
    write(STDOUT, b"parent: SHARED after my write   = 0x");
    write_hex(STDOUT, after);
    write(STDOUT, b"\n");
    exit(0);
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    exit(1);
}
