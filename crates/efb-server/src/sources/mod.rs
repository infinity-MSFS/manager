//! Providers that need nothing but a file or a public HTTP API, and helpers
//! for the ones the app has to write itself.

mod charts;
mod ctl_file;
mod local_docs;
mod tiles;
mod weather;

pub use charts::charts_index_for_efb;
pub use ctl_file::FileCtlStore;
pub use local_docs::{
    LOCAL_DOC_CATEGORY, LOCAL_DOC_PREFIX, LocalDoc, sanitize_doc_id, scan_local_docs,
};
pub use tiles::PublicTiles;
pub use weather::AviationWeather;
