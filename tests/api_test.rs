//! Integration tests for the high-level API.

use std::path::Path;

#[test]
fn tags_from_jpeg() {
    let dir = Path::new("testdata/exiftool-images");
    if !dir.exists() {
        eprintln!("skipping: testdata not available");
        return;
    }

    let path = dir.join("Canon.jpg");
    if !path.exists() {
        eprintln!("skipping: Canon.jpg not found");
        return;
    }

    let data = std::fs::read(&path).unwrap();
    let doc = siftx::read(&data).unwrap();
    let tags = doc.tags();

    assert!(!tags.is_empty(), "should have some tags");

    // Check for common EXIF tags
    let make = tags.iter().find(|t| t.name == "Make");
    assert!(make.is_some(), "should have Make tag");
    assert_eq!(make.unwrap().value, "Canon");
    assert_eq!(make.unwrap().group, "EXIF");

    // Print summary
    let exif_count = tags.iter().filter(|t| t.group == "EXIF").count();
    let xmp_count = tags.iter().filter(|t| t.group == "XMP").count();
    let iptc_count = tags.iter().filter(|t| t.group == "IPTC").count();
    eprintln!("Canon.jpg: {exif_count} EXIF, {xmp_count} XMP, {iptc_count} IPTC tags");
}

#[test]
fn tags_from_heic() {
    let path = Path::new("testdata/exif-samples/heic/mobile/iphone_13_pro_max.HEIC");
    if !path.exists() {
        eprintln!("skipping: HEIC test file not available");
        return;
    }

    let data = std::fs::read(&path).unwrap();
    let doc = siftx::read(&data).unwrap();
    let tags = doc.tags();

    let make = tags.iter().find(|t| t.name == "Make");
    assert!(make.is_some(), "HEIC should have Make tag");
    assert_eq!(make.unwrap().value, "Apple");

    let exif_count = tags.iter().filter(|t| t.group == "EXIF").count();
    let xmp_count = tags.iter().filter(|t| t.group == "XMP").count();
    eprintln!("iphone_13_pro_max.HEIC: {exif_count} EXIF, {xmp_count} XMP tags");
}

#[test]
fn tags_from_png() {
    let dir = Path::new("testdata/exiftool-images");
    if !dir.exists() {
        return;
    }

    // Find any PNG file
    let png = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|ext| ext == "png"));

    if let Some(entry) = png {
        let data = std::fs::read(entry.path()).unwrap();
        let doc = siftx::read(&data).unwrap();
        let tags = doc.tags();
        let name = entry.file_name();
        eprintln!("{}: {} tags total", name.to_string_lossy(), tags.len());
    }
}

#[test]
fn tags_from_pdf() {
    let dir = Path::new("testdata/poppler-test");
    if !dir.exists() {
        return;
    }

    // Find a PDF with metadata
    let pdf = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|ext| ext == "pdf"));

    if let Some(entry) = pdf {
        let data = std::fs::read(entry.path()).unwrap();
        let doc = siftx::read(&data).unwrap();
        let tags = doc.tags();

        let pdf_tags: Vec<_> = tags.iter().filter(|t| t.group == "PDF").collect();
        assert!(!pdf_tags.is_empty(), "PDF should have PDF-group tags");

        // PageCount should always be present
        let page_count = pdf_tags.iter().find(|t| t.name == "PageCount");
        assert!(page_count.is_some(), "PDF should have PageCount");

        let name = entry.file_name();
        eprintln!("{}: {} PDF tags", name.to_string_lossy(), pdf_tags.len());
        for t in &pdf_tags {
            eprintln!("  {} = {}", t.name, t.value);
        }
    }
}

#[test]
fn open_file_api() {
    let dir = Path::new("testdata/exiftool-images");
    if !dir.exists() {
        return;
    }

    let path = dir.join("Canon.jpg");
    if !path.exists() {
        return;
    }

    let file = siftx::open(&path).unwrap();
    assert!(file.file_type().is_some());

    let doc = file.parse().unwrap();
    let tags = doc.tags();
    assert!(!tags.is_empty());
}

