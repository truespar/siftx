//! PDF font & encoding resolution (FE1-FE11).
//!
//! Maps raw character codes from content streams to Unicode text.
//! Per ISO 32000-2 §9.6-9.10, Adobe Glyph List, Adobe CMap resources.

use super::decode;
use super::document::Document;
use super::object::PdfObject;
use crate::core::Result;

// ---------------------------------------------------------------------------
// FE1: Font types and core structures
// ---------------------------------------------------------------------------

/// Font subtype from /Subtype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontSubtype {
    Type1,
    TrueType,
    Type0,
    Type3,
    CIDFontType0,
    CIDFontType2,
    MMType1,
    Unknown,
}

/// A resolved PDF font ready for text extraction.
#[derive(Debug, Clone)]
pub struct PdfFont {
    /// /BaseFont name.
    pub name: Vec<u8>,
    /// Font subtype.
    pub subtype: FontSubtype,
    /// Encoding for mapping bytes -> glyph names or Unicode.
    pub encoding: FontEncoding,
    /// ToUnicode CMap (highest priority mapping).
    pub to_unicode: Option<ToUnicodeMap>,
    /// Glyph widths for positioning.
    pub widths: FontWidths,
    /// Whether this is a 2-byte (CID) font.
    pub is_two_byte: bool,
    /// Embedded font cmap: fallback code->Unicode from TrueType cmap or CFF charset.
    pub embedded_cmap: Option<Vec<(u32, u32)>>,
    /// Font matrix x-scale: 0.001 for standard fonts, varies for Type3.
    /// Glyph widths are multiplied by this to get text-space widths.
    pub font_matrix_scale: f64,
}

pub type LoadedFont = (Vec<u8>, Option<(u32, u16)>, PdfFont);

/// FE1: Parse a font dictionary from a resolved PdfObject.
pub fn parse_font(doc: &Document, font_obj: &PdfObject) -> Result<PdfFont> {
    let subtype = font_obj
        .dict_get(b"Subtype")
        .and_then(|s| s.as_name_str())
        .map(|s| match s {
            "Type1" => FontSubtype::Type1,
            "TrueType" => FontSubtype::TrueType,
            "Type0" => FontSubtype::Type0,
            "Type3" => FontSubtype::Type3,
            "CIDFontType0" => FontSubtype::CIDFontType0,
            "CIDFontType2" => FontSubtype::CIDFontType2,
            "MMType1" => FontSubtype::MMType1,
            _ => FontSubtype::Unknown,
        })
        .unwrap_or(FontSubtype::Unknown);

    let name = font_obj
        .dict_get(b"BaseFont")
        .and_then(|n| n.as_name())
        .unwrap_or(b"")
        .to_vec();

    // FE2: ToUnicode CMap
    let to_unicode = font_obj.dict_get(b"ToUnicode").and_then(|tu| {
        let resolved = doc.resolve_obj(tu).ok()?;
        let raw = resolved.stream_data()?;
        let decoded = decode::decode_stream(&resolved, raw).ok()?;
        Some(parse_to_unicode(&decoded))
    });

    // FE3 + FE4: Encoding
    let (mut encoding, is_two_byte) = if subtype == FontSubtype::Type0 {
        // FE5 + FE10: Type0 composite font encoding
        let (enc, byte_width) = parse_type0_encoding(doc, font_obj);
        (enc, byte_width == 2)
    } else {
        (parse_simple_encoding(doc, font_obj), false)
    };

    // Detect encoding by BaseFont name when encoding is Builtin
    if matches!(encoding, FontEncoding::Builtin) {
        let name_str = std::str::from_utf8(&name).unwrap_or("");
        if name_str.contains("ZapfDingbats") {
            encoding = FontEncoding::Named(StandardEncoding::ZapfDingbats);
        } else if name_str.contains("Symbol") && !name_str.contains("SymbolMT") {
            encoding = FontEncoding::Named(StandardEncoding::Symbol);
        } else if is_standard_14_font(name_str) {
            // Standard 14 fonts default to StandardEncoding per PDF spec
            encoding = FontEncoding::Named(StandardEncoding::Standard);
        }
    }

    // FE9: Widths
    let widths = if subtype == FontSubtype::Type0 {
        // FE10: Get widths from descendant CIDFont
        parse_type0_widths(doc, font_obj)
    } else {
        parse_simple_widths(doc, font_obj, &name)
    };

    // FE12: Extract embedded font cmap as fallback when ToUnicode is absent/incomplete
    let embedded_cmap = if to_unicode.is_none() {
        extract_embedded_cmap(doc, font_obj, subtype)
    } else {
        None
    };

    // Parse FontMatrix for Type3 fonts (default is [0.001 0 0 0.001 0 0])
    let font_matrix_scale = if subtype == FontSubtype::Type3 {
        font_obj
            .dict_get(b"FontMatrix")
            .and_then(|fm| fm.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_f64())
            .unwrap_or(0.001)
    } else {
        0.001
    };

    Ok(PdfFont {
        name,
        subtype,
        encoding,
        to_unicode,
        widths,
        is_two_byte,
        embedded_cmap,
        font_matrix_scale,
    })
}

/// Load all fonts from a page's /Resources /Font dictionary.
pub fn load_page_fonts(doc: &Document, resources: Option<&PdfObject>) -> Vec<LoadedFont> {
    let mut fonts = Vec::new();

    let resources = match resources {
        Some(r) => match doc.resolve_obj(r) {
            Ok(resolved) => resolved,
            Err(_) => return fonts,
        },
        None => return fonts,
    };

    let font_dict = match resources.dict_get(b"Font") {
        Some(fd) => match doc.resolve_obj(fd) {
            Ok(resolved) => resolved,
            Err(_) => return fonts,
        },
        None => return fonts,
    };

    if let Some(entries) = font_dict.as_dict() {
        for (key, val) in entries {
            // The font object's identity, so the interpreter can key fonts
            // uniquely: two scopes may bind the same name (e.g. /F5) to
            // different font objects.
            let obj_ref = val.as_ref().map(|r| (r.num, r.generation));
            if let Ok(resolved) = doc.resolve_obj(val) {
                if let Ok(font) = parse_font(doc, &resolved) {
                    fonts.push((key.clone(), obj_ref, font));
                }
            }
        }
    }

    fonts
}

// ---------------------------------------------------------------------------
// FE2: ToUnicode CMap parsing
// ---------------------------------------------------------------------------

/// ToUnicode CMap: maps character codes to Unicode strings.
#[derive(Debug, Clone)]
pub struct ToUnicodeMap {
    /// beginbfchar entries: (src_code, unicode_string).
    singles: Vec<(u32, String)>,
    /// beginbfrange entries: (start, end, base_string).
    ranges: Vec<(u32, u32, String)>,
}

impl ToUnicodeMap {
    /// Look up a character code, returning its Unicode mapping.
    pub fn lookup(&self, code: u32) -> Option<String> {
        // Check singles first (exact match) - later entries override earlier
        for (src, dst) in self.singles.iter().rev() {
            if *src == code {
                return Some(dst.clone());
            }
        }

        // Check ranges - later entries override earlier (last match wins,
        // per PDF spec §9.10.3: overlapping CMap entries resolved by last definition)
        for (start, end, base) in self.ranges.iter().rev() {
            if code >= *start && code <= *end {
                let offset = code - *start;
                if offset == 0 {
                    return Some(base.clone());
                }
                // Increment the last character of the base string
                let mut chars: Vec<char> = base.chars().collect();
                if let Some(last) = chars.last_mut() {
                    *last = char::from_u32(*last as u32 + offset).unwrap_or(*last);
                }
                return Some(chars.into_iter().collect());
            }
        }

        None
    }
}

/// FE2: Parse a ToUnicode CMap stream.
pub fn parse_to_unicode(data: &[u8]) -> ToUnicodeMap {
    let text = String::from_utf8_lossy(data).replace('\r', "\n");
    let mut singles = Vec::new();
    let mut ranges = Vec::new();

    let mut lines = text.lines().peekable();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();

        if trimmed.ends_with("beginbfchar") {
            // Read single char mappings until endbfchar
            while let Some(mapping_line) = lines.next() {
                let ml = mapping_line.trim();
                if ml.starts_with("endbfchar") {
                    break;
                }

                // Format: <srcCode> <dstString>  (may lack whitespace)
                let parts = split_hex_tokens(ml);
                if parts.len() >= 2 {
                    if let (Some(src), Some(dst)) =
                        (parse_hex_token(parts[0]), hex_to_unicode_string(parts[1]))
                    {
                        singles.push((src, dst));
                    }
                }
            }
        } else if trimmed.ends_with("beginbfrange") {
            // Read range mappings until endbfrange
            while let Some(mapping_line) = lines.next() {
                let ml = mapping_line.trim();
                if ml.starts_with("endbfrange") {
                    break;
                }

                // Format: <start> <end> <dstStart>  (may lack whitespace)
                // Or:     <start> <end> [<dst1> <dst2> ...]  (array may span multiple lines)
                if ml.contains('[') {
                    // Array format - collect all content until ']'
                    let hex_before_bracket =
                        split_hex_tokens(ml.split_once('[').map(|(pre, _)| pre).unwrap_or(""));
                    if hex_before_bracket.len() >= 2 {
                        if let (Some(start), Some(end)) = (
                            parse_hex_token(hex_before_bracket[0]),
                            parse_hex_token(hex_before_bracket[1]),
                        ) {
                            // Gather the full array content (may span lines)
                            let mut array_content = String::new();
                            let after_bracket = ml.split_once('[').map(|(_, r)| r).unwrap_or("");
                            array_content.push_str(after_bracket);
                            while !array_content.contains(']') {
                                match lines.next() {
                                    Some(next_line) => {
                                        array_content.push(' ');
                                        array_content.push_str(next_line.trim());
                                    }
                                    None => break,
                                }
                            }
                            let array_str = array_content.split(']').next().unwrap_or("");
                            let mut code = start;
                            for hex_tok in split_hex_tokens(array_str) {
                                if code > end {
                                    break;
                                }
                                if let Some(s) = hex_to_unicode_string(hex_tok) {
                                    singles.push((code, s));
                                }
                                code += 1;
                            }
                        }
                    }
                } else {
                    let parts = split_hex_tokens(ml);
                    if parts.len() >= 3 {
                        if let (Some(start), Some(end)) =
                            (parse_hex_token(parts[0]), parse_hex_token(parts[1]))
                        {
                            if let Some(base) = hex_to_unicode_string(parts[2]) {
                                ranges.push((start, end, base));
                            }
                        }
                    }
                }
            }
        }
    }

    ToUnicodeMap { singles, ranges }
}

/// Split a CMap line into hex tokens, handling both `<01> <02> <03>` and `<01><02><03>`.
fn split_hex_tokens(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = line.trim();
    while let Some(start) = rest.find('<') {
        if let Some(end) = rest[start..].find('>') {
            tokens.push(&rest[start..start + end + 1]);
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    tokens
}

/// Parse a hex token like `<0041>` -> 0x0041.
fn parse_hex_token(s: &str) -> Option<u32> {
    let hex = s.trim_start_matches('<').trim_end_matches('>');
    u32::from_str_radix(hex, 16).ok()
}

/// Parse a hex token like `<0041>` -> Unicode string "A".
/// Handles multi-byte sequences (UTF-16BE encoded).
fn hex_to_unicode_string(s: &str) -> Option<String> {
    let hex = s.trim_start_matches('<').trim_end_matches('>');
    if hex.is_empty() {
        return Some(String::new());
    }

    let bytes = hex_str_to_bytes(hex)?;

    if bytes.len() <= 2 {
        // Single Unicode codepoint
        let val = if bytes.len() == 1 {
            bytes[0] as u32
        } else {
            ((bytes[0] as u32) << 8) | (bytes[1] as u32)
        };
        char::from_u32(val).map(|c| c.to_string())
    } else {
        // UTF-16BE sequence
        let mut result = String::new();
        let mut i = 0;
        while i + 1 < bytes.len() {
            let unit = ((bytes[i] as u16) << 8) | (bytes[i + 1] as u16);
            i += 2;
            if (0xD800..=0xDBFF).contains(&unit) && i + 1 < bytes.len() {
                // High surrogate - read low surrogate
                let low = ((bytes[i] as u16) << 8) | (bytes[i + 1] as u16);
                i += 2;
                let cp = 0x10000 + ((unit as u32 - 0xD800) << 10) + (low as u32 - 0xDC00);
                if let Some(c) = char::from_u32(cp) {
                    result.push(c);
                }
            } else if let Some(c) = char::from_u32(unit as u32) {
                result.push(c);
            }
        }
        Some(result)
    }
}

/// Decode a hex string like "0041" to bytes [0x00, 0x41].
fn hex_str_to_bytes(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&hex[i..i + 2], 16).ok()?);
    }
    Some(bytes)
}

// ---------------------------------------------------------------------------
// FE3: Standard encoding tables
// ---------------------------------------------------------------------------

/// Standard encoding names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardEncoding {
    WinAnsi,
    MacRoman,
    MacExpert,
    Standard,
    Symbol,
    ZapfDingbats,
}

/// FE3: Map a character code through a standard encoding to Unicode.
pub fn standard_decode(enc: StandardEncoding, code: u8) -> Option<char> {
    let table = match enc {
        StandardEncoding::WinAnsi => &WIN_ANSI_TABLE[..],
        StandardEncoding::MacRoman => &MAC_ROMAN_TABLE[..],
        StandardEncoding::Standard => &STANDARD_ENCODING_TABLE[..],
        StandardEncoding::ZapfDingbats => &ZAPF_DINGBATS_TABLE[..],
        StandardEncoding::Symbol => &SYMBOL_TABLE[..],
        // For MacExpert, fall through to identity
        _ => return char::from_u32(code as u32),
    };

    for &(c, ch) in table {
        if c == code {
            return Some(ch);
        }
    }

    // For WinAnsi and MacExpert: codes 0x20-0x7E are ASCII, and 0xA0-0xFF
    // map directly to their Latin-1 (Unicode) code points.  The table above
    // only contains the 0x80-0x9F entries that *differ* from Latin-1.
    if matches!(enc, StandardEncoding::WinAnsi) {
        if (code >= 0x20 && code <= 0x7E) || code >= 0xA0 {
            return char::from_u32(code as u32);
        }
        return None;
    }

    // Standard / MacRoman: ASCII range passthrough only
    // (not for ZapfDingbats/Symbol - they have unique mappings for all codes)
    if !matches!(
        enc,
        StandardEncoding::ZapfDingbats | StandardEncoding::Symbol
    ) && code >= 0x20
        && code <= 0x7E
    {
        return char::from_u32(code as u32);
    }

    None
}

