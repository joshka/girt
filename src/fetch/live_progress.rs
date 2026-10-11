/// A cursor over retained response bytes. Prefix scanning finds a boundary; the existing parser
/// validates its semantics once before any notices are delivered.
pub(super) struct LiveProgress {
    offset: usize,
    phase: Phase,
    buffered: Vec<u8>,
    shallow_left: usize,
    acknowledgements_left: usize,
}

enum Phase {
    Shallow,
    Acknowledgements,
    Sideband,
    Stopped,
}

impl LiveProgress {
    pub(super) fn new(depth: bool, max_shallow_roots: usize) -> Self {
        Self {
            offset: 0,
            buffered: Vec::new(),
            shallow_left: max_shallow_roots,
            acknowledgements_left: 33,
            phase: if depth {
                Phase::Shallow
            } else {
                Phase::Acknowledgements
            },
        }
    }

    /// Accept chunks without retaining completed sideband packets. The negotiation prefix stays
    /// buffered until the authoritative parser has checked it; at most one partial packet remains.
    pub(super) fn feed(
        &mut self,
        chunk: &[u8],
        validate_prefix: &mut impl FnMut(&[u8]) -> bool,
        progress: &mut impl FnMut(&[u8]),
    ) {
        for part in chunk.chunks(64 * 1024) {
            let mut bytes = std::mem::take(&mut self.buffered);
            bytes.extend_from_slice(part);
            self.observe(&bytes, validate_prefix, progress);
            if matches!(self.phase, Phase::Sideband | Phase::Stopped) {
                bytes.drain(..self.offset);
                self.offset = 0;
            }
            if matches!(self.phase, Phase::Stopped) {
                bytes.clear();
            }
            self.buffered = bytes;
        }
    }

