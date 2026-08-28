//! High-level API for SiftX.
//!
//! Three entry points:
//! - [`read`] - parse from a byte slice (caller owns the data)
//! - [`open`] - memory-map a file, then parse
//! - [`tags`] - convenience: open + extract metadata tags in one call
//!
//! # Examples
//!
//! ```no_run
//! // Quick metadata extraction
//! let tags = siftx::tags("photo.jpg").unwrap();
//! for tag in &tags {
//!     println!("[{}] {} = {}", tag.group, tag.name, tag.value);
//! }
//!
//! // Zero-copy pipeline
//! let file = siftx::open("document.pdf").unwrap();
//! let doc = file.parse().unwrap();
//! let images = doc.images().unwrap();
//! // images borrow from file - safe to scatter across threads
//! ```

use crate::core::{FileType, MappedFile, Result};

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Open a file via memory-map and return a handle for parsing.
///
/// The returned [`SiftFile`] owns the memory mapping. Call
/// [`SiftFile::parse`] to get a [`SiftDocument`] that borrows from it.
pub fn open(path: impl AsRef<std::path::Path>) -> Result<SiftFile> {
    let path = path.as_ref();
    let mmap = MappedFile::open(path)?;
    // Use extension hint to disambiguate TIFF-based RAW formats
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let file_type = FileType::detect_with_ext(mmap.as_bytes(), ext);
    Ok(SiftFile { mmap, file_type })
}

/// Parse metadata and content from a byte slice.
///
/// The returned [`SiftDocument`] borrows from `data` - the caller must
/// keep `data` alive for the lifetime of the document.
pub fn read(data: &[u8]) -> Result<SiftDocument<'_>> {
    let file_type = FileType::detect(data);
    SiftDocument::parse(data, file_type)
}

/// Convenience: open a file and extract all metadata tags.
///
/// Returns owned [`Tag`] values with no lifetime constraints.
/// Use [`open`] + [`SiftFile::parse`] if you also need images or text.
pub fn tags(path: impl AsRef<std::path::Path>) -> Result<Vec<Tag>> {
    let file = open(path)?;
    let doc = file.parse()?;
    Ok(doc.tags())
}

// ---------------------------------------------------------------------------
// SiftFile - owns the memory mapping
// ---------------------------------------------------------------------------

/// A memory-mapped file ready for parsing.
pub struct SiftFile {
    mmap: MappedFile,
    file_type: Option<FileType>,
}

impl SiftFile {
    /// Parse the file contents into a [`SiftDocument`].
    pub fn parse(&self) -> Result<SiftDocument<'_>> {
        SiftDocument::parse(self.mmap.as_bytes(), self.file_type)
    }

    /// Raw bytes of the memory-mapped file.
    pub fn data(&self) -> &[u8] {
        self.mmap.as_bytes()
    }

    /// Detected file type, or `None` if unrecognized.
    pub fn file_type(&self) -> Option<FileType> {
        self.file_type
    }
}

// ---------------------------------------------------------------------------
// SiftDocument - parsed content, borrows from data
// ---------------------------------------------------------------------------

/// A parsed document. Borrows from the input data for zero-copy access.
///
/// Provides three views:
/// - [`tags()`](Self::tags) - flat metadata tags (like ExifTool)
/// - [`images()`](Self::images) - extracted images (PDF only)
/// - [`text_pages()`](Self::text_pages) - extracted text per page (PDF only)
pub struct SiftDocument<'a> {
    #[allow(dead_code)]
    data: &'a [u8],
    file_type: Option<FileType>,
    inner: DocumentInner<'a>,
}