/// WinAnsiEncoding - Windows code page 1252 (ISO 32000-2 §D.1).
/// Only entries that differ from Latin-1 / need explicit mapping.
static WIN_ANSI_TABLE: [(u8, char); 27] = [
    (0x80, '\u{20AC}'), // Euro sign
    (0x82, '\u{201A}'), // single low-9 quotation mark
    (0x83, '\u{0192}'), // latin small f with hook
    (0x84, '\u{201E}'), // double low-9 quotation mark
    (0x85, '\u{2026}'), // horizontal ellipsis
    (0x86, '\u{2020}'), // dagger
    (0x87, '\u{2021}'), // double dagger
    (0x88, '\u{02C6}'), // modifier letter circumflex accent
    (0x89, '\u{2030}'), // per mille sign
    (0x8A, '\u{0160}'), // latin capital S with caron
    (0x8B, '\u{2039}'), // single left-pointing angle quotation mark
    (0x8C, '\u{0152}'), // latin capital ligature OE
    (0x8E, '\u{017D}'), // latin capital Z with caron
    (0x91, '\u{2018}'), // left single quotation mark
    (0x92, '\u{2019}'), // right single quotation mark
    (0x93, '\u{201C}'), // left double quotation mark
    (0x94, '\u{201D}'), // right double quotation mark
    (0x95, '\u{2022}'), // bullet
    (0x96, '\u{2013}'), // en dash
    (0x97, '\u{2014}'), // em dash
    (0x98, '\u{02DC}'), // small tilde
    (0x99, '\u{2122}'), // trade mark sign
    (0x9A, '\u{0161}'), // latin small s with caron
    (0x9B, '\u{203A}'), // single right-pointing angle quotation mark
    (0x9C, '\u{0153}'), // latin small ligature oe
    (0x9E, '\u{017E}'), // latin small z with caron
    (0x9F, '\u{0178}'), // latin capital Y with diaeresis
];

/// MacRomanEncoding (ISO 32000-2 §D.1).
/// Entries in 0x80-0xFF that differ from Latin-1.
static MAC_ROMAN_TABLE: [(u8, char); 128] = [
    (0x80, '\u{00C4}'),
    (0x81, '\u{00C5}'),
    (0x82, '\u{00C7}'),
    (0x83, '\u{00C9}'),
    (0x84, '\u{00D1}'),
    (0x85, '\u{00D6}'),
    (0x86, '\u{00DC}'),
    (0x87, '\u{00E1}'),
    (0x88, '\u{00E0}'),
    (0x89, '\u{00E2}'),
    (0x8A, '\u{00E4}'),
    (0x8B, '\u{00E3}'),
    (0x8C, '\u{00E5}'),
    (0x8D, '\u{00E7}'),
    (0x8E, '\u{00E9}'),
    (0x8F, '\u{00E8}'),
    (0x90, '\u{00EA}'),
    (0x91, '\u{00EB}'),
    (0x92, '\u{00ED}'),
    (0x93, '\u{00EC}'),
    (0x94, '\u{00EE}'),
    (0x95, '\u{00EF}'),
    (0x96, '\u{00F1}'),
    (0x97, '\u{00F3}'),
    (0x98, '\u{00F2}'),
    (0x99, '\u{00F4}'),
    (0x9A, '\u{00F6}'),
    (0x9B, '\u{00F5}'),
    (0x9C, '\u{00FA}'),
    (0x9D, '\u{00F9}'),
    (0x9E, '\u{00FB}'),
    (0x9F, '\u{00FC}'),
    (0xA0, '\u{2020}'),
    (0xA1, '\u{00B0}'),
    (0xA2, '\u{00A2}'),
    (0xA3, '\u{00A3}'),
    (0xA4, '\u{00A7}'),
    (0xA5, '\u{2022}'),
    (0xA6, '\u{00B6}'),
    (0xA7, '\u{00DF}'),
    (0xA8, '\u{00AE}'),
    (0xA9, '\u{00A9}'),
    (0xAA, '\u{2122}'),
    (0xAB, '\u{00B4}'),
    (0xAC, '\u{00A8}'),
    (0xAD, '\u{2260}'),
    (0xAE, '\u{00C6}'),
    (0xAF, '\u{00D8}'),
    (0xB0, '\u{221E}'),
    (0xB1, '\u{00B1}'),
    (0xB2, '\u{2264}'),
    (0xB3, '\u{2265}'),
    (0xB4, '\u{00A5}'),
    (0xB5, '\u{00B5}'),
    (0xB6, '\u{2202}'),
    (0xB7, '\u{2211}'),
    (0xB8, '\u{220F}'),
    (0xB9, '\u{03C0}'),
    (0xBA, '\u{222B}'),
    (0xBB, '\u{00AA}'),
    (0xBC, '\u{00BA}'),
    (0xBD, '\u{2126}'),
    (0xBE, '\u{00E6}'),
    (0xBF, '\u{00F8}'),
    (0xC0, '\u{00BF}'),
    (0xC1, '\u{00A1}'),
    (0xC2, '\u{00AC}'),
    (0xC3, '\u{221A}'),
    (0xC4, '\u{0192}'),
    (0xC5, '\u{2248}'),
    (0xC6, '\u{2206}'),
    (0xC7, '\u{00AB}'),
    (0xC8, '\u{00BB}'),
    (0xC9, '\u{2026}'),
    (0xCA, '\u{00A0}'),
    (0xCB, '\u{00C0}'),
    (0xCC, '\u{00C3}'),
    (0xCD, '\u{00D5}'),
    (0xCE, '\u{0152}'),
    (0xCF, '\u{0153}'),
    (0xD0, '\u{2013}'),
    (0xD1, '\u{2014}'),
    (0xD2, '\u{201C}'),
    (0xD3, '\u{201D}'),
    (0xD4, '\u{2018}'),
    (0xD5, '\u{2019}'),
    (0xD6, '\u{00F7}'),
    (0xD7, '\u{25CA}'),
    (0xD8, '\u{00FF}'),
    (0xD9, '\u{0178}'),
    (0xDA, '\u{2044}'),
    (0xDB, '\u{20AC}'),
    (0xDC, '\u{2039}'),
    (0xDD, '\u{203A}'),
    (0xDE, '\u{FB01}'),
    (0xDF, '\u{FB02}'),
    (0xE0, '\u{2021}'),
    (0xE1, '\u{00B7}'),
    (0xE2, '\u{201A}'),
    (0xE3, '\u{201E}'),
    (0xE4, '\u{2030}'),
    (0xE5, '\u{00C2}'),
    (0xE6, '\u{00CA}'),
    (0xE7, '\u{00C1}'),
    (0xE8, '\u{00CB}'),
    (0xE9, '\u{00C8}'),
    (0xEA, '\u{00CD}'),
    (0xEB, '\u{00CE}'),
    (0xEC, '\u{00CF}'),
    (0xED, '\u{00CC}'),
    (0xEE, '\u{00D3}'),
    (0xEF, '\u{00D4}'),
    (0xF0, '\u{F8FF}'),
    (0xF1, '\u{00D2}'),
    (0xF2, '\u{00DA}'),
    (0xF3, '\u{00DB}'),
    (0xF4, '\u{00D9}'),
    (0xF5, '\u{0131}'),
    (0xF6, '\u{02C6}'),
    (0xF7, '\u{02DC}'),
    (0xF8, '\u{00AF}'),
    (0xF9, '\u{02D8}'),
    (0xFA, '\u{02D9}'),
    (0xFB, '\u{02DA}'),
    (0xFC, '\u{00B8}'),
    (0xFD, '\u{02DD}'),
    (0xFE, '\u{02DB}'),
    (0xFF, '\u{02C7}'),
];

/// Adobe Standard Encoding (ISO 32000-2 §D.1).
/// Only the entries that differ from ASCII/Latin-1.
static STANDARD_ENCODING_TABLE: [(u8, char); 49] = [
    (0x27, '\u{2019}'), // quoteright
    (0x60, '\u{2018}'), // quoteleft
    (0xA1, '\u{00A1}'),
    (0xA2, '\u{00A2}'),
    (0xA3, '\u{00A3}'),
    (0xA4, '\u{2044}'), // fraction
    (0xA5, '\u{00A5}'),
    (0xA6, '\u{0192}'), // florin
    (0xA7, '\u{00A7}'),
    (0xA8, '\u{00A4}'), // currency
    (0xA9, '\u{0027}'), // quotesingle
    (0xAA, '\u{201C}'), // quotedblleft
    (0xAB, '\u{00AB}'),
    (0xAC, '\u{2039}'), // guilsinglleft
    (0xAD, '\u{203A}'), // guilsinglright
    (0xAE, '\u{FB01}'), // fi
    (0xAF, '\u{FB02}'), // fl
    (0xB1, '\u{2013}'), // endash
    (0xB2, '\u{2020}'), // dagger
    (0xB3, '\u{2021}'), // daggerdbl
    (0xB4, '\u{00B7}'), // periodcentered
    (0xB6, '\u{00B6}'), // paragraph
    (0xB7, '\u{2022}'), // bullet
    (0xB8, '\u{201A}'), // quotesinglbase
    (0xB9, '\u{201E}'), // quotedblbase
    (0xBA, '\u{201D}'), // quotedblright
    (0xBB, '\u{00BB}'), // guillemotright
    (0xBC, '\u{2026}'), // ellipsis
    (0xBD, '\u{2030}'), // perthousand
    (0xC1, '\u{0060}'), // grave
    (0xC2, '\u{00B4}'), // acute
    (0xC3, '\u{02C6}'), // circumflex
    (0xC4, '\u{02DC}'), // tilde
    (0xC5, '\u{00AF}'), // macron
    (0xC6, '\u{02D8}'), // breve
    (0xC7, '\u{02D9}'), // dotaccent
    (0xC8, '\u{00A8}'), // dieresis
    (0xCA, '\u{02DA}'), // ring
    (0xCB, '\u{00B8}'), // cedilla
    (0xCD, '\u{02DD}'), // hungarumlaut
    (0xCE, '\u{02DB}'), // ogonek
    (0xCF, '\u{02C7}'), // caron
    (0xD0, '\u{2014}'), // emdash
    (0xE1, '\u{00C6}'), // AE
    (0xE3, '\u{00AA}'), // ordfeminine
    (0xE8, '\u{0141}'), // Lslash
    (0xE9, '\u{00D8}'), // Oslash
    (0xEA, '\u{0152}'), // OE
    (0xEB, '\u{00BA}'), // ordmasculine
];

