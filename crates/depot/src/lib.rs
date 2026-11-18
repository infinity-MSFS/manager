//! The depot format: how a product package is stored on the CDN as
//! content-addressed chunks, and how a client knows what to fetch.
//!
//! A package folder is cut into chunks with content-defined chunking
//! ([`cdc`]), so an edit in the middle of a 2 GB texture bank only changes
//! the chunks around the edit. Each chunk is named by the BLAKE3 hash of
//! its bytes and stored, usually zstd-compressed, inside a *pack*: a
//! ~64 MiB object holding many chunks back to back. Packs are named by
//! their own hash and shared between every version and channel of a
//! product, so a release only uploads, and a client only downloads, the
//! chunks that are new.
//!
//! The [`Manifest`] of a version lists its packs, its chunks (hash, size,
//! where in which pack) and its files, each as the ordered chunks that make
//! it up. A file on disk is intact exactly when its size matches and every
//! chunk-sized slice of it hashes to the chunk it should be, which is how
//! installs are verified and how damage is narrowed down to the few chunks
//! that need fetching again.
//!
//! Storage layout under the product's prefix (see [`keys`]):
//!
//! ```text
//! depots/<product>/packs/<blake3>.pack
//! depots/<product>/manifests/<channel>/<version>/<blake3>.manifest.zst
//! ```
//!
//! This crate has no network or install policy; the installer and the
//! publishing tools build on it.

#[cfg(feature = "build")]
pub mod build;
pub mod cdc;
pub mod codec;
mod error;
mod hash;
pub mod keys;
pub mod manifest;
pub mod ranges;

pub use error::{Error, Result};
pub use hash::Hash;
pub use manifest::{Chunk, Encoding, File, Manifest, Pack};

/// Files the manager writes into an installed package. Never part of a
/// manifest, and left alone when an install is compared with one.
pub const RESERVED_FILES: [&str; 2] = [MARKER_FILE, STATE_FILE];

/// The manager's version marker, shared with v3.
pub const MARKER_FILE: &str = "infinity.json";

/// The installed depot state: the manifest the package was installed from
/// and the size and modification time of each file when it was verified.
pub const STATE_FILE: &str = "infinity.depot";
