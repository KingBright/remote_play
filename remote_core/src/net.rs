use protocol::{ControlMessage, RtpPacket};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;

pub struct UdpMultiplexer {
    socket: Arc<UdpSocket>,
}

impl UdpMultiplexer {
    pub async fn bind(addr: &str) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let std_addr: SocketAddr = addr.parse()?;
        let socket2_sock = socket2::Socket::new(
            if std_addr.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            },
            socket2::Type::DGRAM,
            None,
        )?;

        let _ = socket2_sock.set_recv_buffer_size(2 * 1024 * 1024);
        let _ = socket2_sock.set_send_buffer_size(2 * 1024 * 1024);
        socket2_sock.set_nonblocking(true)?;
        socket2_sock.bind(&std_addr.into())?;

        let std_socket: std::net::UdpSocket = socket2_sock.into();
        let socket = UdpSocket::from_std(std_socket)?;

        Ok(Self {
            socket: Arc::new(socket),
        })
    }

    pub fn split(&self) -> (UdpSender, UdpReceiver) {
        (
            UdpSender {
                socket: self.socket.clone(),
            },
            UdpReceiver {
                socket: self.socket.clone(),
                fragments: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            },
        )
    }
}

#[derive(Clone)]
pub struct UdpSender {
    socket: Arc<UdpSocket>,
}
impl UdpSender {
    async fn send_multiplexed(
        &self,
        header: u8,
        bytes: &[u8],
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let max_payload = 1400;
        if bytes.len() <= max_payload {
            let mut buf = Vec::with_capacity(bytes.len() + 1);
            buf.push(header);
            buf.extend_from_slice(bytes);
            self.socket.send_to(&buf, target).await?;
        } else {
            // Fragment the packet
            // Format: 0x03, original_header(1), fragment_id(4), chunk_idx(2), total_chunks(2), data...
            let fragment_id = rand::random::<u32>();
            let chunks = bytes.chunks(max_payload);
            let total_chunks = chunks.len() as u16;

            for (i, chunk) in chunks.enumerate() {
                let mut buf = Vec::with_capacity(chunk.len() + 10);
                buf.push(0x03); // Fragment multiplex header
                buf.push(header);
                buf.extend_from_slice(&fragment_id.to_be_bytes());
                buf.extend_from_slice(&(i as u16).to_be_bytes());
                buf.extend_from_slice(&total_chunks.to_be_bytes());
                buf.extend_from_slice(chunk);
                self.socket.send_to(&buf, target).await?;

                // Pace the burst: sleep every 10 packets (~14KB) to avoid OS buffer overflow.
                // 100us usually takes ~1ms on macOS. 10 sleeps = ~10ms for a 150KB I-frame.
                if i % 10 == 9 {
                    tokio::time::sleep(std::time::Duration::from_micros(100)).await;
                }
            }
        }
        Ok(())
    }

    pub async fn send_rtp(
        &self,
        packet: &RtpPacket,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let bytes = packet.encode()?;
        self.send_multiplexed(0x01, &bytes, target).await
    }

    pub async fn send_control(
        &self,
        msg: &ControlMessage,
        target: SocketAddr,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let bytes = msg.encode()?;
        self.send_multiplexed(0x02, &bytes, target).await
    }
}

use std::collections::HashMap;
use tokio::sync::Mutex;

pub struct UdpReceiver {
    socket: Arc<UdpSocket>,
    fragments: Arc<Mutex<HashMap<u32, (u16, Vec<Option<Vec<u8>>>)>>>,
}

pub enum MultiplexedPacket {
    Rtp(RtpPacket, SocketAddr),
    Control(ControlMessage, SocketAddr),
}

impl UdpReceiver {
    pub async fn recv(&self) -> Result<MultiplexedPacket, Box<dyn Error + Send + Sync>> {
        loop {
            let mut buf = vec![0u8; 65536];
            let (len, addr) = self.socket.recv_from(&mut buf).await?;
            println!("UdpReceiver received {} bytes, type {}", len, buf[0]);
            if len == 0 {
                continue;
            }
            match buf[0] {
                0x01 => {
                    let rtp = RtpPacket::decode(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Rtp(rtp, addr));
                }
                0x02 => {
                    let msg = ControlMessage::decode(&buf[1..len])?;
                    return Ok(MultiplexedPacket::Control(msg, addr));
                }
                0x03 => {
                    if len < 10 {
                        continue;
                    }
                    let header = buf[1];
                    let fragment_id = u32::from_be_bytes(buf[2..6].try_into().unwrap());
                    let chunk_idx = u16::from_be_bytes(buf[6..8].try_into().unwrap());
                    let total_chunks = u16::from_be_bytes(buf[8..10].try_into().unwrap());

                    let mut fragments = self.fragments.lock().await;
                    let entry = fragments
                        .entry(fragment_id)
                        .or_insert_with(|| (0, vec![None; total_chunks as usize]));

                    if entry.1[chunk_idx as usize].is_none() {
                        entry.1[chunk_idx as usize] = Some(buf[10..len].to_vec());
                        entry.0 += 1;
                    }

                    if entry.0 == total_chunks {
                        let mut full_data = Vec::new();
                        for chunk in entry.1.iter() {
                            full_data.extend_from_slice(chunk.as_ref().unwrap());
                        }
                        fragments.remove(&fragment_id);

                        // Parse full_data
                        match header {
                            0x01 => {
                                let rtp = RtpPacket::decode(&full_data)?;
                                return Ok(MultiplexedPacket::Rtp(rtp, addr));
                            }
                            0x02 => {
                                let msg = ControlMessage::decode(&full_data)?;
                                return Ok(MultiplexedPacket::Control(msg, addr));
                            }
                            _ => {
                                return Err("Unknown UDP multiplexing header inside fragment".into());
                            }
                        }
                    } else if fragments.len() > 10 {
                        // Periodic cleanup of incomplete fragments to prevent memory leak and detect loss
                        let ids: Vec<u32> = fragments.keys().cloned().collect();
                        for id in ids {
                            if id != fragment_id {
                                let old = fragments.remove(&id).unwrap();
                                eprintln!(
                                    "Dropped incomplete packet (id: {}, received {}/{} chunks)",
                                    id,
                                    old.0,
                                    old.1.len()
                                );
                            }
                        }
                    }
                }
                _ => return Err("Unknown UDP multiplexing header".into()),
            }
        }
    }
}
