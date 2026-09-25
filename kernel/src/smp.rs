use alloc::boxed::Box;
use core::arch::x86_64::__cpuid;
use core::fmt::Write as _;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::{acpi, gdt, interrupts, memory, percpu, scheduler, serial};

pub const TRAMPOLINE_PHYS: u64 = 0x8000;

pub const MAX_CPUS: usize = 8;

pub fn initial_apic_id() -> u32 {
    let result = __cpuid(1);
    result.ebx >> 24
}

unsafe extern "C" {
    static trampoline_start: u8;
    static trampoline_end: u8;
    static trampoline_cr3: u8;
    static trampoline_stack_top: u8;
    static trampoline_entry: u8;
}

core::arch::global_asm!(include_str!("smp_trampoline.s"));

const LAPIC_ID: u32 = 0x020;
const LAPIC_ICR_LOW: u32 = 0x300;
const LAPIC_ICR_HIGH: u32 = 0x310;

const ICR_DELIVERY_INIT: u32 = 0b101 << 8;
const ICR_DELIVERY_STARTUP: u32 = 0b110 << 8;
const ICR_LEVEL_ASSERT: u32 = 1 << 14;
const ICR_DELIVERY_STATUS_PENDING: u32 = 1 << 12;

static APS_STARTED: AtomicU32 = AtomicU32::new(0);
static LAPIC_VIRT: AtomicU64 = AtomicU64::new(0);

fn spin_delay(iterations: u64) {
    for i in 0..iterations {
        core::hint::black_box(i);
    }
}

fn lapic_virt() -> VirtAddr {
    VirtAddr::new(LAPIC_VIRT.load(Ordering::Relaxed))
}

fn lapic_read(offset: u32) -> u32 {
    unsafe { ((lapic_virt().as_u64() + u64::from(offset)) as *const u32).read_volatile() }
}

fn lapic_write(offset: u32, value: u32) {
    unsafe { ((lapic_virt().as_u64() + u64::from(offset)) as *mut u32).write_volatile(value) };
}

pub const LAPIC_TIMER_VECTOR: u8 = 0x40;
const LAPIC_TIMER_LVT: u32 = 0x320;
const LAPIC_TIMER_INITIAL_COUNT: u32 = 0x380;
const LAPIC_TIMER_DIVIDE_CONFIG: u32 = 0x3E0;
const LAPIC_EOI: u32 = 0x0B0;
const LAPIC_TIMER_PERIODIC: u32 = 1 << 17;

fn setup_lapic_timer() {
    lapic_write(LAPIC_TIMER_DIVIDE_CONFIG, 0b1011);
    lapic_write(LAPIC_TIMER_LVT, LAPIC_TIMER_PERIODIC | u32::from(LAPIC_TIMER_VECTOR));
    lapic_write(LAPIC_TIMER_INITIAL_COUNT, 10_000_000);
}

pub fn lapic_prepared() -> bool {
    LAPIC_VIRT.load(Ordering::Relaxed) != 0
}

pub fn halt_other_cores() {
    if !lapic_prepared() {
        return;
    }
    const ICR_DEST_ALL_EXCLUDING_SELF: u32 = 0b11 << 18;
    const ICR_DELIVERY_NMI: u32 = 0b100 << 8;
    lapic_write(
        LAPIC_ICR_LOW,
        ICR_DEST_ALL_EXCLUDING_SELF | ICR_DELIVERY_NMI | ICR_LEVEL_ASSERT,
    );
}

pub fn lapic_eoi() {
    lapic_write(LAPIC_EOI, 0);
}

fn send_ipi(apic_id: u8, icr_low: u32) {
    lapic_write(LAPIC_ICR_HIGH, u32::from(apic_id) << 24);
    lapic_write(LAPIC_ICR_LOW, icr_low);
    for _ in 0..100_000 {
        if lapic_read(LAPIC_ICR_LOW) & ICR_DELIVERY_STATUS_PENDING == 0 {
            break;
        }
        core::hint::spin_loop();
    }
}