/// ZapfDingbats encoding (PDF spec Table D.5).
static ZAPF_DINGBATS_TABLE: &[(u8, char)] = &[
    (0x20, ' '),
    (0x21, '\u{2701}'),
    (0x22, '\u{2702}'),
    (0x23, '\u{2703}'),
    (0x24, '\u{2704}'),
    (0x25, '\u{260E}'),
    (0x26, '\u{2706}'),
    (0x27, '\u{2707}'),
    (0x28, '\u{2708}'),
    (0x29, '\u{2709}'),
    (0x2A, '\u{261B}'),
    (0x2B, '\u{261E}'),
    (0x2C, '\u{270C}'),
    (0x2D, '\u{270D}'),
    (0x2E, '\u{270E}'),
    (0x2F, '\u{270F}'),
    (0x30, '\u{2710}'),
    (0x31, '\u{2711}'),
    (0x32, '\u{2712}'),
    (0x33, '\u{2713}'),
    (0x34, '\u{2714}'),
    (0x35, '\u{2715}'),
    (0x36, '\u{2716}'),
    (0x37, '\u{2717}'),
    (0x38, '\u{2718}'),
    (0x39, '\u{2719}'),
    (0x3A, '\u{271A}'),
    (0x3B, '\u{271B}'),
    (0x3C, '\u{271C}'),
    (0x3D, '\u{271D}'),
    (0x3E, '\u{271E}'),
    (0x3F, '\u{271F}'),
    (0x40, '\u{2720}'),
    (0x41, '\u{2721}'),
    (0x42, '\u{2722}'),
    (0x43, '\u{2723}'),
    (0x44, '\u{2724}'),
    (0x45, '\u{2725}'),
    (0x46, '\u{2726}'),
    (0x47, '\u{2727}'),
    (0x48, '\u{2605}'),
    (0x49, '\u{2729}'),
    (0x4A, '\u{272A}'),
    (0x4B, '\u{272B}'),
    (0x4C, '\u{272C}'),
    (0x4D, '\u{272D}'),
    (0x4E, '\u{272E}'),
    (0x4F, '\u{272F}'),
    (0x50, '\u{2730}'),
    (0x51, '\u{2731}'),
    (0x52, '\u{2732}'),
    (0x53, '\u{2733}'),
    (0x54, '\u{2734}'),
    (0x55, '\u{2735}'),
    (0x56, '\u{2736}'),
    (0x57, '\u{2737}'),
    (0x58, '\u{2738}'),
    (0x59, '\u{2739}'),
    (0x5A, '\u{273A}'),
    (0x5B, '\u{273B}'),
    (0x5C, '\u{273C}'),
    (0x5D, '\u{273D}'),
    (0x5E, '\u{273E}'),
    (0x5F, '\u{273F}'),
    (0x60, '\u{2740}'),
    (0x61, '\u{2741}'),
    (0x62, '\u{2742}'),
    (0x63, '\u{2743}'),
    (0x64, '\u{2744}'),
    (0x65, '\u{2745}'),
    (0x66, '\u{2746}'),
    (0x67, '\u{2747}'),
    (0x68, '\u{2748}'),
    (0x69, '\u{2749}'),
    (0x6A, '\u{274A}'),
    (0x6B, '\u{274B}'),
    (0x6C, '\u{25CF}'),
    (0x6D, '\u{274D}'),
    (0x6E, '\u{25A0}'),
    (0x6F, '\u{274F}'),
    (0x70, '\u{2750}'),
    (0x71, '\u{2751}'),
    (0x72, '\u{2752}'),
    (0x73, '\u{25B2}'),
    (0x74, '\u{25BC}'),
    (0x75, '\u{25C6}'),
    (0x76, '\u{2756}'),
    (0x77, '\u{25D7}'),
    (0x78, '\u{2758}'),
    (0x79, '\u{2759}'),
    (0x7A, '\u{275A}'),
    (0x7B, '\u{275B}'),
    (0x7C, '\u{275C}'),
    (0x7D, '\u{275D}'),
    (0x7E, '\u{275E}'),
    (0x80, '\u{2768}'),
    (0x81, '\u{2769}'),
    (0x82, '\u{276A}'),
    (0x83, '\u{276B}'),
    (0x84, '\u{276C}'),
    (0x85, '\u{276D}'),
    (0x86, '\u{276E}'),
    (0x87, '\u{276F}'),
    (0x88, '\u{2770}'),
    (0x89, '\u{2771}'),
    (0x8A, '\u{2772}'),
    (0x8B, '\u{2773}'),
    (0x8C, '\u{2774}'),
    (0x8D, '\u{2775}'),
    (0xA1, '\u{2761}'),
    (0xA2, '\u{2762}'),
    (0xA3, '\u{2763}'),
    (0xA4, '\u{2764}'),
    (0xA5, '\u{2765}'),
    (0xA6, '\u{2766}'),
    (0xA7, '\u{2767}'),
    (0xA8, '\u{2663}'),
    (0xA9, '\u{2666}'),
    (0xAA, '\u{2665}'),
    (0xAB, '\u{2660}'),
    (0xAC, '\u{2460}'),
    (0xAD, '\u{2461}'),
    (0xAE, '\u{2462}'),
    (0xAF, '\u{2463}'),
    (0xB0, '\u{2464}'),
    (0xB1, '\u{2465}'),
    (0xB2, '\u{2466}'),
    (0xB3, '\u{2467}'),
    (0xB4, '\u{2468}'),
    (0xB5, '\u{2469}'),
    (0xB6, '\u{2776}'),
    (0xB7, '\u{2777}'),
    (0xB8, '\u{2778}'),
    (0xB9, '\u{2779}'),
    (0xBA, '\u{277A}'),
    (0xBB, '\u{277B}'),
    (0xBC, '\u{277C}'),
    (0xBD, '\u{277D}'),
    (0xBE, '\u{277E}'),
    (0xBF, '\u{277F}'),
    (0xC0, '\u{2780}'),
    (0xC1, '\u{2781}'),
    (0xC2, '\u{2782}'),
    (0xC3, '\u{2783}'),
    (0xC4, '\u{2784}'),
    (0xC5, '\u{2785}'),
    (0xC6, '\u{2786}'),
    (0xC7, '\u{2787}'),
    (0xC8, '\u{2788}'),
    (0xC9, '\u{2789}'),
    (0xCA, '\u{278A}'),
    (0xCB, '\u{278B}'),
    (0xCC, '\u{278C}'),
    (0xCD, '\u{278D}'),
    (0xCE, '\u{278E}'),
    (0xCF, '\u{278F}'),
    (0xD0, '\u{2790}'),
    (0xD1, '\u{2791}'),
    (0xD2, '\u{2792}'),
    (0xD3, '\u{2793}'),
    (0xD4, '\u{2794}'),
    (0xD5, '\u{2192}'),
    (0xD6, '\u{2194}'),
    (0xD7, '\u{2195}'),
    (0xD8, '\u{2798}'),
    (0xD9, '\u{2799}'),
    (0xDA, '\u{279A}'),
    (0xDB, '\u{279B}'),
    (0xDC, '\u{279C}'),
    (0xDD, '\u{279D}'),
    (0xDE, '\u{279E}'),
    (0xDF, '\u{279F}'),
    (0xE0, '\u{27A0}'),
    (0xE1, '\u{27A1}'),
    (0xE2, '\u{27A2}'),
    (0xE3, '\u{27A3}'),
    (0xE4, '\u{27A4}'),
    (0xE5, '\u{27A5}'),
    (0xE6, '\u{27A6}'),
    (0xE7, '\u{27A7}'),
    (0xE8, '\u{27A8}'),
    (0xE9, '\u{27A9}'),
    (0xEA, '\u{27AA}'),
    (0xEB, '\u{27AB}'),
    (0xEC, '\u{27AC}'),
    (0xED, '\u{27AD}'),
    (0xEE, '\u{27AE}'),
    (0xEF, '\u{27AF}'),
    (0xF1, '\u{27B1}'),
    (0xF2, '\u{27B2}'),
    (0xF3, '\u{27B3}'),
    (0xF4, '\u{27B4}'),
    (0xF5, '\u{27B5}'),
    (0xF6, '\u{27B6}'),
    (0xF7, '\u{27B7}'),
    (0xF8, '\u{27B8}'),
    (0xF9, '\u{27B9}'),
    (0xFA, '\u{27BA}'),
    (0xFB, '\u{27BB}'),
    (0xFC, '\u{27BC}'),
    (0xFD, '\u{27BD}'),
    (0xFE, '\u{27BE}'),
];

/// Symbol encoding (PDF spec Table D.4).
static SYMBOL_TABLE: &[(u8, char)] = &[
    (0x20, ' '),
    (0x21, '!'),
    (0x22, '\u{2200}'),
    (0x23, '#'),
    (0x24, '\u{2203}'),
    (0x25, '%'),
    (0x26, '&'),
    (0x27, '\u{220B}'),
    (0x28, '('),
    (0x29, ')'),
    (0x2A, '\u{2217}'),
    (0x2B, '+'),
    (0x2C, ','),
    (0x2D, '\u{2212}'),
    (0x2E, '.'),
    (0x2F, '/'),
    (0x30, '0'),
    (0x31, '1'),
    (0x32, '2'),
    (0x33, '3'),
    (0x34, '4'),
    (0x35, '5'),
    (0x36, '6'),
    (0x37, '7'),
    (0x38, '8'),
    (0x39, '9'),
    (0x3A, ':'),
    (0x3B, ';'),
    (0x3C, '<'),
    (0x3D, '='),
    (0x3E, '>'),
    (0x3F, '?'),
    (0x40, '\u{2245}'),
    (0x41, '\u{0391}'),
    (0x42, '\u{0392}'),
    (0x43, '\u{03A7}'),
    (0x44, '\u{0394}'),
    (0x45, '\u{0395}'),
    (0x46, '\u{03A6}'),
    (0x47, '\u{0393}'),
    (0x48, '\u{0397}'),
    (0x49, '\u{0399}'),
    (0x4A, '\u{03D1}'),
    (0x4B, '\u{039A}'),
    (0x4C, '\u{039B}'),
    (0x4D, '\u{039C}'),
    (0x4E, '\u{039D}'),
    (0x4F, '\u{039F}'),
    (0x50, '\u{03A0}'),
    (0x51, '\u{0398}'),
    (0x52, '\u{03A1}'),
    (0x53, '\u{03A3}'),
    (0x54, '\u{03A4}'),
    (0x55, '\u{03A5}'),
    (0x56, '\u{03C2}'),
    (0x57, '\u{03A9}'),
    (0x58, '\u{039E}'),
    (0x59, '\u{03A8}'),
    (0x5A, '\u{0396}'),
    (0x5B, '['),
    (0x5C, '\u{2234}'),
    (0x5D, ']'),
    (0x5E, '\u{22A5}'),
    (0x5F, '_'),
    (0x60, '\u{F8E5}'),
    (0x61, '\u{03B1}'),
    (0x62, '\u{03B2}'),
    (0x63, '\u{03C7}'),
    (0x64, '\u{03B4}'),
    (0x65, '\u{03B5}'),
    (0x66, '\u{03C6}'),
    (0x67, '\u{03B3}'),
    (0x68, '\u{03B7}'),
    (0x69, '\u{03B9}'),
    (0x6A, '\u{03D5}'),
    (0x6B, '\u{03BA}'),
    (0x6C, '\u{03BB}'),
    (0x6D, '\u{03BC}'),
    (0x6E, '\u{03BD}'),
    (0x6F, '\u{03BF}'),
    (0x70, '\u{03C0}'),
    (0x71, '\u{03B8}'),
    (0x72, '\u{03C1}'),
    (0x73, '\u{03C3}'),
    (0x74, '\u{03C4}'),
    (0x75, '\u{03C5}'),
    (0x76, '\u{03D6}'),
    (0x77, '\u{03C9}'),
    (0x78, '\u{03BE}'),
    (0x79, '\u{03C8}'),
    (0x7A, '\u{03B6}'),
    (0x7B, '{'),
    (0x7C, '|'),
    (0x7D, '}'),
    (0x7E, '\u{223C}'),
    (0xA0, '\u{20AC}'),
    (0xA1, '\u{03D2}'),
    (0xA2, '\u{2032}'),
    (0xA3, '\u{2264}'),
    (0xA4, '\u{2044}'),
    (0xA5, '\u{221E}'),
    (0xA6, '\u{0192}'),
    (0xA7, '\u{2663}'),
    (0xA8, '\u{2666}'),
    (0xA9, '\u{2665}'),
    (0xAA, '\u{2660}'),
    (0xAB, '\u{2194}'),
    (0xAC, '\u{2190}'),
    (0xAD, '\u{2191}'),
    (0xAE, '\u{2192}'),
    (0xAF, '\u{2193}'),
    (0xB0, '\u{00B0}'),
    (0xB1, '\u{00B1}'),
    (0xB2, '\u{2033}'),
    (0xB3, '\u{2265}'),
    (0xB4, '\u{00D7}'),
    (0xB5, '\u{221D}'),
    (0xB6, '\u{2202}'),
    (0xB7, '\u{2022}'),
    (0xB8, '\u{00F7}'),
    (0xB9, '\u{2260}'),
    (0xBA, '\u{2261}'),
    (0xBB, '\u{2248}'),
    (0xBC, '\u{2026}'),
    (0xBD, '\u{F8E6}'),
    (0xBE, '\u{F8E7}'),
    (0xBF, '\u{21B5}'),
    (0xC0, '\u{2135}'),
    (0xC1, '\u{2111}'),
    (0xC2, '\u{211C}'),
    (0xC3, '\u{2118}'),
    (0xC4, '\u{2297}'),
    (0xC5, '\u{2295}'),
    (0xC6, '\u{2205}'),
    (0xC7, '\u{2229}'),
    (0xC8, '\u{222A}'),
    (0xC9, '\u{2283}'),
    (0xCA, '\u{2287}'),
    (0xCB, '\u{2284}'),
    (0xCC, '\u{2282}'),
    (0xCD, '\u{2286}'),
    (0xCE, '\u{2208}'),
    (0xCF, '\u{2209}'),
    (0xD0, '\u{2220}'),
    (0xD1, '\u{2207}'),
    (0xD2, '\u{00AE}'),
    (0xD3, '\u{00A9}'),
    (0xD4, '\u{2122}'),
    (0xD5, '\u{220F}'),
    (0xD6, '\u{221A}'),
    (0xD7, '\u{22C5}'),
    (0xD8, '\u{00AC}'),
    (0xD9, '\u{2227}'),
    (0xDA, '\u{2228}'),
    (0xDB, '\u{21D4}'),
    (0xDC, '\u{21D0}'),
    (0xDD, '\u{21D1}'),
    (0xDE, '\u{21D2}'),
    (0xDF, '\u{21D3}'),
    (0xE0, '\u{25CA}'),
    (0xE1, '\u{2329}'),
    (0xE2, '\u{00AE}'),
    (0xE3, '\u{00A9}'),
    (0xE4, '\u{2122}'),
    (0xE5, '\u{2211}'),
    (0xE6, '\u{239B}'),
    (0xE7, '\u{239C}'),
    (0xE8, '\u{239D}'),
    (0xE9, '\u{23A1}'),
    (0xEA, '\u{23A2}'),
    (0xEB, '\u{23A3}'),
    (0xEC, '\u{23A7}'),
    (0xED, '\u{23A8}'),
    (0xEE, '\u{23A9}'),
    (0xEF, '\u{23AA}'),
    (0xF1, '\u{232A}'),
    (0xF2, '\u{222B}'),
    (0xF3, '\u{2320}'),
    (0xF4, '\u{23AE}'),
    (0xF5, '\u{2321}'),
    (0xF6, '\u{239E}'),
    (0xF7, '\u{239F}'),
    (0xF8, '\u{23A0}'),
    (0xF9, '\u{23A4}'),
    (0xFA, '\u{23A5}'),
    (0xFB, '\u{23A6}'),
    (0xFC, '\u{23AB}'),
    (0xFD, '\u{23AC}'),
    (0xFE, '\u{23AD}'),
];

// ---------------------------------------------------------------------------
// FE4: Encoding differences
// ---------------------------------------------------------------------------

/// Font encoding - how character codes map to glyphs/Unicode.
#[derive(Debug, Clone)]
pub enum FontEncoding {
    /// FE3: Named standard encoding.
    Named(StandardEncoding),
    /// FE4: Standard encoding + differences array.
    Differences {
        base: StandardEncoding,
        /// (code, glyph_name) overrides.
        diffs: Vec<(u8, Vec<u8>)>,
    },
    /// FE5: Identity-H (2-byte horizontal CID).
    IdentityH,
    /// FE5: Identity-V (2-byte vertical CID).
    IdentityV,
    /// FE6: Type1 built-in encoding (no explicit encoding).
    Builtin,
    /// Unknown/unspecified encoding.
    None,
}

/// Check if a font name matches one of the PDF standard 14 fonts.
fn is_standard_14_font(name: &str) -> bool {
    // Strip subset prefix (e.g., "ABCDEF+Helvetica" -> "Helvetica")
    let base = if let Some(pos) = name.find('+') {
        &name[pos + 1..]
    } else {
        name
    };
    matches!(
        base,
        "Courier"
            | "Courier-Bold"
            | "Courier-Oblique"
            | "Courier-BoldOblique"
            | "Helvetica"
            | "Helvetica-Bold"
            | "Helvetica-Oblique"
            | "Helvetica-BoldOblique"
            | "Times-Roman"
            | "Times-Bold"
            | "Times-Italic"
            | "Times-BoldItalic"
    )
}

