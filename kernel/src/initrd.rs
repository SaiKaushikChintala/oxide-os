use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

pub trait FileSystem {
    fn read(&self, name: &str) -> Option<Vec<u8>>;
}

pub struct Initrd {
    data: Vec<u8>,
}

static HELLO_ELF: &[u8] = include_bytes!("../../userspace/hello.elf");

impl Initrd {
    pub fn build() -> Self {
        let mut data = Vec::new();
        let files: [(&str, Cow<[u8]>); 6] = [
            ("init", Cow::Owned(build_elf(&INIT_CODE, INIT_ENTRY_VADDR))),
            ("producer", Cow::Owned(build_elf(&PRODUCER_CODE, PRODUCER_ENTRY_VADDR))),
            ("consumer", Cow::Owned(build_elf(&CONSUMER_CODE, CONSUMER_ENTRY_VADDR))),
            ("forker", Cow::Owned(build_elf(&FORKER_CODE, FORKER_ENTRY_VADDR))),
            ("hello", Cow::Borrowed(HELLO_ELF)),
            ("pie_demo", Cow::Owned(build_pie_demo())),
        ];

        data.extend_from_slice(&(files.len() as u32).to_le_bytes());
        for (name, contents) in &files {
            write_entry(&mut data, name, contents);
        }

        Initrd { data }
    }
}

fn write_entry(out: &mut Vec<u8>, name: &str, contents: &[u8]) {
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(contents.len() as u32).to_le_bytes());
    out.extend_from_slice(contents);
}

impl FileSystem for Initrd {
    fn read(&self, name: &str) -> Option<Vec<u8>> {
        let mut pos = 0usize;
        let count = read_u32(&self.data, &mut pos)?;

        for _ in 0..count {
            let name_len = read_u32(&self.data, &mut pos)? as usize;
            let entry_name = String::from_utf8(self.data.get(pos..pos + name_len)?.to_vec()).ok()?;
            pos += name_len;

            let data_len = read_u32(&self.data, &mut pos)? as usize;
            let entry_data = self.data.get(pos..pos + data_len)?;
            pos += data_len;

            if entry_name == name {
                return Some(entry_data.to_vec());
            }
        }
        None
    }
}

fn read_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let bytes = data.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_le_bytes(bytes.try_into().unwrap()))
}

const INIT_ENTRY_VADDR: u64 = 0x7777_7777_0000;

#[rustfmt::skip]
static INIT_CODE: [u8; 16] = [
    0xB8, 0x03, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0x89, 0xC7,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x0F, 0x05,
];

const PRODUCER_ENTRY_VADDR: u64 = 0x2222_2222_0000;
const CONSUMER_ENTRY_VADDR: u64 = 0x3333_3333_0000;

#[rustfmt::skip]
static PRODUCER_CODE: [u8; 38] = [
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x04, 0x00, 0x00, 0x00,
    0x48, 0x8D, 0x35, 0x10, 0x00, 0x00, 0x00,
    0xBA, 0x05, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x31, 0xFF,
    0x0F, 0x05,
    b'p', b'i', b'n', b'g', b'\n',
];

#[rustfmt::skip]
static CONSUMER_CODE: [u8; 53] = [
    0xB8, 0x05, 0x00, 0x00, 0x00,
    0xBF, 0x03, 0x00, 0x00, 0x00,
    0x48, 0x81, 0xEC, 0x40, 0x00, 0x00, 0x00,
    0x48, 0x89, 0xE6,
    0xBA, 0x20, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0x89, 0xC2,
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x48, 0x89, 0xE6,
    0x0F, 0x05,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x31, 0xFF,
    0x0F, 0x05,
];

const FORKER_ENTRY_VADDR: u64 = 0x4000_0000_0000;

