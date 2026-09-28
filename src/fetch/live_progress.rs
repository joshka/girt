/// A cursor over retained response bytes. Prefix scanning finds a boundary; the existing parser
/// validates its semantics once before any notices are delivered.
pub(super) struct LiveProgress {
    offset: usize,
    phase: Phase,
}

enum Phase {
    Shallow,
    Acknowledgements,
    Sideband,
    Stopped,
}

impl LiveProgress {
    pub(super) fn new(depth: bool) -> Self {
        Self {
            offset: 0,
            phase: if depth {
                Phase::Shallow
            } else {
                Phase::Acknowledgements
            },
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
                Phase::Shallow => {}
                Phase::Acknowledgements => {
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

    #[rstest]
    #[case::first_header(2)]
    #[case::nak(6)]
    #[case::sideband_header(10)]
    #[case::sideband_payload(13)]
    #[case::complete(25)]
    fn live_notices_wait_for_complete_prefix_and_packets(#[case] split: usize) {
        let bytes = b"0008NAK\n0008\x02a\xff\r0006\x01x0006\x02b0000";
        let mut live = LiveProgress::new(false);
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
        let mut live = LiveProgress::new(true);
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
        LiveProgress::new(false).observe(&bytes, &mut |_| true, &mut |bytes| {
            messages.push(bytes.to_vec())
        });
        assert_eq!(messages, [b"a".to_vec()]);
    }

    #[test]
    fn live_notices_stop_when_authoritative_prefix_parser_rejects() {
        let mut messages = Vec::new();
        LiveProgress::new(false).observe(b"0008NAK\n0006\x02a0000", &mut |_| false, &mut |bytes| {
            messages.push(bytes.to_vec())
        });
        assert!(messages.is_empty());
    }
}
