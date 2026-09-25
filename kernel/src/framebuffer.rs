use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::fmt;
use lazy_static::lazy_static;
use noto_sans_mono_bitmap::{
    FontWeight, RasterHeight, RasterizedChar, get_raster, get_raster_width,
};
use spin::Mutex;

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const CHAR_RASTER_HEIGHT: RasterHeight = RasterHeight::Size16;
const LINE_SPACING: usize = 2;
const LETTER_SPACING: usize = 0;
const BORDER_PADDING: usize = 4;

fn char_raster_width() -> usize {
    get_raster_width(FONT_WEIGHT, CHAR_RASTER_HEIGHT)
}

fn get_char_raster(c: char) -> RasterizedChar {
    get_raster(c, FONT_WEIGHT, CHAR_RASTER_HEIGHT)
        .or_else(|| get_raster('?', FONT_WEIGHT, CHAR_RASTER_HEIGHT))
        .unwrap_or_else(|| {
            get_raster(' ', FONT_WEIGHT, CHAR_RASTER_HEIGHT)
                .expect("basic latin space glyph must be present")
        })
}

pub struct Writer {
    buffer: &'static mut [u8],
    info: FrameBufferInfo,
    x_pos: usize,
    y_pos: usize,
}

unsafe impl Send for Writer {}

impl Writer {
    pub fn new(buffer: &'static mut [u8], info: FrameBufferInfo) -> Self {
        let mut writer = Self {
            buffer,
            info,
            x_pos: 0,
            y_pos: 0,
        };
        writer.clear();
        writer
    }

    pub fn clear(&mut self) {
        self.x_pos = BORDER_PADDING;
        self.y_pos = BORDER_PADDING;
        self.buffer.fill(0);
    }

    fn newline(&mut self) {
        self.y_pos += CHAR_RASTER_HEIGHT.val() + LINE_SPACING;
        self.carriage_return();
    }

    fn carriage_return(&mut self) {
        self.x_pos = BORDER_PADDING;
    }

    fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.newline(),
            '\r' => self.carriage_return(),
            c => {
                let new_x = self.x_pos + char_raster_width();
                if new_x >= self.info.width {
                    self.newline();
                }
                let new_y = self.y_pos + CHAR_RASTER_HEIGHT.val() + BORDER_PADDING;
                if new_y >= self.info.height {
                    self.clear();
                }
                self.write_rendered_char(get_char_raster(c));
            }
        }
    }

    fn write_rendered_char(&mut self, rendered: RasterizedChar) {
        for (y, row) in rendered.raster().iter().enumerate() {
            for (x, byte) in row.iter().enumerate() {
                self.write_pixel(self.x_pos + x, self.y_pos + y, *byte);
            }
        }
        self.x_pos += rendered.width() + LETTER_SPACING;
    }

    fn write_pixel(&mut self, x: usize, y: usize, intensity: u8) {
        let pixel_offset = y * self.info.stride + x;
        let bpp = self.info.bytes_per_pixel;
        let byte_offset = pixel_offset * bpp;

        let Some(pixel_bytes) = self.buffer.get_mut(byte_offset..byte_offset + bpp) else {
            return;
        };

        match self.info.pixel_format {
            PixelFormat::Rgb => {
                pixel_bytes[0] = intensity;
                pixel_bytes[1] = intensity;
                pixel_bytes[2] = intensity;
            }
            PixelFormat::Bgr => {
                pixel_bytes[0] = intensity;
                pixel_bytes[1] = intensity;
                pixel_bytes[2] = intensity;
            }
            PixelFormat::U8 => {
                pixel_bytes[0] = intensity;
            }
            _ => {}
        }
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for c in s.chars() {
            self.write_char(c);
        }
        Ok(())
    }
}

lazy_static! {
    pub static ref WRITER: Mutex<Option<Writer>> = Mutex::new(None);
}

pub fn init(buffer: &'static mut [u8], info: FrameBufferInfo) {
    *WRITER.lock() = Some(Writer::new(buffer, info));
}

pub fn clear_screen() {
    use x86_64::instructions::interrupts;
    interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.lock().as_mut() {
            writer.clear();
        }
    });
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use fmt::Write;
    use x86_64::instructions::interrupts;

    interrupts::without_interrupts(|| {
        if let Some(writer) = WRITER.lock().as_mut() {
            writer.write_fmt(args).unwrap();
        }
    });
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::framebuffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}