#[rustfmt::skip]
static FORKER_CODE: [u8; 185] = [
    0xB8, 0x09, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0x85, 0xC0,
    0x75, 0x2D,
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x48, 0x8D, 0x35, 0x60, 0x00, 0x00, 0x00,
    0xBA, 0x15, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x0A, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x31, 0xFF,
    0x0F, 0x05,
    0x89, 0xC3,
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x48, 0x8D, 0x35, 0x46, 0x00, 0x00, 0x00,
    0xBA, 0x15, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x07, 0x00, 0x00, 0x00,
    0x89, 0xDF,
    0x0F, 0x05,
    0xB8, 0x01, 0x00, 0x00, 0x00,
    0xBF, 0x01, 0x00, 0x00, 0x00,
    0x48, 0x8D, 0x35, 0x3A, 0x00, 0x00, 0x00,
    0xBA, 0x13, 0x00, 0x00, 0x00,
    0x0F, 0x05,
    0xB8, 0x02, 0x00, 0x00, 0x00,
    0x31, 0xFF,
    0x0F, 0x05,
    b'c', b'h', b'i', b'l', b'd', b':', b' ', b'e', b'x', b'e', b'c', b' ',
    b'p', b'r', b'o', b'd', b'u', b'c', b'e', b'r', b'\n',
    b'p', b'a', b'r', b'e', b'n', b't', b':', b' ', b'f', b'o', b'r', b'k',
    b'e', b'd', b' ', b'c', b'h', b'i', b'l', b'd', b'\n',
    b'p', b'a', b'r', b'e', b'n', b't', b':', b' ', b'c', b'h', b'i', b'l',
    b'd', b' ', b'd', b'o', b'n', b'e', b'\n',
];