/// Parse encoding from a simple (8-bit) font dictionary.
fn parse_simple_encoding(doc: &Document, font_obj: &PdfObject) -> FontEncoding {
    let enc_val = match font_obj.dict_get(b"Encoding") {
        Some(v) => v,
        None => return FontEncoding::Builtin,
    };

    // Resolve indirect reference if needed
    let resolved;
    let enc = if enc_val.as_ref().is_some() {
        match doc.resolve_obj(enc_val) {
            Ok(r) => {
                resolved = r;
                &resolved
            }
            Err(_) => return FontEncoding::Builtin,
        }
    } else {
        enc_val
    };

    if let Some(name) = enc.as_name() {
        return match name {
            b"WinAnsiEncoding" => FontEncoding::Named(StandardEncoding::WinAnsi),
            b"MacRomanEncoding" => FontEncoding::Named(StandardEncoding::MacRoman),
            b"MacExpertEncoding" => FontEncoding::Named(StandardEncoding::MacExpert),
            b"StandardEncoding" => FontEncoding::Named(StandardEncoding::Standard),
            _ => FontEncoding::None,
        };
    }

    if enc.as_dict().is_some() {
        // FE4: Encoding dictionary with /BaseEncoding + /Differences
        let base = enc
            .dict_get(b"BaseEncoding")
            .and_then(|b: &PdfObject| b.as_name_str())
            .map(|s| match s {
                "WinAnsiEncoding" => StandardEncoding::WinAnsi,
                "MacRomanEncoding" => StandardEncoding::MacRoman,
                "MacExpertEncoding" => StandardEncoding::MacExpert,
                _ => StandardEncoding::Standard,
            })
            .unwrap_or(StandardEncoding::Standard);

        let diffs = parse_differences(enc);

        if diffs.is_empty() {
            FontEncoding::Named(base)
        } else {
            FontEncoding::Differences { base, diffs }
        }
    } else {
        FontEncoding::Builtin
    }
}

/// FE4: Parse /Differences array: [code /name1 /name2 ... code /name ...]
fn parse_differences(enc_dict: &PdfObject) -> Vec<(u8, Vec<u8>)> {
    let mut diffs = Vec::new();

    let arr = match enc_dict.dict_get(b"Differences").and_then(|d| d.as_array()) {
        Some(a) => a,
        None => return diffs,
    };

    let mut code: Option<u8> = None;

    for item in arr {
        match item {
            PdfObject::Int(n) => {
                code = Some(*n as u8);
            }
            PdfObject::Name(name) => {
                if let Some(c) = code {
                    diffs.push((c, name.clone()));
                    code = Some(c.wrapping_add(1));
                }
            }
            _ => {}
        }
    }

    diffs
}

// ---------------------------------------------------------------------------
// FE5 + FE10: Type0 (composite) font encoding
// ---------------------------------------------------------------------------

/// Parse encoding for a Type0 composite font.
/// Returns (encoding, byte_width) where byte_width is 1 or 2.
fn parse_type0_encoding(doc: &Document, font_obj: &PdfObject) -> (FontEncoding, usize) {
    let enc_val = match font_obj.dict_get(b"Encoding") {
        Some(v) => v,
        None => return (FontEncoding::IdentityH, 2),
    };

    // Resolve indirect reference if needed
    let resolved;
    let enc = if enc_val.as_ref().is_some() {
        match doc.resolve_obj(enc_val) {
            Ok(r) => {
                resolved = r;
                &resolved
            }
            Err(_) => return (FontEncoding::IdentityH, 2),
        }
    } else {
        enc_val
    };

    if let Some(name) = enc.as_name() {
        return match name {
            b"Identity-H" => (FontEncoding::IdentityH, 2),
            b"Identity-V" => (FontEncoding::IdentityV, 2),
            _ => (FontEncoding::None, 2),
        };
    }

    // Custom CMap stream - parse codespacerange to determine byte width
    if enc.stream_data().is_some() {
        let raw = enc.stream_data().unwrap();
        let decoded = decode::decode_stream(enc, raw).unwrap_or_else(|_| raw.to_vec());
        let text = String::from_utf8_lossy(&decoded).replace('\r', "\n");
        let byte_width = parse_cmap_codespace_width(&text);
        // Also check for bfchar/bfrange mappings that act as ToUnicode
        return (FontEncoding::IdentityH, byte_width);
    }

    (FontEncoding::IdentityH, 2) // Default for CID fonts
}

/// Parse a CMap stream's codespacerange to determine the byte width.
/// Returns 1 for single-byte, 2 for two-byte.
fn parse_cmap_codespace_width(text: &str) -> usize {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with("begincodespacerange") {
            // Next lines contain: <start> <end>
            // Single-byte: <00> <FF> (2 hex chars)
            // Two-byte: <0000> <FFFF> (4 hex chars)
            continue;
        }
        if trimmed.starts_with("endcodespacerange") {
            break;
        }
        // Look for hex tokens in the codespace range definition
        if let Some(start) = trimmed.find('<') {
            if let Some(end) = trimmed[start..].find('>') {
                let hex = &trimmed[start + 1..start + end];
                // 2 hex chars = 1 byte, 4 hex chars = 2 bytes
                return if hex.len() <= 2 { 1 } else { 2 };
            }
        }
    }
    2 // Default
}

/// FE10: Parse widths from Type0 font's DescendantFonts.
fn parse_type0_widths(doc: &Document, font_obj: &PdfObject) -> FontWidths {
    let descendant = font_obj
        .dict_get(b"DescendantFonts")
        .and_then(|df| doc.resolve_obj(df).ok())
        .and_then(|df| df.as_array().map(|a| a.to_vec()))
        .and_then(|arr| arr.into_iter().next())
        .and_then(|r| doc.resolve_obj(&r).ok());

    let descendant = match descendant {
        Some(d) => d,
        None => return FontWidths::Default(1000.0),
    };

    // Default width
    let dw = descendant
        .dict_get(b"DW")
        .and_then(|w| w.as_f64())
        .unwrap_or(1000.0);

    // /W array
    let w_array = descendant
        .dict_get(b"W")
        .and_then(|w| doc.resolve_obj(w).ok())
        .and_then(|w| w.as_array().map(|a| a.to_vec()));

    match w_array {
        Some(arr) => {
            let entries = parse_cid_widths(&arr);
            if entries.is_empty() {
                FontWidths::Default(dw)
            } else {
                FontWidths::Cid {
                    default: dw,
                    entries,
                }
            }
        }
        None => FontWidths::Default(dw),
    }
}

// ---------------------------------------------------------------------------
// FE8: Adobe Glyph List (AGL) - glyph name -> Unicode
// ---------------------------------------------------------------------------

/// FE8: Map a glyph name to Unicode.
///
/// Handles:
/// 1. Known AGL names (common subset)
/// 2. "uniXXXX" format
/// 3. "uXXXX" / "uXXXXX" format
pub fn glyph_name_to_unicode(name: &[u8]) -> Option<char> {
    let name_str = std::str::from_utf8(name).ok()?;

    // Try uniXXXX format (also uniXXXXXXXX for supplementary)
    if name_str.starts_with("uni") && name_str.len() >= 7 {
        let hex = if name_str.contains('.') {
            &name_str[3..name_str.find('.').unwrap()]
        } else {
            &name_str[3..]
        };
        if hex.len() == 4 || hex.len() == 8 {
            if let Ok(cp) = u32::from_str_radix(hex, 16) {
                return char::from_u32(cp);
            }
        }
    }

    // Try uXXXX or uXXXXX format
    if name_str.starts_with('u') && name_str.len() >= 5 && name_str.len() <= 6 {
        if name_str[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            let cp = u32::from_str_radix(&name_str[1..], 16).ok()?;
            return char::from_u32(cp);
        }
    }

    // Look up in AGL table
    if let Some(ch) = agl_lookup(name_str) {
        return Some(ch);
    }

    // AGL suffix stripping: "zero.os" -> "zero", "A.alt" -> "A"
    if let Some(dot_pos) = name_str.find('.') {
        let base = &name_str[..dot_pos];
        if !base.is_empty() {
            return agl_lookup(base);
        }
    }

    None
}

/// Adobe Glyph List common subset (~200 most-used entries).
fn agl_lookup(name: &str) -> Option<char> {
    // Binary search on sorted table
    AGL_TABLE
        .binary_search_by(|(n, _)| (*n).cmp(name))
        .ok()
        .map(|i| AGL_TABLE[i].1)
}

/// Adobe Glyph List - comprehensive subset sorted by name for binary search.
static AGL_TABLE: &[(&str, char)] = &[
    ("A", 'A'),
    ("AE", '\u{00C6}'),
    ("Aacute", '\u{00C1}'),
    ("Abreve", '\u{0102}'),
    ("Acircumflex", '\u{00C2}'),
    ("Adieresis", '\u{00C4}'),
    ("Agrave", '\u{00C0}'),
    ("Amacron", '\u{0100}'),
    ("Aogonek", '\u{0104}'),
    ("Aring", '\u{00C5}'),
    ("Atilde", '\u{00C3}'),
    ("B", 'B'),
    ("C", 'C'),
    ("Cacute", '\u{0106}'),
    ("Ccaron", '\u{010C}'),
    ("Ccedilla", '\u{00C7}'),
    ("D", 'D'),
    ("Dcaron", '\u{010E}'),
    ("Dcroat", '\u{0110}'),
    ("Delta", '\u{0394}'),
    ("E", 'E'),
    ("Eacute", '\u{00C9}'),
    ("Ecaron", '\u{011A}'),
    ("Ecircumflex", '\u{00CA}'),
    ("Edieresis", '\u{00CB}'),
    ("Edotaccent", '\u{0116}'),
    ("Egrave", '\u{00C8}'),
    ("Emacron", '\u{0112}'),
    ("Eogonek", '\u{0118}'),
    ("Eth", '\u{00D0}'),
    ("Euro", '\u{20AC}'),
    ("F", 'F'),
    ("G", 'G'),
    ("Gbreve", '\u{011E}'),
    ("Gcommaaccent", '\u{0122}'),
    ("H", 'H'),
    ("I", 'I'),
    ("Iacute", '\u{00CD}'),
    ("Icircumflex", '\u{00CE}'),
    ("Idieresis", '\u{00CF}'),
    ("Idotaccent", '\u{0130}'),
    ("Igrave", '\u{00CC}'),
    ("Imacron", '\u{012A}'),
    ("Iogonek", '\u{012E}'),
    ("J", 'J'),
    ("K", 'K'),
    ("Kcommaaccent", '\u{0136}'),
    ("L", 'L'),
    ("Lacute", '\u{0139}'),
    ("Lcaron", '\u{013D}'),
    ("Lcommaaccent", '\u{013B}'),
    ("Lslash", '\u{0141}'),
    ("M", 'M'),
    ("N", 'N'),
    ("Nacute", '\u{0143}'),
    ("Ncaron", '\u{0147}'),
    ("Ncommaaccent", '\u{0145}'),
    ("Ntilde", '\u{00D1}'),
    ("O", 'O'),
    ("OE", '\u{0152}'),
    ("Oacute", '\u{00D3}'),
    ("Ocircumflex", '\u{00D4}'),
    ("Odieresis", '\u{00D6}'),
    ("Ograve", '\u{00D2}'),
    ("Ohungarumlaut", '\u{0150}'),
    ("Omacron", '\u{014C}'),
    ("Oslash", '\u{00D8}'),
    ("Otilde", '\u{00D5}'),
    ("P", 'P'),
    ("Q", 'Q'),
    ("R", 'R'),
    ("Racute", '\u{0154}'),
    ("Rcaron", '\u{0158}'),
    ("Rcommaaccent", '\u{0156}'),
    ("S", 'S'),
    ("Sacute", '\u{015A}'),
    ("Scaron", '\u{0160}'),
    ("Scedilla", '\u{015E}'),
    ("Scommaaccent", '\u{0218}'),
    ("T", 'T'),
    ("Tcaron", '\u{0164}'),
    ("Tcommaaccent", '\u{0162}'),
    ("Thorn", '\u{00DE}'),
    ("U", 'U'),
    ("Uacute", '\u{00DA}'),
    ("Ucircumflex", '\u{00DB}'),
    ("Udieresis", '\u{00DC}'),
    ("Ugrave", '\u{00D9}'),
    ("Uhungarumlaut", '\u{0170}'),
    ("Umacron", '\u{016A}'),
    ("Uogonek", '\u{0172}'),
    ("Uring", '\u{016E}'),
    ("V", 'V'),
    ("W", 'W'),
    ("X", 'X'),
    ("Y", 'Y'),
    ("Yacute", '\u{00DD}'),
    ("Ydieresis", '\u{0178}'),
    ("Z", 'Z'),
    ("Zacute", '\u{0179}'),
    ("Zcaron", '\u{017D}'),
    ("Zdotaccent", '\u{017B}'),
    ("a", 'a'),
    ("aacute", '\u{00E1}'),
    ("abreve", '\u{0103}'),
    ("acircumflex", '\u{00E2}'),
    ("acute", '\u{00B4}'),
    ("adieresis", '\u{00E4}'),
    ("ae", '\u{00E6}'),
    ("agrave", '\u{00E0}'),
    ("amacron", '\u{0101}'),
    ("ampersand", '&'),
    ("aogonek", '\u{0105}'),
    ("aring", '\u{00E5}'),
    ("asciicircum", '^'),
    ("asciitilde", '~'),
    ("asterisk", '*'),
    ("at", '@'),
    ("atilde", '\u{00E3}'),
    ("b", 'b'),
    ("backslash", '\\'),
    ("bar", '|'),
    ("braceleft", '{'),
    ("braceright", '}'),
    ("bracketleft", '['),
    ("bracketright", ']'),
    ("breve", '\u{02D8}'),
    ("brokenbar", '\u{00A6}'),
    ("bullet", '\u{2022}'),
    ("c", 'c'),
    ("cacute", '\u{0107}'),
    ("caron", '\u{02C7}'),
    ("ccaron", '\u{010D}'),
    ("ccedilla", '\u{00E7}'),
    ("cedilla", '\u{00B8}'),
    ("cent", '\u{00A2}'),
    ("circumflex", '\u{02C6}'),
    ("colon", ':'),
    ("comma", ','),
    ("commaaccent", '\u{F6C3}'),
    ("copyright", '\u{00A9}'),
    ("currency", '\u{00A4}'),
    ("d", 'd'),
    ("dagger", '\u{2020}'),
    ("daggerdbl", '\u{2021}'),
    ("dcaron", '\u{010F}'),
    ("dcroat", '\u{0111}'),
    ("degree", '\u{00B0}'),
    ("dieresis", '\u{00A8}'),
    ("divide", '\u{00F7}'),
    ("dollar", '$'),
    ("dotaccent", '\u{02D9}'),
    ("dotlessi", '\u{0131}'),
    ("e", 'e'),
    ("eacute", '\u{00E9}'),
    ("ecaron", '\u{011B}'),
    ("ecircumflex", '\u{00EA}'),
    ("edieresis", '\u{00EB}'),
    ("edotaccent", '\u{0117}'),
    ("egrave", '\u{00E8}'),
    ("eight", '8'),
    ("ellipsis", '\u{2026}'),
    ("emacron", '\u{0113}'),
    ("emdash", '\u{2014}'),
    ("endash", '\u{2013}'),
    ("eogonek", '\u{0119}'),
    ("equal", '='),
    ("eth", '\u{00F0}'),
    ("exclam", '!'),
    ("exclamdown", '\u{00A1}'),
    ("f", 'f'),
    ("ff", '\u{FB00}'),
    ("ffi", '\u{FB03}'),
    ("ffl", '\u{FB04}'),
    ("fi", '\u{FB01}'),
    ("five", '5'),
    ("fl", '\u{FB02}'),
    ("florin", '\u{0192}'),
    ("four", '4'),
    ("fraction", '\u{2044}'),
    ("g", 'g'),
    ("gbreve", '\u{011F}'),
    ("gcommaaccent", '\u{0123}'),
    ("germandbls", '\u{00DF}'),
    ("grave", '`'),
    ("greater", '>'),
    ("greaterequal", '\u{2265}'),
    ("guillemotleft", '\u{00AB}'),
    ("guillemotright", '\u{00BB}'),
    ("guilsinglleft", '\u{2039}'),
    ("guilsinglright", '\u{203A}'),
    ("h", 'h'),
    ("hungarumlaut", '\u{02DD}'),
    ("hyphen", '-'),
    ("i", 'i'),
    ("iacute", '\u{00ED}'),
    ("icircumflex", '\u{00EE}'),
    ("idieresis", '\u{00EF}'),
    ("igrave", '\u{00EC}'),
    ("imacron", '\u{012B}'),
    ("iogonek", '\u{012F}'),
    ("j", 'j'),
    ("k", 'k'),
    ("kcommaaccent", '\u{0137}'),
    ("l", 'l'),
    ("lacute", '\u{013A}'),
    ("lcaron", '\u{013E}'),
    ("lcommaaccent", '\u{013C}'),
    ("less", '<'),
    ("lessequal", '\u{2264}'),
    ("logicalnot", '\u{00AC}'),
    ("longs", '\u{017F}'),
    ("lslash", '\u{0142}'),
    ("m", 'm'),
    ("macron", '\u{00AF}'),
    ("minus", '\u{2212}'),
    ("mu", '\u{00B5}'),
    ("multiply", '\u{00D7}'),
    ("n", 'n'),
    ("nacute", '\u{0144}'),
    ("ncaron", '\u{0148}'),
    ("ncommaaccent", '\u{0146}'),
    ("nine", '9'),
    ("notequal", '\u{2260}'),
    ("ntilde", '\u{00F1}'),
    ("numbersign", '#'),
    ("o", 'o'),
    ("oacute", '\u{00F3}'),
    ("ocircumflex", '\u{00F4}'),
    ("odieresis", '\u{00F6}'),
    ("oe", '\u{0153}'),
    ("ogonek", '\u{02DB}'),
    ("ograve", '\u{00F2}'),
    ("ohungarumlaut", '\u{0151}'),
    ("omacron", '\u{014D}'),
    ("one", '1'),
    ("onehalf", '\u{00BD}'),
    ("onequarter", '\u{00BC}'),
    ("onesuperior", '\u{00B9}'),
    ("ordfeminine", '\u{00AA}'),
    ("ordmasculine", '\u{00BA}'),
    ("oslash", '\u{00F8}'),
    ("otilde", '\u{00F5}'),
    ("p", 'p'),
    ("paragraph", '\u{00B6}'),
    ("parenleft", '('),
    ("parenright", ')'),
    ("percent", '%'),
    ("period", '.'),
    ("periodcentered", '\u{00B7}'),
    ("perthousand", '\u{2030}'),
    ("plus", '+'),
    ("plusminus", '\u{00B1}'),
    ("q", 'q'),
    ("question", '?'),
    ("questiondown", '\u{00BF}'),
    ("quotedbl", '"'),
    ("quotedblbase", '\u{201E}'),
    ("quotedblleft", '\u{201C}'),
    ("quotedblright", '\u{201D}'),
    ("quoteleft", '\u{2018}'),
    ("quoteright", '\u{2019}'),
    ("quotesinglbase", '\u{201A}'),
    ("quotesingle", '\''),
    ("r", 'r'),
    ("racute", '\u{0155}'),
    ("radical", '\u{221A}'),
    ("rcaron", '\u{0159}'),
    ("rcommaaccent", '\u{0157}'),
    ("registered", '\u{00AE}'),
    ("ring", '\u{02DA}'),
    ("s", 's'),
    ("sacute", '\u{015B}'),
    ("scaron", '\u{0161}'),
    ("scedilla", '\u{015F}'),
    ("scommaaccent", '\u{0219}'),
    ("section", '\u{00A7}'),
    ("semicolon", ';'),
    ("seven", '7'),
    ("six", '6'),
    ("slash", '/'),
    ("space", ' '),
    ("sterling", '\u{00A3}'),
    ("summation", '\u{2211}'),
    ("t", 't'),
    ("tcaron", '\u{0165}'),
    ("tcommaaccent", '\u{0163}'),
    ("thorn", '\u{00FE}'),
    ("three", '3'),
    ("threequarters", '\u{00BE}'),
    ("threesuperior", '\u{00B3}'),
    ("tilde", '\u{02DC}'),
    ("trademark", '\u{2122}'),
    ("two", '2'),
    ("twosuperior", '\u{00B2}'),
    ("u", 'u'),
    ("uacute", '\u{00FA}'),
    ("ucircumflex", '\u{00FB}'),
    ("udieresis", '\u{00FC}'),
    ("ugrave", '\u{00F9}'),
    ("uhungarumlaut", '\u{0171}'),
    ("umacron", '\u{016B}'),
    ("underscore", '_'),
    ("uogonek", '\u{0173}'),
    ("uring", '\u{016F}'),
    ("v", 'v'),
    ("w", 'w'),
    ("x", 'x'),
    ("y", 'y'),
    ("yacute", '\u{00FD}'),
    ("ydieresis", '\u{00FF}'),
    ("yen", '\u{00A5}'),
    ("z", 'z'),
    ("zacute", '\u{017A}'),
    ("zcaron", '\u{017E}'),
    ("zdotaccent", '\u{017C}'),
    ("zero", '0'),
];

