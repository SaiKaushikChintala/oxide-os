use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

pub const TIMER_HZ: u32 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);

const PIT_FREQUENCY: u32 = 1_193_182;

pub fn init() {
    let divisor = PIT_FREQUENCY / TIMER_HZ;

    let mut command: Port<u8> = Port::new(0x43);
    let mut channel0: Port<u8> = Port::new(0x40);
    unsafe {
        command.write(0x36u8);
        channel0.write((divisor & 0xff) as u8);
        channel0.write((divisor >> 8) as u8);
    }
}

pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
