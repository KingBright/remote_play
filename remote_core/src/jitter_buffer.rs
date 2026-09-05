use crate::timing::quanta_now_us;
use protocol::{FrameTimingCheckpoints, RtpPacket};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Eq, PartialEq)]
struct JitterPacket {
    packet: RtpPacket,
    timing: FrameTimingCheckpoints,
    enter_ts_us: u64,
}

impl PartialOrd for JitterPacket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JitterPacket {
    fn cmp(&self, other: &Self) -> Ordering {
        let diff = other
            .packet
            .header
            .sequence_number
            .wrapping_sub(self.packet.header.sequence_number) as i16;
        if diff == 0 {
            Ordering::Equal
        } else if diff > 0 {
            Ordering::Greater
        } else {
            Ordering::Less
        }
    }
}

pub struct JitterBuffer {
    heap: BinaryHeap<JitterPacket>,
    expected_seq: Option<u16>,
    late_frames_dropped: u64,
    queue_full_dropped: u64,
}

impl JitterBuffer {
    pub fn new(_start_seq: u16) -> Self {
        Self {
            heap: BinaryHeap::new(),
            expected_seq: None,
            late_frames_dropped: 0,
            queue_full_dropped: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn late_frames_dropped(&self) -> u64 {
        self.late_frames_dropped
    }

    pub fn queue_full_dropped(&self) -> u64 {
        self.queue_full_dropped
    }

    pub fn push(&mut self, packet: RtpPacket) {
        let capture_ts_us = if packet.header.timestamp > 0 {
            packet.header.timestamp as u64 * 1_000
        } else {
            quanta_now_us()
        };
        let timing = FrameTimingCheckpoints::new(capture_ts_us);
        self.push_with_timing(packet, timing);
    }

    pub fn push_with_timing(&mut self, packet: RtpPacket, mut timing: FrameTimingCheckpoints) {
        let enter_ts_us = quanta_now_us();
        if timing.jitter_enter_ts_us == 0 {
            if timing.recv_ts_us > 0 {
                timing.jitter_enter_ts_us = timing.recv_ts_us;
            } else if timing.capture_ts_us > 0 {
                timing.jitter_enter_ts_us =
                    crate::timing::client_stage_offset_us(timing.capture_ts_us, enter_ts_us, 0);
            }
        }

        if let Some(expected) = self.expected_seq {
            let diff = packet.header.sequence_number.wrapping_sub(expected) as i16;
            if !(-1000..=1000).contains(&diff) {
                self.heap.clear();
                self.expected_seq = Some(packet.header.sequence_number);
            }
        } else {
            self.expected_seq = Some(packet.header.sequence_number);
        }

        self.heap.push(JitterPacket {
            packet,
            timing,
            enter_ts_us,
        });
    }

    pub fn pop(&mut self) -> Option<RtpPacket> {
        self.pop_with_timing().map(|(packet, _)| packet)
    }

    pub fn pop_with_timing(&mut self) -> Option<(RtpPacket, FrameTimingCheckpoints)> {
        let max_buffer_depth = 15;
        let expected = self.expected_seq?;

        while let Some(peek) = self.heap.peek() {
            let diff = peek.packet.header.sequence_number.wrapping_sub(expected) as i16;

            if diff == 0 {
                let mut item = self.heap.pop().unwrap();
                self.expected_seq = Some(expected.wrapping_add(1));
                let exit_ts_us = quanta_now_us();
                let residency_us = exit_ts_us.saturating_sub(item.enter_ts_us) as u32;
                item.timing.jitter_exit_ts_us =
                    item.timing.jitter_enter_ts_us.saturating_add(residency_us);
                return Some((item.packet, item.timing));
            } else if diff < 0 {
                self.heap.pop();
                self.late_frames_dropped += 1;
            } else if self.heap.len() > max_buffer_depth {
                let mut item = self.heap.pop().unwrap();
                self.expected_seq = Some(item.packet.header.sequence_number.wrapping_add(1));
                self.queue_full_dropped += 1;
                let exit_ts_us = quanta_now_us();
                let residency_us = exit_ts_us.saturating_sub(item.enter_ts_us) as u32;
                item.timing.jitter_exit_ts_us =
                    item.timing.jitter_enter_ts_us.saturating_add(residency_us);
                return Some((item.packet, item.timing));
            } else {
                break;
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::RtpHeader;

    fn packet(sequence_number: u16) -> RtpPacket {
        RtpPacket {
            header: RtpHeader {
                version: 2,
                payload_type: 96,
                sequence_number,
                timestamp: sequence_number as u32 * 100,
                ssrc: 7,
            },
            payload: sequence_number.to_be_bytes().to_vec(),
        }
    }

    fn pop_seq(buffer: &mut JitterBuffer) -> Option<u16> {
        buffer.pop().map(|packet| packet.header.sequence_number)
    }

    #[test]
    fn jitter_enter_keeps_recv_domain_when_client_clock_is_behind() {
        let mut buffer = JitterBuffer::new(10);
        let mut timing = FrameTimingCheckpoints::new(quanta_now_us() + 5_000_000);
        timing.recv_ts_us = 8_000;
        buffer.push_with_timing(packet(1), timing);
        let (_, out) = buffer.pop_with_timing().expect("packet");
        assert_eq!(out.jitter_enter_ts_us, 8_000);
        assert!(out.jitter_exit_ts_us >= 8_000);
    }

    #[test]
    fn pops_in_order_packets_immediately() {
        let mut buffer = JitterBuffer::new(10);

        buffer.push(packet(10));
        buffer.push(packet(11));
        buffer.push(packet(12));

        assert_eq!(pop_seq(&mut buffer), Some(10));
        assert_eq!(pop_seq(&mut buffer), Some(11));
        assert_eq!(pop_seq(&mut buffer), Some(12));
        assert_eq!(pop_seq(&mut buffer), None);
    }

    #[test]
    fn waits_for_missing_packet_before_releasing_future_packet() {
        let mut buffer = JitterBuffer::new(10);

        buffer.push(packet(10));
        buffer.push(packet(12));

        assert_eq!(pop_seq(&mut buffer), Some(10));
        assert_eq!(pop_seq(&mut buffer), None);

        buffer.push(packet(11));

        assert_eq!(pop_seq(&mut buffer), Some(11));
        assert_eq!(pop_seq(&mut buffer), Some(12));
        assert_eq!(pop_seq(&mut buffer), None);
    }

    #[test]
    fn drops_stale_packets_and_continues_with_expected_sequence() {
        let mut buffer = JitterBuffer::new(10);

        buffer.push(packet(10));
        assert_eq!(pop_seq(&mut buffer), Some(10));

        buffer.push(packet(9));
        buffer.push(packet(11));

        assert_eq!(pop_seq(&mut buffer), Some(11));
        assert_eq!(pop_seq(&mut buffer), None);
    }

    #[test]
    fn skips_missing_packet_after_buffer_depth_limit() {
        let mut buffer = JitterBuffer::new(1);

        buffer.push(packet(1));
        assert_eq!(pop_seq(&mut buffer), Some(1));

        for sequence_number in 3..=18 {
            buffer.push(packet(sequence_number));
        }

        assert_eq!(pop_seq(&mut buffer), Some(3));
        assert_eq!(pop_seq(&mut buffer), Some(4));
    }

    #[test]
    fn handles_sequence_number_wraparound() {
        let mut buffer = JitterBuffer::new(u16::MAX - 1);

        buffer.push(packet(u16::MAX - 1));
        buffer.push(packet(0));
        buffer.push(packet(u16::MAX));

        assert_eq!(pop_seq(&mut buffer), Some(u16::MAX - 1));
        assert_eq!(pop_seq(&mut buffer), Some(u16::MAX));
        assert_eq!(pop_seq(&mut buffer), Some(0));
        assert_eq!(pop_seq(&mut buffer), None);
    }

    #[test]
    fn resets_when_sequence_jumps_far_from_expected() {
        let mut buffer = JitterBuffer::new(10);

        buffer.push(packet(10));
        assert_eq!(pop_seq(&mut buffer), Some(10));

        buffer.push(packet(2_000));

        assert_eq!(pop_seq(&mut buffer), Some(2_000));
        assert_eq!(pop_seq(&mut buffer), None);
    }

    #[test]
    fn tracks_timing_and_drop_reasons() {
        let mut buffer = JitterBuffer::new(100);

        let mut timing1 = FrameTimingCheckpoints::new(1_000_000);
        timing1.recv_ts_us = 5_000;
        timing1.jitter_enter_ts_us = 5_200;

        buffer.push_with_timing(packet(100), timing1);

        let (popped_pkt, popped_timing) = buffer.pop_with_timing().expect("should pop 100");
        assert_eq!(popped_pkt.header.sequence_number, 100);
        assert_eq!(popped_timing.capture_ts_us, 1_000_000);
        assert_eq!(popped_timing.recv_ts_us, 5_000);
        assert_eq!(popped_timing.jitter_enter_ts_us, 5_200);
        assert!(popped_timing.jitter_exit_ts_us >= popped_timing.jitter_enter_ts_us);

        // Push stale packet (seq 99)
        buffer.push(packet(99));
        assert_eq!(buffer.pop(), None);
        assert_eq!(buffer.late_frames_dropped(), 1);

        // Push overflowing packets
        for seq in 102..=120 {
            buffer.push(packet(seq));
        }
        let overflow_pkt = buffer.pop().expect("should pop overflow packet");
        assert_eq!(overflow_pkt.header.sequence_number, 102);
        assert!(buffer.queue_full_dropped() >= 1);
    }
}
