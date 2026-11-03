//! EFB documents: published manuals plus PDFs the user drops into their
//! documents folder, rasterised page by page with PDFium.
//!
//! Pages are JPEG: the in-sim browser cannot decode WebP. Each document is
//! cached under `<cache>/efb-docs/<id>/` as `source.pdf`, `page-<n>.jpg`
//! and a `pages` file holding the page count, so reopening is a file read.
//! A local PDF newer than its cache is rendered again.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use infinity_efb_server::sources::{LOCAL_DOC_CATEGORY, sanitize_doc_id, scan_local_docs};
use infinity_efb_server::{DocMeta, DocProvider, ProviderError};
use infinity_providers::ContentProvider;
use pdfium_render::prelude::*;

/// Wide enough to zoom, small enough for the in-sim browser to paint fast.
const RENDER_WIDTH: i32 = 1280;
const JPEG_QUALITY: u8 = 85;

pub(crate) struct Docs {
    content: std::sync::Arc<dyn ContentProvider>,
    cache: PathBuf,
    user_dir: PathBuf,
    /// Folder holding the bundled PDFium library; `None` tries the system.
    pdfium_dir: Option<PathBuf>,
}

impl Docs {
    pub(crate) fn new(
        content: std::sync::Arc<dyn ContentProvider>,
        cache: PathBuf,
        user_dir: PathBuf,
        pdfium_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            content,
            cache,
            user_dir,
            pdfium_dir,
        }
    }

    fn dir(&self, id: &str) -> Result<PathBuf, ProviderError> {
        sanitize_doc_id(id)
            .map(|id| self.cache.join(id))
            .ok_or_else(|| ProviderError::new("not_found", format!("bad document id {id:?}")))
    }

    fn local(&self, id: &str) -> Option<PathBuf> {
        scan_local_docs(&self.user_dir)
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| d.path)
    }

    /// The PDF on disk: the user's own file, or a download of a published one.
    async fn source(&self, id: &str, dir: &Path) -> Result<PathBuf, ProviderError> {
        let user_dir = self.user_dir.clone();
        let wanted = id.to_owned();
        let local = blocking(move || {
            Ok(scan_local_docs(&user_dir)
                .into_iter()
                .find(|d| d.id == wanted)
                .map(|d| d.path))
        })
        .await?;
        if let Some(path) = local {
            return Ok(path);
        }
        let path = dir.join("source.pdf");
        if path.is_file() {
            return Ok(path);
        }
        let bytes = self
            .content
            .efb_doc(id)
            .await
            .map_err(|e| ProviderError::new("network", e.message))?;
        tokio::fs::create_dir_all(dir).await.map_err(io)?;
        let tmp = dir.join("source.pdf.tmp");
        tokio::fs::write(&tmp, bytes).await.map_err(io)?;
        tokio::fs::rename(&tmp, &path).await.map_err(io)?;
        Ok(path)
    }
}

#[async_trait]
impl DocProvider for Docs {
    async fn list(&self) -> Result<Vec<DocMeta>, ProviderError> {
        let published = match self.content.efb_docs().await {
            Ok(docs) => docs,
            Err(e) => {
                tracing::warn!(error = %e, "EFB documents unavailable");
                Vec::new()
            }
        };
        let user_dir = self.user_dir.clone();
        let local = blocking(move || Ok(scan_local_docs(&user_dir))).await?;
        Ok(published
            .into_iter()
            .map(|d| DocMeta {
                id: d.id,
                title: d.title,
                category: d.category,
            })
            .chain(local.into_iter().map(|d| DocMeta {
                id: d.id,
                title: d.title,
                category: LOCAL_DOC_CATEGORY.to_owned(),
            }))
            .collect())
    }

    async fn ensure(&self, id: &str) -> Result<usize, ProviderError> {
        let dir = self.dir(id)?;
        let source = self.source(id, &dir).await?;
        let marker = dir.join("pages");
        if fresh(&source, &marker) {
            if let Ok(text) = tokio::fs::read_to_string(&marker).await
                && let Ok(n) = text.trim().parse()
            {
                return Ok(n);
            }
        } else if self.local(id).is_some() {
            // The user's file changed: drop pages rendered from the old one.
            let _ = tokio::fs::remove_dir_all(&dir).await;
        }
        let lib = self.pdfium_dir.clone();
        let pages = blocking(move || count_pages(&source, lib.as_deref())).await?;
        tokio::fs::create_dir_all(&dir).await.map_err(io)?;
        tokio::fs::write(&marker, pages.to_string())
            .await
            .map_err(io)?;
        Ok(pages)
    }

    async fn page(&self, id: &str, page: usize) -> Result<PathBuf, ProviderError> {
        let dir = self.dir(id)?;
        let out = dir.join(format!("page-{page}.jpg"));
        if out.is_file() {
            return Ok(out);
        }
        let source = self.source(id, &dir).await?;
        let lib = self.pdfium_dir.clone();
        let target = out.clone();
        blocking(move || render_page(&source, page, &target, lib.as_deref())).await?;
        Ok(out)
    }
}

/// The page count was recorded after the source last changed.
fn fresh(source: &Path, marker: &Path) -> bool {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((modified(source), modified(marker)), (Some(s), Some(m)) if m >= s)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ProviderError> + Send + 'static,
) -> Result<T, ProviderError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ProviderError::new("internal", e.to_string()))?
}

fn io(e: std::io::Error) -> ProviderError {
    ProviderError::new("io", e.to_string())
}

fn render_error(e: impl std::fmt::Display) -> ProviderError {
    ProviderError::new("render", e.to_string())
}

fn bind(lib_dir: Option<&Path>) -> Result<Pdfium, ProviderError> {
    let bundled = lib_dir.and_then(|d| {
        Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(d)).ok()
    });
    let bindings = match bundled {
        Some(b) => b,
        None => Pdfium::bind_to_system_library()
            .map_err(|e| ProviderError::new("render", format!("PDFium is not available: {e}")))?,
    };
    Ok(Pdfium::new(bindings))
}

fn count_pages(pdf: &Path, lib_dir: Option<&Path>) -> Result<usize, ProviderError> {
    let pdfium = bind(lib_dir)?;
    let doc = pdfium.load_pdf_from_file(pdf, None).map_err(render_error)?;
    Ok(usize::from(doc.pages().len()))
}

fn render_page(
    pdf: &Path,
    index: usize,
    out: &Path,
    lib_dir: Option<&Path>,
) -> Result<(), ProviderError> {
    let pdfium = bind(lib_dir)?;
    let doc = pdfium.load_pdf_from_file(pdf, None).map_err(render_error)?;
    let index = u16::try_from(index).map_err(render_error)?;
    let page = doc.pages().get(index).map_err(render_error)?;
    let rgb = page
        .render_with_config(&PdfRenderConfig::new().set_target_width(RENDER_WIDTH))
        .map_err(render_error)?
        .as_image()
        .into_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(render_error)?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let tmp = out.with_extension("jpg.tmp");
    std::fs::write(&tmp, jpeg).map_err(io)?;
    std::fs::rename(&tmp, out).map_err(io)
}
