use alloc::vec::Vec;
use bootloader_api::info::{MemoryRegionKind, MemoryRegions};
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();

    let phys = level_4_table_frame.start_address();
    let virt = physical_memory_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();

    unsafe { &mut *page_table_ptr }
}

pub unsafe fn init(physical_memory_offset: VirtAddr) -> OffsetPageTable<'static> {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) }
}

pub struct BootInfoFrameAllocator {
    memory_regions: &'static MemoryRegions,
    region_idx: usize,
    next_addr_in_region: u64,
}

impl BootInfoFrameAllocator {
    pub unsafe fn init(memory_regions: &'static MemoryRegions) -> Self {
        let next_addr_in_region = memory_regions.first().map_or(u64::MAX, |r| r.start);
        BootInfoFrameAllocator {
            memory_regions,
            region_idx: 0,
            next_addr_in_region,
        }
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        loop {
            let region = self.memory_regions.get(self.region_idx)?;
            if region.kind != MemoryRegionKind::Usable || self.next_addr_in_region >= region.end {
                self.region_idx += 1;
                self.next_addr_in_region = self
                    .memory_regions
                    .get(self.region_idx)
                    .map_or(u64::MAX, |r| r.start);
                continue;
            }

            let addr = self.next_addr_in_region;
            self.next_addr_in_region += 4096;

            if addr == crate::smp::TRAMPOLINE_PHYS {
                continue;
            }

            return Some(PhysFrame::containing_address(PhysAddr::new(addr)));
        }
    }
}

pub static MAPPER: Mutex<Option<OffsetPageTable<'static>>> = Mutex::new(None);
pub static FRAME_ALLOCATOR: Mutex<Option<BootInfoFrameAllocator>> = Mutex::new(None);
static PHYSICAL_MEMORY_OFFSET: Mutex<Option<VirtAddr>> = Mutex::new(None);
static ROOT_PML4_FRAME: Mutex<Option<PhysFrame>> = Mutex::new(None);

pub unsafe fn init_globals(physical_memory_offset: VirtAddr, memory_regions: &'static MemoryRegions) {
    let mapper = unsafe { init(physical_memory_offset) };
    let frame_allocator = unsafe { BootInfoFrameAllocator::init(memory_regions) };
    let (boot_pml4_frame, _) = Cr3::read();
    *MAPPER.lock() = Some(mapper);
    *FRAME_ALLOCATOR.lock() = Some(frame_allocator);
    *PHYSICAL_MEMORY_OFFSET.lock() = Some(physical_memory_offset);
    *ROOT_PML4_FRAME.lock() = Some(boot_pml4_frame);
}

pub fn allocate_dma_frames(count: usize) -> (PhysAddr, VirtAddr) {
    let mut fa_guard = FRAME_ALLOCATOR.lock();
    let frame_allocator = fa_guard.as_mut().expect("memory::init_globals not called");

    let first = frame_allocator
        .allocate_frame()
        .expect("out of physical memory");
    let mut expected = first.start_address().as_u64() + 4096;
    for _ in 1..count {
        let frame = frame_allocator
            .allocate_frame()
            .expect("out of physical memory");
        assert_eq!(
            frame.start_address().as_u64(),
            expected,
            "DMA frames were not physically contiguous"
        );
        expected += 4096;
    }

    let offset = PHYSICAL_MEMORY_OFFSET
        .lock()
        .expect("memory::init_globals not called");
    let virt = offset + first.start_address().as_u64();
    (first.start_address(), virt)
}

pub fn map_page(page: Page<Size4KiB>, flags: PageTableFlags) -> Result<(), MapToError<Size4KiB>> {
    let mut mapper_guard = MAPPER.lock();
    let mapper = mapper_guard.as_mut().expect("memory::init_globals not called");
    let mut fa_guard = FRAME_ALLOCATOR.lock();
    let frame_allocator = fa_guard
        .as_mut()
        .expect("memory::init_globals not called");

    let frame = frame_allocator
        .allocate_frame()
        .ok_or(MapToError::FrameAllocationFailed)?;
    unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)?.flush();
    }
    Ok(())
}

