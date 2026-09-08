//! OneDrive's documented synchronization checksum plus a local cryptographic binding.
//! QuickXorHash is deliberately NOT treated as an identity signature.
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

#[derive(Default)]
pub struct QuickXor {
    bytes: [u8; 20],
    offset: usize,
    length: u64,
}
impl QuickXor {
    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            let index = self.offset / 8;
            let shift = self.offset % 8;
            self.bytes[index] ^= byte << shift;
            if shift != 0 {
                self.bytes[(index + 1) % 20] ^= byte >> (8 - shift);
            }
            self.offset = (self.offset + 11) % 160;
        }
        self.length += bytes.len() as u64;
    }
    pub fn finish(mut self) -> [u8; 20] {
        for (index, byte) in self.length.to_le_bytes().iter().enumerate() {
            self.bytes[12 + index] ^= byte;
        }
        self.bytes
    }
}

/// Call only AFTER the provider's identity is eligible. A placeholder is never
/// hydrated merely to find out that it belonged to somebody else.
pub fn verified_local_hash(
    path: &Path,
    expected_size: u64,
    expected_quick_xor: &str,
) -> io::Result<String> {
    let expected = STANDARD
        .decode(expected_quick_xor)
        .map_err(|_| io::Error::other("Microsoft checksum is malformed"))?;
    if expected.len() != 20 {
        return Err(io::Error::other("Microsoft checksum has an invalid size"));
    }
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() != expected_size {
        return Err(io::Error::other("The local file is still syncing"));
    }
    let mut quick = QuickXor::default();
    let mut sha = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > expected_size {
            return Err(io::Error::other("The local file changed"));
        }
        quick.update(&buffer[..count]);
        sha.update(&buffer[..count]);
    }
    let after = file.metadata()?;
    if total != expected_size
        || before.modified()? != after.modified()?
        || after.len() != total
        || quick.finish().as_slice() != expected.as_slice()
    {
        return Err(io::Error::other(
            "The local and Microsoft revisions do not agree",
        ));
    }
    Ok(format!("{:x}", sha.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_and_single_byte_vectors() {
        assert_eq!(QuickXor::default().finish(), [0; 20]);
        let mut q = QuickXor::default();
        q.update(b"a");
        let mut expected = [0; 20];
        expected[0] = 97;
        expected[12] = 1;
        assert_eq!(q.finish(), expected);
    }
    #[test]
    fn chunks_match_the_documented_bit_rotation_reference() {
        let input: Vec<u8> = (0..1027).map(|i| (i % 251) as u8).collect();
        let mut expected = [0; 20];
        for (index, &byte) in input.iter().enumerate() {
            for bit in 0..8 {
                if byte & (1 << bit) != 0 {
                    let position = (index * 11 + bit) % 160;
                    expected[position / 8] ^= 1 << (position % 8);
                }
            }
        }
        for (i, b) in (input.len() as u64).to_le_bytes().iter().enumerate() {
            expected[12 + i] ^= b;
        }
        for chunk in [1, 3, 19, 160, 1024] {
            let mut q = QuickXor::default();
            for part in input.chunks(chunk) {
                q.update(part);
            }
            assert_eq!(q.finish(), expected);
        }
    }
    #[test]
    fn rejects_mismatched_or_missing_cloud_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, b"hello").unwrap();
        assert!(verified_local_hash(&path, 5, "").is_err());
        assert!(verified_local_hash(&path, 5, "AAAAAAAAAAAAAAAAAAAAAAAAAAA=").is_err());
        let mut q = QuickXor::default();
        q.update(b"hello");
        assert_eq!(
            verified_local_hash(&path, 5, &STANDARD.encode(q.finish())).unwrap(),
            format!("{:x}", Sha256::digest(b"hello"))
        );
    }
}
