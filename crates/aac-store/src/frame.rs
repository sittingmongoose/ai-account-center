//! The frame (docs/store.md 2.3): Puppet Master's SeglogFrameV2 prefix with AAC's magic.
//! All prefix integers are little-endian and read field by field, never by casting a struct.

pub const MAGIC: [u8; 8] = *b"AACSEG2\0";
pub const PREFIX_LEN: usize = 48;
pub const FRAME_VERSION: u8 = 2;
pub const MAX_HEADER: usize = 4096;
pub const MAX_PAYLOAD: usize = 1 << 20;

/// CRC-32/ISO-HDLC, the checksum of PM's frame (reflected 0x04c11db7, init and xorout all ones).
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

pub fn encode(generation: u64, sequence: u64, header: &[u8], payload: &[u8]) -> Vec<u8> {
    assert!(header.len() <= MAX_HEADER && payload.len() <= MAX_PAYLOAD);
    let mut out = Vec::with_capacity(PREFIX_LEN + header.len() + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(FRAME_VERSION);
    out.extend_from_slice(&(header.len() as u16).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.push(0); // flags
    out.extend_from_slice(&generation.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&crc32(header).to_le_bytes());
    out.extend_from_slice(&crc32(payload).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]); // prefix CRC, filled below
    out.extend_from_slice(&[0, 0, 0, 0]); // reserved
    let prefix_crc = crc32(&out[..PREFIX_LEN]);
    out[40..44].copy_from_slice(&prefix_crc.to_le_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Clone, Copy)]
pub struct FrameRef<'a> {
    pub sequence: u64,
    pub generation: u64,
    pub header: &'a [u8],
    pub payload: &'a [u8],
    pub payload_crc: u32,
    /// Total bytes: prefix, header and payload.
    pub len: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// Fewer bytes remain than the frame needs: a torn tail when at end of file.
    Incomplete,
    /// The bytes are not a valid frame.
    Corrupt(&'static str),
    /// A valid prefix of a frame version or flags this reader does not know.
    Unsupported(&'static str),
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// Validates the frame starting at `buf[0]` in PM's order: magic, prefix CRC before trusting
/// lengths, version/flags/reserved, caps and file bounds, generation and sequence, header CRC,
/// payload CRC. Decoding the header and payload (steps 8 to 10) is the event layer's job.
pub fn decode(buf: &[u8], generation: u64, previous: Option<u64>) -> Result<FrameRef<'_>, Problem> {
    if buf.len() < MAGIC.len() {
        return if MAGIC.starts_with(buf) {
            Err(Problem::Incomplete)
        } else {
            Err(Problem::Corrupt("magic"))
        };
    }
    if buf[..8] != MAGIC {
        return Err(Problem::Corrupt("magic"));
    }
    if buf.len() < PREFIX_LEN {
        return Err(Problem::Incomplete);
    }
    let prefix = &buf[..PREFIX_LEN];
    let mut zeroed = [0u8; PREFIX_LEN];
    zeroed.copy_from_slice(prefix);
    zeroed[40..44].copy_from_slice(&[0, 0, 0, 0]);
    if crc32(&zeroed) != u32_at(prefix, 40) {
        return Err(Problem::Corrupt("prefix_crc"));
    }
    if prefix[8] != FRAME_VERSION {
        return Err(Problem::Unsupported("frame_version"));
    }
    if u32_at(prefix, 44) != 0 {
        return Err(Problem::Corrupt("reserved"));
    }
    if prefix[15] != 0 {
        return Err(Problem::Unsupported("flags"));
    }
    let header_len = u16_at(prefix, 9) as usize;
    let payload_len = u32_at(prefix, 11) as usize;
    if header_len > MAX_HEADER || payload_len > MAX_PAYLOAD {
        return Err(Problem::Corrupt("length_cap"));
    }
    let total = PREFIX_LEN + header_len + payload_len;
    if buf.len() < total {
        return Err(Problem::Incomplete);
    }
    let frame_generation = u64_at(prefix, 16);
    if frame_generation != generation {
        return Err(Problem::Corrupt("generation"));
    }
    let sequence = u64_at(prefix, 24);
    if let Some(prev) = previous
        && sequence <= prev
    {
        return Err(Problem::Corrupt("sequence"));
    }
    let header = &buf[PREFIX_LEN..PREFIX_LEN + header_len];
    if crc32(header) != u32_at(prefix, 32) {
        return Err(Problem::Corrupt("header_crc"));
    }
    let payload = &buf[PREFIX_LEN + header_len..total];
    let payload_crc = u32_at(prefix, 36);
    if crc32(payload) != payload_crc {
        return Err(Problem::Corrupt("payload_crc"));
    }
    Ok(FrameRef {
        sequence,
        generation: frame_generation,
        header,
        payload,
        payload_crc,
        len: total,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn golden_prefix_layout() {
        let frame = encode(1, 7, &[0x90], &[0xc0]);
        assert_eq!(&frame[..8], b"AACSEG2\0");
        assert_eq!(frame[8], 2);
        assert_eq!(&frame[9..11], &[1, 0]);
        assert_eq!(&frame[11..15], &[1, 0, 0, 0]);
        assert_eq!(frame[15], 0);
        assert_eq!(&frame[16..24], &1u64.to_le_bytes());
        assert_eq!(&frame[24..32], &7u64.to_le_bytes());
        assert_eq!(&frame[32..36], &crc32(&[0x90]).to_le_bytes());
        assert_eq!(&frame[36..40], &crc32(&[0xc0]).to_le_bytes());
        assert_eq!(&frame[44..48], &[0, 0, 0, 0]);
        assert_eq!(frame.len(), 50);
        let decoded = decode(&frame, 1, Some(6)).unwrap();
        assert_eq!(decoded.sequence, 7);
        assert_eq!(decoded.len, 50);
    }

    #[test]
    fn rejections() {
        let frame = encode(1, 7, &[0x90], &[0xc0]);
        assert_eq!(decode(&frame[..30], 1, None).unwrap_err(), Problem::Incomplete);
        assert_eq!(decode(&frame[..49], 1, None).unwrap_err(), Problem::Incomplete);
        assert_eq!(decode(&frame, 2, None).unwrap_err(), Problem::Corrupt("generation"));
        assert_eq!(decode(&frame, 1, Some(7)).unwrap_err(), Problem::Corrupt("sequence"));
        let mut bad = frame.clone();
        bad[0] = b'X';
        assert_eq!(decode(&bad, 1, None).unwrap_err(), Problem::Corrupt("magic"));
        let mut bad = frame.clone();
        bad[11] = 0xff; // length change breaks the prefix CRC before it is trusted
        assert_eq!(decode(&bad, 1, None).unwrap_err(), Problem::Corrupt("prefix_crc"));
        let mut bad = frame.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(decode(&bad, 1, None).unwrap_err(), Problem::Corrupt("payload_crc"));
        let mut bad = frame.clone();
        bad[48] ^= 1;
        assert_eq!(decode(&bad, 1, None).unwrap_err(), Problem::Corrupt("header_crc"));
        // Nonzero flags with a valid prefix CRC are unsupported, never guessed.
        let mut flagged = frame.clone();
        flagged[15] = 1;
        flagged[40..44].copy_from_slice(&[0, 0, 0, 0]);
        let crc = crc32(&flagged[..48]);
        flagged[40..44].copy_from_slice(&crc.to_le_bytes());
        assert_eq!(decode(&flagged, 1, None).unwrap_err(), Problem::Unsupported("flags"));
    }
}
