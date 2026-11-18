//! Grouping the chunks a client needs into few HTTP range requests.

use std::collections::BTreeMap;

use crate::Manifest;

/// How to merge chunks into requests.
#[derive(Debug, Clone, Copy)]
pub struct Coalesce {
    /// Unneeded bytes worth downloading to save a request.
    pub max_gap: u64,
    /// Upper bound of one request, so a dropped connection costs little
    /// and work spreads across connections.
    pub max_len: u64,
}

impl Coalesce {
    pub const DEFAULT: Self = Self {
        max_gap: 512 * 1024,
        max_len: 32 * 1024 * 1024,
    };
}

/// One request: a byte range of one pack and the chunks inside it, in
/// pack order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Range {
    pub pack: u32,
    pub start: u64,
    /// Exclusive.
    pub end: u64,
    pub chunks: Vec<u32>,
}

impl Range {
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// Requests covering every chunk in `needed` (indices into the manifest's
/// chunks; duplicates and chunks stored at the same place collapse).
pub fn coalesce(
    manifest: &Manifest,
    needed: impl IntoIterator<Item = u32>,
    opts: Coalesce,
) -> Vec<Range> {
    // (pack, offset) -> chunk: one fetch per stored copy.
    let mut by_place: BTreeMap<(u32, u64), u32> = BTreeMap::new();
    for c in needed {
        let chunk = manifest.chunk(c);
        by_place.entry((chunk.pack, chunk.offset)).or_insert(c);
    }
    let mut out: Vec<Range> = Vec::new();
    for ((pack, offset), c) in by_place {
        let end = manifest.chunk(c).stored_end();
        if let Some(last) = out.last_mut()
            && last.pack == pack
            && offset >= last.end
            && offset - last.end <= opts.max_gap
            && end - last.start <= opts.max_len
        {
            last.end = end;
            last.chunks.push(c);
            continue;
        }
        out.push(Range {
            pack,
            start: offset,
            end,
            chunks: vec![c],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Chunk, Encoding, Hash, Pack};

    fn manifest(chunks: &[(u32, u64, u32)]) -> Manifest {
        Manifest {
            schema_version: 1,
            hash: "blake3".into(),
            product_id: "p".into(),
            version: "1".into(),
            packs: vec![
                Pack {
                    id: Hash::of(b"0"),
                    size: 1 << 40,
                },
                Pack {
                    id: Hash::of(b"1"),
                    size: 1 << 40,
                },
            ],
            chunks: chunks
                .iter()
                .enumerate()
                .map(|(i, &(pack, offset, stored))| Chunk {
                    id: Hash::of(&i.to_le_bytes()),
                    size: stored,
                    pack,
                    offset,
                    stored,
                    encoding: Encoding::Raw,
                })
                .collect(),
            files: vec![],
            dirs: vec![],
        }
    }

    #[test]
    fn merges_neighbours_and_splits_on_gaps_packs_and_length() {
        let m = manifest(&[
            (0, 0, 100),
            (0, 100, 100),
            (0, 250, 100),   // gap 50: merged
            (0, 10_000, 10), // gap too big
            (1, 0, 10),      // other pack
            (0, 0, 100),     // same place as chunk 0
        ]);
        let opts = Coalesce {
            max_gap: 64,
            max_len: 1000,
        };
        let got = coalesce(&m, [3, 0, 1, 2, 4, 5, 1], opts);
        assert_eq!(
            got,
            vec![
                Range {
                    pack: 0,
                    start: 0,
                    end: 350,
                    chunks: vec![0, 1, 2]
                },
                Range {
                    pack: 0,
                    start: 10_000,
                    end: 10_010,
                    chunks: vec![3]
                },
                Range {
                    pack: 1,
                    start: 0,
                    end: 10,
                    chunks: vec![4]
                },
            ]
        );
        let short = coalesce(
            &m,
            [0, 1, 2],
            Coalesce {
                max_gap: 64,
                max_len: 200,
            },
        );
        assert_eq!(short.len(), 2);
    }
}