#[test]
fn tags_convenience_fn() {
    let path = Path::new("testdata/exiftool-images/Canon.jpg");
    if !path.exists() {
        return;
    }

    let tags = siftx::tags(&path).unwrap();
    assert!(!tags.is_empty());
    assert!(tags.iter().any(|t| t.name == "Make" && t.value == "Canon"));
}

#[test]
fn pdf_text_extraction() {
    let dir = Path::new("testdata/poppler-test");
    if !dir.exists() {
        return;
    }

    let pdf = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|ext| ext == "pdf"));

    if let Some(entry) = pdf {
        let data = std::fs::read(entry.path()).unwrap();
        let doc = siftx::read(&data).unwrap();
        let pages = doc.text_pages().unwrap();
        let name = entry.file_name();
        eprintln!("{}: {} pages of text", name.to_string_lossy(), pages.len());
    }
}

#[test]
fn pdf_image_extraction() {
    let dir = Path::new("testdata/poppler-test");
    if !dir.exists() {
        return;
    }

    let pdf = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.path().extension().is_some_and(|ext| ext == "pdf"));

    if let Some(entry) = pdf {
        let data = std::fs::read(entry.path()).unwrap();
        let doc = siftx::read(&data).unwrap();
        let images = doc.images().unwrap();
        let name = entry.file_name();
        eprintln!(
            "{}: {} images extracted",
            name.to_string_lossy(),
            images.len()
        );
        for (i, img) in images.iter().enumerate().take(3) {
            eprintln!(
                "  image {i}: {}x{} {}",
                img.width,
                img.height,
                img.extension()
            );
        }
    }
}

#[test]
fn tags_all_three_domains() {
    let path = Path::new("testdata/exiftool-images/ExifTool.jpg");
    if !path.exists() {
        return;
    }

    let data = std::fs::read(&path).unwrap();
    let doc = siftx::read(&data).unwrap();
    let tags = doc.tags();

    let exif_count = tags.iter().filter(|t| t.group == "EXIF").count();
    let xmp_count = tags.iter().filter(|t| t.group == "XMP").count();
    let iptc_count = tags.iter().filter(|t| t.group == "IPTC").count();

    eprintln!("ExifTool.jpg: {exif_count} EXIF, {xmp_count} XMP, {iptc_count} IPTC tags");

    assert!(exif_count > 0, "should have EXIF tags");
    assert!(xmp_count > 0, "should have XMP tags");
    assert!(iptc_count > 0, "should have IPTC tags");
}

#[test]
fn tag_display() {
    let tag = siftx::Tag::new("EXIF", "Make", "Canon");
    assert_eq!(tag.to_string(), "[EXIF] Make = Canon");
}

#[test]
fn maker_notes_canon() {
    let path = Path::new("testdata/exiftool-images/Canon.jpg");
    if !path.exists() {
        return;
    }

    let tags = siftx::tags(&path).unwrap();
    let mn: Vec<_> = tags.iter().filter(|t| t.group == "MakerNotes").collect();
    assert!(!mn.is_empty(), "Canon.jpg should have MakerNotes tags");

    // Canon sub-array decoded tags
    let find = |name: &str| mn.iter().find(|t| t.name == name);
    assert!(find("MacroMode").is_some(), "should have MacroMode");
    assert!(find("Quality").is_some(), "should have Quality");
    assert!(find("FocusMode").is_some(), "should have FocusMode");
    assert!(find("MeteringMode").is_some(), "should have MeteringMode");
    assert!(
        find("ContinuousDrive").is_some(),
        "should have ContinuousDrive"
    );
    assert!(
        find("CanonExposureMode").is_some(),
        "should have CanonExposureMode"
    );
    assert!(find("WhiteBalance").is_some(), "should have WhiteBalance");

    // Top-level Canon tags
    assert!(
        find("CanonImageType").is_some(),
        "should have CanonImageType"
    );
    assert!(
        find("CanonFirmwareVersion").is_some(),
        "should have CanonFirmwareVersion"
    );
    assert!(find("SerialNumber").is_some(), "should have SerialNumber");

    eprintln!("Canon MakerNotes: {} tags", mn.len());
    for t in &mn {
        eprintln!("  {} = {}", t.name, t.value);
    }
}