/// Format-specific parsed state.
#[allow(dead_code)]
enum DocumentInner<'a> {
    #[cfg(feature = "jpeg")]
    Jpeg {
        segments: Vec<crate::jpeg::Segment<'a>>,
    },
    #[cfg(feature = "png")]
    Png { chunks: Vec<crate::png::Chunk<'a>> },
    #[cfg(feature = "webp")]
    WebP { webp: crate::webp::WebP<'a> },
    #[cfg(feature = "heif")]
    Heif { info: crate::heif::HeifInfo<'a> },
    #[cfg(feature = "gif")]
    Gif { info: crate::gif::GifInfo<'a> },
    #[cfg(feature = "bmp")]
    Bmp { info: crate::bmp::BmpInfo },
    #[cfg(feature = "tiff")]
    Tiff {
        header: crate::tiff::TiffHeader,
        ifds: Vec<crate::tiff::Ifd<'a>>,
    },
    #[cfg(feature = "pdf")]
    Pdf {
        doc: crate::pdf::document::Document<'a>,
    },
    #[cfg(feature = "quicktime")]
    QuickTime {
        info: crate::quicktime::QuickTimeInfo<'a>,
    },
    /// Unrecognized or unsupported format.
    Unknown,
    /// Never constructed. Every other variant is behind a format feature, so
    /// with all of them off nothing would mention `'a` and the parameter
    /// becomes an error. This keeps the type well-formed in that
    /// configuration without making the lifetime conditional.
    #[allow(dead_code)]
    _Formats(std::marker::PhantomData<&'a ()>),
}

/// Format a PDF date string (D:YYYYMMDDHHmmSS+HH'mm') into a readable form.
#[cfg(feature = "pdf")] // Only the PDF metadata path formats dates.
fn format_pdf_date(raw: &str) -> String {
    let s = raw.strip_prefix("D:").unwrap_or(raw);
    if s.len() < 8 || !s.is_char_boundary(4) {
        return raw.to_string();
    }
    let year = &s[0..4];
    let month = s.get(4..6).unwrap_or("01");
    let day = s.get(6..8).unwrap_or("01");
    let hour = s.get(8..10).unwrap_or("00");
    let min = s.get(10..12).unwrap_or("00");
    let sec = s.get(12..14).unwrap_or("00");

    // Timezone: `Z`, or `+HH'mm'` with the minutes optional (ISO 32000-2
    // §7.9.4). Keep the minutes when they are there: +05'30' is India and
    // -03'30' is Newfoundland, and truncating either to the hour reports a
    // time that is off by half an hour. Some writers append `00'00'` after
    // `Z`; that is still UTC.
    let tz_part = s.get(14..).unwrap_or("");
    let tz = if tz_part.starts_with('Z') {
        " UTC".to_string()
    } else if let Some(sign) = tz_part.chars().next().filter(|c| *c == '+' || *c == '-') {
        let digits: Vec<char> = tz_part[1..]
            .chars()
            .filter(char::is_ascii_digit)
            .take(4)
            .collect();
        if digits.len() >= 2 {
            let hh: String = digits[..2].iter().collect();
            let mm: String = if digits.len() >= 4 {
                digits[2..4].iter().collect()
            } else {
                "00".to_string()
            };
            format!(" {sign}{hh}:{mm}")
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    format!("{year}-{month}-{day} {hour}:{min}:{sec}{tz}")
}

/// Detect common page sizes from dimensions in points.
#[cfg(feature = "pdf")] // Only the PDF metadata path names page sizes.
fn detect_page_size(w: f64, h: f64) -> &'static str {
    // Normalize to portrait (w < h)
    let (w, h) = if w > h { (h, w) } else { (w, h) };
    // Tolerances: ±3 pts
    let approx = |val: f64, target: f64| (val - target).abs() < 3.0;

    if approx(w, 595.0) && approx(h, 842.0) {
        return "A4";
    }
    if approx(w, 612.0) && approx(h, 792.0) {
        return "Letter";
    }
    if approx(w, 612.0) && approx(h, 1008.0) {
        return "Legal";
    }
    if approx(w, 420.0) && approx(h, 595.0) {
        return "A5";
    }
    if approx(w, 842.0) && approx(h, 1190.0) {
        return "A3";
    }
    if approx(w, 297.0) && approx(h, 420.0) {
        return "A6";
    }
    ""
}

/// Format encryption info to match pdfinfo style.
#[cfg(feature = "pdf")]
fn format_encryption(enc: &crate::pdf::metadata::EncryptionInfo) -> String {
    // Decode permission flags (ISO 32000-2 Table 22)
    let p = enc.permissions;
    let print = if p & 4 != 0 { "yes" } else { "no" };
    let copy = if p & 16 != 0 { "yes" } else { "no" };
    let change = if p & 8 != 0 { "yes" } else { "no" };
    let add_notes = if p & 32 != 0 { "yes" } else { "no" };
    format!(
        "print:{print} copy:{copy} change:{change} addNotes:{add_notes} algorithm:{}",
        enc.algorithm
    )
}

impl<'a> SiftDocument<'a> {
    /// Parse raw data into a document.
    fn parse(data: &'a [u8], file_type: Option<FileType>) -> Result<Self> {
        let inner = match file_type {
            #[cfg(feature = "jpeg")]
            Some(FileType::Jpeg) => DocumentInner::Jpeg {
                segments: crate::jpeg::parse_segments(data)?,
            },
            #[cfg(feature = "png")]
            Some(FileType::Png) => DocumentInner::Png {
                chunks: crate::png::parse_chunks(data)?,
            },
            #[cfg(feature = "webp")]
            Some(FileType::WebP) => DocumentInner::WebP {
                webp: crate::webp::parse_webp(data)?,
            },
            #[cfg(feature = "heif")]
            Some(FileType::Heif) => DocumentInner::Heif {
                info: crate::heif::parse_heif(data)?,
            },
            #[cfg(feature = "gif")]
            Some(FileType::Gif) => DocumentInner::Gif {
                info: crate::gif::parse_gif(data)?,
            },
            #[cfg(feature = "bmp")]
            Some(FileType::Bmp) => DocumentInner::Bmp {
                info: crate::bmp::parse_bmp(data)?,
            },
            #[cfg(feature = "tiff")]
            Some(ft) if ft.is_tiff_based() => {
                let (header, ifds) = crate::tiff::parse_tiff(data)?;
                DocumentInner::Tiff { header, ifds }
            }
            #[cfg(feature = "pdf")]
            Some(FileType::Pdf) => DocumentInner::Pdf {
                doc: crate::pdf::document::Document::parse(data)?,
            },
            #[cfg(feature = "quicktime")]
            Some(FileType::Cr3) => DocumentInner::QuickTime {
                info: crate::quicktime::parse_quicktime(data)?,
            },
            #[cfg(feature = "quicktime")]
            Some(FileType::QuickTime) => DocumentInner::QuickTime {
                info: crate::quicktime::parse_quicktime(data)?,
            },
            Some(FileType::Raf) => {
                // RAF: parse the embedded JPEG to extract EXIF/MakerNotes
                Self::parse_raf(data)?
            }
            _ => DocumentInner::Unknown,
        };
        Ok(Self {
            data,
            file_type,
            inner,
        })
    }

    /// Parse Fujifilm RAF format.
    ///
    /// RAF files have a custom header with an embedded JPEG at a specified offset.
    /// We extract the JPEG and parse its EXIF/MakerNotes via the JPEG parser.
    fn parse_raf(data: &'a [u8]) -> Result<DocumentInner<'a>> {
        // RAF header layout:
        //   0x00..0x08: "FUJIFILM"
        //   0x54..0x58: JPEG offset (u32 BE)
        //   0x58..0x5C: JPEG length (u32 BE)
        if data.len() < 0x5C {
            return Err(crate::core::Error::Truncated {
                needed: 0x5C,
                available: data.len(),
            });
        }

        let jpeg_offset =
            u32::from_be_bytes([data[0x54], data[0x55], data[0x56], data[0x57]]) as usize;
        let jpeg_length =
            u32::from_be_bytes([data[0x58], data[0x59], data[0x5A], data[0x5B]]) as usize;

        if jpeg_offset == 0 || jpeg_length == 0 {
            return Ok(DocumentInner::Unknown);
        }

        let jpeg_end = jpeg_offset.saturating_add(jpeg_length);
        if jpeg_end > data.len() {
            return Err(crate::core::Error::Truncated {
                needed: jpeg_end,
                available: data.len(),
            });
        }

        let jpeg_data = &data[jpeg_offset..jpeg_end];

        // Verify it's actually JPEG
        if jpeg_data.len() >= 3 && jpeg_data[0] == 0xFF && jpeg_data[1] == 0xD8 {
            #[cfg(feature = "jpeg")]
            {
                let segments = crate::jpeg::parse_segments(jpeg_data)?;
                return Ok(DocumentInner::Jpeg { segments });
            }
        }

        Ok(DocumentInner::Unknown)
    }

    /// Detected file type, or `None` if unrecognized.
    pub fn file_type(&self) -> Option<FileType> {
        self.file_type
    }

    // -- Metadata tags ----------------------------------------------------

    /// Extract all metadata as flat tags, similar to ExifTool output.
    ///
    /// Tags are returned in group order: EXIF, XMP, IPTC, ICC, PDF, QuickTime.
    /// All values are display-ready strings. Repeatable fields (e.g., IPTC
    /// Keywords) are joined with `", "`.
    pub fn tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        self.collect_exif_tags(&mut tags);
        self.collect_xmp_tags(&mut tags);
        self.collect_iptc_tags(&mut tags);
        self.collect_icc_tags(&mut tags);
        self.collect_pdf_tags(&mut tags);
        self.collect_quicktime_tags(&mut tags);
        self.collect_heif_tags(&mut tags);
        self.collect_composite_tags(&mut tags);
        tags
    }

    /// Extract only EXIF tags.
    pub fn exif_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        self.collect_exif_tags(&mut tags);
        tags
    }

    /// Extract only XMP tags.
    pub fn xmp_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        self.collect_xmp_tags(&mut tags);
        tags
    }

    /// Extract only IPTC tags.
    pub fn iptc_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        self.collect_iptc_tags(&mut tags);
        tags
    }

    // -- GPS coordinates ---------------------------------------------------

    /// Extract GPS coordinates from EXIF or XMP metadata.
    ///
    /// Returns decimal degrees (WGS84). Negative latitude = south,
    /// negative longitude = west.
    ///
    /// Checks EXIF GPS IFD first, then falls back to XMP `exif:GPS*` properties.
    pub fn gps(&self) -> Option<GpsCoordinates> {
        // Try EXIF GPS IFD first
        #[cfg(feature = "tiff")]
        if let Some(coords) = self.gps_from_exif() {
            return Some(coords);
        }

        // Fall back to XMP
        #[cfg(feature = "xmp")]
        if let Some(coords) = self.gps_from_xmp() {
            return Some(coords);
        }

        None
    }

    #[cfg(feature = "tiff")]
    fn gps_from_exif(&self) -> Option<GpsCoordinates> {
        use crate::core::TagValue;

        let tiff_data = self.find_exif_data()?;
        let exif = crate::tiff::exif::ExifData::parse(tiff_data).ok()?;
        let gps_ifd = exif.gps_ifd.as_ref()?;
        let be = exif.header.big_endian;

        // Helper: read a GPS coordinate (tag is RationalArray[3] = deg/min/sec)
        let read_coord = |tag_id: u16| -> Option<f64> {
            let entry = gps_ifd.entry(tag_id)?;
            let val = TagValue::from_entry(entry, be)?;
            match val {
                TagValue::RationalArray(ref rats) if rats.len() >= 3 => {
                    let deg = if rats[0].1 != 0 {
                        rats[0].0 as f64 / rats[0].1 as f64
                    } else {
                        0.0
                    };
                    let min = if rats[1].1 != 0 {
                        rats[1].0 as f64 / rats[1].1 as f64
                    } else {
                        0.0
                    };
                    let sec = if rats[2].1 != 0 {
                        rats[2].0 as f64 / rats[2].1 as f64
                    } else {
                        0.0
                    };
                    Some(deg + min / 60.0 + sec / 3600.0)
                }
                TagValue::Rational(n, d) if d != 0 => {
                    // Single rational = decimal degrees
                    Some(n as f64 / d as f64)
                }
                _ => None,
            }
        };

        // Helper: read ref tag ("N"/"S" or "E"/"W")
        let read_ref = |tag_id: u16| -> Option<String> {
            let entry = gps_ifd.entry(tag_id)?;
            let val = TagValue::from_entry(entry, be)?;
            Some(val.as_ascii()?.to_string())
        };

        let mut lat = read_coord(0x0002)?; // GPSLatitude
        let mut lon = read_coord(0x0004)?; // GPSLongitude

        if let Some(r) = read_ref(0x0001) {
            // GPSLatitudeRef
            if r == "S" {
                lat = -lat;
            }
        }
        if let Some(r) = read_ref(0x0003) {
            // GPSLongitudeRef
            if r == "W" {
                lon = -lon;
            }
        }

        // Altitude (optional)
        let altitude = read_coord(0x0006).map(|alt| {
            // GPSAltitude
            let alt_ref = gps_ifd.entry(0x0005) // GPSAltitudeRef
                .and_then(|e| TagValue::from_entry(e, be))
                .and_then(|v| v.to_u32());
            if alt_ref == Some(1) { -alt } else { alt } // 1 = below sea level
        });

        // GPS timestamp (optional)
        let timestamp = read_ref(0x001D); // GPSDateStamp "YYYY:MM:DD"

        Some(GpsCoordinates {
            latitude: lat,
            longitude: lon,
            altitude,
            timestamp,
        })
    }

    #[cfg(feature = "xmp")]
    fn gps_from_xmp(&self) -> Option<GpsCoordinates> {
        let xmp_bytes = self.find_xmp_data()?;
        let xmp = try_parse_xmp(xmp_bytes)?;

        // XMP stores GPS as "DD,MM.MMMN" or "DD,MM,SS.SSN"
        let lat_str = xmp.get_str(crate::xmp::ns::EXIF, "GPSLatitude")?;
        let lon_str = xmp.get_str(crate::xmp::ns::EXIF, "GPSLongitude")?;

        let lat = parse_xmp_gps_coord(lat_str)?;
        let lon = parse_xmp_gps_coord(lon_str)?;

        // Altitude
        let altitude = xmp
            .get_str(crate::xmp::ns::EXIF, "GPSAltitude")
            .and_then(|s| {
                let (n, d) = s.split_once('/')?;
                let alt = n.trim().parse::<f64>().ok()? / d.trim().parse::<f64>().ok()?;
                let alt_ref = xmp.get_str(crate::xmp::ns::EXIF, "GPSAltitudeRef");
                if alt_ref == Some("1") {
                    Some(-alt)
                } else {
                    Some(alt)
                }
            });

        Some(GpsCoordinates {
            latitude: lat,
            longitude: lon,
            altitude,
            timestamp: None,
        })
    }

    // -- Thumbnail --------------------------------------------------------

    /// Extract the EXIF thumbnail image (IFD1 JPEG), if present.
    ///
    /// Returns raw JPEG bytes that can be written directly to a `.jpg` file.
    /// Most JPEG, TIFF, and HEIF files contain a small embedded preview.
    #[cfg(feature = "tiff")]
    pub fn thumbnail(&self) -> Option<Vec<u8>> {
        let tiff_data = self.find_exif_data()?;
        let exif = crate::tiff::exif::ExifData::parse(tiff_data).ok()?;
        exif.thumbnail.map(|t| t.to_vec())
    }

    // -- PDF-specific: images and text ------------------------------------

    /// Extract all images from a PDF document.
    ///
    /// Returns images with data that can be written directly to disk
    /// (JPEG/JP2 passthrough) or processed further (decoded pixels).
    ///
    /// Returns `Ok(vec![])` for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn images(&self) -> Result<Vec<Image>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => {
                let pdf_images = crate::pdf::image_extract::extract_all_images(doc)?;
                Ok(pdf_images.into_iter().map(Image::from_pdf_image).collect())
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Extract text from all pages of a PDF document.
    ///
    /// Uses layout-preserving extraction (like `pdftotext -layout`).
    /// Returns one [`String`] per page.
    ///
    /// Returns `Ok(vec![])` for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn text_pages(&self) -> Result<Vec<String>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => {
                let page_count = doc.page_count().unwrap_or(0) as usize;
                let mut pages = Vec::with_capacity(page_count);
                for i in 0..page_count {
                    let page = doc.page(i as u32)?;
                    let text = crate::pdf::text_layout::extract_text_layout(doc, &page)
                        .unwrap_or_default();
                    pages.push(text);
                }
                Ok(pages)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// Extract raw text from all pages (no layout reconstruction).
    ///
    /// Faster than [`text_pages()`](Self::text_pages) but may lose
    /// whitespace structure. Returns one [`String`] per page.
    #[cfg(feature = "pdf")]
    pub fn text_pages_raw(&self) -> Result<Vec<String>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => {
                let page_count = doc.page_count().unwrap_or(0) as usize;
                let mut pages = Vec::with_capacity(page_count);
                for i in 0..page_count {
                    let page = doc.page(i as u32)?;
                    let text =
                        crate::pdf::text_layout::extract_text_raw(doc, &page).unwrap_or_default();
                    pages.push(text);
                }
                Ok(pages)
            }
            _ => Ok(Vec::new()),
        }
    }

    // -- PDF password authentication --------------------------------------

    /// Authenticate an encrypted PDF with a password.
    ///
    /// Returns `true` if the password was accepted (user or owner),
    /// `false` if wrong or the document is not encrypted.
    /// After successful authentication, text/image extraction will work.
    ///
    /// No-op (returns `false`) for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn authenticate(&mut self, password: &[u8]) -> bool {
        match &mut self.inner {
            DocumentInner::Pdf { doc } => doc.authenticate(password),
            _ => false,
        }
    }

    // -- PDF form fields --------------------------------------------------

    /// Extract the AcroForm (interactive form fields) from a PDF.
    ///
    /// Returns `Ok(None)` if the document has no form, or for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn acro_form(&self) -> Result<Option<crate::pdf::annot::AcroForm>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => doc.acro_form(),
            _ => Ok(None),
        }
    }

    // -- PDF annotations --------------------------------------------------

    /// Extract all annotations from all pages of a PDF.
    ///
    /// Returns `Ok(vec![])` for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn all_annotations(&self) -> Result<Vec<crate::pdf::annot::Annotation>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => doc.all_annotations(),
            _ => Ok(Vec::new()),
        }
    }

    // -- PDF structure tree -----------------------------------------------

    /// Extract the tagged structure tree from a PDF.
    ///
    /// Returns `Ok(None)` if the document is not tagged, or for non-PDF formats.
    #[cfg(feature = "pdf")]
    pub fn struct_tree(&self) -> Result<Option<crate::pdf::struct_tree::StructTree>> {
        match &self.inner {
            DocumentInner::Pdf { doc } => doc.struct_tree(),
            _ => Ok(None),
        }
    }

    // -- Private tag collectors -------------------------------------------

    /// Locate EXIF TIFF data within the parsed format.
    fn find_exif_data(&self) -> Option<&'a [u8]> {
        match &self.inner {
            #[cfg(feature = "jpeg")]
            DocumentInner::Jpeg { segments } => segments.iter().find_map(|s| s.exif_tiff_data()),
            #[cfg(feature = "png")]
            DocumentInner::Png { chunks } => crate::png::find_exif_chunk(chunks),
            #[cfg(feature = "webp")]
            DocumentInner::WebP { webp } => crate::webp::find_exif(webp),
            #[cfg(feature = "heif")]
            DocumentInner::Heif { info } => info.exif_data,
            #[cfg(feature = "tiff")]
            DocumentInner::Tiff { .. } => Some(self.data),
            _ => None,
        }
    }

    /// Locate XMP data within the parsed format.
    // Reached only by the XMP collector.
    #[cfg(feature = "xmp")]
    fn find_xmp_data(&self) -> Option<&'a [u8]> {
        match &self.inner {
            #[cfg(feature = "jpeg")]
            DocumentInner::Jpeg { segments } => segments.iter().find_map(|s| s.xmp_data()),
            #[cfg(feature = "png")]
            DocumentInner::Png { chunks } => crate::png::find_xmp_data(chunks),
            #[cfg(feature = "webp")]
            DocumentInner::WebP { webp } => crate::webp::find_xmp(webp),
            #[cfg(feature = "heif")]
            DocumentInner::Heif { info } => info.xmp_data,
            #[cfg(feature = "tiff")]
            DocumentInner::Tiff { ifds, .. } => {
                // Tag 0x02BC (700) = XMP data in IFD0
                if let Some(ifd0) = ifds.first() {
                    if let Some(entry) = ifd0.entry(0x02BC) {
                        if !entry.data.is_empty() {
                            return Some(entry.data);
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Locate ICC profile data within the parsed format.
    #[cfg(feature = "icc")]
    fn find_icc_data(&self) -> Option<Vec<u8>> {
        match &self.inner {
            #[cfg(feature = "jpeg")]
            DocumentInner::Jpeg { segments } => crate::jpeg::reassemble_icc_profile(segments),
            #[cfg(feature = "png")]
            DocumentInner::Png { chunks } => {
                crate::png::find_iccp_chunk(chunks).and_then(|(_, compressed)| {
                    miniz_oxide::inflate::decompress_to_vec_zlib(compressed).ok()
                })
            }
            #[cfg(feature = "webp")]
            DocumentInner::WebP { webp } => crate::webp::find_iccp(webp).map(|d| d.to_vec()),
            #[cfg(feature = "heif")]
            DocumentInner::Heif { info } => info.icc_data.map(|d| d.to_vec()),
            _ => None,
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "tiff"), allow(unused_variables))]
    fn collect_exif_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "tiff")]
        {
            // Try the standard path first (eXIf chunk, APP1 segment, etc.)
            if let Some(tiff_data) = self.find_exif_data() {
                // Compute TIFF base offset: position of TIFF header from start of file
                let tiff_base = tiff_data.as_ptr() as usize - self.data.as_ptr() as usize;
                emit_exif_from_tiff(tiff_data, tiff_base, tags);
                return;
            }

            // Fallback: PNG "Raw profile type exif" in text chunks
            #[cfg(feature = "png")]
            if let DocumentInner::Png { chunks } = &self.inner {
                if let Some(raw_exif) = crate::png::find_raw_profile_exif(chunks) {
                    emit_exif_from_tiff(&raw_exif, 0, tags);
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "xmp"), allow(unused_variables))]
    fn collect_xmp_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "xmp")]
        {
            let xmp_bytes = match self.find_xmp_data() {
                Some(d) => d,
                None => {
                    // For PDF, try extracting from metadata
                    #[cfg(feature = "pdf")]
                    if let DocumentInner::Pdf { doc } = &self.inner {
                        if let Ok(meta) = doc.metadata() {
                            if let Some(ref xmp_data) = meta.xmp {
                                if let Ok(xml) = std::str::from_utf8(xmp_data) {
                                    if let Ok(xmp) = crate::xmp::parse_xmp(xml) {
                                        emit_xmp_tags(&xmp, tags);
                                    }
                                }
                            }
                        }
                    }
                    return;
                }
            };

            if let Some(xmp) = try_parse_xmp(xmp_bytes) {
                emit_xmp_tags(&xmp, tags);
            }

            // Merge extended XMP properties (JPEG only)
            #[cfg(feature = "jpeg")]
            if let DocumentInner::Jpeg { segments } = &self.inner {
                if let Some(ext_xml) = crate::jpeg::reassemble_extended_xmp(segments) {
                    if let Ok(ext_xmp) = crate::xmp::parse_xmp(&ext_xml) {
                        emit_xmp_tags(&ext_xmp, tags);
                    }
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "iptc"), allow(unused_variables))]
    fn collect_iptc_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "iptc")]
        {
            // Annotated because every arm below is behind a format feature; with
            // none of them enabled only `_ => None` is left and the type is
            // otherwise unconstrained.
            let iptc_data: Option<Vec<u8>> = match &self.inner {
                #[cfg(feature = "jpeg")]
                DocumentInner::Jpeg { segments } => segments
                    .iter()
                    .find(|s| s.is_photoshop())
                    .and_then(|s| crate::iptc::extract_from_app13(s.data)),
                #[cfg(feature = "png")]
                DocumentInner::Png { chunks } => crate::png::find_raw_profile_iptc(chunks),
                #[cfg(feature = "tiff")]
                DocumentInner::Tiff { ifds, .. } => {
                    // Tag 0x83BB (33723) = IPTC data in IFD0
                    ifds.first()
                        .and_then(|ifd| ifd.entry(0x83BB))
                        .map(|entry| entry.data.to_vec())
                }
                _ => None,
            };

            if let Some(data) = iptc_data {
                if let Ok(iptc) = crate::iptc::parse_iptc(&data) {
                    emit_iptc_tags(&iptc, tags);
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "icc"), allow(unused_variables))]
    fn collect_icc_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "icc")]
        {
            let icc_data = match self.find_icc_data() {
                Some(d) => d,
                None => return,
            };

            if let Ok(profile) = crate::icc::parse_icc_profile(&icc_data) {
                if let Some(desc) = &profile.description {
                    tags.push(Tag::new("ICC", "ProfileDescription", desc.clone()));
                }
                if let Some(cprt) = &profile.copyright {
                    tags.push(Tag::new("ICC", "ProfileCopyright", cprt.clone()));
                }
                // CMM Type
                let cmm = profile.cmm_name();
                if !cmm.is_empty() {
                    tags.push(Tag::new("ICC", "ProfileCMMType", cmm));
                } else {
                    let raw = String::from_utf8_lossy(&profile.cmm_type)
                        .trim()
                        .to_string();
                    if !raw.is_empty() {
                        tags.push(Tag::new("ICC", "ProfileCMMType", raw));
                    }
                }
                tags.push(Tag::new(
                    "ICC",
                    "ProfileVersion",
                    format!(
                        "{}.{}.{}",
                        profile.version.0, profile.version.1, profile.version.2
                    ),
                ));
                tags.push(Tag::new(
                    "ICC",
                    "ProfileClass",
                    profile.device_class.as_str(),
                ));
                tags.push(Tag::new(
                    "ICC",
                    "ColorSpaceData",
                    profile.color_space.as_str(),
                ));
                tags.push(Tag::new(
                    "ICC",
                    "ProfileConnectionSpace",
                    profile.pcs.as_str(),
                ));
                tags.push(Tag::new("ICC", "ProfileDateTime", profile.date_time_str()));
                tags.push(Tag::new("ICC", "ProfileFileSignature", "acsp"));
                // Primary Platform
                let plat = profile.platform_name();
                if !plat.is_empty() {
                    tags.push(Tag::new("ICC", "PrimaryPlatform", plat));
                }
                // CMM Flags
                tags.push(Tag::new("ICC", "CMMFlags", profile.flags_str()));
                // Device Manufacturer
                let mfr = profile.manufacturer_name();
                if !mfr.is_empty() {
                    tags.push(Tag::new("ICC", "DeviceManufacturer", mfr));
                }
                // Device Model
                let mdl_raw = String::from_utf8_lossy(&profile.device_model)
                    .trim()
                    .to_string();
                if !mdl_raw.is_empty() && mdl_raw != "\0\0\0\0" && profile.device_model != [0; 4] {
                    tags.push(Tag::new("ICC", "DeviceModel", mdl_raw));
                }
                // Device Attributes
                tags.push(Tag::new(
                    "ICC",
                    "DeviceAttributes",
                    profile.attributes_str(),
                ));
                tags.push(Tag::new(
                    "ICC",
                    "RenderingIntent",
                    profile.rendering_intent.as_str(),
                ));
                // Connection Space Illuminant
                tags.push(Tag::new(
                    "ICC",
                    "ConnectionSpaceIlluminant",
                    profile.illuminant_str(),
                ));
                // Profile Creator
                let creator = profile.creator_name();
                if !creator.is_empty() {
                    tags.push(Tag::new("ICC", "ProfileCreator", creator));
                }
                // Profile ID
                let id_hex = profile.profile_id_hex();
                if id_hex != "00000000000000000000000000000000" {
                    tags.push(Tag::new("ICC", "ProfileID", id_hex));
                }
                // Media White Point (wtpt tag)
                if let Some(xyz) = profile.xyz_tag(b"wtpt") {
                    tags.push(Tag::new(
                        "ICC",
                        "MediaWhitePoint",
                        format!("{:.5} {:.5} {:.5}", xyz[0], xyz[1], xyz[2]),
                    ));
                }
                // Chromatic Adaptation (chad tag)
                if let Some(vals) = profile.chromatic_adaptation() {
                    if vals.len() >= 9 {
                        tags.push(Tag::new(
                            "ICC",
                            "ChromaticAdaptation",
                            format!(
                                "{:.5} {:.5} {:.5} {:.5} {:.5} {:.5} {:.5} {:.5} {:.5}",
                                vals[0],
                                vals[1],
                                vals[2],
                                vals[3],
                                vals[4],
                                vals[5],
                                vals[6],
                                vals[7],
                                vals[8]
                            ),
                        ));
                    }
                }
                // Matrix columns (rXYZ, gXYZ, bXYZ)
                if let Some(xyz) = profile.xyz_tag(b"rXYZ") {
                    tags.push(Tag::new(
                        "ICC",
                        "RedMatrixColumn",
                        format!("{:.5} {:.5} {:.5}", xyz[0], xyz[1], xyz[2]),
                    ));
                }
                if let Some(xyz) = profile.xyz_tag(b"gXYZ") {
                    tags.push(Tag::new(
                        "ICC",
                        "GreenMatrixColumn",
                        format!("{:.5} {:.5} {:.5}", xyz[0], xyz[1], xyz[2]),
                    ));
                }
                if let Some(xyz) = profile.xyz_tag(b"bXYZ") {
                    tags.push(Tag::new(
                        "ICC",
                        "BlueMatrixColumn",
                        format!("{:.5} {:.5} {:.5}", xyz[0], xyz[1], xyz[2]),
                    ));
                }
                // Tone Reproduction Curves (rTRC, gTRC, bTRC)
                if let Some(desc) = profile.trc_tag(b"rTRC") {
                    tags.push(Tag::new("ICC", "RedToneReproductionCurve", desc));
                }
                if let Some(desc) = profile.trc_tag(b"gTRC") {
                    tags.push(Tag::new("ICC", "GreenToneReproductionCurve", desc));
                }
                if let Some(desc) = profile.trc_tag(b"bTRC") {
                    tags.push(Tag::new("ICC", "BlueToneReproductionCurve", desc));
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "pdf"), allow(unused_variables))]
    fn collect_pdf_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "pdf")]
        {
            if let DocumentInner::Pdf { doc } = &self.inner {
                if let Ok(meta) = doc.metadata() {
                    if let Some(v) = &meta.title {
                        tags.push(Tag::new("PDF", "Title", v.clone()));
                    }
                    if let Some(v) = &meta.author {
                        tags.push(Tag::new("PDF", "Author", v.clone()));
                    }
                    if let Some(v) = &meta.subject {
                        tags.push(Tag::new("PDF", "Subject", v.clone()));
                    }
                    if let Some(v) = &meta.keywords {
                        tags.push(Tag::new("PDF", "Keywords", v.clone()));
                    }
                    if let Some(v) = &meta.creator {
                        tags.push(Tag::new("PDF", "Creator", v.clone()));
                    }
                    if let Some(v) = &meta.producer {
                        tags.push(Tag::new("PDF", "Producer", v.clone()));
                    }
                    if let Some(v) = &meta.creation_date {
                        tags.push(Tag::new("PDF", "CreateDate", format_pdf_date(v)));
                    }
                    if let Some(v) = &meta.mod_date {
                        tags.push(Tag::new("PDF", "ModifyDate", format_pdf_date(v)));
                    }
                    if let Some(v) = &meta.version {
                        tags.push(Tag::new("PDF", "PDFVersion", v.clone()));
                    }
                    tags.push(Tag::new("PDF", "PageCount", meta.page_count.to_string()));
                    // Page size from first page's MediaBox
                    if let Some(page) = meta.pages.first() {
                        let w = page.media_box[2] - page.media_box[0];
                        let h = page.media_box[3] - page.media_box[1];
                        let size_label = detect_page_size(w, h);
                        let size_str = if size_label.is_empty() {
                            format!("{w:.2} x {h:.2} pts")
                        } else {
                            format!("{w:.2} x {h:.2} pts ({size_label})")
                        };
                        tags.push(Tag::new("PDF", "PageSize", size_str));
                    }
                    tags.push(Tag::new(
                        "PDF",
                        "Tagged",
                        if meta.is_tagged { "Yes" } else { "No" },
                    ));
                    tags.push(Tag::new(
                        "PDF",
                        "Linearized",
                        if meta.is_linearized { "Yes" } else { "No" },
                    ));
                    if meta.has_javascript {
                        tags.push(Tag::new("PDF", "JavaScript", "Yes"));
                    }
                    if let Some(ref enc) = meta.encryption {
                        tags.push(Tag::new("PDF", "Encryption", format_encryption(enc)));
                    }
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "quicktime"), allow(unused_variables))]
    fn collect_quicktime_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "quicktime")]
        {
            if let DocumentInner::QuickTime { info } = &self.inner {
                use crate::quicktime::{format_brand, format_duration, format_qt_date};

                // Movie-level metadata
                tags.push(Tag::new(
                    "QuickTime",
                    "MajorBrand",
                    format_brand(&info.major_brand),
                ));
                tags.push(Tag::new(
                    "QuickTime",
                    "MinorVersion",
                    format!(
                        "{}.{}.{}",
                        info.minor_version >> 24,
                        (info.minor_version >> 16) & 0xFF,
                        info.minor_version & 0xFFFF
                    ),
                ));
                if !info.compatible_brands.is_empty() {
                    let brands: Vec<String> = info
                        .compatible_brands
                        .iter()
                        .map(|b| String::from_utf8_lossy(b).trim().to_string())
                        .collect();
                    tags.push(Tag::new("QuickTime", "CompatibleBrands", brands.join(", ")));
                }

                if let Some(ct) = info.creation_time {
                    tags.push(Tag::new("QuickTime", "CreateDate", format_qt_date(ct)));
                }
                if let Some(mt) = info.modification_time {
                    tags.push(Tag::new("QuickTime", "ModifyDate", format_qt_date(mt)));
                }
                if let Some(ts) = info.time_scale {
                    tags.push(Tag::new("QuickTime", "TimeScale", ts.to_string()));
                }
                if let (Some(dur), Some(ts)) = (info.duration, info.time_scale) {
                    if ts > 0 {
                        let secs = dur as f64 / ts as f64;
                        tags.push(Tag::new("QuickTime", "Duration", format_duration(secs)));
                    }
                }

                // Per-track metadata
                for track in &info.tracks {
                    match track.track_type {
                        crate::quicktime::TrackType::Video => {
                            if track.width > 0 && track.height > 0 {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "ImageWidth",
                                    track.width.to_string(),
                                ));
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "ImageHeight",
                                    track.height.to_string(),
                                ));
                            }
                            let codec_str =
                                String::from_utf8_lossy(&track.codec).trim().to_string();
                            tags.push(Tag::new("QuickTime", "CompressorID", codec_str));
                            if !track.codec_name.is_empty() {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "CompressorName",
                                    &track.codec_name,
                                ));
                            }
                            if track.frame_rate > 0.0 {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "VideoFrameRate",
                                    format!("{:.0}", track.frame_rate),
                                ));
                            }
                        }
                        crate::quicktime::TrackType::Audio => {
                            let codec_str =
                                String::from_utf8_lossy(&track.codec).trim().to_string();
                            tags.push(Tag::new("QuickTime", "AudioFormat", codec_str));
                            if track.audio_channels > 0 {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "AudioChannels",
                                    track.audio_channels.to_string(),
                                ));
                            }
                            if track.audio_bps > 0 {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "AudioBitsPerSample",
                                    track.audio_bps.to_string(),
                                ));
                            }
                            if track.audio_sample_rate > 0 {
                                tags.push(Tag::new(
                                    "QuickTime",
                                    "AudioSampleRate",
                                    track.audio_sample_rate.to_string(),
                                ));
                            }
                        }
                        _ => {}
                    }
                    if !track.handler_description.is_empty() {
                        tags.push(Tag::new(
                            "QuickTime",
                            "HandlerDescription",
                            &track.handler_description,
                        ));
                    }
                }

                if let Some(ref gps) = info.gps_string {
                    tags.push(Tag::new("QuickTime", "GPSCoordinates", gps.as_str()));
                }
            }
        }
    }

    // The body is gated on the feature that provides the parser, so with
    // that feature off there is nothing to push and `tags` goes unread.
    #[cfg_attr(not(feature = "heif"), allow(unused_variables))]
    fn collect_heif_tags(&self, tags: &mut Vec<Tag>) {
        #[cfg(feature = "heif")]
        {
            if let DocumentInner::Heif { info } = &self.inner {
                if let Some(rot) = info.rotation {
                    tags.push(Tag::new("HEIF", "Rotation", format!("{rot}")));
                }
                if let Some(ref depths) = info.pixel_depths {
                    let s: Vec<String> = depths.iter().map(|d| d.to_string()).collect();
                    tags.push(Tag::new("HEIF", "ImagePixelDepth", s.join(" ")));
                }
                // ISOBMFF container metadata
                let major_raw = String::from_utf8_lossy(&info.ftyp.major_brand)
                    .trim()
                    .to_string();
                let major_desc = match major_raw.as_str() {
                    "heic" => "High Efficiency Image Format HEVC still image (.HEIC)",
                    "heix" => "High Efficiency Image Format still image (.HEIF)",
                    "hevc" => "High Efficiency Image Format HEVC sequence (.HEICS)",
                    "hevx" => "High Efficiency Image Format sequence (.HEIFS)",
                    "heim" => "High Efficiency Image Format still image (.HEIF)",
                    "heis" => "High Efficiency Image Format still image (.HEIF)",
                    "mif1" => "Multi-Image Application Format (.HEIF)",
                    "msf1" => "Multi-Image Application Format sequence (.HEIFS)",
                    "avif" => "AV1 Image File Format (.AVIF)",
                    "avis" => "AV1 Image File Format sequence (.AVIFS)",
                    other => other,
                };
                tags.push(Tag::new("HEIF", "MajorBrand", major_desc));
                tags.push(Tag::new(
                    "HEIF",
                    "MinorVersion",
                    format!("0.{}.0", info.ftyp.minor_version),
                ));
                let compat: Vec<String> = info
                    .ftyp
                    .compatible_brands
                    .iter()
                    .map(|b| String::from_utf8_lossy(b).trim().to_string())
                    .collect();
                tags.push(Tag::new("HEIF", "CompatibleBrands", compat.join(", ")));
                if let Some(pid) = info.primary_item_id {
                    tags.push(Tag::new("HEIF", "PrimaryItemReference", pid.to_string()));
                }
                if let (Some(w), Some(h)) = (info.width, info.height) {
                    tags.push(Tag::new("HEIF", "ImageSpatialExtent", format!("{w}x{h}")));
                    tags.push(Tag::new("HEIF", "MetaImageSize", format!("{w}x{h}")));
                }
                if let (Some(off), Some(sz)) = (info.mdat_offset, info.mdat_size) {
                    tags.push(Tag::new("HEIF", "MediaDataOffset", off.to_string()));
                    tags.push(Tag::new("HEIF", "MediaDataSize", sz.to_string()));
                }
                if let Some(ref cc) = info.codec_config {
                    let codec_name = match &cc.codec {
                        b"hvcC" => "HEVC",
                        b"av1C" => "AV1",
                        _ => "Unknown",
                    };
                    tags.push(Tag::new("HEIF", "CompressorID", codec_name));
                    tags.push(Tag::new(
                        "HEIF",
                        "BitDepthLuma",
                        cc.bit_depth_luma.to_string(),
                    ));
                    tags.push(Tag::new(
                        "HEIF",
                        "BitDepthChroma",
                        cc.bit_depth_chroma.to_string(),
                    ));
                    let chroma_str = match cc.chroma_format {
                        0 => "4:0:0",
                        1 => "4:2:0",
                        2 => "4:2:2",
                        3 => "4:4:4",
                        _ => "Unknown",
                    };
                    tags.push(Tag::new("HEIF", "ChromaFormat", chroma_str));
                    // HEVC-specific config fields
                    if &cc.codec == b"hvcC" {
                        tags.push(Tag::new(
                            "HEIF",
                            "HEVCConfigurationVersion",
                            cc.config_version.to_string(),
                        ));
                        let profile_space = match cc.general_profile_space {
                            0 => "Conforming",
                            1 => "Reserved 1",
                            2 => "Reserved 2",
                            _ => "Reserved 3",
                        };
                        tags.push(Tag::new("HEIF", "GeneralProfileSpace", profile_space));
                        tags.push(Tag::new(
                            "HEIF",
                            "GeneralTierFlag",
                            if cc.general_tier_flag {
                                "High Tier"
                            } else {
                                "Main Tier"
                            },
                        ));
                        // ExifTool uses the highest-priority compatible profile name
                        let profile_name = if cc.general_profile_compat_flags & 0x10000000 != 0 {
                            "Main Still Picture"
                        } else if cc.general_profile_compat_flags & 0x20000000 != 0 {
                            "Main 10"
                        } else if cc.general_profile_compat_flags & 0x40000000 != 0 {
                            "Main"
                        } else {
                            match cc.profile_idc {
                                1 => "Main",
                                2 => "Main 10",
                                3 => "Main Still Picture",
                                4 => "Format Range Extensions",
                                _ => "Unknown",
                            }
                        };
                        tags.push(Tag::new("HEIF", "GeneralProfileIDC", profile_name));
                        tags.push(Tag::new(
                            "HEIF",
                            "GeneralLevelIDC",
                            format!("{} (Level {:.1})", cc.level_idc, cc.level_idc as f64 / 30.0),
                        ));
                        // Decode profile compat flags to names (bit j from MSB = profile j)
                        let mut profiles = Vec::new();
                        let f = cc.general_profile_compat_flags;
                        if f & 0x40000000 != 0 {
                            profiles.push("Main");
                        } // bit 1
                        if f & 0x20000000 != 0 {
                            profiles.push("Main 10");
                        } // bit 2
                        if f & 0x10000000 != 0 {
                            profiles.push("Main Still Picture");
                        } // bit 3
                        if f & 0x08000000 != 0 {
                            profiles.push("Format Range Extensions");
                        } // bit 4
                        let compat_str = if profiles.is_empty() {
                            format!("0x{:08x}", cc.general_profile_compat_flags)
                        } else {
                            profiles.join(", ")
                        };
                        tags.push(Tag::new("HEIF", "GenProfileCompatibilityFlags", compat_str));
                        let cf = &cc.constraint_indicator_flags;
                        tags.push(Tag::new(
                            "HEIF",
                            "ConstraintIndicatorFlags",
                            format!(
                                "{} {} {} {} {} {}",
                                cf[0], cf[1], cf[2], cf[3], cf[4], cf[5]
                            ),
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "MinSpatialSegmentationIDC",
                            cc.min_spatial_segmentation_idc.to_string(),
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "ParallelismType",
                            cc.parallelism_type.to_string(),
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "NumTemporalLayers",
                            cc.num_temporal_layers.to_string(),
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "TemporalIdNested",
                            if cc.temporal_id_nested { "Yes" } else { "No" },
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "ConstantFrameRate",
                            if cc.constant_frame_rate > 0 {
                                "Yes"
                            } else {
                                "Unknown"
                            },
                        ));
                        tags.push(Tag::new(
                            "HEIF",
                            "AverageFrameRate",
                            format!("{}", cc.avg_frame_rate),
                        ));
                    }
                }
                if let Some(ref aux) = info.aux_type {
                    tags.push(Tag::new("HEIF", "AuxiliaryImageType", aux.as_str()));
                }
                if let Some(ht) = info.handler_type {
                    let s = String::from_utf8_lossy(&ht).trim().to_string();
                    tags.push(Tag::new("HEIF", "HandlerType", s));
                }
            }
        }
    }

    fn collect_composite_tags(&self, tags: &mut Vec<Tag>) {
        // Build an owned lookup map from existing tags (avoids borrow issues)
        let lookup: std::collections::HashMap<String, String> = tags
            .iter()
            .map(|t| (t.name.clone(), t.value.clone()))
            .collect();
        let find = |name: &str| -> Option<String> { lookup.get(name).cloned() };

        // ImageSize, ImageWidth, ImageHeight
        let (w, h) = self.image_dimensions(&find);
        if let (Some(w), Some(h)) = (w, h) {
            tags.push(Tag::new("Composite", "ImageWidth", w.to_string()));
            tags.push(Tag::new("Composite", "ImageHeight", h.to_string()));
            tags.push(Tag::new("Composite", "ImageSize", format!("{w}x{h}")));
            let mp = (w as f64) * (h as f64) / 1_000_000.0;
            let mp_str = if mp >= 1.0 {
                format!("{mp:.1}")
            } else if mp >= 0.001 {
                format!("{mp:.3}")
            } else {
                format!("{mp:.6}")
            };
            tags.push(Tag::new("Composite", "Megapixels", mp_str));
        }

        // Aperture (from FNumber or ApertureValue)
        if let Some(fnum) = find("FNumber") {
            if let Ok(f) = fnum.parse::<f64>() {
                let s = if f < 1.0 {
                    format!("{f:.2}")
                } else {
                    format!("{f:.1}")
                };
                tags.push(Tag::new("Composite", "Aperture", s));
            }
        }

        // ShutterSpeed (from ExposureTime)
        if let Some(et) = find("ExposureTime") {
            tags.push(Tag::new("Composite", "ShutterSpeed", et));
        }

        // ScaleFactor35efl + FocalLength35efl
        if let Some(fl_str) = find("FocalLength") {
            let fl: Option<f64> = fl_str.trim_end_matches(" mm").parse().ok();
            if let Some(fl) = fl {
                let scale = self.compute_scale_factor_35(&find, fl);
                if let Some(sf) = scale {
                    tags.push(Tag::new(
                        "Composite",
                        "ScaleFactor35efl",
                        format!("{sf:.1}"),
                    ));
                    let fl35 = fl * sf;
                    tags.push(Tag::new(
                        "Composite",
                        "FocalLength35efl",
                        format!("{fl:.1} mm (35 mm equivalent: {fl35:.1} mm)"),
                    ));

                    // CircleOfConfusion
                    let coc = 43.2666 / (sf * 1440.0);
                    tags.push(Tag::new(
                        "Composite",
                        "CircleOfConfusion",
                        format!("{coc:.3} mm"),
                    ));

                    // FOV (horizontal, assumes 36mm sensor width equiv)
                    let fov = 2.0 * (36.0 / (2.0 * fl * sf)).atan().to_degrees();
                    tags.push(Tag::new("Composite", "FOV", format!("{fov:.1} deg")));

                    // HyperfocalDistance
                    if let Some(ap_str) = find("FNumber") {
                        if let Ok(ap) = ap_str.parse::<f64>() {
                            if ap > 0.0 && coc > 0.0 {
                                let hyper = (fl * fl) / (ap * coc * 1000.0);
                                tags.push(Tag::new(
                                    "Composite",
                                    "HyperfocalDistance",
                                    format!("{hyper:.2} m"),
                                ));
                            }
                        }
                    }
                } else {
                    tags.push(Tag::new(
                        "Composite",
                        "FocalLength35efl",
                        format!("{fl:.1} mm"),
                    ));
                }
            }
        }

        // LightValue
        if let (Some(ap_str), Some(et_str), Some(iso_str)) = (
            find("FNumber").or_else(|| find("Aperture")),
            find("ExposureTime"),
            find("ISO").or_else(|| find("ISOSpeedRatings")),
        ) {
            if let (Ok(ap), Ok(iso)) = (ap_str.parse::<f64>(), iso_str.parse::<f64>()) {
                // Parse exposure time - could be "1/250" or "4.0"
                let et: Option<f64> = if let Some((n, d)) = et_str.split_once('/') {
                    n.parse::<f64>()
                        .ok()
                        .and_then(|num| d.parse::<f64>().ok().map(|den| num / den))
                } else {
                    et_str.parse().ok()
                };
                if let Some(et) = et {
                    if ap > 0.0 && et > 0.0 && iso > 0.0 {
                        let lv = 2.0 * ap.log2() - et.log2() - (iso / 100.0).log2();
                        tags.push(Tag::new("Composite", "LightValue", format!("{lv:.1}")));
                    }
                }
            }
        }

        // GPSPosition
        if let Some(gps) = self.gps() {
            tags.push(Tag::new(
                "Composite",
                "GPSPosition",
                format!("{:.6} {:.6}", gps.latitude, gps.longitude),
            ));
        }

        // SubSecDateTimeOriginal (with timezone if available)
        if let Some(dt) = find("DateTimeOriginal") {
            if let Some(ss) = find("SubSecTimeOriginal") {
                let tz = find("OffsetTimeOriginal").unwrap_or_default();
                let combined = if tz.is_empty() {
                    format!("{dt}.{ss}")
                } else {
                    format!("{dt}.{ss}{tz}")
                };
                tags.push(Tag::new("Composite", "SubSecDateTimeOriginal", combined));
            }
        }

        // SubSecCreateDate (with timezone if available)
        if let Some(dt) = find("DateTimeDigitized").or_else(|| find("CreateDate")) {
            if let Some(ss) = find("SubSecTimeDigitized") {
                let tz = find("OffsetTimeDigitized").unwrap_or_default();
                let combined = if tz.is_empty() {
                    format!("{dt}.{ss}")
                } else {
                    format!("{dt}.{ss}{tz}")
                };
                tags.push(Tag::new("Composite", "SubSecCreateDate", combined));
            }
        }

        // SubSecModifyDate (with timezone if available)
        if let Some(dt) = find("DateTime").or_else(|| find("ModifyDate")) {
            if let Some(ss) = find("SubSecTime") {
                let tz = find("OffsetTime").unwrap_or_default();
                let combined = if tz.is_empty() {
                    format!("{dt}.{ss}")
                } else {
                    format!("{dt}.{ss}{tz}")
                };
                tags.push(Tag::new("Composite", "SubSecModifyDate", combined));
            }
        }

        // RunTimeSincePowerUp (from Apple RunTime fields)
        if let Some(val) = find("RunTimeValue") {
            if let Some(scale) = find("RunTimeScale") {
                if let (Ok(v), Ok(s)) = (val.parse::<u64>(), scale.parse::<u64>()) {
                    if s > 0 {
                        let secs = (v as f64 / s as f64).round() as u64;
                        let h = secs / 3600;
                        let m = (secs % 3600) / 60;
                        let s_rem = secs % 60;
                        tags.push(Tag::new(
                            "Composite",
                            "RunTimeSincePowerUp",
                            format!("{h}:{m:02}:{s_rem:02}"),
                        ));
                    }
                }
            }
        }

        // ExifTool-compatible aliases
        if let Some(model) = find("Model") {
            tags.push(Tag::new("Composite", "CameraModelName", model));
        }
        // LensID (alias of LensModel)
        if let Some(lens) = find("LensModel") {
            tags.push(Tag::new("Composite", "LensID", lens));
        }
        // DateTimeOriginal / CreateDate / ModifyDate with timezone
        if let Some(dt) = find("DateTimeOriginal") {
            let tz = find("OffsetTimeOriginal").unwrap_or_default();
            tags.push(Tag::new(
                "Composite",
                "DateTimeOriginal",
                format!("{dt}{tz}"),
            ));
        }
        if let Some(dt) = find("DateTimeDigitized") {
            let tz = find("OffsetTimeDigitized").unwrap_or_default();
            tags.push(Tag::new("Composite", "CreateDate", format!("{dt}{tz}")));
        }
        if let Some(dt) = find("DateTime") {
            let tz = find("OffsetTime").unwrap_or_default();
            tags.push(Tag::new("Composite", "ModifyDate", format!("{dt}{tz}")));
        }
        // ScaleFactorTo35mmEquivalent (alias of ScaleFactor35efl - search tags directly since it's added above)
        if let Some(sf) = tags.iter().find(|t| t.name == "ScaleFactor35efl") {
            let val = sf.value.clone();
            tags.push(Tag::new("Composite", "ScaleFactorTo35mmEquivalent", val));
        }
        // FieldOfView (alias of FOV - search tags directly)
        if let Some(fov) = tags.iter().find(|t| t.name == "FOV") {
            let val = fov.value.clone();
            tags.push(Tag::new("Composite", "FieldOfView", val));
        }
        // ExifByteOrder - detect from TIFF header in EXIF data
        if let Some(exif) = self.find_exif_data() {
            if exif.len() >= 2 {
                let order = if exif[0] == b'I' && exif[1] == b'I' {
                    "Little-endian (Intel, II)"
                } else if exif[0] == b'M' && exif[1] == b'M' {
                    "Big-endian (Motorola, MM)"
                } else {
                    ""
                };
                if !order.is_empty() {
                    // Group "File", not "ExifTool": this is a fact about the
                    // container, and the group name is shown to people. Naming
                    // a group after the tool we replace reads as a leak rather
                    // than a source.
                    tags.push(Tag::new("File", "ExifByteOrder", order));
                }
            }
        }
    }

    /// Get image dimensions from the parsed document.
    fn image_dimensions(
        &self,
        find: &dyn Fn(&str) -> Option<String>,
    ) -> (Option<u32>, Option<u32>) {
        // Try HEIF dimensions first
        #[cfg(feature = "heif")]
        if let DocumentInner::Heif { info } = &self.inner {
            if let (Some(w), Some(h)) = (info.width, info.height) {
                return (Some(w), Some(h));
            }
        }

        // Try QuickTime track dimensions
        #[cfg(feature = "quicktime")]
        if let DocumentInner::QuickTime { info } = &self.inner {
            for track in &info.tracks {
                if track.width > 0 && track.height > 0 {
                    return (Some(track.width), Some(track.height));
                }
            }
        }

        // Try EXIF tags
        let w = find("ExifImageWidth")
            .or_else(|| find("ImageWidth"))
            .and_then(|s| s.parse().ok());
        let h = find("ExifImageHeight")
            .or_else(|| find("ImageHeight"))
            .and_then(|s| s.parse().ok());
        (w, h)
    }

    /// Compute 35mm crop factor from FocalLengthIn35mmFormat or sensor size.
    fn compute_scale_factor_35(
        &self,
        find: &dyn Fn(&str) -> Option<String>,
        focal_length: f64,
    ) -> Option<f64> {
        if focal_length <= 0.0 {
            return None;
        }

        // Method 1: FocalLengthIn35mmFormat tag
        if let Some(fl35_str) = find("FocalLengthIn35mmFormat") {
            let fl35: f64 = fl35_str.trim_end_matches(" mm").parse().ok()?;
            if fl35 > 0.0 {
                return Some(fl35 / focal_length);
            }
        }

        // Method 2: FocalPlaneXSize/YSize (from maker notes or EXIF)
        if let (Some(xsize_str), Some(ysize_str)) =
            (find("FocalPlaneXSize"), find("FocalPlaneYSize"))
        {
            let x: f64 = xsize_str.trim_end_matches(" mm").parse().ok()?;
            let y: f64 = ysize_str.trim_end_matches(" mm").parse().ok()?;
            if x > 0.0 && y > 0.0 {
                let diag = (x * x + y * y).sqrt();
                return Some(43.2666 / diag);
            }
        }

        None
    }
}

