use protocol::RtpPacket;
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Eq, PartialEq)]
struct JitterPacket {
    packet: RtpPacket,
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
}

impl JitterBuffer {
    pub fn new(_start_seq: u16) -> Self {
        Self {
            heap: BinaryHeap::new(),
            expected_seq: None,
        }
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn push(&mut self, packet: RtpPacket) {
        if let Some(expected) = self.expected_seq {
            let diff = packet.header.sequence_number.wrapping_sub(expected) as i16;
            if !(-1000..=1000).contains(&diff) {
                self.heap.clear();
                self.expected_seq = Some(packet.header.sequence_number);
            }
        } else {
            self.expected_seq = Some(packet.header.sequence_number);
        }
        self.heap.push(JitterPacket { packet });
    }

    pub fn pop(&mut self) -> Option<RtpPacket> {
        let max_buffer_depth = 15;
        let expected = self.expected_seq?;

        while let Some(peek) = self.heap.peek() {
            let diff = peek.packet.header.sequence_number.wrapping_sub(expected) as i16;

            if diff == 0 {
                let packet = self.heap.pop().unwrap().packet;
                self.expected_seq = Some(expected.wrapping_add(1));
                return Some(packet);
            } else if diff < 0 {
                self.heap.pop();
            } else if self.heap.len() > max_buffer_depth {
                let packet = self.heap.pop().unwrap().packet;
                self.expected_seq = Some(packet.header.sequence_number.wrapping_add(1));
                return Some(packet);
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
}
