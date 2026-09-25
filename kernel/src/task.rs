use alloc::boxed::Box;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::memory::AddressSpace;

pub type TaskId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Blocked(u64),
}

pub struct Task {
    pub id: TaskId,
    pub rsp: u64,
    pub state: TaskState,
    pub(crate) _stack: Box<[u8]>,
    pub address_space: Option<AddressSpace>,
    pub fork_resume: Option<(u64, u64)>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> TaskId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}
