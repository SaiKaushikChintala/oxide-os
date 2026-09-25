use alloc::string::String;

use crate::task::TaskState;
use crate::{acpi, allocator, devfs, diskfs, framebuffer, keyboard, print, println, scheduler, timer, usb};

pub fn run() -> ! {
    print_prompt();
    let mut line = String::new();

    loop {
        match keyboard::try_pop_char() {
            Some('\n') => {
                println!();
                execute(&line);
                line.clear();
                print_prompt();
            }
            Some('\u{8}') | Some('\u{7f}') => {
                if line.pop().is_some() {
                    print!("\u{8} \u{8}");
                }
            }
            Some(c) if !c.is_control() => {
                line.push(c);
                print!("{c}");
            }
            Some(_) => {}
            None => x86_64::instructions::hlt(),
        }
    }
}

fn print_prompt() {
    print!("> ");
}

fn execute(line: &str) {
    let mut parts = line.trim().split_whitespace();
    let Some(cmd) = parts.next() else {
        return;
    };

    match cmd {
        "help" => {
            println!(
                "Commands: help, clear, echo, meminfo, uptime, ps, kill, devwrite, cat, ls, write, shutdown, reboot, usb"
            )
        }
        "usb" => match usb::port_status() {
            Some(ports) => {
                for (i, status) in ports.iter().enumerate() {
                    let connected = status & 0x1 != 0;
                    println!("port {}: {} (raw status {status:#06x})", i + 1, if connected { "connected" } else { "empty" });
                }
            }
            None => println!("usb: no UHCI controller found"),
        },
        "shutdown" => {
            println!("Shutting down...");
            acpi::shutdown();
        }
        "reboot" => {
            println!("Rebooting...");
            acpi::reboot();
        }
        "clear" => framebuffer::clear_screen(),
        "echo" => {
            let mut out = String::new();
            for (i, arg) in parts.enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                out.push_str(arg);
            }
            println!("{out}");
        }
        "meminfo" => {
            let (used, free) = allocator::heap_stats();
            println!(
                "heap: {used} bytes used, {free} bytes free ({} bytes total)",
                used + free
            );
        }
        "uptime" => {
            let ticks = timer::ticks();
            println!(
                "uptime: {ticks} ticks (~{}s)",
                ticks / timer::TIMER_HZ as u64
            );
        }
        "ps" => {
            for (id, state) in scheduler::list_tasks() {
                let state = match state {
                    TaskState::Ready => "ready",
                    TaskState::Blocked(_) => "blocked",
                };
                println!("task {id}: {state}");
            }
        }
        "cat" => {
            let path = parts.next().unwrap_or("/dev/keyboard");
            if let Some(name) = path.strip_prefix("/disk/") {
                match diskfs::read(name) {
                    Some(bytes) => match core::str::from_utf8(&bytes) {
                        Ok(s) => println!("{s}"),
                        Err(_) => println!("cat {path}: not valid UTF-8 ({} bytes)", bytes.len()),
                    },
                    None => println!("cat {path}: no such file on disk"),
                }
            } else {
                println!("(reading a line from {path}; press enter to finish)");
                match devfs::read_line(path) {
                    Ok(line) => println!("{line}"),
                    Err(e) => println!("cat {path}: {e}"),
                }
            }
        }
        "ls" => match diskfs::list() {
            Some(files) => {
                if files.is_empty() {
                    println!("(no files)");
                }
                for f in files {
                    println!("/disk/{} ({} bytes)", f.name, f.size_bytes);
                }
            }
            None => println!("ls: no disk filesystem found"),
        },
        "devwrite" => {
            let path = parts.next().unwrap_or("/dev/null");
            let rest: String = parts.collect::<alloc::vec::Vec<_>>().join(" ");
            match devfs::write(path, rest.as_bytes()) {
                Ok(()) => println!("wrote {} bytes to {path}", rest.len()),
                Err(e) => println!("devwrite {path}: {e}"),
            }
        }
        "write" => {
            let Some(path) = parts.next() else {
                println!("usage: write /disk/<name> <text...>");
                return;
            };
            let Some(name) = path.strip_prefix("/disk/") else {
                println!("write: only /disk/<name> is writable");
                return;
            };
            let mut rest: String = parts.collect::<alloc::vec::Vec<_>>().join(" ");
            rest.push('\n');
            if diskfs::write(name, rest.as_bytes()) {
                println!("wrote {} bytes to {path}", rest.len());
            } else {
                println!("write {path}: failed (disk full, unformatted, or name too long)");
            }
        }
        "kill" => match parts.next().and_then(|s| s.parse::<u64>().ok()) {
            Some(id) => {
                if scheduler::kill(id) {
                    println!("killed task {id}");
                } else {
                    println!("no such runnable task: {id}");
                }
            }
            None => println!("usage: kill <task id>"),
        },
        other => println!("unknown command: {other}"),
    }
}
