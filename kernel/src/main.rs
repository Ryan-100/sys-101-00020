#![feature(sync_unsafe_cell)]
#![feature(abi_x86_interrupt)]
#![no_std] // don't link the Rust standard library
#![no_main] // disable all Rust-level entry points

extern crate alloc;

mod screen;
mod allocator;
mod frame_allocator;
mod interrupts;
mod gdt;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::slice;
use bootloader_api::{entry_point, BootInfo, BootloaderConfig};
use bootloader_api::config::Mapping::Dynamic;
use bootloader_api::info::MemoryRegionKind;
use kernel::{HandlerTable, serial};
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, KeyCode};
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::VirtAddr;
use crate::frame_allocator::BootInfoFrameAllocator;
use crate::screen::{Writer, screenwriter};

const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Dynamic); // obtain physical memory offset
    config.kernel_stack_size = 256 * 1024; // 256 KiB kernel stack size
    config
};
entry_point!(kernel_main, config = &BOOTLOADER_CONFIG);

struct Shell {
    buf: String,
    ticks: u64,
}

impl Shell {
    fn new() -> Self { Self { buf: String::new(), ticks: 0 } }
    fn prompt(&self) {
        write!(Writer, "> ").ok();
    }
    fn redraw_line(&self) {
        // erase current line by rewriting spaces, then redraw prompt + buffer
        let len = 2 + self.buf.len();
        write!(Writer, "\r").ok();
        for _ in 0..len { write!(Writer, " ").ok(); }
        write!(Writer, "\r").ok();
        write!(Writer, "> ").ok();
        write!(Writer, "{}", self.buf).ok();
    }
    fn handle_key(&mut self, key: DecodedKey) {
        match key {
            DecodedKey::Unicode('\n') => {
                writeln!(Writer, "").ok();
                self.execute();
                self.buf.clear();
                self.prompt();
            }
            DecodedKey::Unicode(c) => {
                self.buf.push(c);
                write!(Writer, "{}", c).ok();
            }
            DecodedKey::RawKey(KeyCode::Backspace) => {
                if !self.buf.is_empty() {
                    self.buf.pop();
                    self.redraw_line();
                }
            }
            _ => {}
        }
    }
    fn execute(&mut self) {
        let input = core::mem::take(&mut self.buf);
        let mut parts = input.split_whitespace();
        if let Some(cmd) = parts.next() {
            let args: Vec<&str> = parts.collect();
            match cmd {
                "echo" => {
                    if !args.is_empty() { writeln!(Writer, "{}", args.join(" ")).ok(); }
                }
                "clear" => {
                    screenwriter().clear();
                }
                "ticks" => {
                    writeln!(Writer, "{}", self.ticks).ok();
                }
                "memstat" => {
                    let (used, total) = allocator::memstat();
                    writeln!(Writer, "used: {} / {} bytes", used, total).ok();
                }
                "help" => {
                    writeln!(Writer, "Built-ins:").ok();
                    writeln!(Writer, "  echo [text...]  - print text").ok();
                    writeln!(Writer, "  clear           - clear screen").ok();
                    writeln!(Writer, "  ticks           - show timer ticks").ok();
                    writeln!(Writer, "  memstat         - show allocator usage").ok();
                    writeln!(Writer, "  help            - this message").ok();
                }
                _ => {
                    writeln!(Writer, "unknown: {}", cmd).ok();
                }
            }
        }
    }
}

lazy_static! {
    static ref SHELL: Mutex<Shell> = Mutex::new(Shell::new());
}

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    writeln!(serial(), "Entered kernel with boot info: {boot_info:?}").unwrap();
    writeln!(serial(), "Frame Buffer: {:p}", boot_info.framebuffer.as_ref().unwrap().buffer()).unwrap();

    let frame_info = boot_info.framebuffer.as_ref().unwrap().info();
    let framebuffer = boot_info.framebuffer.as_mut().unwrap();
    screen::init(framebuffer);
    for x in 0..frame_info.width {
        screenwriter().draw_pixel(x, frame_info.height-15, 0xff, 0, 0);
        screenwriter().draw_pixel(x, frame_info.height-10, 0, 0xff, 0);
        screenwriter().draw_pixel(x, frame_info.height-5, 0, 0, 0xff);
    }

    for r in boot_info.memory_regions.iter() {
        writeln!(serial(), "{:?} {:?} {:?} {}", r, r.start as *mut u8, r.end as *mut usize, r.end-r.start).unwrap();
    }

    let usable_region = boot_info.memory_regions.iter().filter(|x|x.kind == MemoryRegionKind::Usable).last().unwrap();
    writeln!(serial(), "{usable_region:?}").unwrap();

    let physical_offset = boot_info.physical_memory_offset.take().expect("Failed to find physical memory offset");
    let ptr = (physical_offset + usable_region.start) as *mut u8;
    writeln!(serial(), "Physical memory offset: {:X}; usable range: {:p}", physical_offset, ptr).unwrap();

    // print out values stored in specific memory address
    let vault = unsafe { slice::from_raw_parts_mut(ptr, 100) };
    vault[0] = 65;
    vault[1] = 66;
    writeln!(Writer, "{} {}", vault[0] as char, vault[1] as char).unwrap();

    //read CR3 for current page table
    let cr3 = Cr3::read().0.start_address().as_u64();
    writeln!(serial(), "CR3 read: {:#x}", cr3).unwrap();

    let cr3_page = unsafe { slice::from_raw_parts_mut((cr3 + physical_offset) as *mut usize, 6) };
    writeln!(serial(), "CR3 Page table virtual address {cr3_page:#p}").unwrap();

    allocator::init_heap((physical_offset + usable_region.start) as usize);

    let rsdp = boot_info.rsdp_addr.take();
    let mut mapper = frame_allocator::init(VirtAddr::new(physical_offset));
    let mut frame_allocator = BootInfoFrameAllocator::new(&boot_info.memory_regions);
    
    gdt::init();

    // print out values from heap allocation
    let x = Box::new(42);
    let y = Box::new(24);
    writeln!(Writer, "x + y = {}", *x + *y).unwrap();
    writeln!(Writer, "{x:#p} {:?}", *x).unwrap();
    writeln!(Writer, "{y:#p} {:?}", *y).unwrap();
    
    writeln!(serial(), "Starting kernel...").unwrap();

    let lapic_ptr = interrupts::init_apic(rsdp.expect("Failed to get RSDP address") as usize, physical_offset, &mut mapper, &mut frame_allocator);
    HandlerTable::new()
        .keyboard(key)
        .timer(tick)
        .startup(start)
        .start(lapic_ptr)
}

fn start() {
    SHELL.lock().prompt();
}

fn tick() {
    // increment tick counter; avoid cluttering screen with dots
    let mut sh = SHELL.lock();
    sh.ticks = sh.ticks.wrapping_add(1);
}

fn key(key: DecodedKey) {
    SHELL.lock().handle_key(key);
}
