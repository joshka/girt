/// Walks the retained response without copying it or changing authoritative parser results.
#[derive(Default)]
pub(super) struct LiveProgress {
    offset: usize,
    stopped: bool,
}

impl LiveProgress {
    pub(super) fn observe(&mut self, bytes: &[u8], progress: &mut impl FnMut(&[u8])) {
        while !self.stopped {
            let remaining = &bytes[self.offset..];
            let Some(header) = remaining.first_chunk::<4>() else {
                return;
            };
            let Ok(length) = crate::packet::packet_length(header) else {
                self.stopped = true;
                return;
            };
            if length == 0 {
                self.stopped = true;
                return;
            }
            let Some(packet) = remaining.get(4..length) else {
                return;
            };
            self.offset += length;
            match packet.split_first() {
                Some((1, _)) => {}
                Some((2, payload)) => progress(payload),
                _ => self.stopped = true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::LiveProgress;

    #[rstest]
    #[case::before_header(0)]
    #[case::header_byte(1)]
    #[case::half_header(2)]
    #[case::almost_header(3)]
    #[case::header(4)]
    #[case::channel(5)]
    #[case::payload(6)]
    #[case::first_packet(8)]
    #[case::second_header(10)]
    #[case::all(16)]
    fn progress_preserves_bytes_across_response_chunks(#[case] split: usize) {
        let bytes = b"0008\x02a\xff\n0008\x02b\x00\r0000";
        let mut observer = LiveProgress::default();
        let mut received = Vec::new();
        let mut callback = |payload: &[u8]| received.push(payload.to_vec());
        observer.observe(&bytes[..split], &mut callback);
        observer.observe(bytes, &mut callback);
        observer.observe(bytes, &mut callback);
        assert_eq!(received, [b"a\xff\n".to_vec(), b"b\x00\r".to_vec()]);
    }

    #[rstest]
    #[case::flush(b"0000")]
    #[case::remote_error(b"0006\x03x")]
    #[case::unknown_channel(b"0006\x04x")]
    #[case::missing_channel(b"0004")]
    #[case::invalid_header(b"zzzz")]
    #[case::reserved_length(b"0001")]
    #[case::oversized(b"ffff")]
    #[case::git_error(b"0009ERR x")]
    fn progress_stops_at_terminal_or_invalid_packets(#[case] terminal: &[u8]) {
        let bytes = [b"0006\x02a".as_slice(), terminal, b"0006\x02b"].concat();
        let mut received = Vec::new();
        LiveProgress::default().observe(&bytes, &mut |payload| received.push(payload.to_vec()));
        assert_eq!(received, [b"a".to_vec()]);
    }

    #[test]
    fn progress_skips_status_and_incomplete_payloads() {
        let bytes = b"0006\x01x0006\x02a0009\x02b";
        let mut received = Vec::new();
        LiveProgress::default().observe(bytes, &mut |payload| received.push(payload.to_vec()));
        assert_eq!(received, [b"a".to_vec()]);
    }
}
