use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use siftx::ImageData;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        print_usage();
        return ExitCode::from(1);
    }

    let result = match args[1].as_str() {
        "tags" => cmd_tags(&args[2..]),
        "text" => cmd_text(&args[2..]),
        "images" => cmd_images(&args[2..]),
        "info" => cmd_info(&args[2..]),
        "thumbnail" => cmd_thumbnail(&args[2..]),
        "forms" => cmd_forms(&args[2..]),
        "annots" => cmd_annots(&args[2..]),
        "struct" => cmd_struct(&args[2..]),
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        "-V" | "--version" => {
            println!("siftx {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // Default: if it looks like a file, treat as `siftx tags <file>`
        _ => {
            if Path::new(&args[1]).exists() {
                cmd_tags(&args[1..])
            } else {
                eprintln!("siftx: unknown command '{}'\n", args[1]);
                print_usage();
                Err(1)
            }
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

fn print_usage() {
    eprintln!(
        "\
siftx {} - metadata and content extraction tool

USAGE:
    siftx <COMMAND> [OPTIONS] <FILE>

COMMANDS:
    tags <file>               Extract metadata tags (like exiftool)
    text <file>               Extract text from PDF (like pdftotext)
    images <file> [outdir]    Extract images from PDF (like pdfimages)
    info <file>               Show document info (like pdfinfo)
    thumbnail <file> [out]    Extract EXIF thumbnail image
    forms <file>              Show PDF form fields
    annots <file>             Show PDF annotations
    struct <file>             Show PDF tagged structure tree

OPTIONS:
    -g, --group               Group tags by category (tags command)
    -j, --json                Output as JSON (tags command)
    --exif                    Show only EXIF tags (tags command)
    --xmp                     Show only XMP tags (tags command)
    --iptc                    Show only IPTC tags (tags command)
    -r, --raw                 Raw text extraction, no layout (text command)
    -p, --pages <range>       Page range, e.g. 1-5 (text/images/annots)
    --password <pwd>          PDF password for encrypted files
    -l, --list                List images without extracting (images command)
    -h, --help                Show this help
    -V, --version             Show version

EXAMPLES:
    siftx tags photo.jpg
    siftx tags --exif photo.jpg
    siftx text document.pdf
    siftx text --password secret encrypted.pdf
    siftx images document.pdf ./output/
    siftx info document.pdf
    siftx thumbnail photo.jpg thumb.jpg
    siftx forms fillable.pdf
    siftx annots annotated.pdf
    siftx struct tagged.pdf
    siftx photo.jpg                  # shorthand for 'siftx tags photo.jpg'",
        env!("CARGO_PKG_VERSION")
    );
}

// ---------------------------------------------------------------------------
// Helpers: open + authenticate
// ---------------------------------------------------------------------------

/// Open a file, parse it, and optionally authenticate with a password.
fn open_and_parse<'a>(
    path: &str,
    sf: &'a siftx::SiftFile,
    password: Option<&str>,
) -> Result<siftx::SiftDocument<'a>, u8> {
    let mut doc = sf.parse().map_err(|e| {
        eprintln!("siftx: error parsing {path}: {e}");
        1u8
    })?;

    if let Some(pwd) = password {
        if !doc.authenticate(pwd.as_bytes()) {
            eprintln!("siftx: incorrect password for {path}");
            return Err(1);
        }
    }

    Ok(doc)
}

// ---------------------------------------------------------------------------
// tags - metadata extraction (like exiftool)
// ---------------------------------------------------------------------------

fn cmd_tags(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut group = false;
    let mut json = false;
    let mut filter: Option<&str> = None;
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-g" | "--group" => group = true,
            "-j" | "--json" => json = true,
            "--exif" => filter = Some("EXIF"),
            "--xmp" => filter = Some("XMP"),
            "--iptc" => filter = Some("IPTC"),
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx tags: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx tags [OPTIONS] <FILE>...\n\nOptions:\n  -g, --group    Group by category\n  -j, --json     JSON output\n  --exif         EXIF tags only\n  --xmp          XMP tags only\n  --iptc         IPTC tags only\n  --password <p> PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx tags: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx tags: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;
    let mut first = true;

    if json {
        print!("[");
    }

    for path in &files {
        if multi && !json {
            if !first {
                println!();
            }
            println!("---- {path}");
        }

        match siftx::open(path) {
            Ok(sf) => match open_and_parse(path, &sf, password) {
                Ok(doc) => {
                    let tags = match filter {
                        Some("EXIF") => doc.exif_tags(),
                        Some("XMP") => doc.xmp_tags(),
                        Some("IPTC") => doc.iptc_tags(),
                        _ => doc.tags(),
                    };
                    if json {
                        print_tags_json(&tags, path, !first);
                    } else if group {
                        print_tags_grouped(&tags);
                    } else {
                        print_tags_flat(&tags);
                    }
                }
                Err(_) => {
                    if !json {
                        continue;
                    }
                }
            },
            Err(e) => {
                eprintln!("siftx: error opening {path}: {e}");
                if !json {
                    continue;
                }
            }
        }

        first = false;
    }

    if json {
        println!("]");
    }

    Ok(())
}

fn print_tags_flat(tags: &[siftx::Tag]) {
    let max_name = tags.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for tag in tags {
        println!("{:<width$} : {}", tag.name, tag.value, width = max_name);
    }
}

fn print_tags_grouped(tags: &[siftx::Tag]) {
    let mut current_group = "";
    let max_name = tags.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for tag in tags {
        if tag.group != current_group {
            if !current_group.is_empty() {
                println!();
            }
            println!("---- {} ----", tag.group);
            current_group = tag.group;
        }
        println!("{:<width$} : {}", tag.name, tag.value, width = max_name);
    }
}

fn print_tags_json(tags: &[siftx::Tag], file: &str, comma_before: bool) {
    if comma_before {
        print!(",");
    }
    print!("{{\"SourceFile\":{}", json_string(file));
    for tag in tags {
        let key = format!("{}:{}", tag.group, tag.name);
        print!(",{}:{}", json_string(&key), json_string(&tag.value));
    }
    print!("}}");
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---------------------------------------------------------------------------
// text - PDF text extraction (like pdftotext)
// ---------------------------------------------------------------------------

fn cmd_text(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut raw = false;
    let mut page_range: Option<(u32, u32)> = None;
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-r" | "--raw" => raw = true,
            "-p" | "--pages" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx text: --pages requires an argument");
                    return Err(1);
                }
                page_range = Some(parse_page_range(&args[i])?);
            }
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx text: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx text [OPTIONS] <FILE>...\n\nOptions:\n  -r, --raw           Raw text (no layout)\n  -p, --pages <N-M>   Page range (1-based)\n  --password <pwd>     PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx text: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx text: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;
        let doc = open_and_parse(path, &sf, password)?;

        let pages = if raw {
            doc.text_pages_raw()
        } else {
            doc.text_pages()
        };
        let pages = pages.map_err(|e| {
            eprintln!("siftx: error extracting text from {path}: {e}");
            1u8
        })?;

        let (start, end) = page_range.unwrap_or((0, pages.len().saturating_sub(1) as u32));
        let stdout = std::io::stdout();
        let mut out = stdout.lock();

        for (i, page_text) in pages.iter().enumerate() {
            let page_num = i as u32;
            if page_num < start || page_num > end {
                continue;
            }
            if page_num > start {
                let _ = out.write_all(b"\x0c\n");
            }
            let _ = out.write_all(page_text.as_bytes());
            if !page_text.ends_with('\n') {
                let _ = out.write_all(b"\n");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// images - PDF image extraction (like pdfimages)
// ---------------------------------------------------------------------------

fn cmd_images(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut outdir = None;
    let mut list_only = false;
    let mut page_range: Option<(u32, u32)> = None;
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-l" | "--list" => list_only = true,
            "-p" | "--pages" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx images: --pages requires an argument");
                    return Err(1);
                }
                page_range = Some(parse_page_range(&args[i])?);
            }
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx images: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx images [OPTIONS] <FILE>... [OUTDIR]\n\nOptions:\n  -l, --list           List images without extracting\n  -p, --pages <N-M>    Page range (1-based)\n  --password <pwd>      PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx images: unknown option '{arg}'");
                return Err(1);
            }
            _ => {
                files.push(&args[i]);
            }
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx images: no input files");
        return Err(1);
    }

    // Last arg could be output directory if it's a directory or doesn't look like a file
    if !list_only && files.len() >= 2 {
        let last = files.last().unwrap();
        if Path::new(last.as_str()).is_dir() || !Path::new(last.as_str()).exists() {
            // Only treat as outdir if it doesn't look like a PDF
            if !last.ends_with(".pdf") && !last.ends_with(".PDF") {
                outdir = Some(files.pop().unwrap().as_str());
            }
        }
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;
        let doc = open_and_parse(path, &sf, password)?;

        let images = doc.images().map_err(|e| {
            eprintln!("siftx: error extracting images from {path}: {e}");
            1u8
        })?;

        let images: Vec<_> = images
            .into_iter()
            .filter(|img| {
                if let Some((start, end)) = page_range {
                    img.page >= start && img.page <= end
                } else {
                    true
                }
            })
            .collect();

        if list_only {
            println!(
                "{:<6} {:<6} {:<8} {:<8} {:<5} {:<5} {:<6} {:<8}",
                "page", "num", "type", "width", "height", "color", "comp", "bpc"
            );
            println!("{}", "-".repeat(60));
            for (i, img) in images.iter().enumerate() {
                let color = match img.components {
                    1 => "gray",
                    3 => "rgb",
                    4 => "cmyk",
                    _ => "?",
                };
                println!(
                    "{:<6} {:<6} {:<8} {:<8} {:<5} {:<5} {:<6} {:<8}",
                    img.page + 1,
                    i,
                    img.extension(),
                    img.width,
                    img.height,
                    color,
                    img.components,
                    img.bpc,
                );
            }
            println!("\n{} images total", images.len());
            continue;
        }

        let dir = outdir.unwrap_or(".");
        std::fs::create_dir_all(dir).map_err(|e| {
            eprintln!("siftx: cannot create directory {dir}: {e}");
            1u8
        })?;

        let stem = Path::new(path.as_str())
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("image");

        for (i, img) in images.iter().enumerate() {
            let ext = img.extension();
            let out_path = format!("{dir}/{stem}-{i:04}.{ext}");

            if let ImageData::Pixels(pixels) = &img.data {
                write_ppm(&out_path, img.width, img.height, img.components, pixels)?;
            } else {
                std::fs::write(&out_path, img.bytes()).map_err(|e| {
                    eprintln!("siftx: error writing {out_path}: {e}");
                    1u8
                })?;
            }

            println!(
                "  page {:>3}  {:>5}x{:<5} {:>2}bpc {:>4}  -> {}",
                img.page + 1,
                img.width,
                img.height,
                img.bpc,
                ext,
                out_path,
            );
        }

        println!("\n{} images extracted to {dir}/", images.len());
    }

    Ok(())
}

