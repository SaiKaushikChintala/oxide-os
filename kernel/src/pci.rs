use x86_64::instructions::port::Port;

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;

#[derive(Debug, Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciDevice {
    fn address(&self, offset: u8) -> u32 {
        0x8000_0000
            | (u32::from(self.bus) << 16)
            | (u32::from(self.device) << 11)
            | (u32::from(self.function) << 8)
            | u32::from(offset & 0xFC)
    }

    pub fn read_u32(&self, offset: u8) -> u32 {
        unsafe {
            let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
            let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
            addr_port.write(self.address(offset));
            data_port.read()
        }
    }

    pub fn write_u32(&self, offset: u8, value: u32) {
        unsafe {
            let mut addr_port: Port<u32> = Port::new(CONFIG_ADDRESS);
            let mut data_port: Port<u32> = Port::new(CONFIG_DATA);
            addr_port.write(self.address(offset));
            data_port.write(value);
        }
    }

    pub fn read_u16(&self, offset: u8) -> u16 {
        let shift = (offset & 2) * 8;
        (self.read_u32(offset) >> shift) as u16
    }

    pub fn vendor_id(&self) -> u16 {
        self.read_u16(0x00)
    }

    pub fn device_id(&self) -> u16 {
        self.read_u16(0x02)
    }

    pub fn enable_bus_mastering_and_io(&self) {
        let command = self.read_u32(0x04);
        self.write_u32(0x04, command | 0x1 | 0x4);
    }

    pub fn io_bar(&self, bar_index: u8) -> Option<u16> {
        let raw = self.read_u32(0x10 + u8::from(bar_index) * 4);
        if raw & 0x1 == 0 {
            return None;
        }
        Some((raw & 0xFFFC) as u16)
    }
}

pub fn find_device(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    for device in 0..32 {
        for function in 0..8 {
            let dev = PciDevice {
                bus: 0,
                device,
                function,
            };
            if dev.vendor_id() == 0xFFFF {
                continue;
            }
            if dev.vendor_id() == vendor_id && dev.device_id() == device_id {
                return Some(dev);
            }
        }
    }
    None
}
