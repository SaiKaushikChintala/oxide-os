use core::fmt::Write as _;
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::gdt;
use crate::println;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> =
    Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault
            .set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);

        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }

        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        idt[crate::smp::LAPIC_TIMER_VECTOR].set_handler_fn(lapic_timer_interrupt_handler);

        for vector in PIC_1_OFFSET..=(PIC_2_OFFSET + 7) {
            if vector != InterruptIndex::Timer.as_u8() && vector != InterruptIndex::Keyboard.as_u8() {
                idt[vector].set_handler_fn(spurious_interrupt_handler);
            }
        }

        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

pub fn load_idt_on_ap() {
    IDT.load();
}

pub unsafe fn init_pics() {
    unsafe {
        PICS.lock().initialize();
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::timer::tick();
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
    crate::scheduler::schedule();
}

extern "x86-interrupt" fn lapic_timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::smp::lapic_eoi();
    crate::scheduler::schedule();
}

extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let _ = writeln!(crate::serial(), "[interrupts] spurious/unhandled PIC interrupt");
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port: Port<u8> = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    crate::keyboard::handle_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    panic!("EXCEPTION: DIVIDE ERROR\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    panic!("EXCEPTION: INVALID OPCODE\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    if is_user_mode(&stack_frame) {
        println!(
            "[fault] user process hit a general protection fault (error code {error_code:#x}) - killing it"
        );
        let _ = writeln!(
            crate::serial(),
            "[fault] user process GP fault (error code {error_code:#x}), killing it"
        );
        crate::scheduler::exit_current_task(crate::scheduler::KILLED);
    }
    panic!(
        "EXCEPTION: GENERAL PROTECTION FAULT\nerror code: {:#x}\n{:#?}",
        error_code, stack_frame
    );
}

fn is_user_mode(stack_frame: &InterruptStackFrame) -> bool {
    stack_frame.code_segment.0 & 0b11 == 3
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    let Ok(addr) = Cr2::read() else {
        panic!("EXCEPTION: PAGE FAULT\nCR2 held a non-canonical address\n{stack_frame:#?}");
    };

    if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        let handled =
            crate::scheduler::with_current_address_space(|space| space.resolve_cow_fault(addr));
        if handled == Some(true) {
            return;
        }
    }

    if is_user_mode(&stack_frame) {
        println!(
            "[fault] user process hit a page fault at {addr:?} (error code {error_code:?}) - killing it"
        );
        let _ = writeln!(
            crate::serial(),
            "[fault] user process page fault at {addr:?} ({error_code:?}), killing it"
        );
        crate::scheduler::exit_current_task(crate::scheduler::KILLED);
    }

    panic!("EXCEPTION: PAGE FAULT\naccessed address: {addr:?}\nerror code: {error_code:?}\n{stack_frame:#?}");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:#?}", stack_frame);
}