// ---------------------------------------------------------------------------
// EXIF tag emission from raw TIFF data
// ---------------------------------------------------------------------------

#[cfg(feature = "tiff")]
fn emit_exif_from_tiff(tiff_data: &[u8], tiff_base: usize, tags: &mut Vec<Tag>) {
    use crate::core::TagValue;
    use crate::tiff::tags::{self, TagGroup};

    let exif = match crate::tiff::exif::ExifData::parse(tiff_data) {
        Ok(e) => e,
        Err(_) => return,
    };

    let be = exif.header.big_endian;

    let mut emit_ifd = |ifd: &crate::tiff::Ifd, group: TagGroup| {
        let is_ifd1 = group == TagGroup::Ifd1;
        for entry in &ifd.entries {
            let tag_def = tags::find_tag(entry.tag, group).or_else(|| {
                if group == TagGroup::Ifd0 {
                    tags::find_tag(entry.tag, TagGroup::ExifIfd)
                } else {
                    None
                }
            });
            if let Some(tag_def) = tag_def {
                if matches!(
                    tag_def.name,
                    "ExifIFD" | "GPSIFD" | "InteropIFD" | "MakerNote"
                ) {
                    continue;
                }
                if let Some(val) = TagValue::from_entry(entry, be) {
                    let display = tags::print_value(tag_def, &val);
                    if is_ifd1
                        && tags
                            .iter()
                            .any(|t| t.group == "EXIF" && t.name == tag_def.name)
                    {
                        continue;
                    }
                    tags.push(Tag::with_typed("EXIF", tag_def.name, display, val));
                }
            }
        }
    };

    emit_ifd(&exif.ifd0, TagGroup::Ifd0);
    if let Some(ref ifd) = exif.exif_ifd {
        emit_ifd(ifd, TagGroup::ExifIfd);
    }
    if let Some(ref ifd) = exif.gps_ifd {
        emit_ifd(ifd, TagGroup::GpsIfd);
    }
    if let Some(ref ifd) = exif.interop_ifd {
        emit_ifd(ifd, TagGroup::InteropIfd);
    }
    if let Some(ref ifd) = exif.ifd1 {
        emit_ifd(ifd, TagGroup::Ifd1);
    }

    // MakerNotes: decode vendor-specific tags
    if let Some(ref mnr) = exif.maker_note {
        use crate::tiff::maker_notes;

        // Determine vendor from Make tag in IFD0
        let mut vendor = maker_notes::detect_vendor(mnr.data);
        if vendor == maker_notes::Vendor::Unknown {
            // Try to identify from EXIF Make string
            for entry in &exif.ifd0.entries {
                if entry.tag == 0x010F {
                    // Make tag
                    if let Some(val) = TagValue::from_entry(entry, be) {
                        let make_str = val.display();
                        vendor = maker_notes::vendor_from_make(&make_str);
                    }
                    break;
                }
            }
        }

        if let Some(mut mn) = maker_notes::parse_maker_note(mnr, tiff_data, be) {
            // Set vendor if it was Unknown from header detection
            if mn.vendor == maker_notes::Vendor::Unknown {
                mn.vendor = vendor;
            }
            let mn_file_offset = tiff_base + mnr.offset;
            let decoded = maker_notes::decode_maker_tags_with_tiff(
                &mn,
                mnr.data,
                tiff_base,
                mn_file_offset,
                tiff_data,
            );
            for dt in decoded {
                tags.push(Tag::new("MakerNotes", &dt.name, dt.value));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Private helpers for tag emission
// ---------------------------------------------------------------------------

#[cfg(feature = "xmp")]
fn emit_xmp_tags(xmp: &crate::xmp::XmpData, tags: &mut Vec<Tag>) {
    for prop in &xmp.properties {
        let strings = prop.value.all_strings();
        if !strings.is_empty() {
            let value = strings.join(", ");
            // Use namespace short prefix for sub-group
            let sub = xmp_ns_short(&prop.namespace);
            let name = capitalize_first(&prop.name);
            // TODO: surface as `group_name` (e.g. "XMP-dc") once consumers
            // are ready for namespaced groups; today we flatten under "XMP".
            let _group_name = if sub.is_empty() {
                "XMP".to_string()
            } else {
                format!("XMP-{sub}")
            };
            // Only insert first occurrence (some files have duplicate properties)
            if !tags.iter().any(|t| t.group == "XMP" && t.name == name) {
                tags.push(Tag::new("XMP", name, value));
            }
        }
    }
}

#[cfg(feature = "xmp")]
fn xmp_ns_short(ns: &str) -> &'static str {
    match ns {
        "http://purl.org/dc/elements/1.1/" => "dc",
        "http://ns.adobe.com/xap/1.0/" => "xmp",
        "http://ns.adobe.com/exif/1.0/" => "exif",
        "http://ns.adobe.com/tiff/1.0/" => "tiff",
        "http://ns.adobe.com/photoshop/1.0/" => "photoshop",
        "http://ns.adobe.com/xap/1.0/mm/" => "xmpMM",
        "http://ns.adobe.com/xap/1.0/rights/" => "xmpRights",
        "http://ns.adobe.com/camera-raw-settings/1.0/" => "crs",
        "http://ns.adobe.com/exif/1.0/aux/" => "aux",
        "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" => "iptcCore",
        "http://ns.google.com/photos/1.0/container/" => "Container",
        "http://ns.adobe.com/hdr-gain-map/1.0/" => "HDRGainMap",
        _ => "",
    }
}

#[cfg(feature = "xmp")]
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(feature = "xmp")]
fn try_parse_xmp(data: &[u8]) -> Option<crate::xmp::XmpData> {
    // Try UTF-8 first
    if let Ok(xml) = std::str::from_utf8(data) {
        return crate::xmp::parse_xmp(xml).ok();
    }
    // Try UTF-16BE
    if data.len() >= 4 && data[0] == 0x00 && data[1] == b'<' {
        let u16s: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        let xml = String::from_utf16_lossy(&u16s);
        return crate::xmp::parse_xmp(&xml).ok();
    }
    // Try UTF-16LE
    if data.len() >= 4 && data[0] == b'<' && data[1] == 0x00 {
        let u16s: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let xml = String::from_utf16_lossy(&u16s);
        return crate::xmp::parse_xmp(&xml).ok();
    }
    None
}

#[cfg(feature = "iptc")]
fn emit_iptc_tags(iptc: &crate::iptc::IptcData, tags: &mut Vec<Tag>) {
    use std::collections::HashMap;

    let mut groups: HashMap<&str, Vec<String>> = HashMap::new();

    for ds in &iptc.datasets {
        let name = ds.name();
        if name == "Unknown" {
            continue;
        }

        if ds.record == 2 {
            if name == "ApplicationRecordVersion" {
                if ds.value.len() == 2 {
                    let ver = u16::from_be_bytes([ds.value[0], ds.value[1]]);
                    groups.entry(name).or_default().push(ver.to_string());
                }
            } else {
                let val = ds.as_string_lossy();
                groups.entry(name).or_default().push(val);
            }
        } else if ds.record == 1 && name == "CodedCharacterSet" {
            let val = if ds.value == b"\x1b\x25\x47" || ds.value == [0x1b, 0x2e, 0x41] {
                "UTF8".to_string()
            } else {
                ds.value
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            groups.entry(name).or_default().push(val);
        }
    }

    for (name, values) in groups {
        tags.push(Tag::new("IPTC", name, values.join(", ")));
    }
}

// ---------------------------------------------------------------------------
// GpsCoordinates
// ---------------------------------------------------------------------------

/// GPS coordinates extracted from image or document metadata.
///
/// Latitude and longitude are in decimal degrees (WGS84).
/// Negative latitude = south, negative longitude = west.
#[derive(Debug, Clone, PartialEq)]
pub struct GpsCoordinates {
    /// Decimal degrees, negative = south.
    pub latitude: f64,
    /// Decimal degrees, negative = west.
    pub longitude: f64,
    /// Meters above sea level (negative = below). `None` if not present.
    pub altitude: Option<f64>,
    /// GPS date stamp from EXIF (`"YYYY:MM:DD"`). `None` if not present.
    pub timestamp: Option<String>,
}

impl std::fmt::Display for GpsCoordinates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:.6}, {:.6}", self.latitude, self.longitude)?;
        if let Some(alt) = self.altitude {
            write!(f, ", {:.1}m", alt)?;
        }
        Ok(())
    }
}

