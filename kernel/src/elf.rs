const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;

const DT_NULL: u64 = 0;
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;

const R_X86_64_RELATIVE: u32 = 8;

pub const ET_DYN: u16 = 3;

#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub vaddr: u64,
    pub offset: u64,
    pub filesz: u64,
    pub memsz: u64,
}

pub struct ElfFile<'a> {
    data: &'a [u8],
}

impl<'a> ElfFile<'a> {
    pub fn parse(data: &'a [u8]) -> Option<Self> {
        if data.len() < 64 || &data[0..4] != b"\x7fELF" {
            return None;
        }
        if data[4] != 2 {
            return None;
        }
        Some(ElfFile { data })
    }

    fn u16_at(&self, off: usize) -> u16 {
        u16::from_le_bytes(self.data[off..off + 2].try_into().unwrap())
    }

    fn u64_at(&self, off: usize) -> u64 {
        u64::from_le_bytes(self.data[off..off + 8].try_into().unwrap())
    }

    pub fn entry_point(&self) -> u64 {
        self.u64_at(24)
    }

    pub fn e_type(&self) -> u16 {
        self.u16_at(16)
    }

    fn phoff(&self) -> u64 {
        self.u64_at(32)
    }

    fn phentsize(&self) -> u16 {
        self.u16_at(54)
    }

    fn phnum(&self) -> u16 {
        self.u16_at(56)
    }

    fn program_headers(&self) -> impl Iterator<Item = (u32, u64, u64, u64, u64)> + '_ {
        let phoff = self.phoff() as usize;
        let phentsize = self.phentsize() as usize;
        let phnum = self.phnum() as usize;

        (0..phnum).map(move |i| {
            let base = phoff + i * phentsize;
            let p_type = u32::from_le_bytes(self.data[base..base + 4].try_into().unwrap());
            (
                p_type,
                self.u64_at(base + 8),
                self.u64_at(base + 16),
                self.u64_at(base + 32),
                self.u64_at(base + 40),
            )
        })
    }

    pub fn load_segments(&self) -> impl Iterator<Item = Segment> + '_ {
        self.program_headers().filter_map(|(p_type, offset, vaddr, filesz, memsz)| {
            if p_type != PT_LOAD {
                return None;
            }
            Some(Segment { vaddr, offset, filesz, memsz })
        })
    }

    pub fn segment_data(&self, seg: Segment) -> &'a [u8] {
        let start = seg.offset as usize;
        let end = start + seg.filesz as usize;
        &self.data[start..end]
    }

    pub fn dynamic_vaddr(&self) -> Option<u64> {
        self.program_headers()
            .find(|&(p_type, ..)| p_type == PT_DYNAMIC)
            .map(|(_, _, vaddr, ..)| vaddr)
    }
}

pub unsafe fn apply_relative_relocations(dynamic_addr: u64, bias: u64) {
    let mut rela_addr: Option<u64> = None;
    let mut rela_size: Option<u64> = None;

    let mut entry_addr = dynamic_addr;
    loop {
        let tag = unsafe { (entry_addr as *const u64).read() };
        let val = unsafe { ((entry_addr + 8) as *const u64).read() };
        if tag == DT_NULL {
            break;
        }
        if tag == DT_RELA {
            rela_addr = Some(val);
        } else if tag == DT_RELASZ {
            rela_size = Some(val);
        }
        entry_addr += 16;
    }

    let (Some(rela_vaddr), Some(size)) = (rela_addr, rela_size) else {
        return;
    };

    const RELA_ENTRY_SIZE: u64 = 24;
    let count = size / RELA_ENTRY_SIZE;
    let table = bias + rela_vaddr;

    for i in 0..count {
        let entry = table + i * RELA_ENTRY_SIZE;
        let r_offset = unsafe { (entry as *const u64).read() };
        let r_info = unsafe { ((entry + 8) as *const u64).read() };
        let r_addend = unsafe { ((entry + 16) as *const i64).read() };
        let r_type = (r_info & 0xFFFF_FFFF) as u32;

        if r_type != R_X86_64_RELATIVE {
            continue;
        }
        let target = bias + r_offset;
        let value = bias.wrapping_add(r_addend as u64);
        unsafe { (target as *mut u64).write(value) };
    }
}
