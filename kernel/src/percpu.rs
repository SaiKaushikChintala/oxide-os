use alloc::boxed::Box;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::GsBase;

#[repr(C)]
pub struct PerCpu {
    self_ptr: u64,
    pub syscall_stack_top: u64,
    user_rsp_scratch: u64,
    user_rip_scratch: u64,
    cpu_id: u32,
    _pad: u32,
    tss_ptr: u64,
}

pub const SYSCALL_STACK_TOP_OFFSET: usize = core::mem::offset_of!(PerCpu, syscall_stack_top);
pub const USER_RSP_OFFSET: usize = core::mem::offset_of!(PerCpu, user_rsp_scratch);
pub const USER_RIP_OFFSET: usize = core::mem::offset_of!(PerCpu, user_rip_scratch);

pub fn init_this_cpu(cpu_id: u32, tss_ptr: u64) {
    let block = Box::leak(Box::new(PerCpu {
        self_ptr: 0,
        syscall_stack_top: 0,
        user_rsp_scratch: 0,
        user_rip_scratch: 0,
        cpu_id,
        _pad: 0,
        tss_ptr,
    }));
    block.self_ptr = block as *const PerCpu as u64;
    GsBase::write(VirtAddr::new(block.self_ptr));
}

fn this() -> &'static mut PerCpu {
    unsafe { &mut *(GsBase::read().as_u64() as *mut PerCpu) }
}

pub fn cpu_id() -> u32 {
    this().cpu_id
}

pub fn set_syscall_stack_top(top: u64) {
    this().syscall_stack_top = top;
}

pub fn tss_ptr() -> u64 {
    this().tss_ptr
}

pub fn user_rip_scratch() -> u64 {
    this().user_rip_scratch
}

pub fn user_rsp_scratch() -> u64 {
    this().user_rsp_scratch
}