/// Parse XMP GPS coordinate string to decimal degrees.
///
/// Formats: `"DD,MM.MMMN"`, `"DD,MM,SS.SSN"`, or `"DD,MM.MMMM,N"`.
/// The direction letter (N/S/E/W) determines sign.
#[cfg(feature = "xmp")]
fn parse_xmp_gps_coord(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Extract direction letter (last char or after last comma)
    let (num_part, dir) = if s.ends_with(|c: char| "NSEW".contains(c)) {
        (&s[..s.len() - 1], &s[s.len() - 1..])
    } else {
        // Direction might be after last comma: "DD,MM.MM,N"
        let last_comma = s.rfind(',')?;
        let candidate = s[last_comma + 1..].trim();
        if candidate.len() == 1 && "NSEW".contains(candidate) {
            (&s[..last_comma], candidate)
        } else {
            return None;
        }
    };

    let parts: Vec<&str> = num_part.split(',').collect();
    let degrees = match parts.len() {
        2 => {
            // DD,MM.MMMM
            let deg: f64 = parts[0].trim().parse().ok()?;
            let min: f64 = parts[1].trim().parse().ok()?;
            deg + min / 60.0
        }
        3 => {
            // DD,MM,SS.SS
            let deg: f64 = parts[0].trim().parse().ok()?;
            let min: f64 = parts[1].trim().parse().ok()?;
            let sec: f64 = parts[2].trim().parse().ok()?;
            deg + min / 60.0 + sec / 3600.0
        }
        _ => return None,
    };

    let sign = if dir == "S" || dir == "W" { -1.0 } else { 1.0 };
    Some(degrees * sign)
}

