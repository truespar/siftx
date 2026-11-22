//! Integration test: `SiftDocument::page_count` - a PDF's page tree count, a TIFF's pages (its
//! reduced-resolution previews not counted), and nothing for a format without pages.
//!
//! The files are built here, so the test runs without the downloaded corpora; when
//! `testdata/poppler-test` is present, every PDF under it is also checked against its
//! `PageCount` tag.

use std::path::{Path, PathBuf};

/// One little-endian IFD entry: tag, type (3 SHORT, 4 LONG), count 1, value inline.
fn entry(out: &mut Vec<u8>, tag: u16, kind: u16, value: u32) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
}

/// A TIFF of three images in its main chain: a page (NewSubfileType 2), that page's
/// reduced-resolution preview (NewSubfileType 1), and a page with no NewSubfileType at all.
fn three_image_tiff() -> Vec<u8> {
    let mut t = b"II*\0".to_vec();
    t.extend_from_slice(&8u32.to_le_bytes());
    // the first IFD at 8: three entries, 2 + 36 + 4 = 42 bytes, the next at 50
    t.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut t, 254, 4, 2);
    entry(&mut t, 256, 3, 1700);
    entry(&mut t, 257, 3, 2200);
    t.extend_from_slice(&50u32.to_le_bytes());
    // the preview at 50, the next at 92
    t.extend_from_slice(&3u16.to_le_bytes());
    entry(&mut t, 254, 4, 1);
    entry(&mut t, 256, 3, 170);
    entry(&mut t, 257, 3, 220);
    t.extend_from_slice(&92u32.to_le_bytes());
    // a page at 92 without NewSubfileType: two entries, the last IFD
    t.extend_from_slice(&2u16.to_le_bytes());
    entry(&mut t, 256, 3, 1700);
    entry(&mut t, 257, 3, 2200);
    t.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(t.len(), 122);
    t
}

/// A PDF of `pages` empty pages with a correct cross-reference table.
fn pdf_of(pages: usize) -> Vec<u8> {
    let kids = (0..pages).map(|i| format!("{} 0 R", i + 3)).collect::<Vec<_>>().join(" ");
    let mut objects = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{kids}] /Count {pages} >>")];
    for _ in 0..pages {
        objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] >>".to_string());
    }
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    pdf
}

#[test]
fn a_tiff_counts_its_pages_not_its_previews() {
    let data = three_image_tiff();
    let doc = siftx::read(&data).expect("a TIFF");
    assert_eq!(doc.file_type(), Some(siftx::core::FileType::Tiff));
    assert_eq!(doc.page_count(), Some(2));
}

#[test]
fn a_pdf_counts_its_page_tree() {
    for pages in [1, 3, 12] {
        let data = pdf_of(pages);
        let doc = siftx::read(&data).expect("a PDF");
        assert_eq!(doc.page_count(), Some(pages as u32), "{pages} pages");
        assert_eq!(doc.text_pages().expect("its text").len(), pages, "one text per page");
    }
}

#[test]
fn a_photo_has_no_pages() {
    let data = [0xFF, 0xD8, 0xFF, 0xD9];
    let doc = siftx::read(&data).expect("a JPEG");
    assert_eq!(doc.page_count(), None);
}

/// Every file under `dir`, its subdirectories too, whose extension is `pdf`.
fn pdfs_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let path = e.path();
        if path.is_dir() {
            pdfs_under(&path, out);
        } else if path.extension().is_some_and(|x| x == "pdf") {
            out.push(path);
        }
    }
}

#[test]
fn every_poppler_pdf_agrees_with_its_page_count_tag() {
    let dir = Path::new("testdata/poppler-test");
    if !dir.exists() {
        return;
    }
    let mut files = Vec::new();
    pdfs_under(dir, &mut files);
    assert!(!files.is_empty(), "the corpus holds PDFs");
    for path in files {
        let data = std::fs::read(&path).unwrap();
        let Ok(doc) = siftx::read(&data) else { continue };
        let tag = doc.tags().into_iter().find(|t| t.group == "PDF" && t.name == "PageCount").and_then(|t| t.value.parse::<u32>().ok());
        assert_eq!(doc.page_count(), tag, "{}", path.display());
    }
}
