use spin::Mutex;

use crate::virtio_net;

const ETHERTYPE_ARP: u16 = 0x0806;
const ETHERTYPE_IPV4: u16 = 0x0800;
const BROADCAST_MAC: [u8; 6] = [0xff; 6];

pub const OUR_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];

static GATEWAY_MAC: Mutex<Option<[u8; 6]>> = Mutex::new(None);

fn checksum16(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = data.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let [last] = chunks.remainder() {
        sum += u32::from(*last) << 8;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

fn eth_header(out: &mut alloc::vec::Vec<u8>, dst: [u8; 6], src: [u8; 6], ethertype: u16) {
    out.extend_from_slice(&dst);
    out.extend_from_slice(&src);
    out.extend_from_slice(&ethertype.to_be_bytes());
}

fn send_arp_request(our_mac: [u8; 6], target_ip: [u8; 4]) {
    let mut frame = alloc::vec::Vec::new();
    eth_header(&mut frame, BROADCAST_MAC, our_mac, ETHERTYPE_ARP);
    frame.extend_from_slice(&1u16.to_be_bytes());
    frame.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    frame.push(6);
    frame.push(4);
    frame.extend_from_slice(&1u16.to_be_bytes());
    frame.extend_from_slice(&our_mac);
    frame.extend_from_slice(&OUR_IP);
    frame.extend_from_slice(&[0; 6]);
    frame.extend_from_slice(&target_ip);
    virtio_net::send(&frame);
}

fn send_arp_reply(our_mac: [u8; 6], requester_mac: [u8; 6], requester_ip: [u8; 4]) {
    let mut frame = alloc::vec::Vec::new();
    eth_header(&mut frame, requester_mac, our_mac, ETHERTYPE_ARP);
    frame.extend_from_slice(&1u16.to_be_bytes());
    frame.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    frame.push(6);
    frame.push(4);
    frame.extend_from_slice(&2u16.to_be_bytes());
    frame.extend_from_slice(&our_mac);
    frame.extend_from_slice(&OUR_IP);
    frame.extend_from_slice(&requester_mac);
    frame.extend_from_slice(&requester_ip);
    virtio_net::send(&frame);
}

fn ipv4_header(total_len: u16, protocol: u8, src: [u8; 4], dst: [u8; 4]) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[0] = 0x45;
    h[2..4].copy_from_slice(&total_len.to_be_bytes());
    h[6] = 0x40;
    h[8] = 64;
    h[9] = protocol;
    h[12..16].copy_from_slice(&src);
    h[16..20].copy_from_slice(&dst);
    let sum = checksum16(&h);
    h[10..12].copy_from_slice(&sum.to_be_bytes());
    h
}

fn send_icmp_echo_reply(
    our_mac: [u8; 6],
    dst_mac: [u8; 6],
    dst_ip: [u8; 4],
    identifier: u16,
    sequence: u16,
    payload: &[u8],
) {
    let mut icmp = alloc::vec::Vec::new();
    icmp.push(0);
    icmp.push(0);
    icmp.extend_from_slice(&0u16.to_be_bytes());
    icmp.extend_from_slice(&identifier.to_be_bytes());
    icmp.extend_from_slice(&sequence.to_be_bytes());
    icmp.extend_from_slice(payload);
    let sum = checksum16(&icmp);
    icmp[2..4].copy_from_slice(&sum.to_be_bytes());

    let ip_header = ipv4_header((20 + icmp.len()) as u16, 1, OUR_IP, dst_ip);

    let mut frame = alloc::vec::Vec::new();
    eth_header(&mut frame, dst_mac, our_mac, ETHERTYPE_IPV4);
    frame.extend_from_slice(&ip_header);
    frame.extend_from_slice(&icmp);
    virtio_net::send(&frame);

    let _ = our_mac;
}

pub fn send_udp(our_mac: [u8; 6], dst_ip: [u8; 4], dst_port: u16, src_port: u16, payload: &[u8]) {
    let Some(gateway_mac) = *GATEWAY_MAC.lock() else {
        return;
    };

    let mut udp = alloc::vec::Vec::new();
    udp.extend_from_slice(&src_port.to_be_bytes());
    udp.extend_from_slice(&dst_port.to_be_bytes());
    udp.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    udp.extend_from_slice(&0u16.to_be_bytes());
    udp.extend_from_slice(payload);

    let ip_header = ipv4_header((20 + udp.len()) as u16, 17, OUR_IP, dst_ip);

    let mut frame = alloc::vec::Vec::new();
    eth_header(&mut frame, gateway_mac, our_mac, ETHERTYPE_IPV4);
    frame.extend_from_slice(&ip_header);
    frame.extend_from_slice(&udp);
    virtio_net::send(&frame);
}

fn handle_arp(our_mac: [u8; 6], packet: &[u8]) {
    if packet.len() < 28 {
        return;
    }
    let opcode = u16::from_be_bytes([packet[6], packet[7]]);
    let sender_mac: [u8; 6] = packet[8..14].try_into().unwrap();
    let sender_ip: [u8; 4] = packet[14..18].try_into().unwrap();
    let target_ip: [u8; 4] = packet[24..28].try_into().unwrap();

    match opcode {
        1 if target_ip == OUR_IP => send_arp_reply(our_mac, sender_mac, sender_ip),
        2 if sender_ip == GATEWAY_IP => *GATEWAY_MAC.lock() = Some(sender_mac),
        _ => {}
    }
}

fn handle_ipv4(our_mac: [u8; 6], src_mac: [u8; 6], packet: &[u8]) {
    if packet.len() < 20 {
        return;
    }
    let ihl = usize::from(packet[0] & 0x0F) * 4;
    if packet.len() < ihl {
        return;
    }
    let protocol = packet[9];
    let dst_ip: [u8; 4] = packet[16..20].try_into().unwrap();
    if dst_ip != OUR_IP {
        return;
    }
    let payload = &packet[ihl..];

    if protocol == 1 && payload.len() >= 8 && payload[0] == 8 {
        let src_ip: [u8; 4] = packet[12..16].try_into().unwrap();
        let identifier = u16::from_be_bytes([payload[4], payload[5]]);
        let sequence = u16::from_be_bytes([payload[6], payload[7]]);
        send_icmp_echo_reply(our_mac, src_mac, src_ip, identifier, sequence, &payload[8..]);
    }

    if protocol == 6 {
        let src_ip: [u8; 4] = packet[12..16].try_into().unwrap();
        crate::tcp::handle(our_mac, src_mac, src_ip, payload);
    }
}

pub fn run() -> ! {
    if !virtio_net::init() {
        let _ = writeln_serial("[net] no virtio-net device found; networking disabled");
        loop {
            x86_64::instructions::hlt();
        }
    }
    let our_mac = virtio_net::mac_address().expect("virtio_net::init just succeeded");
    let _ = writeln_serial(&alloc::format!(
        "[net] up: mac={our_mac:02x?} ip={OUR_IP:?}"
    ));
    let _ = writeln_serial(&alloc::format!(
        "[net] TCP server listening on port {}",
        crate::tcp::LISTEN_PORT
    ));

    send_arp_request(our_mac, GATEWAY_IP);

    let mut buf = [0u8; 1600];
    let mut announced_gateway = false;
    let mut sent_demo_udp = false;

    loop {
        if let Some(len) = virtio_net::try_receive(&mut buf) {
            if len >= 14 {
                let src_mac: [u8; 6] = buf[6..12].try_into().unwrap();
                let ethertype = u16::from_be_bytes([buf[12], buf[13]]);
                match ethertype {
                    ETHERTYPE_ARP => handle_arp(our_mac, &buf[14..len]),
                    ETHERTYPE_IPV4 => handle_ipv4(our_mac, src_mac, &buf[14..len]),
                    _ => {}
                }
            }
        }

        if !announced_gateway {
            if let Some(mac) = *GATEWAY_MAC.lock() {
                announced_gateway = true;
                let _ = writeln_serial(&alloc::format!("[net] gateway resolved: {mac:02x?}"));
            }
        }

        if announced_gateway && !sent_demo_udp {
            sent_demo_udp = true;
            send_udp(our_mac, GATEWAY_IP, 9, 4444, b"hello from oxide\n");
            let _ = writeln_serial("[net] sent demo UDP datagram to gateway:9");
        }

        crate::scheduler::schedule();
    }
}

fn writeln_serial(s: &str) -> core::fmt::Result {
    use core::fmt::Write;
    writeln!(crate::serial(), "{s}")
}