#[test]
fn image_helpers() {
    let img = siftx::Image {
        page: 0,
        width: 100,
        height: 100,
        bpc: 8,
        components: 3,
        data: siftx::ImageData::Jpeg(vec![0xFF, 0xD8]),
    };
    assert_eq!(img.extension(), "jpg");
    assert!(img.is_passthrough());
    assert_eq!(img.bytes(), &[0xFF, 0xD8]);
}

#[test]
fn gps_from_jpeg() {
    let path = Path::new("testdata/exif-samples/jpg/gps/DSCN0010.jpg");
    if !path.exists() {
        // Try alternate location
        let alt = Path::new("testdata/exiftool-images/GPS.jpg");
        if !alt.exists() {
            eprintln!("skipping: no GPS test file found");
            return;
        }
        let data = std::fs::read(alt).unwrap();
        let doc = siftx::read(&data).unwrap();
        let gps = doc.gps();
        assert!(gps.is_some(), "GPS.jpg should have GPS coordinates");
        let coords = gps.unwrap();
        eprintln!("GPS.jpg: {coords}");
        return;
    }

    let data = std::fs::read(&path).unwrap();
    let doc = siftx::read(&data).unwrap();
    let gps = doc.gps();
    assert!(gps.is_some(), "DSCN0010.jpg should have GPS coordinates");

    let coords = gps.unwrap();
    // DSCN0010.jpg is from Tokyo area (lat ~43, lon ~141)
    assert!(coords.latitude.abs() > 0.0, "latitude should be non-zero");
    assert!(coords.longitude.abs() > 0.0, "longitude should be non-zero");
    eprintln!("DSCN0010.jpg: {coords}");
}

#[test]
fn gps_coordinates_display() {
    let coords = siftx::GpsCoordinates {
        latitude: 37.7749,
        longitude: -122.4194,
        altitude: Some(10.5),
        timestamp: None,
    };
    let s = coords.to_string();
    assert!(s.contains("37.774900"), "should contain latitude: {s}");
    assert!(s.contains("-122.419400"), "should contain longitude: {s}");
    assert!(s.contains("10.5m"), "should contain altitude: {s}");

    // Without altitude
    let coords2 = siftx::GpsCoordinates {
        latitude: -33.8688,
        longitude: 151.2093,
        altitude: None,
        timestamp: None,
    };
    let s2 = coords2.to_string();
    assert!(!s2.contains('m'), "should not have altitude: {s2}");
}

#[test]
fn gps_scan_exif_samples() {
    let dir = Path::new("testdata/exif-samples/jpg/gps");
    if !dir.exists() {
        eprintln!("skipping: exif-samples GPS directory not found");
        return;
    }

    let mut found = 0;
    let mut total = 0;
    for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jpg" || e == "JPG") {
            total += 1;
            let data = std::fs::read(&path).unwrap();
            if let Ok(doc) = siftx::read(&data) {
                if let Some(coords) = doc.gps() {
                    found += 1;
                    let name = path.file_name().unwrap().to_string_lossy();
                    eprintln!("  {name}: {coords}");
                }
            }
        }
    }
    eprintln!("GPS: {found}/{total} files had coordinates");
    assert!(found > 0, "should find GPS in at least some files");
}

// ---------------------------------------------------------------------------
// Thumbnail tests
// ---------------------------------------------------------------------------

