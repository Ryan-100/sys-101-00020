#[global_allocator]
static ALLOCATOR: DummyAllocator = DummyAllocator;

use alloc::alloc::{GlobalAlloc, Layout};
use core::fmt::Write;
use core::ptr::null_mut;

use crate::serial;
pub struct DummyAllocator;

pub static mut HEAP_START: usize = 0x0;
pub static mut OFFSET: usize = 0x0;
pub const HEAP_SIZE: usize = 100 * 1024; // 100 KiB

#[inline]
fn align_up(addr: usize, align: usize) -> usize {
    let mask = align - 1;
    (addr + mask) & !mask
}

unsafe impl GlobalAlloc for DummyAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Simple bump allocator: allocate from HEAP_START + OFFSET
        let heap_start = unsafe { HEAP_START };
        let current = heap_start.saturating_add(unsafe { OFFSET });
        let aligned = align_up(current, layout.align());
        let new_offset = aligned.saturating_sub(heap_start).saturating_add(layout.size());
        if new_offset > HEAP_SIZE {
            // out of memory
            writeln!(serial(), "alloc failed: size={}, align={}", layout.size(), layout.align()).ok();
            return null_mut();
        }
        unsafe { OFFSET = new_offset; }
        aligned as *mut u8
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // no-op (leaky); sufficient for this assignment
        writeln!(serial(), "dealloc was called at {_ptr:?}").ok();
    }
}

pub fn init_heap(offset: usize) {
    unsafe {
        HEAP_START = offset;
        OFFSET = 0;
        let hs = HEAP_START;
        let sz = HEAP_SIZE;
        writeln!(serial(), "heap init at {:#x}, size={} bytes", hs, sz).ok();
    }
}

pub fn memstat() -> (usize, usize) {
    // returns (used, total)
    unsafe { (OFFSET.min(HEAP_SIZE), HEAP_SIZE) }
}
