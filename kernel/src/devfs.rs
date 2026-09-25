use alloc::string::String;

pub fn write(path: &str, _data: &[u8]) -> Result<(), &'static str> {
    match path {
        "/dev/null" => Ok(()),
        _ => Err("no such device"),
    }
}

pub fn read_line(path: &str) -> Result<String, &'static str> {
    match path {
        "/dev/keyboard" => {
            let mut line = String::new();
            loop {
                match crate::keyboard::try_pop_char() {
                    Some('\n') => return Ok(line),
                    Some(c) if !c.is_control() => line.push(c),
                    Some(_) => {}
                    None => x86_64::instructions::hlt(),
                }
            }
        }
        "/dev/null" => Ok(String::new()),
        _ => Err("no such device"),
    }
}