fn prepare(lapic_phys: u64) {
    let frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(lapic_phys));
    let page = Page::<Size4KiB>::containing_address(VirtAddr::new(lapic_phys));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    memory::map_page_to_frame(page, frame, flags).expect("failed to map local APIC MMIO page");
    LAPIC_VIRT.store(lapic_phys, Ordering::Relaxed);

    let trampoline_frame = PhysFrame::<Size4KiB>::containing_address(PhysAddr::new(TRAMPOLINE_PHYS));
    let trampoline_page = Page::<Size4KiB>::containing_address(VirtAddr::new(TRAMPOLINE_PHYS));
    let trampoline_flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    memory::map_page_to_frame(trampoline_page, trampoline_frame, trampoline_flags)
        .expect("failed to identity-map the AP trampoline page");

    let trampoline_len = (&raw const trampoline_end as u64) - (&raw const trampoline_start as u64);
    let dest = memory::phys_to_virt(PhysAddr::new(TRAMPOLINE_PHYS));
    unsafe {
        core::ptr::copy_nonoverlapping(
            &raw const trampoline_start as *const u8,
            dest.as_u64() as *mut u8,
            trampoline_len as usize,
        );
    }
}

fn trampoline_field_offset(field: *const u8) -> u64 {
    (field as u64) - (&raw const trampoline_start as u64)
}

fn write_trampoline_data(cr3: u64, stack_top: u64, entry: u64) {
    let base = memory::phys_to_virt(PhysAddr::new(TRAMPOLINE_PHYS)).as_u64();
    unsafe {
        let cr3_off = trampoline_field_offset(&raw const trampoline_cr3);
        let stack_off = trampoline_field_offset(&raw const trampoline_stack_top);
        let entry_off = trampoline_field_offset(&raw const trampoline_entry);
        ((base + cr3_off) as *mut u64).write_volatile(cr3);
        ((base + stack_off) as *mut u64).write_volatile(stack_top);
        ((base + entry_off) as *mut u64).write_volatile(entry);
    }
}

pub fn start_aps() -> usize {
    let Some(lapic_phys) = acpi::local_apic_phys() else {
        let _ = writeln!(serial(), "[smp] no MADT/local APIC found; running single-CPU");
        return 0;
    };
    let ids = acpi::cpu_apic_ids();
    if ids.len() <= 1 {
        let _ = writeln!(serial(), "[smp] MADT reports {} CPU(s); nothing to start", ids.len());
        return 0;
    }

    prepare(lapic_phys);
    let bsp_id = (lapic_read(LAPIC_ID) >> 24) as u8;
    let (cr3, _) = x86_64::registers::control::Cr3::read();
    let cr3_phys = cr3.start_address().as_u64();

    let mut started = 0;
    for &apic_id in ids.iter().filter(|&&id| id != bsp_id) {
        let before = APS_STARTED.load(Ordering::SeqCst);

        let stack = alloc::vec![0u8; 16 * 1024].into_boxed_slice();
        let stack_top = (Box::leak(stack).as_ptr() as u64 + 16 * 1024) & !0xf;
        write_trampoline_data(cr3_phys, stack_top, ap_entry as *const () as u64);

        send_ipi(apic_id, ICR_DELIVERY_INIT | ICR_LEVEL_ASSERT);
        spin_delay(10_000_000);
        send_ipi(apic_id, ICR_DELIVERY_INIT);
        spin_delay(10_000_000);

        let vector = (TRAMPOLINE_PHYS >> 12) as u32;
        send_ipi(apic_id, ICR_DELIVERY_STARTUP | vector);
        spin_delay(2_000_000);
        send_ipi(apic_id, ICR_DELIVERY_STARTUP | vector);

        let mut waited = 0;
        while APS_STARTED.load(Ordering::SeqCst) == before && waited < 200 {
            spin_delay(5_000_000);
            waited += 1;
        }

        if APS_STARTED.load(Ordering::SeqCst) > before {
            started += 1;
        } else {
            let _ = writeln!(serial(), "[smp] CPU (APIC id {apic_id}) did not respond to SIPI");
        }
    }

    started
}

extern "C" fn ap_entry() -> ! {
    let tss_ptr = gdt::init_on_ap();
    interrupts::load_idt_on_ap();
    let apic_id = lapic_read(LAPIC_ID) >> 24;
    percpu::init_this_cpu(apic_id, tss_ptr);
    scheduler::init_this_cpu();

    let n = APS_STARTED.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = writeln!(serial(), "[smp] AP #{n} alive (APIC id {apic_id}), joining scheduler");

    setup_lapic_timer();
    x86_64::instructions::interrupts::enable();

    loop {
        x86_64::instructions::hlt();
    }
}
