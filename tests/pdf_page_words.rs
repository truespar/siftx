//! Integration test: `SiftDocument::page_words` - words with their boxes.
//!
//! The fixture is a statement-shaped page built here: a row label, then
//! figures set apart in right-aligned columns the way financial tables are
//! typeset, a figure whose thousands groups are separated by space glyphs,
//! and one word drawn invisibly the way an OCR layer is.

/// One-page PDF, 600 x 400 points, Helvetica.
fn statement_pdf() -> Vec<u8> {
    let stream = b"BT /F1 10 Tf 50 350 Td (Skatteint\xe4kter) Tj ET\n\
BT /F1 10 Tf 300 350 Td (1 234 567) Tj ET\n\
BT /F1 10 Tf 420 350 Td (\x968,8) Tj ET\n\
BT /F1 10 Tf 50 330 Td (Summa) Tj ET\n\
BT 3 Tr /F1 10 Tf 50 300 Td (ocr) Tj ET\n"
        .to_vec();
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 400] \
          /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_vec(),
        {
            let mut obj = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
            obj.extend_from_slice(&stream);
            obj.extend_from_slice(b"\nendstream");
            obj
        },
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
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
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
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
fn words_come_with_boxes_from_the_top_left() {
    let data = statement_pdf();
    let doc = siftx::read(&data).expect("fixture should parse");
    let page = doc
        .page_words(0)
        .expect("extraction works")
        .expect("a PDF has pages");
    assert_eq!((page.width, page.height), (600.0, 400.0));

    let texts: Vec<&str> = page.words.iter().map(|w| w.text.as_str()).collect();
    assert_eq!(
        texts,
        ["Skatteintäkter", "1", "234", "567", "–8,8", "Summa", "ocr"]
    );

    let w = |t: &str| page.words.iter().find(|w| w.text == t).unwrap();
    // the label line's baseline is 350 up, so 50 down from the top; the box
    // reaches the font size above it
    let label = w("Skatteintäkter");
    assert!((label.x0 - 50.0).abs() < 0.01, "{label:?}");
    assert!(
        (label.y1 - 50.0).abs() < 0.01 && (label.y0 - 40.0).abs() < 0.01,
        "{label:?}"
    );
    // the space-separated groups are separate words, side by side, close
    let (a, b, c) = (w("1"), w("234"), w("567"));
    assert!((a.x0 - 300.0).abs() < 0.01);
    assert!(a.x1 < b.x0 && b.x1 < c.x0, "{a:?} {b:?} {c:?}");
    assert!(b.x0 - a.x1 < 5.0, "a space's width apart: {a:?} {b:?}");
    // the next line is further down the page
    assert!(w("Summa").y0 > label.y1);
    // only the OCR word is invisible
    assert!(w("ocr").invisible);
    assert!(
        page.words
            .iter()
            .filter(|w| w.text != "ocr")
            .all(|w| !w.invisible)
    );
}

#[test]
fn a_page_past_the_end_is_an_error_and_other_formats_have_none() {
    let data = statement_pdf();
    let doc = siftx::read(&data).expect("fixture should parse");
    assert!(doc.page_words(1).is_err());

    // a 1x1 PNG
    let png: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\rIDATx\x9cc\xf8\x0f\x00\x00\x01\x01\x00\x05\x18\xd8N\x00\x00\x00\x00IEND\xaeB`\x82";
    let doc = siftx::read(png).expect("a PNG parses");
    assert_eq!(doc.page_words(0).expect("no error"), None);
}
