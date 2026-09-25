use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::registers::control::{Cr3, Cr3Flags};

use crate::memory::AddressSpace;
use crate::task::{Task, TaskId, TaskState, next_id};

const STACK_SIZE: usize = 4096 * 16;

static PER_CPU_CURRENT: [Mutex<Option<Task>>; crate::smp::MAX_CPUS] =
    [const { Mutex::new(None) }; crate::smp::MAX_CPUS];
static READY: Mutex<VecDeque<Task>> = Mutex::new(VecDeque::new());
static ZOMBIES: Mutex<Vec<(TaskId, u64)>> = Mutex::new(Vec::new());
pub const KILLED: u64 = u64::MAX;

fn current_slot() -> &'static Mutex<Option<Task>> {
    &PER_CPU_CURRENT[crate::percpu::cpu_id() as usize % crate::smp::MAX_CPUS]
}

fn stack_top_of(task: &Task) -> u64 {
    (task._stack.as_ptr() as u64 + task._stack.len() as u64) & !0xf
}

pub fn init() {
    *current_slot().lock() = Some(Task {
        id: 0,
        rsp: 0,
        state: TaskState::Ready,
        _stack: Box::new([]),
        address_space: None,
        fork_resume: None,
    });
}

pub fn init_this_cpu() {
    *current_slot().lock() = Some(Task {
        id: next_id(),
        rsp: 0,
        state: TaskState::Ready,
        _stack: Box::new([]),
        address_space: None,
        fork_resume: None,
    });
}

pub fn spawn(entry: fn() -> !) -> TaskId {
    spawn_with(entry, None, None)
}

pub fn spawn_process(entry: fn() -> !, address_space: AddressSpace) -> TaskId {
    spawn_with(entry, Some(address_space), None)
}

pub fn spawn_forked(entry: fn() -> !, address_space: AddressSpace, resume: (u64, u64)) -> TaskId {
    spawn_with(entry, Some(address_space), Some(resume))
}

fn spawn_with(
    entry: fn() -> !,
    address_space: Option<AddressSpace>,
    fork_resume: Option<(u64, u64)>,
) -> TaskId {
    let mut stack = alloc::vec![0u8; STACK_SIZE].into_boxed_slice();
    let stack_top = (unsafe { stack.as_mut_ptr().add(STACK_SIZE) } as u64) & !0xf;

    unsafe {
        write_u64(stack_top, 8, entry as u64);
        write_u64(stack_top, 16, task_trampoline as *const () as u64);
        write_u64(stack_top, 24, 0);
        write_u64(stack_top, 32, 0);
        write_u64(stack_top, 40, 0);
        write_u64(stack_top, 48, 0);
        write_u64(stack_top, 56, 0);
        write_u64(stack_top, 64, 0);
        write_u64(stack_top, 72, 0x202);
    }

    let id = next_id();
    let task = Task {
        id,
        rsp: stack_top - 72,
        state: TaskState::Ready,
        _stack: stack,
        address_space,
        fork_resume,
    };
    x86_64::instructions::interrupts::without_interrupts(|| {
        READY.lock().push_back(task);
    });
    id
}

pub fn with_current_address_space<R>(f: impl FnOnce(&mut AddressSpace) -> R) -> Option<R> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        current_slot()
            .lock()
            .as_mut()
            .and_then(|t| t.address_space.as_mut())
            .map(f)
    })
}

pub fn take_current_fork_resume() -> Option<(u64, u64)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        current_slot().lock().as_mut().and_then(|t| t.fork_resume.take())
    })
}

fn switch_address_space(next: &Task) {
    let Some(space) = &next.address_space else {
        return;
    };
    let target = space.cr3_frame();
    let (current, _) = Cr3::read();
    if current != target {
        unsafe {
            Cr3::write(target, Cr3Flags::empty());
        }
    }
}

unsafe fn write_u64(stack_top: u64, offset_from_top: u64, value: u64) {
    unsafe {
        ((stack_top - offset_from_top) as *mut u64).write(value);
    }
}

pub fn schedule() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let now = crate::timer::ticks();
        let mut ready = READY.lock();

        for task in ready.iter_mut() {
            if let TaskState::Blocked(wake_at) = task.state {
                if now >= wake_at {
                    task.state = TaskState::Ready;
                }
            }
        }

        let Some(idx) = ready.iter().position(|t| t.state == TaskState::Ready) else {
            return;
        };
        let next = ready.remove(idx).expect("index came from position() above");

        let mut current_guard = current_slot().lock();
        let old = current_guard
            .take()
            .expect("current_slot() is always Some after init()/init_this_cpu()");

        ready.push_back(old);
        let old_rsp_ptr = &mut ready.back_mut().expect("just pushed").rsp as *mut u64;

        let new_rsp = next.rsp;
        let stack_top = stack_top_of(&next);
        crate::percpu::set_syscall_stack_top(stack_top);
        unsafe {
            crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(stack_top));
        }
        switch_address_space(&next);
        *current_guard = Some(next);

        drop(ready);
        drop(current_guard);

        unsafe {
            context_switch(old_rsp_ptr, new_rsp);
        }
    });
}

