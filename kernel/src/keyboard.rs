use alloc::collections::VecDeque;
use lazy_static::lazy_static;
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use spin::Mutex;

lazy_static! {
    static ref KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(
        Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::Ignore)
    );
}

static QUEUE: Mutex<VecDeque<char>> = Mutex::new(VecDeque::new());

pub fn handle_scancode(scancode: u8) {
    let mut keyboard = KEYBOARD.lock();
    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(DecodedKey::Unicode(character)) = keyboard.process_keyevent(key_event) {
            QUEUE.lock().push_back(character);
        }
    }
}

pub fn push_char(c: char) {
    x86_64::instructions::interrupts::without_interrupts(|| QUEUE.lock().push_back(c));
}

pub fn try_pop_char() -> Option<char> {
    x86_64::instructions::interrupts::without_interrupts(|| QUEUE.lock().pop_front())
}