// ---------------------------------------------------------------------------
// FE9: Font widths
// ---------------------------------------------------------------------------

/// Font width information for glyph advance calculation.
#[derive(Debug, Clone)]
pub enum FontWidths {
    /// Simple font: /FirstChar + /Widths array.
    Simple { first_char: u32, widths: Vec<f64> },
    /// CIDFont: /DW default + /W exceptions.
    Cid {
        default: f64,
        entries: Vec<CidWidthEntry>,
    },
    /// FE11: Default width (missing font).
    Default(f64),
}

/// A CID width entry from the /W array.
#[derive(Debug, Clone)]
pub enum CidWidthEntry {
    /// Range: CIDs start..=end all have the same width.
    Range { start: u32, end: u32, width: f64 },
    /// Individual: starting CID + array of widths.
    Individual { start: u32, widths: Vec<f64> },
}

/// Parse simple font widths from /FirstChar + /Widths.
fn parse_simple_widths(doc: &Document, font_obj: &PdfObject, font_name: &[u8]) -> FontWidths {
    let first_char = font_obj
        .dict_get(b"FirstChar")
        .and_then(|fc| fc.as_int())
        .unwrap_or(0) as u32;

    // Resolve /Widths array (may be indirect ref)
    let widths: Vec<f64> = font_obj
        .dict_get(b"Widths")
        .and_then(|w| {
            let resolved = if w.as_ref().is_some() {
                doc.resolve_obj(w).ok()
            } else {
                None
            };
            let w_ref = resolved.as_ref().unwrap_or(w);
            w_ref.as_array().map(|arr| {
                arr.iter()
                    .map(|v| v.as_f64().unwrap_or(0.0))
                    .collect::<Vec<f64>>()
            })
        })
        .unwrap_or_default();

    if widths.is_empty() {
        // Try standard 14 font built-in width tables first
        let name_str = std::str::from_utf8(font_name).unwrap_or("");
        if super::font_widths::is_courier(name_str) {
            return FontWidths::Default(super::font_widths::COURIER_WIDTH as f64);
        }
        if let Some(table) = super::font_widths::standard_14_widths(name_str) {
            let widths: Vec<f64> = table.iter().map(|&w| w as f64).collect();
            return FontWidths::Simple {
                first_char: 0,
                widths,
            };
        }

        // FE11: No widths - use /MissingWidth from descriptor or default
        let fd_resolved = font_obj
            .dict_get(b"FontDescriptor")
            .and_then(|fd| doc.resolve_obj(fd).ok());
        let missing = fd_resolved
            .as_ref()
            .and_then(|fd| fd.dict_get(b"MissingWidth"))
            .and_then(|mw| mw.as_f64())
            .unwrap_or(600.0);
        FontWidths::Default(missing)
    } else {
        FontWidths::Simple { first_char, widths }
    }
}

/// Parse /W array for CIDFont widths.
fn parse_cid_widths(arr: &[PdfObject]) -> Vec<CidWidthEntry> {
    let mut entries = Vec::new();
    let mut i = 0;

    while i < arr.len() {
        let start = match arr[i].as_int() {
            Some(n) => n as u32,
            None => {
                i += 1;
                continue;
            }
        };
        i += 1;

        if i >= arr.len() {
            break;
        }

        match &arr[i] {
            // Format 1: c_first c_last width
            PdfObject::Int(_) => {
                let end = arr[i].as_int().unwrap_or(0) as u32;
                i += 1;
                if i < arr.len() {
                    let width = arr[i].as_f64().unwrap_or(1000.0);
                    i += 1;
                    entries.push(CidWidthEntry::Range { start, end, width });
                }
            }
            // Format 2: c [w1 w2 w3 ...]
            PdfObject::Array(widths) => {
                let ws: Vec<f64> = widths.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
                i += 1;
                entries.push(CidWidthEntry::Individual { start, widths: ws });
            }
            _ => {
                i += 1;
            }
        }
    }

    entries
}

/// FE9: Get the width for a character code from a font.
pub fn glyph_width(font: &PdfFont, code: u32) -> f64 {
    match &font.widths {
        FontWidths::Simple { first_char, widths } => {
            let idx = code.checked_sub(*first_char).map(|i| i as usize);
            match idx {
                Some(i) if i < widths.len() => widths[i],
                _ => 600.0, // FE11: default
            }
        }
        FontWidths::Cid { default, entries } => {
            for entry in entries {
                match entry {
                    CidWidthEntry::Range { start, end, width } => {
                        if code >= *start && code <= *end {
                            return *width;
                        }
                    }
                    CidWidthEntry::Individual { start, widths } => {
                        let idx = code.checked_sub(*start).map(|i| i as usize);
                        if let Some(i) = idx {
                            if i < widths.len() {
                                return widths[i];
                            }
                        }
                    }
                }
            }
            *default
        }
        FontWidths::Default(w) => *w,
    }
}

// ---------------------------------------------------------------------------
// FE12: Embedded font program cmap extraction
// ---------------------------------------------------------------------------

/// Extract a code->Unicode mapping from an embedded font program.
/// Tries FontFile2 (TrueType) then FontFile3 (CFF) via the FontDescriptor.
fn extract_embedded_cmap(
    doc: &Document,
    font_obj: &PdfObject,
    subtype: FontSubtype,
) -> Option<Vec<(u32, u32)>> {
    let fd = font_obj
        .dict_get(b"FontDescriptor")
        .and_then(|fd| doc.resolve_obj(fd).ok())?;

    // Try TrueType (FontFile2)
    if let Some(ff2) = fd.dict_get(b"FontFile2") {
        if let Ok(resolved) = doc.resolve_obj(ff2) {
            if let Some(raw) = resolved.stream_data() {
                let data = decode::decode_stream(&resolved, raw).ok()?;
                let mappings = parse_truetype_cmap(&data);
                if !mappings.is_empty() {
                    return Some(mappings);
                }
            }
        }
    }

    // Try CFF (FontFile3)
    if let Some(ff3) = fd.dict_get(b"FontFile3") {
        if let Ok(resolved) = doc.resolve_obj(ff3) {
            if let Some(raw) = resolved.stream_data() {
                let data = decode::decode_stream(&resolved, raw).ok()?;
                let mappings = parse_cff_charset(&data);
                if !mappings.is_empty() {
                    return Some(mappings);
                }
            }
        }
    }

    // For Type0 fonts, also check DescendantFonts' FontDescriptor
    if subtype == FontSubtype::Type0 {
        let desc_fonts = font_obj
            .dict_get(b"DescendantFonts")
            .and_then(|df| df.as_array())
            .and_then(|arr| arr.first())
            .and_then(|r| doc.resolve_obj(r).ok())?;
        let desc_fd = desc_fonts
            .dict_get(b"FontDescriptor")
            .and_then(|fd| doc.resolve_obj(fd).ok())?;

        if let Some(ff2) = desc_fd.dict_get(b"FontFile2") {
            if let Ok(resolved) = doc.resolve_obj(ff2) {
                if let Some(raw) = resolved.stream_data() {
                    let data = decode::decode_stream(&resolved, raw).ok()?;
                    let mappings = parse_truetype_cmap(&data);
                    if !mappings.is_empty() {
                        return Some(mappings);
                    }
                }
            }
        }

        if let Some(ff3) = desc_fd.dict_get(b"FontFile3") {
            if let Ok(resolved) = doc.resolve_obj(ff3) {
                if let Some(raw) = resolved.stream_data() {
                    let data = decode::decode_stream(&resolved, raw).ok()?;
                    let mappings = parse_cff_charset(&data);
                    if !mappings.is_empty() {
                        return Some(mappings);
                    }
                }
            }
        }
    }

    None
}

