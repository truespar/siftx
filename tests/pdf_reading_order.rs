//! Integration test: reading order for a page whose text blocks overlap.
//!
//! Regression cover for issue #4, where `siftx text` aborted with "user-provided
//! comparison function does not correctly implement a total order" on a report
//! containing charts. The fixture is built here rather than committed: it is a
//! chart-shaped page - a tall column of axis labels, a short label high up and
//! to the right, and a two-line label bridging the two - which is the block
//! geometry that made the old reading-order comparator intransitive.

/// One chart-shaped cluster, repeated down a tall page.
///
/// Fonts differ between the parts so the layout pass keeps them as separate
/// blocks: lines only join a block when their font sizes are within 5%.
fn content_stream(clusters: usize) -> Vec<u8> {
    let mut ops = String::new();
    for i in 0..clusters {
        let off = (i * 95) as f64;
        // Two lines that together span from above the short label down into
        // the column below it.
        ops.push_str(&format!(
            "BT /F2 11 Tf 66 {} Td (Average change) Tj ET\n",
            440.0 + off
        ));
        ops.push_str(&format!(
            "BT /F2 11 Tf 66 {} Td (Average MW) Tj ET\n",
            424.0 + off
        ));
        // A short label, high and far to the right. Deliberately not "1,500":
        // that is a substring of the axis label "21,500", so a `find` for it
        // could match inside the axis column and point at the wrong block.
        ops.push_str(&format!(
            "BT /F1 9 Tf 320 {} Td (1,505) Tj ET\n",
            431.0 + off
        ));
        // A column of axis labels on the left.
        for (n, label) in ["24,500", "23,500", "22,500", "21,500"].iter().enumerate() {
            let y = 416.0 - 12.0 * n as f64 + off;
            ops.push_str(&format!("BT /F1 9 Tf 60 {y} Td ({label}) Tj ET\n"));
        }
    }
    ops.into_bytes()
}

/// Build a single-page PDF holding `clusters` copies of the pattern.
fn build_chart_pdf(clusters: usize) -> Vec<u8> {
    let stream = content_stream(clusters);
    let height = 500 + clusters * 95;

    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 {height}] \
             /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>"
        )
        .into_bytes(),
        {
            let mut obj = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            obj.extend_from_slice(&stream);
            obj.extend_from_slice(b"\nendstream");
            obj
        },
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec(),
    ];

    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        pdf.extend_from_slice(body);
        pdf.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = pdf.len();
    pdf.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        pdf.extend_from_slice(format!("{offset:010} {:05} n \n", 0).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
            objects.len() + 1,
            xref_offset
        )
        .as_bytes(),
    );

    pdf
}

#[test]
fn overlapping_blocks_extract_without_aborting() {
    let data = build_chart_pdf(8);
    let doc = siftx::read(&data).expect("fixture should parse");

    let pages = doc.text_pages().expect("layout extraction should succeed");
    assert_eq!(pages.len(), 1);
    assert!(
        !pages[0].trim().is_empty(),
        "layout text should not be empty"
    );

    // Text from each part of the cluster survives the reordering.
    for expected in ["Average change", "1,505", "24,500", "21,500"] {
        assert!(
            pages[0].contains(expected),
            "layout text is missing {expected:?}"
        );
    }
}

#[test]
fn overlapping_blocks_come_out_in_reading_order() {
    let data = build_chart_pdf(8);
    let doc = siftx::read(&data).expect("fixture should parse");
    let page = doc
        .text_pages()
        .expect("layout extraction should succeed")
        .remove(0);

    let at = |needle: &str| {
        page.find(needle)
            .unwrap_or_else(|| panic!("layout text is missing {needle:?}"))
    };

    // Presence alone would stay green through a reordering, which is the whole
    // subject of this change - so pin the order down the page. Within a
    // cluster: the two-line label, then the short label to its right, then the
    // axis column, each of which starts lower than the last.
    assert!(at("Average change") < at("Average MW"));
    assert!(at("Average MW") < at("1,505"));
    assert!(at("1,505") < at("24,500"));

    // The axis labels descend, rather than being pulled into some other order
    // by the band they share.
    assert!(at("24,500") < at("23,500"));
    assert!(at("23,500") < at("22,500"));
    assert!(at("22,500") < at("21,500"));
}

#[test]
fn overlapping_blocks_raw_extraction_still_works() {
    let data = build_chart_pdf(8);
    let doc = siftx::read(&data).expect("fixture should parse");

    let pages = doc.text_pages_raw().expect("raw extraction should succeed");
    assert_eq!(pages.len(), 1);
    assert!(!pages[0].trim().is_empty(), "raw text should not be empty");
    assert!(pages[0].contains("Average change"));
}

#[test]
fn reading_order_is_identical_across_runs() {
    let data = build_chart_pdf(8);

    let first = siftx::read(&data).unwrap().text_pages().unwrap();
    let second = siftx::read(&data).unwrap().text_pages().unwrap();

    assert_eq!(first, second, "two runs should produce identical output");
}