fn write_ppm(path: &str, w: u32, h: u32, components: u8, pixels: &[u8]) -> Result<(), u8> {
    let mut f = std::fs::File::create(path).map_err(|e| {
        eprintln!("siftx: error creating {path}: {e}");
        1u8
    })?;

    let header = if components == 1 {
        format!("P5\n{w} {h}\n255\n")
    } else {
        format!("P6\n{w} {h}\n255\n")
    };

    f.write_all(header.as_bytes()).map_err(|e| {
        eprintln!("siftx: error writing {path}: {e}");
        1u8
    })?;
    f.write_all(pixels).map_err(|e| {
        eprintln!("siftx: error writing {path}: {e}");
        1u8
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// info - document info (like pdfinfo)
// ---------------------------------------------------------------------------

fn cmd_info(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx info: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx info [OPTIONS] <FILE>...\n\nOptions:\n  --password <pwd>  PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx info: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx info: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;

        let file_size = sf.data().len();
        let file_type = sf.file_type();

        let doc = open_and_parse(path, &sf, password)?;
        let tags = doc.tags();
        let w = 20;

        println!("{:<w$} {}", "File:", path);
        println!("{:<w$} {}", "File Size:", format_file_size(file_size));
        println!("{:<w$} {}", "File Type:", file_type_name(file_type));

        match file_type {
            Some(siftx::core::FileType::Pdf) => {
                // `info` mirrors pdfinfo, which reads the Info dictionary.
                // A PDF often states the same field in its XMP as well
                // (CreateDate and ModifyDate in particular), and the XMP
                // tags come first in the flat list carrying the value as
                // written in the XML rather than the formatted one. Prefer
                // the PDF group so every file prints the same shape, and
                // fall back to any group for fields the Info dict lacks.
                let find = |name: &str| -> Option<String> {
                    tags.iter()
                        .find(|t| t.group == "PDF" && t.name == name)
                        .or_else(|| tags.iter().find(|t| t.name == name))
                        .map(|t| t.value.clone())
                };
                let find_longest = |name: &str| -> Option<String> {
                    tags.iter()
                        .filter(|t| t.name == name)
                        .max_by_key(|t| t.value.len())
                        .map(|t| t.value.clone())
                };

                if let Some(v) = find("PDFVersion") {
                    println!("{:<w$} {v}", "PDF Version:");
                }
                if let Some(v) = find_longest("Title") {
                    println!("{:<w$} {v}", "Title:");
                }
                if let Some(v) = find("Author") {
                    println!("{:<w$} {v}", "Author:");
                }
                if let Some(v) = find_longest("Subject") {
                    println!("{:<w$} {v}", "Subject:");
                }
                if let Some(v) = find("Creator") {
                    println!("{:<w$} {v}", "Creator:");
                }
                if let Some(v) = find("Producer") {
                    println!("{:<w$} {v}", "Producer:");
                }
                if let Some(v) = find("Keywords") {
                    println!("{:<w$} {v}", "Keywords:");
                }
                if let Some(v) = find("CreateDate") {
                    println!("{:<w$} {v}", "Creation Date:");
                }
                if let Some(v) = find("ModifyDate") {
                    println!("{:<w$} {v}", "Mod Date:");
                }
                if let Some(v) = find("PageCount") {
                    println!("{:<w$} {v}", "Pages:");
                }
                if let Some(v) = find("Encryption") {
                    println!("{:<w$} Yes ({v})", "Encrypted:");
                }
                if let Some(v) = find("PageSize") {
                    println!("{:<w$} {v}", "Page Size:");
                }
                if let Some(v) = find("JavaScript") {
                    println!("{:<w$} {v}", "JavaScript:");
                }
                if let Some(v) = find("Tagged") {
                    println!("{:<w$} {v}", "Tagged:");
                }
                if let Some(v) = find("Linearized") {
                    println!("{:<w$} {v}", "Linearized:");
                }

                // Form field count
                if let Ok(Some(form)) = doc.acro_form() {
                    let count = form.total_field_count();
                    if count > 0 {
                        println!("{:<w$} {count}", "Form Fields:");
                    }
                }

                // Annotation count
                if let Ok(annots) = doc.all_annotations() {
                    if !annots.is_empty() {
                        println!("{:<w$} {}", "Annotations:", annots.len());
                    }
                }
            }
            _ => {
                let find = |name: &str| -> Option<String> {
                    tags.iter()
                        .find(|t| t.name == name)
                        .map(|t| t.value.clone())
                };

                if let Some(v) = find("ImageWidth").or_else(|| find("PixelXDimension")) {
                    if let Some(h) = find("ImageHeight").or_else(|| find("PixelYDimension")) {
                        println!("{:<w$} {v} x {h}", "Dimensions:");
                    }
                }
                if let Some(v) = find("Make") {
                    println!("{:<w$} {v}", "Camera Make:");
                }
                if let Some(v) = find("Model") {
                    println!("{:<w$} {v}", "Camera Model:");
                }
                if let Some(v) = find("DateTimeOriginal") {
                    println!("{:<w$} {v}", "Date Taken:");
                }
                if let Some(v) = find("ExposureTime") {
                    println!("{:<w$} {v}", "Exposure:");
                }
                if let Some(v) = find("FNumber") {
                    println!("{:<w$} f/{v}", "Aperture:");
                }
                if let Some(v) = find("ISOSpeedRatings").or_else(|| find("ISO")) {
                    println!("{:<w$} {v}", "ISO:");
                }
                if let Some(v) = find("FocalLength") {
                    println!("{:<w$} {v}", "Focal Length:");
                }

                if let Some(gps) = doc.gps() {
                    println!("{:<w$} {:.6}, {:.6}", "GPS:", gps.latitude, gps.longitude);
                }

                // Thumbnail presence
                if doc.thumbnail().is_some() {
                    println!("{:<w$} Yes", "Thumbnail:");
                }

                // Tag count summary
                let groups: std::collections::HashMap<&str, usize> =
                    tags.iter()
                        .fold(std::collections::HashMap::new(), |mut m, t| {
                            *m.entry(t.group).or_insert(0) += 1;
                            m
                        });
                let mut group_list: Vec<_> = groups.into_iter().collect();
                group_list.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
                let summary: Vec<String> = group_list
                    .iter()
                    .map(|(g, c)| format!("{g}: {c}"))
                    .collect();
                if !summary.is_empty() {
                    println!(
                        "{:<w$} {} ({})",
                        "Metadata Tags:",
                        tags.len(),
                        summary.join(", "),
                    );
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// thumbnail - extract EXIF thumbnail
// ---------------------------------------------------------------------------

fn cmd_thumbnail(args: &[String]) -> Result<(), u8> {
    let mut file = None;
    let mut output = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx thumbnail <FILE> [OUTPUT]\n\nExtracts the EXIF thumbnail image (IFD1 JPEG).\nIf OUTPUT is omitted, writes to <stem>_thumb.jpg."
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx thumbnail: unknown option '{arg}'");
                return Err(1);
            }
            _ => {
                if file.is_none() {
                    file = Some(&args[i]);
                } else if output.is_none() {
                    output = Some(&args[i]);
                } else {
                    eprintln!("siftx thumbnail: unexpected argument '{}'", args[i]);
                    return Err(1);
                }
            }
        }
        i += 1;
    }

    let path = match file {
        Some(f) => f,
        None => {
            eprintln!("siftx thumbnail: no input file");
            return Err(1);
        }
    };

    let sf = siftx::open(path).map_err(|e| {
        eprintln!("siftx: error opening {path}: {e}");
        1u8
    })?;
    let doc = sf.parse().map_err(|e| {
        eprintln!("siftx: error parsing {path}: {e}");
        1u8
    })?;

    let thumb_data = match doc.thumbnail() {
        Some(data) => data,
        None => {
            eprintln!("siftx: no thumbnail found in {path}");
            return Err(1);
        }
    };

    let out_path = match output {
        Some(o) => o.to_string(),
        None => {
            let stem = Path::new(path.as_str())
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("thumb");
            format!("{stem}_thumb.jpg")
        }
    };

    std::fs::write(&out_path, &thumb_data).map_err(|e| {
        eprintln!("siftx: error writing {out_path}: {e}");
        1u8
    })?;

    println!("Thumbnail ({} bytes) -> {out_path}", thumb_data.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// forms - PDF form fields
// ---------------------------------------------------------------------------

fn cmd_forms(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx forms: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx forms [OPTIONS] <FILE>...\n\nOptions:\n  --password <pwd>  PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx forms: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx forms: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;
        let doc = open_and_parse(path, &sf, password)?;

        let form = doc.acro_form().map_err(|e| {
            eprintln!("siftx: error reading forms from {path}: {e}");
            1u8
        })?;

        match form {
            Some(form) => {
                let total = form.total_field_count();
                println!("{total} form field{}", if total == 1 { "" } else { "s" });
                if form.sig_flags != 0 {
                    println!("Signature flags: 0x{:x}", form.sig_flags);
                }
                println!();
                for field in &form.fields {
                    print_form_field(field, 0);
                }
            }
            None => {
                println!("No form fields in {path}");
            }
        }
    }

    Ok(())
}

fn print_form_field(field: &siftx::FormField, depth: usize) {
    let indent = "  ".repeat(depth);
    let ft = field.field_type.name();
    let name = &field.full_name;

    let mut flags_str = String::new();
    if field.is_read_only() {
        flags_str.push_str(" [read-only]");
    }
    if field.is_required() {
        flags_str.push_str(" [required]");
    }

    if let Some(val) = &field.value {
        println!("{indent}{ft}: {name} = {val}{flags_str}");
    } else {
        println!("{indent}{ft}: {name}{flags_str}");
    }

    if !field.options.is_empty() {
        println!("{indent}  options: {}", field.options.join(", "));
    }

    if let Some(dv) = &field.default_value {
        println!("{indent}  default: {dv}");
    }

    if let Some(rect) = &field.rect {
        println!(
            "{indent}  rect: [{:.1}, {:.1}, {:.1}, {:.1}]",
            rect[0], rect[1], rect[2], rect[3]
        );
    }

    for child in &field.children {
        print_form_field(child, depth + 1);
    }
}

// ---------------------------------------------------------------------------
// annots - PDF annotations
// ---------------------------------------------------------------------------

fn cmd_annots(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut page_range: Option<(u32, u32)> = None;
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--pages" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx annots: --pages requires an argument");
                    return Err(1);
                }
                page_range = Some(parse_page_range(&args[i])?);
            }
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx annots: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx annots [OPTIONS] <FILE>...\n\nOptions:\n  -p, --pages <N-M>   Page range (1-based)\n  --password <pwd>     PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx annots: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx annots: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;
        let doc = open_and_parse(path, &sf, password)?;

        let annots = doc.all_annotations().map_err(|e| {
            eprintln!("siftx: error reading annotations from {path}: {e}");
            1u8
        })?;

        // Filter by page range
        let annots: Vec<_> = annots
            .into_iter()
            .filter(|a| {
                if let Some((start, end)) = page_range {
                    let page = a.page_index as u32;
                    page >= start && page <= end
                } else {
                    true
                }
            })
            .collect();

        if annots.is_empty() {
            println!("No annotations in {path}");
            continue;
        }

        println!(
            "{} annotation{}\n",
            annots.len(),
            if annots.len() == 1 { "" } else { "s" }
        );

        let w = 16;
        for (i, ann) in annots.iter().enumerate() {
            println!("--- Annotation {} ---", i + 1);
            println!("{:<w$} {}", "Type:", ann.annot_type.name());
            println!("{:<w$} {}", "Page:", ann.page_index + 1);
            println!(
                "{:<w$} [{:.1}, {:.1}, {:.1}, {:.1}]",
                "Rect:", ann.rect[0], ann.rect[1], ann.rect[2], ann.rect[3]
            );
            if let Some(contents) = &ann.contents {
                if !contents.is_empty() {
                    println!("{:<w$} {contents}", "Contents:");
                }
            }
            if let Some(dest) = &ann.dest {
                println!("{:<w$} {dest}", "Destination:");
            }
            if let Some(name) = &ann.name {
                println!("{:<w$} {name}", "Name:");
            }
            if let Some(modified) = &ann.modified {
                println!("{:<w$} {modified}", "Modified:");
            }
            if ann.flags != 0 {
                println!("{:<w$} 0x{:x}", "Flags:", ann.flags);
            }
            if let Some(color) = &ann.color {
                let parts: Vec<String> = color.iter().map(|c| format!("{c:.3}")).collect();
                println!("{:<w$} [{}]", "Color:", parts.join(", "));
            }
            if ann.has_appearance {
                println!("{:<w$} Yes", "Appearance:");
            }
            println!();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// struct - PDF tagged structure tree
// ---------------------------------------------------------------------------

fn cmd_struct(args: &[String]) -> Result<(), u8> {
    let mut files = Vec::new();
    let mut password: Option<&str> = None;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--password" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("siftx struct: --password requires an argument");
                    return Err(1);
                }
                password = Some(&args[i]);
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: siftx struct [OPTIONS] <FILE>...\n\nOptions:\n  --password <pwd>  PDF password"
                );
                return Ok(());
            }
            arg if arg.starts_with('-') => {
                eprintln!("siftx struct: unknown option '{arg}'");
                return Err(1);
            }
            _ => files.push(&args[i]),
        }
        i += 1;
    }

    if files.is_empty() {
        eprintln!("siftx struct: no input files");
        return Err(1);
    }

    let multi = files.len() > 1;

    for (fi, path) in files.iter().enumerate() {
        if multi {
            if fi > 0 {
                println!();
            }
            println!("---- {path}");
        }

        let sf = siftx::open(path).map_err(|e| {
            eprintln!("siftx: error opening {path}: {e}");
            1u8
        })?;
        let doc = open_and_parse(path, &sf, password)?;

        let tree = doc.struct_tree().map_err(|e| {
            eprintln!("siftx: error reading structure tree from {path}: {e}");
            1u8
        })?;

        match tree {
            Some(tree) => {
                if !tree.role_map.is_empty() {
                    println!("Role map:");
                    for (custom, standard) in &tree.role_map {
                        println!("  {custom} -> {standard}");
                    }
                    println!();
                }

                println!("Structure tree:");
                for elem in &tree.root_elements {
                    print_struct_element(elem, 0);
                }
            }
            None => {
                println!("Not a tagged PDF: {path}");
            }
        }
    }

    Ok(())
}