// ---------------------------------------------------------------------------
// Tag - a single metadata tag
// ---------------------------------------------------------------------------

/// A metadata tag with group, name, and display value.
///
/// Modeled after ExifTool's flat output: every piece of metadata from
/// EXIF, XMP, IPTC, ICC, and PDF is represented as a simple tag.
///
/// Values are always display-ready strings. For programmatic access
/// to typed values, use the mid-level API (e.g., `ExifData`, `XmpData`).
#[derive(Debug, Clone, PartialEq)]
pub struct Tag {
    /// Tag group: `"EXIF"`, `"MakerNotes"`, `"XMP"`, `"IPTC"`, `"ICC"`,
    /// `"PDF"`, `"QuickTime"`, `"HEIF"`, `"Composite"` (values we derive) or
    /// `"File"` (container facts like byte order).
    ///
    /// A group is a namespace: the same NAME may legitimately appear in two
    /// groups (a PDF states CreateDate in both its Info dictionary and its
    /// XMP), and that is information, not duplication. Two tags with the same
    /// name in the SAME group is the bug - see the corpus guard in
    /// `tests/tag_uniqueness.rs`.
    pub group: &'static str,
    /// Tag name: `"Make"`, `"DateCreated"`, `"Keywords"`, etc.
    pub name: String,
    /// Display-ready value string.
    pub value: String,
    /// Raw typed value from the parser, if available.
    /// Present for EXIF/TIFF tags; `None` for XMP, IPTC, PDF, ICC, etc.
    pub typed_value: Option<crate::core::TagValue>,
}