pub fn current_task_id() -> TaskId {
    x86_64::instructions::interrupts::without_interrupts(|| {
        current_slot()
            .lock()
            .as_ref()
            .expect("current_slot() is always Some after init()/init_this_cpu()")
            .id
    })
}

pub fn list_tasks() -> alloc::vec::Vec<(TaskId, TaskState)> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut out = alloc::vec::Vec::new();
        for slot in PER_CPU_CURRENT.iter() {
            if let Some(t) = slot.lock().as_ref() {
                out.push((t.id, t.state));
            }
        }
        out.extend(READY.lock().iter().map(|t| (t.id, t.state)));
        out
    })
}

pub fn exit_current_task(code: u64) -> ! {
    let own_stack_top = x86_64::instructions::interrupts::without_interrupts(|| {
        let current = current_slot().lock();
        let t = current
            .as_ref()
            .expect("current_slot() is always Some after init()/init_this_cpu()");
        stack_top_of(t)
    });
    unsafe {
        switch_stack_and_call(own_stack_top, exit_current_task_on_own_stack, code);
    }
}

extern "C" fn exit_current_task_on_own_stack(code: u64) -> ! {
    loop {
        let next_rsp = x86_64::instructions::interrupts::without_interrupts(|| {
            let now = crate::timer::ticks();
            let mut ready = READY.lock();

            for task in ready.iter_mut() {
                if let TaskState::Blocked(wake_at) = task.state {
                    if now >= wake_at {
                        task.state = TaskState::Ready;
                    }
                }
            }

            let idx = ready.iter().position(|t| t.state == TaskState::Ready)?;
            let next = ready.remove(idx).expect("index came from position() above");
            let new_rsp = next.rsp;

            let mut current_guard = current_slot().lock();
            let dead = current_guard.take();
            if let Some(dead) = dead {
                ZOMBIES.lock().push((dead.id, code));
                core::mem::forget(dead);
            }
            let stack_top = stack_top_of(&next);
            crate::percpu::set_syscall_stack_top(stack_top);
            unsafe {
                crate::gdt::set_kernel_stack(x86_64::VirtAddr::new(stack_top));
            }
            switch_address_space(&next);
            *current_guard = Some(next);

            Some(new_rsp)
        });

        let Some(new_rsp) = next_rsp else {
            x86_64::instructions::interrupts::enable();
            x86_64::instructions::hlt();
            continue;
        };

        let mut discard: u64 = 0;
        unsafe {
            context_switch(&mut discard as *mut u64, new_rsp);
        }
        unreachable!("a terminated task is never switched back into");
    }
}

#[unsafe(naked)]
unsafe extern "C" fn switch_stack_and_call(new_rsp: u64, target: extern "C" fn(u64) -> !, arg: u64) -> ! {
    core::arch::naked_asm!("mov rax, rdi", "mov rdi, rdx", "mov rsp, rax", "jmp rsi")
}

pub fn wait_for(child: TaskId) -> u64 {
    loop {
        let found = x86_64::instructions::interrupts::without_interrupts(|| {
            let mut zombies = ZOMBIES.lock();
            zombies
                .iter()
                .position(|&(id, _)| id == child)
                .map(|pos| zombies.remove(pos).1)
        });
        if let Some(code) = found {
            return code;
        }
        schedule();
    }
}

pub fn kill(target: TaskId) -> bool {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut ready = READY.lock();
        let Some(pos) = ready.iter().position(|t| t.id == target) else {
            return false;
        };
        let dead = ready.remove(pos).expect("index came from position() above");
        drop(ready);
        ZOMBIES.lock().push((dead.id, KILLED));
        drop(dead);
        true
    })
}

pub fn sleep_ticks(duration: u64) {
    let wake_at = crate::timer::ticks() + duration;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut cur = current_slot().lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Blocked(wake_at);
        }
    });
    while crate::timer::ticks() < wake_at {
        schedule();
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut cur = current_slot().lock();
        if let Some(t) = cur.as_mut() {
            t.state = TaskState::Ready;
        }
    });
}

#[unsafe(naked)]
unsafe extern "C" fn context_switch(old_rsp: *mut u64, new_rsp: u64) {
    core::arch::naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "pushfq",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "popfq",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "ret",
    )
}

#[unsafe(naked)]
extern "C" fn task_trampoline() -> ! {
    core::arch::naked_asm!("pop rdi", "call {task_start}", task_start = sym task_start,)
}

extern "C" fn task_start(entry: u64) -> ! {
    let entry: fn() -> ! = unsafe { core::mem::transmute::<u64, fn() -> !>(entry) };
    entry()
}