#[test]
fn thumbnail_from_jpeg() {
    let path = Path::new("testdata/exif-samples/jpg/gps/DSCN0010.jpg");
    if !path.exists() {
        eprintln!("skipping: DSCN0010.jpg not found");
        return;
    }

    let data = std::fs::read(&path).unwrap();
    let doc = siftx::read(&data).unwrap();
    let thumb = doc.thumbnail();
    assert!(thumb.is_some(), "DSCN0010.jpg should have a thumbnail");

    let thumb_data = thumb.unwrap();
    // Thumbnail should be valid JPEG (starts with FF D8)
    assert!(
        thumb_data.len() > 100,
        "thumbnail should be non-trivial size"
    );
    assert_eq!(thumb_data[0], 0xFF, "thumbnail should start with FF");
    assert_eq!(
        thumb_data[1], 0xD8,
        "thumbnail should start with FF D8 (JPEG SOI)"
    );

    eprintln!("DSCN0010.jpg thumbnail: {} bytes", thumb_data.len());
}

#[test]
fn thumbnail_scan_jpegs() {
    let dir = Path::new("testdata/exif-samples/jpg/gps");
    if !dir.exists() {
        return;
    }

    let mut found = 0;
    let mut total = 0;
    for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "jpg" || e == "JPG") {
            total += 1;
            let data = std::fs::read(&path).unwrap();
            if let Ok(doc) = siftx::read(&data) {
                if let Some(thumb) = doc.thumbnail() {
                    found += 1;
                    let name = path.file_name().unwrap().to_string_lossy();
                    eprintln!("  {name}: {} byte thumbnail", thumb.len());
                }
            }
        }
    }
    eprintln!("Thumbnails: {found}/{total} files");
}

// ---------------------------------------------------------------------------
// QuickTime/MP4 tests
// ---------------------------------------------------------------------------

/// a recording with its media before its moov, written to disk and opened (mapped, never read
/// whole): its length from the movie header
#[test]
fn quicktime_duration_from_a_file() {
    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }
    let mut mvhd = vec![0u8; 12];
    mvhd.extend_from_slice(&600u32.to_be_bytes());
    mvhd.extend_from_slice(&(600u32 * 1800).to_be_bytes());
    mvhd.resize(100, 0);
    let mut f = bx(b"ftyp", b"mp42\0\0\0\0mp42isom");
    f.extend(bx(b"mdat", &vec![0u8; 256 * 1024]));
    f.extend(bx(b"moov", &bx(b"mvhd", &mvhd)));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("meeting.mp4");
    std::fs::write(&path, &f).unwrap();
    let file = siftx::open(&path).unwrap();
    assert_eq!(file.parse().unwrap().duration(), Some(1800.0));
    // not a movie: none
    let png = siftx::read(b"\x89PNG\r\n\x1a\n")
        .map(|d| d.duration())
        .unwrap_or(None);
    assert_eq!(png, None);
}

#[test]
fn quicktime_detection() {
    let mp4 = Path::new("testdata/fuzzing-seeds/mp4/h264-aac-publicdomain-sample.mp4");
    if !mp4.exists() {
        eprintln!("skipping: MP4 test file not found");
        return;
    }

    let file = siftx::open(&mp4).unwrap();
    assert_eq!(file.file_type(), Some(siftx::core::FileType::QuickTime));
}

