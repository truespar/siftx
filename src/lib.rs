//! SiftX - cross-platform document and image processing library.
//!
//! # Quick start
//!
//! ```no_run
//! // Extract metadata tags (like ExifTool)
//! let tags = siftx::tags("photo.jpg").unwrap();
//! for tag in &tags {
//!     println!("[{}] {} = {}", tag.group, tag.name, tag.value);
//! }
//!
//! // Zero-copy PDF pipeline
//! let file = siftx::open("document.pdf").unwrap();
//! let doc = file.parse().unwrap();
//! let images = doc.images().unwrap();   // borrows from file
//! let text = doc.text_pages().unwrap();  // owned strings
//! ```

// ---------------------------------------------------------------------------
// Public surface
//
// The parser modules below are `pub(crate)`. They are implementation: nothing
// in the documented API returns a `pdf::xref::XRefTable` or asks for a
// `tiff::IfdEntry`, and making them public would commit every one of their
// ~500 items to semver - a refactor of the xref reader would be a breaking
// release.
//
// They were public for one reason: integration tests live outside the crate
// and can only see public items. That is the test layout dictating the API,
// which is backwards. The non-default `internals` feature re-exposes them
// under their real paths, and Cargo.toml turns it on for this crate's own
// dev-dependency, so `cargo test` works with no flag while a consumer sees
// only the API below.
//
// `internals` carries no semver guarantee. If you find yourself reaching for
// it from outside this crate, the thing you need is missing from the real API
// and should be added there instead.
// ---------------------------------------------------------------------------

mod api;
pub mod core;

// Re-export high-level API at crate root
pub use api::{GpsCoordinates, Image, ImageData, SiftDocument, SiftFile, Tag, open, read, tags};
#[cfg(feature = "pdf")]
pub use api::{PageWord, PageWords};

// Re-export PDF types used by CLI
#[cfg(feature = "pdf")]
pub use pdf::annot::{AcroForm, Annotation, AnnotationType, FieldType, FormField};
#[cfg(feature = "pdf")]
pub use pdf::struct_tree::{McidRef, StructChild, StructElement, StructTree};

#[cfg(all(feature = "jpeg", feature = "internals"))]
pub mod jpeg;
#[cfg(all(feature = "jpeg", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod jpeg;

#[cfg(all(feature = "tiff", feature = "internals"))]
pub mod tiff;
#[cfg(all(feature = "tiff", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod tiff;

#[cfg(all(feature = "png", feature = "internals"))]
pub mod png;
#[cfg(all(feature = "png", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod png;

#[cfg(all(feature = "webp", feature = "internals"))]
pub mod webp;
#[cfg(all(feature = "webp", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod webp;

#[cfg(all(feature = "heif", feature = "internals"))]
pub mod heif;
#[cfg(all(feature = "heif", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod heif;

#[cfg(all(feature = "quicktime", feature = "internals"))]
pub mod quicktime;
#[cfg(all(feature = "quicktime", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod quicktime;

#[cfg(all(feature = "gif", feature = "internals"))]
pub mod gif;
#[cfg(all(feature = "gif", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod gif;

#[cfg(all(feature = "bmp", feature = "internals"))]
pub mod bmp;
#[cfg(all(feature = "bmp", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod bmp;

#[cfg(all(feature = "xmp", feature = "internals"))]
pub mod xmp;
#[cfg(all(feature = "xmp", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod xmp;

#[cfg(all(feature = "iptc", feature = "internals"))]
pub mod iptc;
#[cfg(all(feature = "iptc", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod iptc;

#[cfg(all(feature = "icc", feature = "internals"))]
pub mod icc;
#[cfg(all(feature = "icc", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod icc;

#[cfg(all(feature = "pdf", feature = "internals"))]
pub mod pdf;
#[cfg(all(feature = "pdf", not(feature = "internals")))]
#[allow(dead_code)] // some items are reached only through `internals`
pub(crate) mod pdf;

#[cfg(feature = "ffi")]
pub mod ffi;
