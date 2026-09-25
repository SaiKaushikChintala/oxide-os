use alloc::boxed::Box;
use lazy_static::lazy_static;
use x86_64::VirtAddr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

pub const DOUBLE_FAULT_IST_INDEX: u16 = 0;

const STACK_SIZE: usize = 4096 * 5;

lazy_static! {
    static ref TSS: TaskStateSegment = {
        let mut tss = TaskStateSegment::new();
        tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            stack_start + STACK_SIZE as u64
        };
        tss.privilege_stack_table[0] = {
            static mut STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];

            let stack_start = VirtAddr::from_ptr(&raw const STACK);
            stack_start + STACK_SIZE as u64
        };
        tss
    };
}

pub struct Selectors {
    pub kernel_code_selector: SegmentSelector,
    pub kernel_data_selector: SegmentSelector,
    pub user_data_selector: SegmentSelector,
    pub user_code_selector: SegmentSelector,
    pub tss_selector: SegmentSelector,
}

lazy_static! {
    static ref GDT: (GlobalDescriptorTable, Selectors) = {
        let mut gdt = GlobalDescriptorTable::new();
        let kernel_code_selector = gdt.append(Descriptor::kernel_code_segment());
        let kernel_data_selector = gdt.append(Descriptor::kernel_data_segment());
        let _unused = gdt.append(Descriptor::user_data_segment());
        let user_data_selector =
            SegmentSelector::new(gdt.append(Descriptor::user_data_segment()).index(), x86_64::PrivilegeLevel::Ring3);
        let user_code_selector =
            SegmentSelector::new(gdt.append(Descriptor::user_code_segment()).index(), x86_64::PrivilegeLevel::Ring3);
        let tss_selector = gdt.append(Descriptor::tss_segment(&TSS));
        (
            gdt,
            Selectors {
                kernel_code_selector,
                kernel_data_selector,
                user_data_selector,
                user_code_selector,
                tss_selector,
            },
        )
    };
}

pub fn selectors() -> &'static Selectors {
    &GDT.1
}

pub unsafe fn set_kernel_stack(top: VirtAddr) {
    unsafe {
        let tss_ptr = crate::percpu::tss_ptr() as *mut TaskStateSegment;
        (*tss_ptr).privilege_stack_table[0] = top;
    }
}

pub fn init() -> u64 {
    use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS, Segment};
    use x86_64::instructions::tables::load_tss;
    use x86_64::structures::gdt::SegmentSelector;
    use x86_64::PrivilegeLevel;

    let null_selector = SegmentSelector::new(0, PrivilegeLevel::Ring0);

    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.kernel_code_selector);
        load_tss(GDT.1.tss_selector);

        SS::set_reg(GDT.1.kernel_data_selector);
        DS::set_reg(GDT.1.kernel_data_selector);
        ES::set_reg(GDT.1.kernel_data_selector);
        FS::set_reg(null_selector);
        GS::set_reg(null_selector);
    }

    &*TSS as *const TaskStateSegment as u64
}

pub fn init_on_ap() -> u64 {
    use x86_64::PrivilegeLevel;
    use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS, Segment};
    use x86_64::instructions::tables::load_tss;
    use x86_64::structures::gdt::SegmentSelector;

    let tss: &'static mut TaskStateSegment = Box::leak(Box::new(TaskStateSegment::new()));
    tss.interrupt_stack_table[DOUBLE_FAULT_IST_INDEX as usize] = {
        let stack = Box::leak(alloc::vec![0u8; STACK_SIZE].into_boxed_slice());
        VirtAddr::from_ptr(stack.as_ptr()) + STACK_SIZE as u64
    };
    tss.privilege_stack_table[0] = {
        let stack = Box::leak(alloc::vec![0u8; STACK_SIZE].into_boxed_slice());
        VirtAddr::from_ptr(stack.as_ptr()) + STACK_SIZE as u64
    };
    let tss_ptr = tss as *const TaskStateSegment as u64;

    let mut gdt = GlobalDescriptorTable::new();
    let kernel_code_selector = gdt.append(Descriptor::kernel_code_segment());
    let kernel_data_selector = gdt.append(Descriptor::kernel_data_segment());
    let _unused = gdt.append(Descriptor::user_data_segment());
    let _unused = gdt.append(Descriptor::user_data_segment());
    let _unused = gdt.append(Descriptor::user_code_segment());
    let tss_selector = gdt.append(Descriptor::tss_segment(tss));
    let gdt: &'static GlobalDescriptorTable = Box::leak(Box::new(gdt));

    let null_selector = SegmentSelector::new(0, PrivilegeLevel::Ring0);
    gdt.load();
    unsafe {
        CS::set_reg(kernel_code_selector);
        load_tss(tss_selector);
        SS::set_reg(kernel_data_selector);
        DS::set_reg(kernel_data_selector);
        ES::set_reg(kernel_data_selector);
        FS::set_reg(null_selector);
        GS::set_reg(null_selector);
    }

    tss_ptr
}