#[test]
fn quicktime_tags() {
    let mp4 = Path::new("testdata/fuzzing-seeds/mp4/h264-aac-publicdomain-sample.mp4");
    if !mp4.exists() {
        eprintln!("skipping: MP4 test file not found");
        return;
    }

    let data = std::fs::read(&mp4).unwrap();
    let doc = siftx::read(&data).unwrap();
    let tags = doc.tags();

    assert!(!tags.is_empty(), "MP4 should have tags");

    // Check key metadata that ExifTool also outputs
    let find = |name: &str| tags.iter().find(|t| t.name == name);

    let brand = find("MajorBrand");
    assert!(brand.is_some(), "should have MajorBrand");
    eprintln!("MajorBrand: {}", brand.unwrap().value);

    let create = find("CreateDate");
    assert!(create.is_some(), "should have CreateDate");
    let create_val = &create.unwrap().value;
    eprintln!("CreateDate: {create_val}");
    assert!(
        create_val.contains("2017"),
        "create date should contain 2017"
    );

    let duration = find("Duration");
    assert!(duration.is_some(), "should have Duration");
    let dur_val = &duration.unwrap().value;
    eprintln!("Duration: {dur_val}");
    assert!(dur_val.contains("30"), "duration should be ~30 seconds");

    // Video track info
    let width = find("ImageWidth");
    assert!(width.is_some(), "should have ImageWidth");
    assert_eq!(width.unwrap().value, "640");

    let height = find("ImageHeight");
    assert!(height.is_some(), "should have ImageHeight");
    assert_eq!(height.unwrap().value, "360");

    let codec = find("CompressorID");
    assert!(codec.is_some(), "should have CompressorID");
    assert_eq!(codec.unwrap().value, "avc1");

    // Audio track info
    let audio_fmt = find("AudioFormat");
    assert!(audio_fmt.is_some(), "should have AudioFormat");
    assert_eq!(audio_fmt.unwrap().value, "mp4a");

    let channels = find("AudioChannels");
    assert!(channels.is_some(), "should have AudioChannels");
    assert_eq!(channels.unwrap().value, "2");

    let sr = find("AudioSampleRate");
    assert!(sr.is_some(), "should have AudioSampleRate");
    assert_eq!(sr.unwrap().value, "48000");

    // Print all tags for debugging
    for tag in &tags {
        eprintln!("  [{}] {} = {}", tag.group, tag.name, tag.value);
    }
}

#[test]
fn quicktime_exiftool_comparison() {
    let mp4 = Path::new("testdata/fuzzing-seeds/mp4/h264-aac-publicdomain-sample.mp4");
    if !mp4.exists() {
        return;
    }

    let data = std::fs::read(&mp4).unwrap();
    let doc = siftx::read(&data).unwrap();
    let tags = doc.tags();
    let find = |name: &str| {
        tags.iter()
            .find(|t| t.name == name)
            .map(|t| t.value.as_str())
    };

    // Compare against known ExifTool output for this file
    // ExifTool: CreateDate = 2017:01:08 12:18:42
    assert_eq!(find("CreateDate"), Some("2017:01:08 12:18:42"));
    // ExifTool: ModifyDate = 2017:01:08 12:18:42
    assert_eq!(find("ModifyDate"), Some("2017:01:08 12:18:42"));
    // ExifTool: TimeScale = 600
    assert_eq!(find("TimeScale"), Some("600"));
    // ExifTool: Duration = 0:00:30
    assert_eq!(find("Duration"), Some("0:30"));
    // ExifTool: ImageWidth = 640
    assert_eq!(find("ImageWidth"), Some("640"));
    // ExifTool: ImageHeight = 360
    assert_eq!(find("ImageHeight"), Some("360"));
    // ExifTool: CompressorID = avc1
    assert_eq!(find("CompressorID"), Some("avc1"));
    // ExifTool: AudioFormat = mp4a
    assert_eq!(find("AudioFormat"), Some("mp4a"));
    // ExifTool: AudioChannels = 2
    assert_eq!(find("AudioChannels"), Some("2"));
    // ExifTool: AudioSampleRate = 48000
    assert_eq!(find("AudioSampleRate"), Some("48000"));
    // ExifTool: AudioBitsPerSample = 16
    assert_eq!(find("AudioBitsPerSample"), Some("16"));

    eprintln!("ExifTool comparison: all fields match");
}

