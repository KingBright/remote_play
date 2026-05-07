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
        // BinaryHeap is a max-heap. We want the packet with the SMALLEST sequence
        // number to have the highest priority (be popped first).
        // A sequence number `a` is "greater" (newer) than `b` if `a - b > 0` in i16 arithmetic.
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

    pub fn push(&mut self, packet: RtpPacket) {
        if let Some(expected) = self.expected_seq {
            let diff = packet.header.sequence_number.wrapping_sub(expected) as i16;
            // If the sequence number jumps significantly (e.g., stream restarted),
            // reset the jitter buffer to the new sequence number.
            if diff < -1000 || diff > 1000 {
                self.heap.clear();
                self.expected_seq = Some(packet.header.sequence_number);
            }
        } else {
            // First packet received, initialize expected_seq
            self.expected_seq = Some(packet.header.sequence_number);
        }
        self.heap.push(JitterPacket { packet });
    }

    pub fn pop(&mut self) -> Option<RtpPacket> {
        let max_buffer_depth = 15; // Max frames to queue before assuming packet loss

        let expected = if let Some(e) = self.expected_seq {
            e
        } else {
            return None;
        };

        while let Some(peek) = self.heap.peek() {
            let diff = peek.packet.header.sequence_number.wrapping_sub(expected) as i16;

            if diff == 0 {
                // Perfect, we got the next expected packet in sequence.
                let pkt = self.heap.pop().unwrap().packet;
                self.expected_seq = Some(expected.wrapping_add(1));
                return Some(pkt);
            } else if diff < 0 {
                // Stale packet
                self.heap.pop();
            } else if self.heap.len() > max_buffer_depth {
                // The expected packet hasn't arrived and our buffer is too deep.
                // We MUST skip the missing packet(s) to break the freeze.
                let pkt = self.heap.pop().unwrap().packet;
                self.expected_seq = Some(pkt.header.sequence_number.wrapping_add(1));
                return Some(pkt);
            } else {
                // The packet is in the future (diff > 0), and our buffer isn't full yet.
                // Wait for the expected packet to arrive.
                break;
            }
        }
        None
    }
}