fn print_struct_element(elem: &siftx::StructElement, depth: usize) {
    let indent = "  ".repeat(depth);
    let mut attrs = Vec::new();
    if let Some(title) = &elem.title {
        attrs.push(format!("title={title:?}"));
    }
    if let Some(lang) = &elem.lang {
        attrs.push(format!("lang={lang}"));
    }
    if let Some(alt) = &elem.alt_text {
        attrs.push(format!("alt={alt:?}"));
    }
    if let Some(actual) = &elem.actual_text {
        attrs.push(format!("text={actual:?}"));
    }

    let attr_str = if attrs.is_empty() {
        String::new()
    } else {
        format!(" ({})", attrs.join(", "))
    };

    println!("{indent}<{}{attr_str}>", elem.struct_type);

    for child in &elem.children {
        match child {
            siftx::StructChild::Element(sub) => {
                print_struct_element(sub, depth + 1);
            }
            siftx::StructChild::ContentRef(mcid) => {
                let page = mcid
                    .page_obj_num
                    .map(|p| format!(" page_obj={p}"))
                    .unwrap_or_default();
                println!("{}  [MCID {}{}]", indent, mcid.mcid, page);
            }
            siftx::StructChild::ObjectRef(obj) => {
                println!("{}  [ObjRef {obj}]", indent);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_page_range(s: &str) -> Result<(u32, u32), u8> {
    if let Some((a, b)) = s.split_once('-') {
        let start: u32 = a.parse().map_err(|_| {
            eprintln!("siftx: invalid page range '{s}'");
            1u8
        })?;
        let end: u32 = b.parse().map_err(|_| {
            eprintln!("siftx: invalid page range '{s}'");
            1u8
        })?;
        if start == 0 || end == 0 || start > end {
            eprintln!("siftx: invalid page range '{s}' (use 1-based, e.g. 1-5)");
            return Err(1);
        }
        Ok((start - 1, end - 1))
    } else {
        let page: u32 = s.parse().map_err(|_| {
            eprintln!("siftx: invalid page number '{s}'");
            1u8
        })?;
        if page == 0 {
            eprintln!("siftx: page numbers are 1-based");
            return Err(1);
        }
        Ok((page - 1, page - 1))
    }
}

fn format_file_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn file_type_name(ft: Option<siftx::core::FileType>) -> &'static str {
    match ft {
        Some(siftx::core::FileType::Jpeg) => "JPEG",
        Some(siftx::core::FileType::Png) => "PNG",
        Some(siftx::core::FileType::Gif) => "GIF",
        Some(siftx::core::FileType::Bmp) => "BMP",
        Some(siftx::core::FileType::Tiff) => "TIFF",
        Some(siftx::core::FileType::WebP) => "WebP",
        Some(siftx::core::FileType::Heif) => "HEIF",
        Some(siftx::core::FileType::Pdf) => "PDF",
        Some(siftx::core::FileType::Icc) => "ICC Profile",
        Some(siftx::core::FileType::QuickTime) => "QuickTime/MP4",
        Some(siftx::core::FileType::Cr2) => "Canon CR2 RAW",
        Some(siftx::core::FileType::Cr3) => "Canon CR3 RAW",
        Some(siftx::core::FileType::Nef) => "Nikon NEF RAW",
        Some(siftx::core::FileType::Arw) => "Sony ARW RAW",
        Some(siftx::core::FileType::Dng) => "Adobe DNG",
        Some(siftx::core::FileType::Orf) => "Olympus ORF RAW",
        Some(siftx::core::FileType::Rw2) => "Panasonic RW2 RAW",
        Some(siftx::core::FileType::Pef) => "Pentax PEF RAW",
        Some(siftx::core::FileType::Raf) => "Fujifilm RAF RAW",
        Some(siftx::core::FileType::Srw) => "Samsung SRW RAW",
        None => "Unknown",
    }
}
