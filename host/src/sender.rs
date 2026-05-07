use protocol::RtpPacket;
use remote_core::{clock::TimeBox, net::UdpSender};
use std::error::Error;
use std::net::SocketAddr;
use std::time::Duration;

pub struct PacedSender {
    udp_sender: UdpSender,
    time_box: TimeBox,
    target: SocketAddr,
}

impl PacedSender {
    pub fn new(udp_sender: UdpSender, target: SocketAddr) -> Self {
        Self {
            udp_sender,
            time_box: TimeBox::new(),
            target,
        }
    }

    pub async fn send_frame(
        &self,
        packets: Vec<RtpPacket>,
        frame_duration: Duration,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        if packets.is_empty() {
            return Ok(());
        }

        // Pacing strategy: Distribute the packets evenly over the frame duration.
        // E.g., for a 60Hz frame (16.6ms), if we have 20 packets, we send one every 833 microseconds.
        let interval = frame_duration / packets.len() as u32;

        for packet in packets {
            self.udp_sender.send_rtp(&packet, self.target).await?;
            // Spin sleep for precision frame pacing
            self.time_box.spin_sleep(interval);
        }

        Ok(())
    }
}
