use std::env;
use std::process::{exit, Command};

fn main() {
    let uefi_path = env!("UEFI_PATH");
    let bios_path = env!("BIOS_PATH");
    let disk_path = env!("DISK_PATH");

    let args: Vec<String> = env::args().collect();
    let prog = &args[0];

    let uefi = match args.get(1).map(|s| s.to_lowercase()) {
        Some(ref s) if s == "uefi" => true,
        Some(ref s) if s == "bios" => false,
        None => false,
        Some(ref s) if s == "-h" || s == "--help" => {
            println!("Usage: {prog} [uefi|bios]");
            println!("  uefi  - boot using OVMF (UEFI)");
            println!("  bios  - boot using legacy BIOS (default)");
            exit(0);
        }
        _ => {
            eprintln!("Usage: {prog} [uefi|bios]");
            exit(1);
        }
    };

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.arg("-serial").arg("mon:stdio");
    cmd.arg("-smp").arg("4");
    cmd.arg("-device")
        .arg("isa-debug-exit,iobase=0xf4,iosize=0x04");
    cmd.arg("-device").arg("piix3-usb-uhci");
    cmd.arg("-device").arg("usb-kbd");
    cmd.arg("-netdev")
        .arg("user,id=net0,hostfwd=tcp::7878-:7878");
    cmd.arg("-device")
        .arg("virtio-net-pci,netdev=net0,disable-legacy=off,disable-modern=on");

    cmd.arg("-drive")
        .arg(format!("format=raw,file={disk_path},if=none,id=blk0"));
    cmd.arg("-device").arg("virtio-blk-pci,drive=blk0");

    if uefi {
        let prebuilt = ovmf_prebuilt::Prebuilt::fetch(ovmf_prebuilt::Source::LATEST, "target/ovmf")
            .expect("failed to update prebuilt");

        let code = prebuilt.get_file(ovmf_prebuilt::Arch::X64, ovmf_prebuilt::FileType::Code);
        let vars = prebuilt.get_file(ovmf_prebuilt::Arch::X64, ovmf_prebuilt::FileType::Vars);

        cmd.arg("-drive").arg(format!("format=raw,file={uefi_path}"));
        cmd.arg("-drive").arg(format!(
            "if=pflash,format=raw,unit=0,file={},readonly=on",
            code.display()
        ));
        cmd.arg("-drive").arg(format!(
            "if=pflash,format=raw,unit=1,file={},snapshot=on",
            vars.display()
        ));
    } else {
        cmd.arg("-drive").arg(format!("format=raw,file={bios_path}"));
    }

    let mut child = cmd.spawn().expect("failed to start qemu-system-x86_64");
    let status = child.wait().expect("failed to wait on qemu");
    let code = match status.code().unwrap_or(1) {
        0x10 => 0,
        0x11 => 1,
        other => other,
    };
    exit(code);
}