impl Tag {
    /// Create a new tag (string value only).
    pub fn new(group: &'static str, name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            group,
            name: name.into(),
            value: value.into(),
            typed_value: None,
        }
    }

    /// Create a new tag with a typed value alongside the display string.
    pub fn with_typed(
        group: &'static str,
        name: impl Into<String>,
        value: impl Into<String>,
        typed: crate::core::TagValue,
    ) -> Self {
        Self {
            group,
            name: name.into(),
            value: value.into(),
            typed_value: Some(typed),
        }
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {} = {}", self.group, self.name, self.value)
    }
}

// ---------------------------------------------------------------------------
// Image - an extracted image
// ---------------------------------------------------------------------------

/// An image extracted from a PDF document.
///
/// Image data is either passthrough bytes (JPEG/JP2 - can be written
/// directly to disk) or decoded pixels (raw bitmap data).
#[derive(Debug, Clone)]
pub struct Image {
    /// 0-based page index the image was found on.
    pub page: u32,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Bits per component (1, 2, 4, 8, 16).
    pub bpc: u8,
    /// Number of color components (1=gray, 3=RGB, 4=CMYK).
    pub components: u8,
    /// The image data and its format.
    pub data: ImageData,
}

/// Image data with format information.
#[derive(Debug, Clone)]
pub enum ImageData {
    /// Complete JPEG file - write directly to `.jpg`.
    Jpeg(Vec<u8>),
    /// Complete JPEG 2000 codestream - write directly to `.jp2`.
    Jpeg2000(Vec<u8>),
    /// JBIG2 page data with optional shared globals.
    Jbig2 {
        data: Vec<u8>,
        globals: Option<Vec<u8>>,
    },
    /// CCITT fax encoded data with decoding parameters.
    Ccitt(Vec<u8>),
    /// Raw decoded pixels (row-major, components interleaved).
    Pixels(Vec<u8>),
}

