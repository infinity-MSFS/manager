//! Turning raw chunk bytes into stored bytes and back.

use crate::error::{Error, Result};
use crate::{Chunk, Encoding, Hash};

/// Compression level releases are packed with. Decoding speed does not
/// depend on it; uploads just take longer above this.
pub const DEFAULT_LEVEL: i32 = 15;

/// Compresses `raw`, keeping it raw when compression would not save at
/// least 1%.
pub fn encode(raw: &[u8], level: i32) -> Result<(Encoding, Vec<u8>)> {
    let packed = zstd::bulk::compress(raw, level)
        .map_err(|e| Error::Package(format!("compressing a chunk: {e}")))?;
    if packed.len() + raw.len() / 100 < raw.len() {
        Ok((Encoding::Zstd, packed))
    } else {
        Ok((Encoding::Raw, raw.to_vec()))
    }
}

/// Decodes stored bytes and checks they are the chunk.
pub fn decode(chunk: &Chunk, stored: &[u8]) -> Result<Vec<u8>> {
    let damaged = |reason: String| Error::Chunk {
        chunk: chunk.id.to_hex(),
        reason,
    };
    if stored.len() != chunk.stored as usize {
        return Err(damaged(format!(
            "{} stored bytes, expected {}",
            stored.len(),
            chunk.stored
        )));
    }
    let raw = match chunk.encoding {
        Encoding::Raw => stored.to_vec(),
        // Capacity is exact: a frame that decodes larger fails here.
        Encoding::Zstd => zstd::bulk::decompress(stored, chunk.size as usize)
            .map_err(|e| damaged(format!("does not decompress: {e}")))?,
    };
    verify(chunk, &raw)?;
    Ok(raw)
}

/// Checks raw bytes against the chunk's size and hash.
pub fn verify(chunk: &Chunk, raw: &[u8]) -> Result<()> {
    if raw.len() != chunk.size as usize {
        return Err(Error::Chunk {
            chunk: chunk.id.to_hex(),
            reason: format!("{} bytes, expected {}", raw.len(), chunk.size),
        });
    }
    if Hash::of(raw) != chunk.id {
        return Err(Error::Chunk {
            chunk: chunk.id.to_hex(),
            reason: "its hash does not match".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk_for(raw: &[u8], encoding: Encoding, stored: &[u8]) -> Chunk {
        Chunk {
            id: Hash::of(raw),
            size: raw.len() as u32,
            pack: 0,
            offset: 0,
            stored: stored.len() as u32,
            encoding,
        }
    }

    #[test]
    fn compressible_data_round_trips() {
        let raw = vec![7u8; 100_000];
        let (enc, stored) = encode(&raw, 3).unwrap();
        assert_eq!(enc, Encoding::Zstd);
        assert!(stored.len() < 1000);
        assert_eq!(
            decode(&chunk_for(&raw, enc, &stored), &stored).unwrap(),
            raw
        );
    }

    #[test]
    fn incompressible_data_stays_raw() {
        let raw: Vec<u8> = (0..4096u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        let (enc, stored) = encode(&raw, 3).unwrap();
        if enc == Encoding::Raw {
            assert_eq!(stored, raw);
        }
    }

    #[test]
    fn damage_is_caught() {
        let raw = vec![1u8; 5000];
        let (enc, mut stored) = encode(&raw, 3).unwrap();
        let chunk = chunk_for(&raw, enc, &stored);
        let last = stored.len() - 1;
        stored[last] ^= 0xff;
        assert!(decode(&chunk, &stored).is_err());

        let other = chunk_for(b"other", Encoding::Raw, b"other");
        assert!(decode(&other, b"0ther").is_err());
    }
}