pub fn map_page_to_frame(
    page: Page<Size4KiB>,
    frame: PhysFrame,
    flags: PageTableFlags,
) -> Result<(), MapToError<Size4KiB>> {
    let mut mapper_guard = MAPPER.lock();
    let mapper = mapper_guard.as_mut().expect("memory::init_globals not called");
    let mut fa_guard = FRAME_ALLOCATOR.lock();
    let frame_allocator = fa_guard.as_mut().expect("memory::init_globals not called");

    unsafe {
        mapper.map_to(page, frame, flags, frame_allocator)?.flush();
    }
    Ok(())
}

pub fn unmap_page(page: Page<Size4KiB>) -> bool {
    match MAPPER
        .lock()
        .as_mut()
        .expect("memory::init_globals not called")
        .unmap(page)
    {
        Ok((_, flush)) => {
            flush.flush();
            true
        }
        Err(_) => false,
    }
}

pub fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
    let offset = PHYSICAL_MEMORY_OFFSET
        .lock()
        .expect("memory::init_globals not called");
    offset + addr.as_u64()
}

fn root_level_4_table_virt() -> VirtAddr {
    let frame = ROOT_PML4_FRAME
        .lock()
        .expect("memory::init_globals not called");
    let offset = PHYSICAL_MEMORY_OFFSET
        .lock()
        .expect("memory::init_globals not called");
    offset + frame.start_address().as_u64()
}

pub struct AddressSpace {
    pml4_frame: PhysFrame,
    regions: Vec<(u64, u64)>,
}