/// Parse TrueType `cmap` table to extract GID->Unicode mappings.
/// Supports format 4 (segment mapping) and format 12 (segmented coverage).
fn parse_truetype_cmap(data: &[u8]) -> Vec<(u32, u32)> {
    // Find the `cmap` table in the TrueType font file
    if data.len() < 12 {
        return Vec::new();
    }

    // TrueType offset table
    let num_tables = u16::from_be_bytes([data[4], data[5]]) as usize;
    let mut cmap_offset = 0usize;
    let mut cmap_length = 0usize;

    for i in 0..num_tables {
        let rec = 12 + i * 16;
        if rec + 16 > data.len() {
            break;
        }
        if &data[rec..rec + 4] == b"cmap" {
            cmap_offset =
                u32::from_be_bytes([data[rec + 8], data[rec + 9], data[rec + 10], data[rec + 11]])
                    as usize;
            cmap_length = u32::from_be_bytes([
                data[rec + 12],
                data[rec + 13],
                data[rec + 14],
                data[rec + 15],
            ]) as usize;
            break;
        }
    }

    if cmap_offset == 0 || cmap_offset >= data.len() {
        return Vec::new();
    }
    let cmap = &data[cmap_offset
        ..data
            .len()
            .min(cmap_offset + cmap_length.max(data.len() - cmap_offset))];

    if cmap.len() < 4 {
        return Vec::new();
    }
    let num_subtables = u16::from_be_bytes([cmap[2], cmap[3]]) as usize;

    // Prefer platform 3 (Windows), encoding 1 (Unicode BMP) or 10 (Unicode full)
    // Then platform 0 (Unicode), any encoding
    // Then platform 1 (Macintosh), encoding 0 (Roman)
    let mut best_offset = 0u32;
    let mut best_priority = 0u8;

    for i in 0..num_subtables {
        let rec = 4 + i * 8;
        if rec + 8 > cmap.len() {
            break;
        }
        let platform = u16::from_be_bytes([cmap[rec], cmap[rec + 1]]);
        let encoding = u16::from_be_bytes([cmap[rec + 2], cmap[rec + 3]]);
        let offset =
            u32::from_be_bytes([cmap[rec + 4], cmap[rec + 5], cmap[rec + 6], cmap[rec + 7]]);

        let priority = match (platform, encoding) {
            (3, 10) => 6, // Windows Unicode full
            (3, 1) => 5,  // Windows Unicode BMP
            (0, 4) => 4,  // Unicode full
            (0, 3) => 3,  // Unicode 2.0 BMP
            (0, _) => 2,  // Unicode other
            (1, 0) => 1,  // Mac Roman
            _ => 0,
        };

        if priority > best_priority {
            best_priority = priority;
            best_offset = offset;
        }
    }

    if best_offset == 0 || best_offset as usize >= cmap.len() {
        return Vec::new();
    }
    let subtable = &cmap[best_offset as usize..];

    if subtable.len() < 2 {
        return Vec::new();
    }
    let format = u16::from_be_bytes([subtable[0], subtable[1]]);

    match format {
        4 => parse_cmap_format4(subtable),
        12 => parse_cmap_format12(subtable),
        6 => parse_cmap_format6(subtable),
        _ => Vec::new(),
    }
}

/// Parse cmap format 4 (segment mapping to delta values).
fn parse_cmap_format4(data: &[u8]) -> Vec<(u32, u32)> {
    if data.len() < 14 {
        return Vec::new();
    }
    let seg_count = u16::from_be_bytes([data[6], data[7]]) as usize / 2;
    let header = 14;

    // Arrays: endCode, reservedPad(2), startCode, idDelta, idRangeOffset
    let end_codes = header;
    let start_codes = end_codes + seg_count * 2 + 2; // +2 for reservedPad
    let id_deltas = start_codes + seg_count * 2;
    let id_range_offsets = id_deltas + seg_count * 2;

    if id_range_offsets + seg_count * 2 > data.len() {
        return Vec::new();
    }

    let mut mappings = Vec::new();

    for i in 0..seg_count {
        let end = u16::from_be_bytes([data[end_codes + i * 2], data[end_codes + i * 2 + 1]]) as u32;
        let start =
            u16::from_be_bytes([data[start_codes + i * 2], data[start_codes + i * 2 + 1]]) as u32;
        let delta =
            i16::from_be_bytes([data[id_deltas + i * 2], data[id_deltas + i * 2 + 1]]) as i32;
        let range_off = u16::from_be_bytes([
            data[id_range_offsets + i * 2],
            data[id_range_offsets + i * 2 + 1],
        ]) as usize;

        if start == 0xFFFF {
            break;
        }

        for code in start..=end {
            let gid = if range_off == 0 {
                ((code as i32 + delta) & 0xFFFF) as u32
            } else {
                let offset = id_range_offsets + i * 2 + range_off + ((code - start) as usize) * 2;
                if offset + 1 < data.len() {
                    let gid_raw = u16::from_be_bytes([data[offset], data[offset + 1]]) as u32;
                    if gid_raw == 0 {
                        continue;
                    }
                    ((gid_raw as i32 + delta) & 0xFFFF) as u32
                } else {
                    continue;
                }
            };
            if gid != 0 {
                // In format 4, the code IS the Unicode codepoint (for platform 3/encoding 1)
                mappings.push((gid, code));
            }
        }
    }

    mappings
}

/// Parse cmap format 12 (segmented coverage - 32-bit).
fn parse_cmap_format12(data: &[u8]) -> Vec<(u32, u32)> {
    if data.len() < 16 {
        return Vec::new();
    }
    let n_groups = u32::from_be_bytes([data[12], data[13], data[14], data[15]]) as usize;
    let mut mappings = Vec::new();

    for i in 0..n_groups {
        let base = 16 + i * 12;
        if base + 12 > data.len() {
            break;
        }
        let start_code =
            u32::from_be_bytes([data[base], data[base + 1], data[base + 2], data[base + 3]]);
        let end_code = u32::from_be_bytes([
            data[base + 4],
            data[base + 5],
            data[base + 6],
            data[base + 7],
        ]);
        let start_gid = u32::from_be_bytes([
            data[base + 8],
            data[base + 9],
            data[base + 10],
            data[base + 11],
        ]);

        for offset in 0..=(end_code - start_code) {
            let code = start_code + offset;
            let gid = start_gid + offset;
            if gid != 0 {
                mappings.push((gid, code));
            }
        }
    }

    mappings
}

/// Parse cmap format 6 (trimmed table mapping).
fn parse_cmap_format6(data: &[u8]) -> Vec<(u32, u32)> {
    if data.len() < 10 {
        return Vec::new();
    }
    let first_code = u16::from_be_bytes([data[6], data[7]]) as u32;
    let entry_count = u16::from_be_bytes([data[8], data[9]]) as usize;
    let mut mappings = Vec::new();

    for i in 0..entry_count {
        let offset = 10 + i * 2;
        if offset + 1 >= data.len() {
            break;
        }
        let gid = u16::from_be_bytes([data[offset], data[offset + 1]]) as u32;
        if gid != 0 {
            mappings.push((gid, first_code + i as u32));
        }
    }

    mappings
}

/// Parse CFF font charset to extract GID->Unicode via AGL glyph names.
fn parse_cff_charset(data: &[u8]) -> Vec<(u32, u32)> {
    // Minimal CFF parser: find Top DICT -> charset offset -> read glyph names -> AGL lookup
    if data.len() < 4 || data[0] != 1 {
        return Vec::new();
    }

    let hdr_size = data[2] as usize;
    if hdr_size >= data.len() {
        return Vec::new();
    }

    // Skip Name INDEX
    let name_idx = hdr_size;
    let after_name = skip_cff_index(data, name_idx);
    if after_name == 0 {
        return Vec::new();
    }

    // Top DICT INDEX
    let top_dict_idx = after_name;
    let (top_dict_data, after_top) = read_cff_index_first(data, top_dict_idx);
    if top_dict_data.is_empty() || after_top == 0 {
        return Vec::new();
    }

    // String INDEX
    let string_idx = after_top;
    let (string_data, string_offsets) = read_cff_index_all(data, string_idx);
    let after_string = skip_cff_index(data, string_idx);
    if after_string == 0 {
        return Vec::new();
    }

    // Parse Top DICT to find charset offset
    let charset_offset = parse_cff_top_dict_charset(&top_dict_data);
    if charset_offset == 0 || charset_offset as usize >= data.len() {
        return Vec::new();
    }

    // Also get CharStrings count for number of glyphs
    let charstrings_offset = parse_cff_top_dict_charstrings(&top_dict_data);
    let n_glyphs = if charstrings_offset > 0 && (charstrings_offset as usize) < data.len() {
        cff_index_count(data, charstrings_offset as usize)
    } else {
        0
    };
    if n_glyphs == 0 {
        return Vec::new();
    }

    // Parse charset
    let charset = &data[charset_offset as usize..];
    let sids = parse_cff_charset_sids(charset, n_glyphs);

    // Map SID -> glyph name -> Unicode via AGL
    let mut mappings = Vec::new();
    for (gid, sid) in sids.iter().enumerate() {
        let gid = gid as u32 + 1; // GID 0 is .notdef
        if let Some(name) = cff_sid_to_name(*sid, &string_data, &string_offsets) {
            if let Some(ch) = glyph_name_to_unicode(name.as_bytes()) {
                mappings.push((gid, ch as u32));
            }
        }
    }

    mappings
}

// CFF helper: skip an INDEX structure, return offset after it
fn skip_cff_index(data: &[u8], offset: usize) -> usize {
    if offset + 2 > data.len() {
        return 0;
    }
    let count = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    if count == 0 {
        return offset + 2;
    }
    if offset + 3 > data.len() {
        return 0;
    }
    let off_size = data[offset + 2] as usize;
    let offsets_start = offset + 3;
    let offsets_end = offsets_start + (count + 1) * off_size;
    if offsets_end > data.len() {
        return 0;
    }
    let last_off = read_cff_offset(data, offsets_start + count * off_size, off_size);
    let data_start = offsets_end;
    data_start + last_off - 1
}

// CFF helper: read first element from INDEX
fn read_cff_index_first(data: &[u8], offset: usize) -> (Vec<u8>, usize) {
    if offset + 2 > data.len() {
        return (Vec::new(), 0);
    }
    let count = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    if count == 0 {
        return (Vec::new(), offset + 2);
    }
    if offset + 3 > data.len() {
        return (Vec::new(), 0);
    }
    let off_size = data[offset + 2] as usize;
    let offsets_start = offset + 3;
    if offsets_start + 2 * off_size > data.len() {
        return (Vec::new(), 0);
    }
    let off1 = read_cff_offset(data, offsets_start, off_size);
    let off2 = read_cff_offset(data, offsets_start + off_size, off_size);
    let data_start = offsets_start + (count + 1) * off_size;
    let after = data_start + read_cff_offset(data, offsets_start + count * off_size, off_size) - 1;
    let start = data_start + off1 - 1;
    let end = data_start + off2 - 1;
    if end > data.len() || start > end {
        return (Vec::new(), after);
    }
    (data[start..end].to_vec(), after)
}

// CFF helper: read all elements from INDEX (returns data slice + per-element offsets)
fn read_cff_index_all(data: &[u8], offset: usize) -> (Vec<u8>, Vec<usize>) {
    if offset + 2 > data.len() {
        return (Vec::new(), Vec::new());
    }
    let count = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    if count == 0 {
        return (Vec::new(), Vec::new());
    }
    if offset + 3 > data.len() {
        return (Vec::new(), Vec::new());
    }
    let off_size = data[offset + 2] as usize;
    let offsets_start = offset + 3;
    let offsets_end = offsets_start + (count + 1) * off_size;
    if offsets_end > data.len() {
        return (Vec::new(), Vec::new());
    }
    let data_start = offsets_end;
    let last_off = read_cff_offset(data, offsets_start + count * off_size, off_size);
    let end = data_start + last_off - 1;
    if end > data.len() {
        return (Vec::new(), Vec::new());
    }

    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        offsets.push(read_cff_offset(data, offsets_start + i * off_size, off_size) - 1);
    }

    (data[data_start..end].to_vec(), offsets)
}

fn read_cff_offset(data: &[u8], pos: usize, size: usize) -> usize {
    let mut val = 0usize;
    for i in 0..size {
        if pos + i < data.len() {
            val = (val << 8) | (data[pos + i] as usize);
        }
    }
    val
}

fn cff_index_count(data: &[u8], offset: usize) -> usize {
    if offset + 2 > data.len() {
        return 0;
    }
    u16::from_be_bytes([data[offset], data[offset + 1]]) as usize
}

// Parse Top DICT for charset offset (operator 15)
fn parse_cff_top_dict_charset(dict_data: &[u8]) -> u32 {
    parse_cff_dict_int(dict_data, 15).unwrap_or(0) as u32
}

// Parse Top DICT for CharStrings offset (operator 17)
fn parse_cff_top_dict_charstrings(dict_data: &[u8]) -> u32 {
    parse_cff_dict_int(dict_data, 17).unwrap_or(0) as u32
}

