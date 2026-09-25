use alloc::collections::VecDeque;
use spin::Mutex;

static BUFFER: Mutex<VecDeque<u8>> = Mutex::new(VecDeque::new());

pub fn write(bytes: &[u8]) {
    BUFFER.lock().extend(bytes.iter().copied());
}

pub fn read(buf: &mut [u8]) -> usize {
    loop {
        {
            let mut b = BUFFER.lock();
            if !b.is_empty() {
                let n = buf.len().min(b.len());
                for slot in buf.iter_mut().take(n) {
                    *slot = b.pop_front().expect("checked non-empty above");
                }
                return n;
            }
        }
        crate::scheduler::schedule();
    }
}
