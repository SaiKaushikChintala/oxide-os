use alloc::vec::Vec;
use spin::Mutex;

use crate::net;
use crate::virtio_net;

pub const LISTEN_PORT: u16 = 7878;

const FLAG_FIN: u8 = 0x01;
const FLAG_SYN: u8 = 0x02;
const FLAG_RST: u8 = 0x04;
const FLAG_PSH: u8 = 0x08;
const FLAG_ACK: u8 = 0x10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    SynReceived,
    Established,
    FinWait,
}

struct Connection {
    state: State,
    peer_mac: [u8; 6],
    peer_ip: [u8; 4],
    peer_port: u16,
    our_seq: u32,
    their_seq: u32,
}

static CONN: Mutex<Option<Connection>> = Mutex::new(None);

fn tcp_checksum(src_ip: [u8; 4], dst_ip: [u8; 4], segment: &[u8]) -> u16 {
    let mut pseudo = Vec::with_capacity(12 + segment.len() + 1);
    pseudo.extend_from_slice(&src_ip);
    pseudo.extend_from_slice(&dst_ip);
    pseudo.push(0);
    pseudo.push(6);
    pseudo.extend_from_slice(&(segment.len() as u16).to_be_bytes());
    pseudo.extend_from_slice(segment);
    checksum16(&pseudo)
}

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

fn ipv4_header(total_len: u16, src: [u8; 4], dst: [u8; 4]) -> [u8; 20] {
    let mut h = [0u8; 20];
    h[0] = 0x45;
    h[2..4].copy_from_slice(&total_len.to_be_bytes());
    h[6] = 0x40;
    h[8] = 64;
    h[9] = 6;
    h[12..16].copy_from_slice(&src);
    h[16..20].copy_from_slice(&dst);
    let sum = checksum16(&h);
    h[10..12].copy_from_slice(&sum.to_be_bytes());
    h
}

fn eth_header(out: &mut Vec<u8>, dst: [u8; 6], src: [u8; 6]) {
    out.extend_from_slice(&dst);
    out.extend_from_slice(&src);
    out.extend_from_slice(&0x0800u16.to_be_bytes());
}

#[allow(clippy::too_many_arguments)]
fn send_segment(
    our_mac: [u8; 6],
    peer_mac: [u8; 6],
    our_ip: [u8; 4],
    peer_ip: [u8; 4],
    our_port: u16,
    peer_port: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    payload: &[u8],
) {
    let mut tcp = Vec::with_capacity(20 + payload.len());
    tcp.extend_from_slice(&our_port.to_be_bytes());
    tcp.extend_from_slice(&peer_port.to_be_bytes());
    tcp.extend_from_slice(&seq.to_be_bytes());
    tcp.extend_from_slice(&ack.to_be_bytes());
    tcp.push(5 << 4);
    tcp.push(flags);
    tcp.extend_from_slice(&4096u16.to_be_bytes());
    tcp.extend_from_slice(&0u16.to_be_bytes());
    tcp.extend_from_slice(&0u16.to_be_bytes());
    tcp.extend_from_slice(payload);

    let sum = tcp_checksum(our_ip, peer_ip, &tcp);
    tcp[16..18].copy_from_slice(&sum.to_be_bytes());

    let ip_header = ipv4_header((20 + tcp.len()) as u16, our_ip, peer_ip);

    let mut frame = Vec::with_capacity(14 + 20 + tcp.len());
    eth_header(&mut frame, peer_mac, our_mac);
    frame.extend_from_slice(&ip_header);
    frame.extend_from_slice(&tcp);
    virtio_net::send(&frame);
}

pub fn handle(our_mac: [u8; 6], src_mac: [u8; 6], src_ip: [u8; 4], packet: &[u8]) {
    if packet.len() < 20 {
        return;
    }
    let src_port = u16::from_be_bytes([packet[0], packet[1]]);
    let dst_port = u16::from_be_bytes([packet[2], packet[3]]);
    if dst_port != LISTEN_PORT {
        return;
    }
    let seq = u32::from_be_bytes(packet[4..8].try_into().unwrap());
    let ack = u32::from_be_bytes(packet[8..12].try_into().unwrap());
    let data_offset = usize::from(packet[12] >> 4) * 4;
    let flags = packet[13];
    let data = packet.get(data_offset..).unwrap_or(&[]);

    let mut conn_guard = CONN.lock();

    if flags & FLAG_SYN != 0 && flags & FLAG_ACK == 0 {
        let our_seq: u32 = 0x1000_0000;
        *conn_guard = Some(Connection {
            state: State::SynReceived,
            peer_mac: src_mac,
            peer_ip: src_ip,
            peer_port: src_port,
            our_seq: our_seq.wrapping_add(1),
            their_seq: seq.wrapping_add(1),
        });
        drop(conn_guard);
        send_segment(
            our_mac, src_mac, net::OUR_IP, src_ip, LISTEN_PORT, src_port,
            our_seq, seq.wrapping_add(1), FLAG_SYN | FLAG_ACK, &[],
        );
        return;
    }

    let Some(conn) = conn_guard.as_mut() else { return };
    if conn.peer_ip != src_ip || conn.peer_port != src_port {
        return;
    }

    if flags & FLAG_RST != 0 {
        *conn_guard = None;
        return;
    }

    match conn.state {
        State::SynReceived if flags & FLAG_ACK != 0 => {
            conn.state = State::Established;
        }
        State::Established => {
            if !data.is_empty() {
                conn.their_seq = conn.their_seq.wrapping_add(data.len() as u32);
                let response = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\n\
Hello from Oxide's from-scratch TCP/IP stack!\n";

                let (our_seq, their_seq, peer_mac, peer_ip, peer_port) =
                    (conn.our_seq, conn.their_seq, conn.peer_mac, conn.peer_ip, conn.peer_port);
                conn.our_seq = conn.our_seq.wrapping_add(response.len() as u32);
                conn.state = State::FinWait;
                let our_seq_after = conn.our_seq;
                drop(conn_guard);

                send_segment(
                    our_mac, peer_mac, net::OUR_IP, peer_ip, LISTEN_PORT, peer_port,
                    our_seq, their_seq, FLAG_ACK, &[],
                );
                send_segment(
                    our_mac, peer_mac, net::OUR_IP, peer_ip, LISTEN_PORT, peer_port,
                    our_seq, their_seq, FLAG_PSH | FLAG_ACK, response,
                );
                send_segment(
                    our_mac, peer_mac, net::OUR_IP, peer_ip, LISTEN_PORT, peer_port,
                    our_seq_after, their_seq, FLAG_FIN | FLAG_ACK, &[],
                );
            } else if flags & FLAG_FIN != 0 {
                conn.their_seq = conn.their_seq.wrapping_add(1);
                let (our_seq, their_seq, peer_mac, peer_ip, peer_port) =
                    (conn.our_seq, conn.their_seq, conn.peer_mac, conn.peer_ip, conn.peer_port);
                *conn_guard = None;
                send_segment(
                    our_mac, peer_mac, net::OUR_IP, peer_ip, LISTEN_PORT, peer_port,
                    our_seq, their_seq, FLAG_ACK, &[],
                );
            }
        }
        State::FinWait if flags & FLAG_ACK != 0 => {
            *conn_guard = None;
        }
        _ => {
            let _ = ack;
        }
    }
}