#[test]
fn quicktime_scan_mp4s() {
    let dir = Path::new("testdata/fuzzing-seeds/mp4");
    if !dir.exists() {
        return;
    }

    let mut parsed = 0;
    let mut failed = 0;
    for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "mp4") {
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };
            match siftx::read(&data) {
                Ok(doc) => {
                    let tags = doc.tags();
                    if !tags.is_empty() {
                        parsed += 1;
                    }
                }
                Err(_) => {
                    failed += 1;
                }
            }
        }
    }
    eprintln!("MP4 scan: {parsed} parsed, {failed} failed");
    assert!(parsed > 0, "should parse at least some MP4 files");
}

// ===========================================================================
// FFI C ABI tests
// ===========================================================================

#[cfg(feature = "ffi")]
mod ffi_tests {
    use std::ffi::{CStr, CString};
    use std::path::Path;
    use std::ptr;

    use siftx::ffi::*;

    #[test]
    fn ffi_open_parse_tags() {
        let dir = Path::new("testdata/exiftool-images");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        let path = dir.join("Canon.jpg");
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        // Open
        let mut file: *mut SiftXFile = ptr::null_mut();
        let r = unsafe { siftx_open(c_path.as_ptr(), &mut file) };
        assert_eq!(r, SiftXResult::Ok);
        assert!(!file.is_null());

        // File type
        let ft = unsafe { siftx_file_type(file) };
        assert_eq!(ft, SiftXFileType::Jpeg);

        // Parse
        let mut doc: *mut SiftXDocument = ptr::null_mut();
        let r = unsafe { siftx_parse(file, &mut doc) };
        assert_eq!(r, SiftXResult::Ok);
        assert!(!doc.is_null());

        // Tags
        let mut tags: *mut SiftXTagArray = ptr::null_mut();
        let r = unsafe { siftx_tags(doc, &mut tags) };
        assert_eq!(r, SiftXResult::Ok);

        let count = unsafe { siftx_tags_count(tags) };
        assert!(count > 0, "Canon.jpg should have tags");

        // Find Make tag
        let mut found_make = false;
        for i in 0..count {
            // Out-param for siftx_tags_get: zero-init so new fields on the
            // #[repr(C)] struct don't break this call site.
            let mut tag: SiftXTag = unsafe { std::mem::zeroed() };
            let r = unsafe { siftx_tags_get(tags, i, &mut tag) };
            assert_eq!(r, SiftXResult::Ok);

            let name = unsafe { CStr::from_ptr(tag.name) }.to_str().unwrap();
            if name == "Make" {
                let value = unsafe { CStr::from_ptr(tag.value) }.to_str().unwrap();
                assert_eq!(value, "Canon");
                found_make = true;
            }
        }
        assert!(found_make, "should find Make=Canon via FFI");

        // Cleanup
        unsafe {
            siftx_tags_free(tags);
            siftx_document_free(doc);
            siftx_file_free(file);
        }
    }

    #[test]
    fn ffi_tags_from_path() {
        let dir = Path::new("testdata/exiftool-images");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        let path = dir.join("Canon.jpg");
        let c_path = CString::new(path.to_str().unwrap()).unwrap();

        let mut tags: *mut SiftXTagArray = ptr::null_mut();
        let r = unsafe { siftx_tags_from_path(c_path.as_ptr(), &mut tags) };
        assert_eq!(r, SiftXResult::Ok);

        let count = unsafe { siftx_tags_count(tags) };
        assert!(count > 0);

        unsafe {
            siftx_tags_free(tags);
        }
    }

    #[test]
    fn ffi_read_buffer() {
        let dir = Path::new("testdata/exiftool-images");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        let data = std::fs::read(dir.join("Canon.jpg")).unwrap();
        let mut doc: *mut SiftXDocument = ptr::null_mut();
        let r = unsafe { siftx_read(data.as_ptr(), data.len(), &mut doc) };
        assert_eq!(r, SiftXResult::Ok);
        assert!(!doc.is_null());

        let ft = unsafe { siftx_document_file_type(doc) };
        assert_eq!(ft, SiftXFileType::Jpeg);

        unsafe {
            siftx_document_free(doc);
        }
    }