impl AddressSpace {
    pub fn new() -> Self {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let (phys, virt) = allocate_dma_frames(1);
            unsafe {
                core::ptr::write_bytes(virt.as_u64() as *mut u8, 0, 4096);
            }
            let new_pml4: &mut PageTable = unsafe { &mut *(virt.as_u64() as *mut PageTable) };

            let current_pml4: &PageTable =
                unsafe { &*(root_level_4_table_virt().as_u64() as *const PageTable) };
            for i in 0..512 {
                new_pml4[i] = current_pml4[i].clone();
            }

            AddressSpace {
                pml4_frame: PhysFrame::containing_address(phys),
                regions: Vec::new(),
            }
        })
    }

    pub fn cr3_frame(&self) -> PhysFrame {
        self.pml4_frame
    }

    fn mapper(&self) -> OffsetPageTable<'static> {
        let offset = PHYSICAL_MEMORY_OFFSET
            .lock()
            .expect("memory::init_globals not called");
        let virt = offset + self.pml4_frame.start_address().as_u64();
        let pml4: &mut PageTable = unsafe { &mut *(virt.as_u64() as *mut PageTable) };
        unsafe { OffsetPageTable::new(pml4, offset) }
    }

    pub fn map_page(
        &mut self,
        page: Page<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<PhysFrame, MapToError<Size4KiB>> {
        let mut mapper = self.mapper();
        let mut fa_guard = FRAME_ALLOCATOR.lock();
        let frame_allocator = fa_guard.as_mut().expect("memory::init_globals not called");

        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        }
        Ok(frame)
    }

    pub fn register_region(&mut self, start: u64, end: u64) {
        self.regions.push((start, end));
    }

    fn clear_pml4_slot_for(&mut self, vaddr: u64) {
        let offset = PHYSICAL_MEMORY_OFFSET
            .lock()
            .expect("memory::init_globals not called");
        let pml4: &mut PageTable =
            unsafe { &mut *((offset + self.pml4_frame.start_address().as_u64()).as_u64() as *mut PageTable) };
        let index = ((vaddr >> 39) & 0x1FF) as usize;
        pml4[index].set_unused();
    }

    pub fn contains(&self, start: u64, end: u64) -> bool {
        self.regions
            .iter()
            .any(|&(lo, hi)| start >= lo && end <= hi)
    }

    fn map_existing_frame(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame,
        flags: PageTableFlags,
    ) -> Result<(), MapToError<Size4KiB>> {
        let mut mapper = self.mapper();
        let mut fa_guard = FRAME_ALLOCATOR.lock();
        let frame_allocator = fa_guard.as_mut().expect("memory::init_globals not called");
        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush();
        }
        Ok(())
    }

    pub fn fork(&self) -> AddressSpace {
        let mut child = AddressSpace::new();
        for &(start, _) in &self.regions {
            child.clear_pml4_slot_for(start);
        }

        let mut parent_mapper = self.mapper();

        let mut pages = alloc::collections::BTreeSet::new();
        for &(start, end) in &self.regions {
            let start_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(start));
            let end_page: Page<Size4KiB> = Page::containing_address(VirtAddr::new(end - 1));
            pages.extend(Page::range_inclusive(start_page, end_page));
        }

        for page in pages {
            let Ok((frame, orig_flags)) = translate_4kib(&parent_mapper, page) else {
                continue;
            };

            if orig_flags.contains(PageTableFlags::WRITABLE) {
                let cow_flags = (orig_flags & !PageTableFlags::WRITABLE) | PageTableFlags::BIT_9;
                unsafe {
                    parent_mapper
                        .update_flags(page, cow_flags)
                        .expect("fork: failed to mark parent page copy-on-write")
                        .flush();
                }
            }
            let child_flags = (orig_flags & !PageTableFlags::WRITABLE) | PageTableFlags::BIT_9;
            child
                .map_existing_frame(page, frame, child_flags)
                .expect("fork: failed to share page into child address space");
        }
        for &(start, end) in &self.regions {
            child.register_region(start, end);
        }

        child
    }

    pub fn resolve_cow_fault(&mut self, addr: VirtAddr) -> bool {
        use x86_64::structures::paging::mapper::MappedFrame;

        let page: Page<Size4KiB> = Page::containing_address(addr);
        let mut mapper = self.mapper();
        let (old_frame, flags) = match translate_any(&mapper, addr) {
            Some((MappedFrame::Size4KiB(frame), flags)) => (frame, flags),
            _ => return false,
        };
        if flags.contains(PageTableFlags::WRITABLE) || !flags.contains(PageTableFlags::BIT_9) {
            return false;
        }

        let offset = PHYSICAL_MEMORY_OFFSET
            .lock()
            .expect("memory::init_globals not called");
        let new_frame = {
            let mut fa_guard = FRAME_ALLOCATOR.lock();
            let frame_allocator = fa_guard.as_mut().expect("memory::init_globals not called");
            frame_allocator
                .allocate_frame()
                .expect("resolve_cow_fault: out of physical memory")
        };
        unsafe {
            let src = (offset.as_u64() + old_frame.start_address().as_u64()) as *const u8;
            let dst = (offset.as_u64() + new_frame.start_address().as_u64()) as *mut u8;
            core::ptr::copy_nonoverlapping(src, dst, 4096);
        }

        let (_, flush) = mapper.unmap(page).expect("resolve_cow_fault: page vanished mid-fault");
        flush.flush();
        let new_flags = (flags | PageTableFlags::WRITABLE) & !PageTableFlags::BIT_9;
        self.map_existing_frame(page, new_frame, new_flags)
            .expect("resolve_cow_fault: failed to remap the freshly copied page");
        true
    }
}

fn translate_4kib(
    mapper: &OffsetPageTable<'static>,
    page: Page<Size4KiB>,
) -> Result<(PhysFrame, PageTableFlags), ()> {
    use x86_64::structures::paging::mapper::{MappedFrame, Translate};
    match mapper.translate(page.start_address()) {
        x86_64::structures::paging::mapper::TranslateResult::Mapped {
            frame: MappedFrame::Size4KiB(frame),
            flags,
            ..
        } => Ok((frame, flags)),
        _ => Err(()),
    }
}

fn translate_any(
    mapper: &OffsetPageTable<'static>,
    addr: VirtAddr,
) -> Option<(x86_64::structures::paging::mapper::MappedFrame, PageTableFlags)> {
    use x86_64::structures::paging::mapper::Translate;
    match mapper.translate(addr) {
        x86_64::structures::paging::mapper::TranslateResult::Mapped { frame, flags, .. } => {
            Some((frame, flags))
        }
        _ => None,
    }
}