impl Image {
    /// Suggested file extension for this image's format.
    pub fn extension(&self) -> &'static str {
        match &self.data {
            ImageData::Jpeg(_) => "jpg",
            ImageData::Jpeg2000(_) => "jp2",
            ImageData::Jbig2 { .. } => "jb2",
            ImageData::Ccitt(_) => "tiff",
            ImageData::Pixels(_) => "ppm",
        }
    }

    /// Raw image bytes (for any variant).
    pub fn bytes(&self) -> &[u8] {
        match &self.data {
            ImageData::Jpeg(d)
            | ImageData::Jpeg2000(d)
            | ImageData::Ccitt(d)
            | ImageData::Pixels(d) => d,
            ImageData::Jbig2 { data, .. } => data,
        }
    }

    /// Whether this image can be written directly to disk as-is
    /// (JPEG and JPEG2000 passthrough).
    pub fn is_passthrough(&self) -> bool {
        matches!(&self.data, ImageData::Jpeg(_) | ImageData::Jpeg2000(_))
    }

    /// Convert from the internal PdfImage type.
    #[cfg(feature = "pdf")]
    fn from_pdf_image(img: crate::pdf::image_extract::PdfImage) -> Self {
        use crate::pdf::image_extract::{ImageData as PdfImageData, ImageEncoding};

        let data = match img.data {
            PdfImageData::Passthrough(bytes) => match img.encoding {
                ImageEncoding::Jpeg => ImageData::Jpeg(bytes),
                ImageEncoding::Jpeg2000 => ImageData::Jpeg2000(bytes),
                _ => ImageData::Pixels(bytes),
            },
            PdfImageData::Jbig2 { page_data, globals } => ImageData::Jbig2 {
                data: page_data,
                globals,
            },
            PdfImageData::Ccitt { data, .. } => ImageData::Ccitt(data),
            PdfImageData::Pixels(bytes) => ImageData::Pixels(bytes),
            PdfImageData::Empty => ImageData::Pixels(Vec::new()),
        };

        Self {
            page: img.page,
            width: img.width,
            height: img.height,
            bpc: img.bpc,
            components: img.components,
            data,
        }
    }
}