// Parse a CFF DICT to find an integer operand for a given operator
fn parse_cff_dict_int(dict_data: &[u8], target_op: u8) -> Option<i64> {
    let mut i = 0;
    let mut operands: Vec<i64> = Vec::new();

    while i < dict_data.len() {
        let b0 = dict_data[i];
        match b0 {
            // Operators
            0..=21 => {
                let op = if b0 == 12 {
                    i += 1;
                    if i >= dict_data.len() {
                        break;
                    }
                    // Two-byte operator: 12 xx - we don't need these for charset/charstrings
                    256 + dict_data[i] as u16
                } else {
                    b0 as u16
                };
                if op == target_op as u16 {
                    return operands.last().copied();
                }
                operands.clear();
                i += 1;
            }
            // Integer operands
            28 => {
                if i + 2 < dict_data.len() {
                    let val = i16::from_be_bytes([dict_data[i + 1], dict_data[i + 2]]) as i64;
                    operands.push(val);
                }
                i += 3;
            }
            29 => {
                if i + 4 < dict_data.len() {
                    let val = i32::from_be_bytes([
                        dict_data[i + 1],
                        dict_data[i + 2],
                        dict_data[i + 3],
                        dict_data[i + 4],
                    ]) as i64;
                    operands.push(val);
                }
                i += 5;
            }
            30 => {
                // Real number - skip it
                i += 1;
                while i < dict_data.len() {
                    let nibbles = dict_data[i];
                    i += 1;
                    if (nibbles & 0x0F) == 0x0F || (nibbles >> 4) == 0x0F {
                        break;
                    }
                }
            }
            32..=246 => {
                operands.push(b0 as i64 - 139);
                i += 1;
            }
            247..=250 => {
                if i + 1 < dict_data.len() {
                    let val = ((b0 as i64 - 247) * 256) + dict_data[i + 1] as i64 + 108;
                    operands.push(val);
                }
                i += 2;
            }
            251..=254 => {
                if i + 1 < dict_data.len() {
                    let val = -((b0 as i64 - 251) * 256) - dict_data[i + 1] as i64 - 108;
                    operands.push(val);
                }
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }
    None
}

// Parse CFF charset: read SIDs for each glyph (GID 1..n_glyphs-1)
fn parse_cff_charset_sids(data: &[u8], n_glyphs: usize) -> Vec<u16> {
    if data.is_empty() || n_glyphs <= 1 {
        return Vec::new();
    }
    let format = data[0];
    let mut sids = Vec::with_capacity(n_glyphs - 1);

    match format {
        0 => {
            // Format 0: array of SIDs
            for i in 0..n_glyphs - 1 {
                let offset = 1 + i * 2;
                if offset + 1 >= data.len() {
                    break;
                }
                sids.push(u16::from_be_bytes([data[offset], data[offset + 1]]));
            }
        }
        1 => {
            // Format 1: ranges with 1-byte count
            let mut i = 1;
            while sids.len() < n_glyphs - 1 && i + 2 < data.len() {
                let first = u16::from_be_bytes([data[i], data[i + 1]]);
                let n_left = data[i + 2] as u16;
                for s in 0..=n_left {
                    if sids.len() >= n_glyphs - 1 {
                        break;
                    }
                    sids.push(first + s);
                }
                i += 3;
            }
        }
        2 => {
            // Format 2: ranges with 2-byte count
            let mut i = 1;
            while sids.len() < n_glyphs - 1 && i + 3 < data.len() {
                let first = u16::from_be_bytes([data[i], data[i + 1]]);
                let n_left = u16::from_be_bytes([data[i + 2], data[i + 3]]);
                for s in 0..=n_left {
                    if sids.len() >= n_glyphs - 1 {
                        break;
                    }
                    sids.push(first + s);
                }
                i += 4;
            }
        }
        _ => {}
    }

    sids
}

/// CFF standard strings (SID 0-390), all 391 entries per Adobe CFF spec.
static CFF_STANDARD_STRINGS: [&str; 392] = [
    // SID 0-31
    ".notdef",
    "space",
    "exclam",
    "quotedbl",
    "numbersign",
    "dollar",
    "percent",
    "ampersand",
    "quoteright",
    "parenleft",
    "parenright",
    "asterisk",
    "plus",
    "comma",
    "hyphen",
    "period",
    "slash",
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "colon",
    "semicolon",
    "less",
    "equal",
    "greater",
    // SID 32-63
    "question",
    "at",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "bracketleft",
    "backslash",
    "bracketright",
    // SID 64-95
    "asciicircum",
    "underscore",
    "quoteleft",
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "braceleft",
    "bar",
    "braceright",
    // SID 96-127
    "asciitilde",
    "exclamdown",
    "cent",
    "sterling",
    "fraction",
    "yen",
    "florin",
    "section",
    "currency",
    "quotesingle",
    "quotedblleft",
    "guillemotleft",
    "guilsinglleft",
    "guilsinglright",
    "fi",
    "fl",
    "endash",
    "dagger",
    "daggerdbl",
    "periodcentered",
    "paragraph",
    "bullet",
    "quotesinglbase",
    "quotedblbase",
    "quotedblright",
    "guillemotright",
    "ellipsis",
    "perthousand",
    "questiondown",
    "grave",
    "acute",
    "circumflex",
    // SID 128-159
    "tilde",
    "macron",
    "breve",
    "dotaccent",
    "dieresis",
    "ring",
    "cedilla",
    "hungarumlaut",
    "ogonek",
    "caron",
    "emdash",
    "AE",
    "ordfeminine",
    "Lslash",
    "Oslash",
    "OE",
    "ordmasculine",
    "ae",
    "dotlessi",
    "lslash",
    "oslash",
    "oe",
    "germandbls",
    "onesuperior",
    "logicalnot",
    "mu",
    "trademark",
    "Eth",
    "onehalf",
    "plusminus",
    "Thorn",
    "onequarter",
    "divide",
    // SID 160-191
    "brokenbar",
    "degree",
    "thorn",
    "threequarters",
    "twosuperior",
    "registered",
    "minus",
    "eth",
    "multiply",
    "threesuperior",
    "copyright",
    "Aacute",
    "Acircumflex",
    "Adieresis",
    "Agrave",
    "Aring",
    "Atilde",
    "Ccedilla",
    "Eacute",
    "Ecircumflex",
    "Edieresis",
    "Egrave",
    "Iacute",
    "Icircumflex",
    "Idieresis",
    "Igrave",
    "Ntilde",
    "Oacute",
    "Ocircumflex",
    "Odieresis",
    "Ograve",
    "Otilde",
    "Scaron",
    // SID 192-228
    "Uacute",
    "Ucircumflex",
    "Udieresis",
    "Ugrave",
    "Yacute",
    "Ydieresis",
    "Zcaron",
    "exclamsmall",
    "Hungarumlautsmall",
    "dollaroldstyle",
    "dollarsuperior",
    "ampersandsmall",
    "Acutesmall",
    "parenleftsuperior",
    "parenrightsuperior",
    "twodotenleader",
    "onedotenleader",
    "zerooldstyle",
    "oneoldstyle",
    "twooldstyle",
    "threeoldstyle",
    "fouroldstyle",
    "fiveoldstyle",
    "sixoldstyle",
    "sevenoldstyle",
    "eightoldstyle",
    "nineoldstyle",
    "commasuperior",
    "threequartersemdash",
    "periodsuperior",
    "questionsmall",
    "asuperior",
    "bsuperior",
    "centsuperior",
    "dsuperior",
    "esuperior",
    // SID 229-260
    "isuperior",
    "lsuperior",
    "msuperior",
    "nsuperior",
    "osuperior",
    "rsuperior",
    "ssuperior",
    "tsuperior",
    "ff",
    "ffi",
    "ffl",
    "parenleftinferior",
    "parenrightinferior",
    "Circumflexsmall",
    "hyphensuperior",
    "Gravesmall",
    "Asmall",
    "Bsmall",
    "Csmall",
    "Dsmall",
    "Esmall",
    "Fsmall",
    "Gsmall",
    "Hsmall",
    "Ismall",
    "Jsmall",
    "Ksmall",
    "Lsmall",
    "Msmall",
    "Nsmall",
    "Osmall",
    "Psmall",
    // SID 261-292
    "Qsmall",
    "Rsmall",
    "Ssmall",
    "Tsmall",
    "Usmall",
    "Vsmall",
    "Wsmall",
    "Xsmall",
    "Ysmall",
    "Zsmall",
    "colonmonetary",
    "onefitted",
    "rupiah",
    "Tildesmall",
    "exclamdownsmall",
    "centoldstyle",
    "Lslashsmall",
    "Scaronsmall",
    "Zcaronsmall",
    "Dieresissmall",
    "Brevesmall",
    "Caronsmall",
    "Dotaccentsmall",
    "Macronsmall",
    "figuredash",
    "hypheninferior",
    "Ogoneksmall",
    "Ringsmall",
    "Cedillasmall",
    "questiondownsmall",
    "oneeighth",
    "threeeighths",
    // SID 293-324
    "fiveeighths",
    "seveneighths",
    "onethird",
    "twothirds",
    "zerosuperior",
    "foursuperior",
    "fivesuperior",
    "sixsuperior",
    "sevensuperior",
    "eightsuperior",
    "ninesuperior",
    "zeroinferior",
    "oneinferior",
    "twoinferior",
    "threeinferior",
    "fourinferior",
    "fiveinferior",
    "sixinferior",
    "seveninferior",
    "eightinferior",
    "nineinferior",
    "centinferior",
    "dollarinferior",
    "periodinferior",
    "commainferior",
    "Agravesmall",
    "Aacutesmall",
    "Acircumflexsmall",
    "Atildesmall",
    "Adieresissmall",
    "Aringsmall",
    "AEsmall",
    // SID 325-356
    "Ccedillasmall",
    "Egravesmall",
    "Eacutesmall",
    "Ecircumflexsmall",
    "Edieresissmall",
    "Igravesmall",
    "Iacutesmall",
    "Icircumflexsmall",
    "Idieresissmall",
    "Ethsmall",
    "Ntildesmall",
    "Ogravesmall",
    "Oacutesmall",
    "Ocircumflexsmall",
    "Otildesmall",
    "Odieresissmall",
    "OEsmall",
    "Oslashsmall",
    "Ugravesmall",
    "Uacutesmall",
    "Ucircumflexsmall",
    "Udieresissmall",
    "Yacutesmall",
    "Thornsmall",
    "Ydieresissmall",
    "001.000",
    "001.001",
    "001.002",
    "001.003",
    "Black",
    "Bold",
    "Book",
    "Light",
    // SID 357-390
    "Medium",
    "Regular",
    "Roman",
    "Semibold",
    "Euro",
    "a.superior",
    "b.superior",
    "c.superior",
    "d.superior",
    "e.superior",
    "f.superior",
    "g.superior",
    "h.superior",
    "i.superior",
    "j.superior",
    "k.superior",
    "l.superior",
    "m.superior",
    "n.superior",
    "o.superior",
    "p.superior",
    "q.superior",
    "r.superior",
    "s.superior",
    "t.superior",
    "u.superior",
    "v.superior",
    "w.superior",
    "x.superior",
    "y.superior",
    "z.superior",
    "Tcommaaccent",
    "tcommaaccent",
    "Ohorn",
];

/// Map a CFF SID to a glyph name string.
fn cff_sid_to_name(sid: u16, string_data: &[u8], string_offsets: &[usize]) -> Option<String> {
    let sid = sid as usize;
    if sid < CFF_STANDARD_STRINGS.len() {
        return Some(CFF_STANDARD_STRINGS[sid].to_string());
    }
    // Custom string from String INDEX
    let custom_idx = sid - CFF_STANDARD_STRINGS.len();
    if custom_idx + 1 < string_offsets.len() {
        let start = string_offsets[custom_idx];
        let end = string_offsets[custom_idx + 1];
        if end <= string_data.len() {
            return std::str::from_utf8(&string_data[start..end])
                .ok()
                .map(|s| s.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Text decoding: raw bytes -> Unicode string
// ---------------------------------------------------------------------------

/// Decode raw character bytes to Unicode using the font's encoding.
///
/// Priority order:
/// 1. ToUnicode CMap (FE2)
/// 2. Encoding differences + AGL (FE4 + FE8)
/// 3. Standard encoding (FE3)
/// 4. Latin-1 fallback (FE11)
pub fn decode_text(font: &PdfFont, raw: &[u8]) -> String {
    let text = if font.is_two_byte {
        decode_two_byte(font, raw)
    } else {
        decode_single_byte(font, raw)
    };
    // Decompose Unicode ligatures to component characters
    decompose_ligatures(&text)
}

/// Decompose Unicode ligatures (U+FB00-FB06) to their component characters.
fn decompose_ligatures(text: &str) -> String {
    if !text.contains([
        '\u{FB00}', '\u{FB01}', '\u{FB02}', '\u{FB03}', '\u{FB04}', '\u{FB05}', '\u{FB06}',
    ]) {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len() + 16);
    for ch in text.chars() {
        match ch {
            '\u{FB00}' => result.push_str("ff"),
            '\u{FB01}' => result.push_str("fi"),
            '\u{FB02}' => result.push_str("fl"),
            '\u{FB03}' => result.push_str("ffi"),
            '\u{FB04}' => result.push_str("ffl"),
            '\u{FB05}' => result.push_str("st"), // long s + t
            '\u{FB06}' => result.push_str("st"),
            _ => result.push(ch),
        }
    }
    result
}

/// Decode single-byte (8-bit) font text.
fn decode_single_byte(font: &PdfFont, raw: &[u8]) -> String {
    let mut result = String::with_capacity(raw.len());

    for &byte in raw {
        let code = byte as u32;

        // Priority 1: ToUnicode CMap
        if let Some(ref tu) = font.to_unicode {
            if let Some(s) = tu.lookup(code) {
                result.push_str(&s);
                continue;
            }
        }

        // Priority 2: Encoding
        let ch = match &font.encoding {
            FontEncoding::Differences { base, diffs } => {
                // Check differences first
                let from_diff = diffs
                    .iter()
                    .find(|(c, _)| *c == byte)
                    .and_then(|(_, name)| {
                        // First try AGL lookup
                        if let Some(c) = glyph_name_to_unicode(name) {
                            return Some(c);
                        }
                        // If glyph name is gXXX (GID), look up in embedded cmap
                        if let Some(ref cmap) = font.embedded_cmap {
                            let name_str = std::str::from_utf8(name).ok()?;
                            if name_str.starts_with('g') {
                                if let Ok(gid) = name_str[1..].parse::<u32>() {
                                    if let Some(&(_, unicode)) =
                                        cmap.iter().find(|(g, _)| *g == gid)
                                    {
                                        return char::from_u32(unicode);
                                    }
                                }
                            }
                        }
                        None
                    });
                from_diff.or_else(|| standard_decode(*base, byte))
            }
            FontEncoding::Named(enc) => standard_decode(*enc, byte),
            FontEncoding::Builtin => {
                // FE6: Try glyph name from encoding, else Latin-1
                char::from_u32(code)
            }
            _ => char::from_u32(code),
        };

        // Priority 4: Fallback
        result.push(ch.unwrap_or(char::REPLACEMENT_CHARACTER));
    }

    result
}

/// Decode two-byte (CID) font text.
fn decode_two_byte(font: &PdfFont, raw: &[u8]) -> String {
    let mut result = String::new();
    let mut i = 0;

    while i + 1 < raw.len() {
        let code = ((raw[i] as u32) << 8) | (raw[i + 1] as u32);
        i += 2;

        // Priority 1: ToUnicode CMap
        if let Some(ref tu) = font.to_unicode {
            if let Some(s) = tu.lookup(code) {
                result.push_str(&s);
                continue;
            }
        }

        // Priority 2: Embedded font cmap (GID->Unicode from TrueType cmap table)
        if let Some(ref cmap) = font.embedded_cmap {
            if let Some(&(_, unicode)) = cmap.iter().find(|(gid, _)| *gid == code) {
                if let Some(c) = char::from_u32(unicode) {
                    result.push(c);
                    continue;
                }
            }
        }

        // FE5: Identity encoding - code IS the Unicode codepoint
        match &font.encoding {
            FontEncoding::IdentityH | FontEncoding::IdentityV => {
                if let Some(c) = char::from_u32(code) {
                    result.push(c);
                } else {
                    result.push(char::REPLACEMENT_CHARACTER);
                }
            }
            _ => {
                result.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
    }

    // Handle odd trailing byte
    if i < raw.len() {
        let code = raw[i] as u32;
        if let Some(ref tu) = font.to_unicode {
            if let Some(s) = tu.lookup(code) {
                result.push_str(&s);
                return result;
            }
        }
        result.push(char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER));
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- FE1: Font dictionary parsing ---

    #[test]
    fn fe1_parse_simple_font() {
        let font_dict = PdfObject::Dict(vec![
            (b"Type".to_vec(), PdfObject::Name(b"Font".to_vec())),
            (b"Subtype".to_vec(), PdfObject::Name(b"Type1".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Helvetica".to_vec())),
            (
                b"Encoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
        ]);

        let doc = make_test_doc();
        let font = parse_font(&doc, &font_dict).unwrap();
        assert_eq!(font.name, b"Helvetica");
        assert_eq!(font.subtype, FontSubtype::Type1);
        assert!(!font.is_two_byte);
        assert!(matches!(
            font.encoding,
            FontEncoding::Named(StandardEncoding::WinAnsi)
        ));
    }

    #[test]
    fn fe1_parse_truetype_font() {
        let font_dict = PdfObject::Dict(vec![
            (b"Subtype".to_vec(), PdfObject::Name(b"TrueType".to_vec())),
            (b"BaseFont".to_vec(), PdfObject::Name(b"Arial".to_vec())),
        ]);

        let doc = make_test_doc();
        let font = parse_font(&doc, &font_dict).unwrap();
        assert_eq!(font.subtype, FontSubtype::TrueType);
        assert!(matches!(font.encoding, FontEncoding::Builtin));
    }

    #[test]
    fn fe1_parse_type0_font() {
        let font_dict = PdfObject::Dict(vec![
            (b"Subtype".to_vec(), PdfObject::Name(b"Type0".to_vec())),
            (
                b"BaseFont".to_vec(),
                PdfObject::Name(b"KozMinPro-Regular".to_vec()),
            ),
            (
                b"Encoding".to_vec(),
                PdfObject::Name(b"Identity-H".to_vec()),
            ),
        ]);

        let doc = make_test_doc();
        let font = parse_font(&doc, &font_dict).unwrap();
        assert_eq!(font.subtype, FontSubtype::Type0);
        assert!(font.is_two_byte);
        assert!(matches!(font.encoding, FontEncoding::IdentityH));
    }

    // --- FE2: ToUnicode CMap parsing ---

    #[test]
    fn fe2_parse_bfchar() {
        let cmap = b"\
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CMapName /test def
1 begincodespacerange
<00> <FF>
endcodespacerange
3 beginbfchar
<01> <0041>
<02> <0042>
<03> <0043>
endbfchar
endcmap
";
        let map = parse_to_unicode(cmap);
        assert_eq!(map.lookup(1), Some("A".to_string()));
        assert_eq!(map.lookup(2), Some("B".to_string()));
        assert_eq!(map.lookup(3), Some("C".to_string()));
        assert_eq!(map.lookup(4), None);
    }

    #[test]
    fn fe2_parse_bfrange() {
        let cmap = b"\
1 beginbfrange
<0041> <0043> <0061>
endbfrange
";
        let map = parse_to_unicode(cmap);
        assert_eq!(map.lookup(0x41), Some("a".to_string()));
        assert_eq!(map.lookup(0x42), Some("b".to_string()));
        assert_eq!(map.lookup(0x43), Some("c".to_string()));
        assert_eq!(map.lookup(0x44), None);
    }

    #[test]
    fn fe2_parse_bfrange_array() {
        let cmap = b"\
1 beginbfrange
<01> <03> [<0041> <0042> <0043>]
endbfrange
";
        let map = parse_to_unicode(cmap);
        assert_eq!(map.lookup(1), Some("A".to_string()));
        assert_eq!(map.lookup(2), Some("B".to_string()));
        assert_eq!(map.lookup(3), Some("C".to_string()));
    }

    #[test]
    fn fe2_multibyte_unicode() {
        let cmap = b"\
1 beginbfchar
<01> <20AC>
endbfchar
";
        let map = parse_to_unicode(cmap);
        assert_eq!(map.lookup(1), Some("\u{20AC}".to_string())); // Euro sign
    }

    // --- FE3: Standard encodings ---

    #[test]
    fn fe3_winansi_ascii() {
        assert_eq!(standard_decode(StandardEncoding::WinAnsi, b'A'), Some('A'));
        assert_eq!(standard_decode(StandardEncoding::WinAnsi, b'z'), Some('z'));
        assert_eq!(standard_decode(StandardEncoding::WinAnsi, b' '), Some(' '));
    }

    #[test]
    fn fe3_winansi_special() {
        assert_eq!(
            standard_decode(StandardEncoding::WinAnsi, 0x80),
            Some('\u{20AC}')
        ); // Euro
        assert_eq!(
            standard_decode(StandardEncoding::WinAnsi, 0x93),
            Some('\u{201C}')
        ); // left double quote
        assert_eq!(
            standard_decode(StandardEncoding::WinAnsi, 0x94),
            Some('\u{201D}')
        ); // right double quote
        assert_eq!(
            standard_decode(StandardEncoding::WinAnsi, 0x96),
            Some('\u{2013}')
        ); // en dash
    }

    #[test]
    fn fe3_macroman() {
        assert_eq!(standard_decode(StandardEncoding::MacRoman, b'A'), Some('A'));
        assert_eq!(
            standard_decode(StandardEncoding::MacRoman, 0x80),
            Some('\u{00C4}')
        ); // Ä
        assert_eq!(
            standard_decode(StandardEncoding::MacRoman, 0xCA),
            Some('\u{00A0}')
        ); // nbsp
    }

    // --- FE4: Encoding differences ---

    #[test]
    fn fe4_differences_parsing() {
        let enc_dict = PdfObject::Dict(vec![
            (
                b"BaseEncoding".to_vec(),
                PdfObject::Name(b"WinAnsiEncoding".to_vec()),
            ),
            (
                b"Differences".to_vec(),
                PdfObject::Array(vec![
                    PdfObject::Int(32),
                    PdfObject::Name(b"space".to_vec()),
                    PdfObject::Name(b"exclam".to_vec()),
                    PdfObject::Int(65),
                    PdfObject::Name(b"A".to_vec()),
                ]),
            ),
        ]);

        let diffs = parse_differences(&enc_dict);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0], (32, b"space".to_vec()));
        assert_eq!(diffs[1], (33, b"exclam".to_vec()));
        assert_eq!(diffs[2], (65, b"A".to_vec()));
    }

    #[test]
    fn fe4_decode_with_differences() {
        let font = PdfFont {
            name: b"TestFont".to_vec(),
            subtype: FontSubtype::Type1,
            encoding: FontEncoding::Differences {
                base: StandardEncoding::WinAnsi,
                diffs: vec![(65, b"Euro".to_vec())],
            },
            to_unicode: None,
            widths: FontWidths::Default(600.0),
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        let text = decode_text(&font, &[65]); // 65 = 'A' normally, but overridden to Euro
        assert_eq!(text, "\u{20AC}");
    }

    // --- FE5: Identity-H encoding ---

    #[test]
    fn fe5_identity_h_decode() {
        let font = PdfFont {
            name: b"CIDFont".to_vec(),
            subtype: FontSubtype::Type0,
            encoding: FontEncoding::IdentityH,
            to_unicode: None,
            widths: FontWidths::Default(1000.0),
            is_two_byte: true,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        // 0x0041 = 'A', 0x0042 = 'B'
        let text = decode_text(&font, &[0x00, 0x41, 0x00, 0x42]);
        assert_eq!(text, "AB");
    }

    // --- FE8: AGL glyph name mapping ---

    #[test]
    fn fe8_agl_common_names() {
        assert_eq!(glyph_name_to_unicode(b"A"), Some('A'));
        assert_eq!(glyph_name_to_unicode(b"space"), Some(' '));
        assert_eq!(glyph_name_to_unicode(b"bullet"), Some('\u{2022}'));
        assert_eq!(glyph_name_to_unicode(b"emdash"), Some('\u{2014}'));
        assert_eq!(glyph_name_to_unicode(b"fi"), Some('\u{FB01}'));
        assert_eq!(glyph_name_to_unicode(b"Euro"), Some('\u{20AC}'));
    }

    #[test]
    fn fe8_uni_format() {
        assert_eq!(glyph_name_to_unicode(b"uni0041"), Some('A'));
        assert_eq!(glyph_name_to_unicode(b"uni20AC"), Some('\u{20AC}'));
    }

    #[test]
    fn fe8_u_format() {
        assert_eq!(glyph_name_to_unicode(b"u0041"), Some('A'));
        assert_eq!(glyph_name_to_unicode(b"u20AC"), Some('\u{20AC}'));
    }

    // --- FE9: Font widths ---

    #[test]
    fn fe9_simple_widths() {
        let font = PdfFont {
            name: b"Test".to_vec(),
            subtype: FontSubtype::Type1,
            encoding: FontEncoding::Builtin,
            to_unicode: None,
            widths: FontWidths::Simple {
                first_char: 32,
                widths: vec![250.0, 333.0, 408.0], // space=250, !=333, "=408
            },
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        assert!((glyph_width(&font, 32) - 250.0).abs() < 0.001);
        assert!((glyph_width(&font, 33) - 333.0).abs() < 0.001);
        assert!((glyph_width(&font, 34) - 408.0).abs() < 0.001);
        assert!((glyph_width(&font, 31) - 600.0).abs() < 0.001); // before FirstChar
        assert!((glyph_width(&font, 35) - 600.0).abs() < 0.001); // after array
    }

    #[test]
    fn fe9_cid_widths_range() {
        let font = PdfFont {
            name: b"CIDTest".to_vec(),
            subtype: FontSubtype::Type0,
            encoding: FontEncoding::IdentityH,
            to_unicode: None,
            widths: FontWidths::Cid {
                default: 1000.0,
                entries: vec![CidWidthEntry::Range {
                    start: 100,
                    end: 200,
                    width: 500.0,
                }],
            },
            is_two_byte: true,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        assert!((glyph_width(&font, 100) - 500.0).abs() < 0.001);
        assert!((glyph_width(&font, 150) - 500.0).abs() < 0.001);
        assert!((glyph_width(&font, 200) - 500.0).abs() < 0.001);
        assert!((glyph_width(&font, 99) - 1000.0).abs() < 0.001); // default
        assert!((glyph_width(&font, 201) - 1000.0).abs() < 0.001); // default
    }

    #[test]
    fn fe9_cid_widths_individual() {
        let font = PdfFont {
            name: b"CIDTest".to_vec(),
            subtype: FontSubtype::Type0,
            encoding: FontEncoding::IdentityH,
            to_unicode: None,
            widths: FontWidths::Cid {
                default: 1000.0,
                entries: vec![CidWidthEntry::Individual {
                    start: 10,
                    widths: vec![500.0, 600.0, 700.0],
                }],
            },
            is_two_byte: true,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        assert!((glyph_width(&font, 10) - 500.0).abs() < 0.001);
        assert!((glyph_width(&font, 11) - 600.0).abs() < 0.001);
        assert!((glyph_width(&font, 12) - 700.0).abs() < 0.001);
        assert!((glyph_width(&font, 13) - 1000.0).abs() < 0.001); // default
    }

    #[test]
    fn fe9_parse_w_array() {
        // Format: [10 [500 600 700] 100 200 800]
        let arr = vec![
            PdfObject::Int(10),
            PdfObject::Array(vec![
                PdfObject::Int(500),
                PdfObject::Int(600),
                PdfObject::Int(700),
            ]),
            PdfObject::Int(100),
            PdfObject::Int(200),
            PdfObject::Int(800),
        ];

        let entries = parse_cid_widths(&arr);
        assert_eq!(entries.len(), 2);
        assert!(
            matches!(&entries[0], CidWidthEntry::Individual { start: 10, widths } if widths.len() == 3)
        );
        assert!(
            matches!(&entries[1], CidWidthEntry::Range { start: 100, end: 200, width } if (*width - 800.0).abs() < 0.001)
        );
    }

    // --- FE11: Missing font handling ---

    #[test]
    fn fe11_default_width() {
        let font = PdfFont {
            name: b"Missing".to_vec(),
            subtype: FontSubtype::Unknown,
            encoding: FontEncoding::None,
            to_unicode: None,
            widths: FontWidths::Default(500.0),
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        assert!((glyph_width(&font, 65) - 500.0).abs() < 0.001);
    }

    #[test]
    fn fe11_missing_font_fallback() {
        let font = PdfFont {
            name: b"Unknown".to_vec(),
            subtype: FontSubtype::Unknown,
            encoding: FontEncoding::None,
            to_unicode: None,
            widths: FontWidths::Default(600.0),
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        // Without encoding, falls through to char::from_u32
        let text = decode_text(&font, &[0x41, 0x42, 0x43]);
        assert_eq!(text, "ABC");
    }

    // --- Integration: ToUnicode takes priority ---

    #[test]
    fn tounicode_overrides_encoding() {
        let mut map = ToUnicodeMap {
            singles: Vec::new(),
            ranges: Vec::new(),
        };
        map.singles.push((65, "\u{03B1}".to_string())); // A -> α

        let font = PdfFont {
            name: b"Test".to_vec(),
            subtype: FontSubtype::Type1,
            encoding: FontEncoding::Named(StandardEncoding::WinAnsi),
            to_unicode: Some(map),
            widths: FontWidths::Default(600.0),
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        let text = decode_text(&font, &[65]);
        assert_eq!(text, "\u{03B1}"); // α, not A
    }

    /// Helper: build a minimal Document for font tests.
    fn make_test_doc() -> Document<'static> {
        static PDF: &[u8] = b"%PDF-1.7\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
            xref\n0 3\n\
            0000000000 65535 f \n\
            0000000009 00000 n \n\
            0000000058 00000 n \n\
            trailer\n<< /Size 3 /Root 1 0 R >>\n\
            startxref\n109\n%%EOF";
        Document::parse(PDF).unwrap()
    }
}
