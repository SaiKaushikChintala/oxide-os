use core::alloc::{GlobalAlloc, Layout};
use linked_list_allocator::LockedHeap;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

use crate::memory;

pub const HEAP_START: usize = 0x_4444_4444_0000;
pub const HEAP_SIZE: usize = 4 * 1024 * 1024;

struct IrqSafeHeap(LockedHeap);

unsafe impl GlobalAlloc for IrqSafeHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| unsafe { self.0.alloc(layout) })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| unsafe { self.0.dealloc(ptr, layout) })
    }
}

#[global_allocator]
static ALLOCATOR: IrqSafeHeap = IrqSafeHeap(LockedHeap::empty());

pub fn init_heap() -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE as u64 - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    for page in page_range {
        memory::map_page(page, flags)?;
    }

    unsafe {
        ALLOCATOR.0.lock().init(HEAP_START as *mut u8, HEAP_SIZE);
    }

    Ok(())
}

pub fn heap_stats() -> (usize, usize) {
    without_interrupts(|| {
        let heap = ALLOCATOR.0.lock();
        (heap.used(), heap.free())
    })
}