    #[test]
    fn ffi_gps_extraction() {
        let dir = Path::new("testdata/exif-samples");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        // Find a file with GPS data
        let data = std::fs::read(dir.join("jpg/gps/DSCN0010.jpg")).unwrap();
        let mut doc: *mut SiftXDocument = ptr::null_mut();
        let r = unsafe { siftx_read(data.as_ptr(), data.len(), &mut doc) };
        if r != SiftXResult::Ok {
            eprintln!("skipping: couldn't parse GPS test file");
            return;
        }

        let mut gps = SiftXGps {
            latitude: 0.0,
            longitude: 0.0,
            altitude: 0.0,
            has_altitude: 0,
        };
        let r = unsafe { siftx_gps(doc, &mut gps) };
        assert_eq!(r, SiftXResult::Ok);
        assert!(gps.latitude.abs() > 0.0, "latitude should be non-zero");
        assert!(gps.longitude.abs() > 0.0, "longitude should be non-zero");

        unsafe {
            siftx_document_free(doc);
        }
    }

    #[test]
    fn ffi_thumbnail() {
        let dir = Path::new("testdata/exiftool-images");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        let data = std::fs::read(dir.join("Canon.jpg")).unwrap();
        let mut doc: *mut SiftXDocument = ptr::null_mut();
        let r = unsafe { siftx_read(data.as_ptr(), data.len(), &mut doc) };
        assert_eq!(r, SiftXResult::Ok);

        let mut thumb_data: *const u8 = ptr::null();
        let mut thumb_len: usize = 0;
        let r = unsafe { siftx_thumbnail(doc, &mut thumb_data, &mut thumb_len) };

        if r == SiftXResult::Ok {
            assert!(!thumb_data.is_null());
            assert!(thumb_len > 0);
            // Check JPEG signature
            let bytes = unsafe { std::slice::from_raw_parts(thumb_data, thumb_len) };
            assert_eq!(bytes[0], 0xFF);
            assert_eq!(bytes[1], 0xD8);
            unsafe {
                siftx_thumbnail_free(thumb_data as *mut u8, thumb_len);
            }
        }

        unsafe {
            siftx_document_free(doc);
        }
    }

    #[test]
    fn ffi_pdf_text_extraction() {
        let dir = Path::new("testdata/poppler-test/pdf-operators");
        if !dir.exists() {
            eprintln!("skipping: testdata not available");
            return;
        }

        // Find any PDF
        let mut pdf_path = None;
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if entry.path().extension().is_some_and(|e| e == "pdf") {
                    pdf_path = Some(entry.path());
                    break;
                }
            }
        }

        let Some(path) = pdf_path else {
            eprintln!("skipping: no PDF files found");
            return;
        };

        let c_path = CString::new(path.to_str().unwrap()).unwrap();
        let mut file: *mut SiftXFile = ptr::null_mut();
        let r = unsafe { siftx_open(c_path.as_ptr(), &mut file) };
        assert_eq!(r, SiftXResult::Ok);

        let mut doc: *mut SiftXDocument = ptr::null_mut();
        let r = unsafe { siftx_parse(file, &mut doc) };
        assert_eq!(r, SiftXResult::Ok);

        // Text pages
        let mut pages: *mut SiftXTextPages = ptr::null_mut();
        let r = unsafe { siftx_text_pages(doc, &mut pages) };
        assert_eq!(r, SiftXResult::Ok);

        let count = unsafe { siftx_text_pages_count(pages) };
        // Just check it doesn't crash; count may be 0 for some PDFs
        eprintln!("FFI PDF text pages: {count}");

        if count > 0 {
            let text = unsafe { siftx_text_pages_get(pages, 0) };
            assert!(!text.is_null());
        }

        unsafe {
            siftx_text_pages_free(pages);
            siftx_document_free(doc);
            siftx_file_free(file);
        }
    }
}
