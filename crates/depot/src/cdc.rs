//! Content-defined chunking (FastCDC with normalized chunking).
//!
//! Boundaries are picked where a rolling gear hash of the last 64 bytes
//! hits a mask, so they follow the content: inserting a byte near the start
//! of a file moves the boundaries after it along with the data instead of
//! shifting every chunk. Near the average size the mask is first strict
//! and then lenient, which keeps chunk sizes tight around the average.
//!
//! The gear table and parameters are part of the format: changing either
//! changes every boundary, which breaks deduplication with earlier
//! releases (never correctness, since manifests record the boundaries).
//! Hence our own implementation rather than a crate whose tables may move.

use std::io::Read;

/// Chunk size bounds. Every chunk but a file's last is at least `min`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub min: usize,
    pub avg: usize,
    pub max: usize,
}

impl Params {
    /// What releases are cut with: big enough to keep request and manifest
    /// overhead low on 15 GB packages, small enough that a patch to one
    /// texture costs about a megabyte.
    pub const DEFAULT: Self = Self {
        min: 256 * 1024,
        avg: 1024 * 1024,
        max: 4 * 1024 * 1024,
    };

    fn masks(self) -> (u64, u64) {
        let bits = self.avg.max(2).ilog2();
        // The gear hash shifts left, so its high bits mix the most input.
        let top = |n: u32| (!0u64) << (64 - n.clamp(1, 63));
        (top(bits + 1), top(bits.saturating_sub(1)))
    }
}

/// The largest chunk any reader of the format must accept. Larger than
/// [`Params::DEFAULT`]'s `max`, leaving room for future parameters.
pub const MAX_CHUNK: usize = 16 * 1024 * 1024;

const GEAR: [u64; 256] = gear_table(0x1f8e_37d1_9a5c_6b02);

const fn gear_table(seed: u64) -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut state = seed;
    let mut i = 0;
    while i < 256 {
        // splitmix64
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        table[i] = z ^ (z >> 31);
        i += 1;
    }
    table
}

/// The length of the first chunk of `data`. `data` should hold at least
/// `max` bytes unless it is the end of the file.
pub fn cut(data: &[u8], params: Params) -> usize {
    let len = data.len();
    if len <= params.min {
        return len;
    }
    let end = len.min(params.max);
    let normal = params.avg.min(end);
    let (strict, lenient) = params.masks();
    let mut hash = 0u64;
    let mut i = params.min;
    while i < normal {
        hash = (hash << 1).wrapping_add(GEAR[data[i] as usize]);
        if hash & strict == 0 {
            return i + 1;
        }
        i += 1;
    }
    while i < end {
        hash = (hash << 1).wrapping_add(GEAR[data[i] as usize]);
        if hash & lenient == 0 {
            return i + 1;
        }
        i += 1;
    }
    end
}

/// Cuts everything `reader` yields into chunks, handing each to `chunk` in
/// order. An empty input yields no chunks.
pub fn chunk_reader<R: Read, E: From<std::io::Error>>(
    mut reader: R,
    params: Params,
    mut chunk: impl FnMut(&[u8]) -> Result<(), E>,
) -> Result<(), E> {
    let mut buf = vec![0u8; params.max * 2];
    let mut start = 0;
    let mut end = 0;
    let mut eof = false;
    loop {
        // Keep at least `max` bytes buffered so a cut sees a full window.
        while !eof && end - start < params.max {
            if end == buf.len() {
                buf.copy_within(start..end, 0);
                end -= start;
                start = 0;
            }
            let n = loop {
                match reader.read(&mut buf[end..]) {
                    Ok(n) => break n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e.into()),
                }
            };
            if n == 0 {
                eof = true;
            }
            end += n;
        }
        if start == end {
            return Ok(());
        }
        let n = cut(&buf[start..end], params);
        chunk(&buf[start..start + n])?;
        start += n;
    }
}

/// Every chunk length of `data`, for tests and small inputs.
pub fn lengths(data: &[u8], params: Params) -> Vec<usize> {
    let mut out = Vec::new();
    let _ = chunk_reader::<_, std::io::Error>(data, params, |c| {
        out.push(c.len());
        Ok(())
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: Params = Params {
        min: 1024,
        avg: 4096,
        max: 16384,
    };

    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut x = seed | 1;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn chunks_tile_the_input_within_bounds() {
        let data = noise(1 << 20, 7);
        let lens = lengths(&data, SMALL);
        assert_eq!(lens.iter().sum::<usize>(), data.len());
        for len in &lens[..lens.len() - 1] {
            assert!((SMALL.min..=SMALL.max).contains(len), "{len}");
        }
        let avg = data.len() / lens.len();
        assert!(
            (SMALL.avg / 2..SMALL.avg * 2).contains(&avg),
            "average {avg}"
        );
    }

    #[test]
    fn boundaries_survive_an_insertion() {
        let data = noise(1 << 20, 11);
        let mut edited = data.clone();
        edited.splice(5000..5000, *b"inserted bytes");
        let ends = |d: &[u8]| {
            let mut at = 0;
            lengths(d, SMALL)
                .into_iter()
                .map(|l| {
                    at += l;
                    at
                })
                .collect::<Vec<_>>()
        };
        let before = ends(&data);
        let after: std::collections::HashSet<usize> = ends(&edited)
            .into_iter()
            .map(|e| if e <= 5000 { e } else { e - 14 })
            .collect();
        let kept = before.iter().filter(|e| after.contains(e)).count();
        // All but the chunk or two around the edit resynchronise.
        assert!(kept + 3 >= before.len(), "{kept} of {}", before.len());
    }

    #[test]
    fn small_and_empty_inputs() {
        assert!(lengths(&[], SMALL).is_empty());
        assert_eq!(lengths(&[1, 2, 3], SMALL), vec![3]);
    }

    #[test]
    fn gear_table_is_frozen() {
        // Changing this changes every chunk boundary of every release.
        let fingerprint = GEAR.iter().fold(0u64, |a, g| a.rotate_left(7) ^ g);
        assert_eq!(fingerprint, FINGERPRINT);
    }

    const FINGERPRINT: u64 = 1_534_502_278_675_197_338;
}