fn build_elf(code: &[u8], entry_vaddr: u64) -> Vec<u8> {
    let mut elf = Vec::new();

    elf.extend_from_slice(b"\x7fELF");
    elf.push(2);
    elf.push(1);
    elf.push(1);
    elf.push(0);
    elf.extend_from_slice(&[0u8; 8]);
    elf.extend_from_slice(&2u16.to_le_bytes());
    elf.extend_from_slice(&0x3Eu16.to_le_bytes());
    elf.extend_from_slice(&1u32.to_le_bytes());
    elf.extend_from_slice(&entry_vaddr.to_le_bytes());
    elf.extend_from_slice(&64u64.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes());
    elf.extend_from_slice(&0u32.to_le_bytes());
    elf.extend_from_slice(&64u16.to_le_bytes());
    elf.extend_from_slice(&56u16.to_le_bytes());
    elf.extend_from_slice(&1u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    debug_assert_eq!(elf.len(), 64);

    let code_offset = 64u64 + 56u64;
    elf.extend_from_slice(&1u32.to_le_bytes());
    elf.extend_from_slice(&5u32.to_le_bytes());
    elf.extend_from_slice(&code_offset.to_le_bytes());
    elf.extend_from_slice(&entry_vaddr.to_le_bytes());
    elf.extend_from_slice(&entry_vaddr.to_le_bytes());
    elf.extend_from_slice(&(code.len() as u64).to_le_bytes());
    elf.extend_from_slice(&(code.len() as u64).to_le_bytes());
    elf.extend_from_slice(&0x1000u64.to_le_bytes());
    debug_assert_eq!(elf.len(), 120);

    elf.extend_from_slice(code);
    elf
}

fn build_pie_demo() -> Vec<u8> {
    let message = b"hello from a relocated (PIE) pointer!\n";

    let mut elf = Vec::new();

    elf.extend_from_slice(b"\x7fELF");
    elf.push(2);
    elf.push(1);
    elf.push(1);
    elf.push(0);
    elf.extend_from_slice(&[0u8; 8]);
    elf.extend_from_slice(&3u16.to_le_bytes());
    elf.extend_from_slice(&0x3Eu16.to_le_bytes());
    elf.extend_from_slice(&1u32.to_le_bytes());
    let entry_fixup_pos = elf.len();
    elf.extend_from_slice(&0u64.to_le_bytes());
    elf.extend_from_slice(&64u64.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes());
    elf.extend_from_slice(&0u32.to_le_bytes());
    elf.extend_from_slice(&64u16.to_le_bytes());
    elf.extend_from_slice(&56u16.to_le_bytes());
    elf.extend_from_slice(&2u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    elf.extend_from_slice(&0u16.to_le_bytes());
    debug_assert_eq!(elf.len(), 64);

    let pt_load_pos = elf.len();
    elf.extend_from_slice(&[0u8; 56]);
    let pt_dynamic_pos = elf.len();
    elf.extend_from_slice(&[0u8; 56]);
    debug_assert_eq!(elf.len(), 64 + 112);

    let code_off = elf.len() as u64;
    elf.extend_from_slice(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
    elf.extend_from_slice(&[0xBF, 0x01, 0x00, 0x00, 0x00]);
    let lea_disp_pos = elf.len() + 3;
    elf.extend_from_slice(&[0x48, 0x8D, 0x35, 0x00, 0x00, 0x00, 0x00]);
    elf.extend_from_slice(&[0x48, 0x8B, 0x36]);
    elf.extend_from_slice(&[0xBA]);
    elf.extend_from_slice(&(message.len() as u32).to_le_bytes());
    elf.extend_from_slice(&[0x0F, 0x05]);
    elf.extend_from_slice(&[0xB8, 0x02, 0x00, 0x00, 0x00]);
    elf.extend_from_slice(&[0x31, 0xFF]);
    elf.extend_from_slice(&[0x0F, 0x05]);
    let code_end = elf.len() as u64;

    let ptr_slot_off = elf.len() as u64;
    elf.extend_from_slice(&0u64.to_le_bytes());

    let next_instr = lea_disp_pos as u64 + 4;
    let disp = (ptr_slot_off as i64 - next_instr as i64) as i32;
    elf[lea_disp_pos..lea_disp_pos + 4].copy_from_slice(&disp.to_le_bytes());

    let dynamic_off = elf.len() as u64;
    let rela_off_pos = elf.len();
    elf.extend_from_slice(&7u64.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes());
    elf.extend_from_slice(&8u64.to_le_bytes());
    elf.extend_from_slice(&24u64.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes());
    elf.extend_from_slice(&0u64.to_le_bytes());

    let rela_off = elf.len() as u64;
    elf[rela_off_pos + 8..rela_off_pos + 16].copy_from_slice(&rela_off.to_le_bytes());
    let message_off = elf.len() as u64 + 24;
    elf.extend_from_slice(&ptr_slot_off.to_le_bytes());
    elf.extend_from_slice(&8u64.to_le_bytes());
    elf.extend_from_slice(&(message_off as i64).to_le_bytes());

    debug_assert_eq!(elf.len() as u64, message_off);
    elf.extend_from_slice(message);
    let file_end = elf.len() as u64;

    elf[entry_fixup_pos..entry_fixup_pos + 8].copy_from_slice(&code_off.to_le_bytes());

    let mut pt_load = Vec::with_capacity(56);
    pt_load.extend_from_slice(&1u32.to_le_bytes());
    pt_load.extend_from_slice(&6u32.to_le_bytes());
    pt_load.extend_from_slice(&0u64.to_le_bytes());
    pt_load.extend_from_slice(&0u64.to_le_bytes());
    pt_load.extend_from_slice(&0u64.to_le_bytes());
    pt_load.extend_from_slice(&file_end.to_le_bytes());
    pt_load.extend_from_slice(&file_end.to_le_bytes());
    pt_load.extend_from_slice(&0x1000u64.to_le_bytes());
    elf[pt_load_pos..pt_load_pos + 56].copy_from_slice(&pt_load);

    let mut pt_dynamic = Vec::with_capacity(56);
    pt_dynamic.extend_from_slice(&2u32.to_le_bytes());
    pt_dynamic.extend_from_slice(&6u32.to_le_bytes());
    pt_dynamic.extend_from_slice(&dynamic_off.to_le_bytes());
    pt_dynamic.extend_from_slice(&dynamic_off.to_le_bytes());
    pt_dynamic.extend_from_slice(&dynamic_off.to_le_bytes());
    pt_dynamic.extend_from_slice(&48u64.to_le_bytes());
    pt_dynamic.extend_from_slice(&48u64.to_le_bytes());
    pt_dynamic.extend_from_slice(&8u64.to_le_bytes());
    elf[pt_dynamic_pos..pt_dynamic_pos + 56].copy_from_slice(&pt_dynamic);

    let _ = code_end;
    debug_assert!(code_end <= ptr_slot_off);
    elf
}
