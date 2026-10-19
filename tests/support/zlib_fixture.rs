//! Independently constructed zlib streams with empty blocks for compressed-input limit tests.

/// Encodes one small raw payload after 10,000 empty stored DEFLATE blocks.
///
/// The payload must fit a single stored block. Padding increases compressed input without changing
/// the decoded bytes; the checksum covers only those bytes.
pub(crate) fn padded(raw: &[u8]) -> Vec<u8> {
    let length = u16::try_from(raw.len()).expect("fixture payload fits one stored block");
    let mut compressed = vec![0x78, 0x01];
    for _ in 0..10_000 {
        compressed.extend_from_slice(&[0, 0, 0, 0xff, 0xff]);
    }
    compressed.push(1);
    compressed.extend_from_slice(&length.to_le_bytes());
    compressed.extend_from_slice(&(!length).to_le_bytes());
    compressed.extend_from_slice(raw);
    let (mut a, mut b) = (1_u32, 0_u32);
    for byte in raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    compressed.extend_from_slice(&((b << 16) | a).to_be_bytes());
    compressed
}