    pub(super) fn observe(
        &mut self,
        bytes: &[u8],
        validate_prefix: &mut impl FnMut(&[u8]) -> bool,
        progress: &mut impl FnMut(&[u8]),
    ) {
        while !matches!(self.phase, Phase::Stopped) {
            let remaining = &bytes[self.offset..];
            let Some(header) = remaining.first_chunk::<4>() else {
                return;
            };
            let Ok(length) = crate::packet::packet_length(header) else {
                self.phase = Phase::Stopped;
                return;
            };
            // A v0 shallow/ACK line contains at most a 64-byte hex ID, its keyword and
            // framing. Stop observing malformed prefixes before they can retain pack-sized data.
            if matches!(self.phase, Phase::Shallow | Phase::Acknowledgements) && length > 82 {
                self.phase = Phase::Stopped;
                return;
            }
            if length == 0 {
                self.offset += 4;
                self.phase = if matches!(self.phase, Phase::Shallow) {
                    Phase::Acknowledgements
                } else {
                    Phase::Stopped
                };
                continue;
            }
            let Some(packet) = remaining.get(4..length) else {
                return;
            };
            self.offset += length;
            match self.phase {
                Phase::Shallow => {
                    if self.shallow_left == 0 {
                        self.phase = Phase::Stopped;
                        return;
                    }
                    self.shallow_left -= 1;
                }
                Phase::Acknowledgements => {
                    // Negotiation offers at most 32 haves, followed by a final ACK/NAK.
                    if self.acknowledgements_left == 0 {
                        self.phase = Phase::Stopped;
                        return;
                    }
                    self.acknowledgements_left -= 1;
                    let line = packet.strip_suffix(b"\n").unwrap_or(packet);
                    if line.starts_with(b"ACK ") && line.ends_with(b" continue") {
                        continue;
                    }
                    self.phase = if validate_prefix(&bytes[..self.offset]) {
                        Phase::Sideband
                    } else {
                        Phase::Stopped
                    };
                }
                Phase::Sideband => match packet.split_first() {
                    Some((1, _)) => {}
                    Some((2, payload)) => progress(payload),
                    _ => self.phase = Phase::Stopped,
                },
                Phase::Stopped => unreachable!(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::LiveProgress;

    #[test]
    fn malformed_ack_prefix_does_not_accumulate_response_bytes() {
        let bytes = b"0013ACK x continue\n".repeat(10_000);
        let mut live = LiveProgress::new(false, 100);
        feed_chunks(
            &mut live,
            &bytes,
            8192,
            &mut |_| panic!("no final acknowledgement"),
            &mut |_| panic!("invalid prefix"),
        );
        assert!(live.buffered.is_empty());
    }

    #[test]
    fn shallow_prefix_retention_obeys_the_negotiated_count_limit() {
        let bytes = b"000dshallow x".repeat(10_000);
        let mut live = LiveProgress::new(true, 2);
        feed_chunks(
            &mut live,
            &bytes,
            8192,
            &mut |_| panic!("no final acknowledgement"),
            &mut |_| panic!("invalid prefix"),
        );
        assert!(live.buffered.is_empty());
    }

    #[rstest]
    #[case::bytes(1)]
    #[case::headers(4)]
    #[case::packets(17)]
    #[case::whole(128)]
    fn chunked_notices_retain_only_the_incomplete_packet(#[case] chunk_size: usize) {
        let bytes = b"0008NAK\n0008\x02a\xff\r0006\x01x0006\x02b0000";
        let mut live = LiveProgress::new(false, 100);
        let mut messages = Vec::new();
        let mut validated = 0;
        feed_chunks(
            &mut live,
            bytes,
            chunk_size,
            &mut |bytes| {
                validated += 1;
                bytes == b"0008NAK\n"
            },
            &mut |bytes| messages.push(bytes.to_vec()),
        );
        assert_eq!(validated, 1);
        assert_eq!(messages, [b"a\xff\r".to_vec(), b"b".to_vec()]);
        assert!(live.buffered.is_empty());
    }

    fn feed_chunks(
        live: &mut LiveProgress,
        bytes: &[u8],
        chunk_size: usize,
        prefix: &mut impl FnMut(&[u8]) -> bool,
        progress: &mut impl FnMut(&[u8]),
    ) {
        for chunk in bytes.chunks(chunk_size) {
            live.feed(chunk, prefix, progress);
        }
    }

    #[test]
    fn pack_packets_do_not_accumulate_in_the_live_progress_buffer() {
        let bytes = [b"0008NAK\n".as_slice(), &b"0006\x01x".repeat(100_000)].concat();
        let mut live = LiveProgress::new(false, 100);
        feed_chunks(&mut live, &bytes, 8192, &mut |_| true, &mut |_| {
            panic!("pack channel is not progress")
        });
        assert!(live.buffered.is_empty());
    }

    #[rstest]
    #[case::first_header(2)]
    #[case::nak(6)]
    #[case::sideband_header(10)]
    #[case::sideband_payload(13)]
    #[case::complete(25)]
    fn live_notices_wait_for_complete_prefix_and_packets(#[case] split: usize) {
        let bytes = b"0008NAK\n0008\x02a\xff\r0006\x01x0006\x02b0000";
        let mut live = LiveProgress::new(false, 100);
        let mut messages = Vec::new();
        let mut callback = |payload: &[u8]| messages.push(payload.to_vec());
        let mut validated = 0;
        let mut prefix = |bytes: &[u8]| {
            validated += 1;
            bytes == b"0008NAK\n"
        };
        live.observe(&bytes[..split], &mut prefix, &mut callback);
        live.observe(bytes, &mut prefix, &mut callback);
        live.observe(bytes, &mut prefix, &mut callback);
        assert_eq!(validated, 1);
        assert_eq!(messages, [b"a\xff\r".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn live_notices_wait_for_shallow_flush_and_final_ack() {
        let prefix = b"000dshallow x00000013ACK x continue\n000aACK x\n";
        let bytes = [prefix.as_slice(), b"0006\x02a0000"].concat();
        let mut live = LiveProgress::new(true, 100);
        let mut messages = Vec::new();
        let mut validated = 0;
        live.observe(
            &bytes[..18],
            &mut |_| panic!("incomplete negotiation"),
            &mut |_| panic!("early progress"),
        );
        live.observe(
            &bytes,
            &mut |bytes| {
                validated += 1;
                bytes == prefix
            },
            &mut |bytes| messages.push(bytes.to_vec()),
        );
        assert_eq!(validated, 1);
        assert_eq!(messages, [b"a".to_vec()]);
    }

    #[rstest]
    #[case::invalid_header(b"zzzz")]
    #[case::reserved_header(b"0001")]
    #[case::flush(b"0000")]
    #[case::remote_error(b"0006\x03x")]
    #[case::invalid_channel(b"0006\x04x")]
    #[case::empty_packet(b"0004")]
    fn live_notices_stop_on_terminal_framing(#[case] terminal: &[u8]) {
        let bytes = [b"0008NAK\n0006\x02a".as_slice(), terminal, b"0006\x02b"].concat();
        let mut messages = Vec::new();
        LiveProgress::new(false, 100).observe(&bytes, &mut |_| true, &mut |bytes| {
            messages.push(bytes.to_vec())
        });
        assert_eq!(messages, [b"a".to_vec()]);
    }

    #[test]
    fn live_notices_stop_when_authoritative_prefix_parser_rejects() {
        let mut messages = Vec::new();
        LiveProgress::new(false, 100).observe(
            b"0008NAK\n0006\x02a0000",
            &mut |_| false,
            &mut |bytes| messages.push(bytes.to_vec()),
        );
        assert!(messages.is_empty());
    }
}
