//! Original pack-v2 fixture: independent REF_DELTA chains appear in reverse dependency order.
//! Each eight-byte blob is unique; literal-only deltas make ordering cost independent of matching.
use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use girt::ObjectId;
use sha1::{Digest, Sha1};

pub fn response(chains: usize, depth: usize) -> (Vec<u8>, ObjectId, Vec<u8>) {
    assert!(depth <= 64);
    let mut pack = b"PACK".to_vec();
    pack.extend(2u32.to_be_bytes());
    pack.extend(
        ((depth + 1) * chains)
            .try_into()
            .map(u32::to_be_bytes)
            .unwrap(),
    );
    for chain in 0..chains {
        for level in (0..=depth).rev() {
            let data = ((chain * (depth + 1) + level) as u64).to_be_bytes();
            let payload = if level == 0 {
                pack.push(0x38); // blob, eight bytes
                data.to_vec()
            } else {
                pack.push(0x7b); // REF_DELTA, eleven-byte program
                let base = ((chain * (depth + 1) + level - 1) as u64).to_be_bytes();
                pack.extend(ObjectId::for_blob(&base).as_bytes());
                let mut program = vec![8, 8, 8]; // base size, result size, literal length
                program.extend(data);
                program
            };
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&payload).unwrap();
            pack.extend(encoder.finish().unwrap());
        }
    }
    pack.extend(Sha1::digest(&pack));
    let tip = ObjectId::for_blob(&(depth as u64).to_be_bytes());
    let mut wire = Vec::new();
    packet(
        &mut wire,
        format!("{tip} refs/tags/blob\0side-band-64k\n").as_bytes(),
    );
    wire.extend(b"0000");
    packet(&mut wire, b"NAK\n");
    for chunk in pack.chunks(65515) {
        let mut band = vec![1];
        band.extend(chunk);
        packet(&mut wire, &band);
    }
    wire.extend(b"0000");
    (wire, tip, pack)
}

fn packet(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(format!("{:04x}", bytes.len() + 4).bytes());
    out.extend(bytes);
}