#[cfg(all(test, feature = "pdf"))]
mod tests {
    use super::format_pdf_date;

    #[test]
    fn pdf_date_whole_hour_offset() {
        assert_eq!(
            format_pdf_date("D:20240422220146+00'00'"),
            "2024-04-22 22:01:46 +00:00"
        );
        assert_eq!(
            format_pdf_date("D:20260723150218+02'00'"),
            "2026-07-23 15:02:18 +02:00"
        );
        assert_eq!(
            format_pdf_date("D:20240101120000-07'00'"),
            "2024-01-01 12:00:00 -07:00"
        );
    }

    #[test]
    fn pdf_date_keeps_offset_minutes() {
        assert_eq!(
            format_pdf_date("D:20240101120000+05'30'"),
            "2024-01-01 12:00:00 +05:30"
        );
        assert_eq!(
            format_pdf_date("D:20240101120000-03'30'"),
            "2024-01-01 12:00:00 -03:30"
        );
        assert_eq!(
            format_pdf_date("D:20240101120000+05'45'"),
            "2024-01-01 12:00:00 +05:45"
        );
    }

    #[test]
    fn pdf_date_utc_forms() {
        assert_eq!(
            format_pdf_date("D:20260723130158Z"),
            "2026-07-23 13:01:58 UTC"
        );
        assert_eq!(
            format_pdf_date("D:20260723130158Z00'00'"),
            "2026-07-23 13:01:58 UTC"
        );
    }

    #[test]
    fn pdf_date_optional_fields() {
        assert_eq!(format_pdf_date("D:20240615"), "2024-06-15 00:00:00");
        assert_eq!(format_pdf_date("20240615120000"), "2024-06-15 12:00:00");
        assert_eq!(
            format_pdf_date("D:20240101120000+02"),
            "2024-01-01 12:00:00 +02:00"
        );
        assert_eq!(
            format_pdf_date("D:20240101120000+02'"),
            "2024-01-01 12:00:00 +02:00"
        );
    }

    #[test]
    fn pdf_date_malformed_passes_through_or_drops_offset() {
        assert_eq!(format_pdf_date("D:2024"), "D:2024");
        assert_eq!(format_pdf_date("D:20240101120000+"), "2024-01-01 12:00:00");
        assert_eq!(format_pdf_date("D:20240101120000+x"), "2024-01-01 12:00:00");
    }
}
