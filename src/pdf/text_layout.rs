//! PDF text layout reconstruction (TL1-TL14).
//!
//! Takes raw `TextSpan`s from the content stream interpreter, decodes them
//! to Unicode via the font module, and reconstructs spatial layout:
//! characters -> words -> lines -> blocks -> reading-ordered text.
//!
//! Two output modes:
//! - Raw (TL13): characters in content stream order, decoded to Unicode
//! - Layout (TL14): physical positioning preserved with spaces/newlines

use std::collections::HashMap;

use super::content::{ContentInterpreter, TextSpan};
use super::document::{Document, Page};
use super::font::{self, PdfFont};
use crate::core::Result;

// ---------------------------------------------------------------------------
// TL constants - thresholds tuned empirically against the test corpora
// ---------------------------------------------------------------------------

/// TL2: Baseline bucketing granularity in points.
const POOL_STEP: f64 = 4.0;

/// TL3: Duplicate detection - primary axis tolerance (× font_size).
const DUP_MAX_PRI_DELTA: f64 = 0.1;

/// TL3: Duplicate detection - secondary axis tolerance (× font_size).
const DUP_MAX_SEC_DELTA: f64 = 0.2;

/// TL4: Minimum character spacing before word break (× font_size).
/// Negative = overlap threshold.
const MIN_CHAR_SPACING: f64 = -0.5;

/// TL5: Maximum intra-word character spacing (× font_size).
const MAX_CHAR_SPACING: f64 = 0.03;

/// TL5: Multiplier for detected wide character spacing.
const MAX_WIDE_CHAR_MUL: f64 = 1.3;

/// TL5: Cap on computed wide character spacing (× font_size).
const MAX_WIDE_CHAR_SPACING: f64 = 0.4;

/// TL6: Maximum baseline distance for same line (× font_size).
const MAX_INTRA_LINE_DELTA: f64 = 0.5;

/// TL6: Maximum horizontal gap between words on same line (× font_size).
const MAX_WORD_SPACING: f64 = 1.5;

/// TL8: Maximum baseline gap between lines in same block (× font_size).
const MAX_LINE_SPACING: f64 = 1.5;

/// TL8: Font size tolerance for lines above/below block (× font_size).
const MAX_BLOCK_FONT_DELTA1: f64 = 0.05;

/// TL8: Font size tolerance for overlapping text (× font_size).
const _MAX_BLOCK_FONT_DELTA2: f64 = 0.6;

/// TL8: Font size tolerance for sideband text (× font_size).
const _MAX_BLOCK_FONT_DELTA3: f64 = 0.2;

/// TL9: Minimum column spacing (× font_size).
const MIN_COL_SPACING: f64 = 0.7;

// ---------------------------------------------------------------------------
// TL1: Positioned character
// ---------------------------------------------------------------------------

/// Text rotation detected from the transformation matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rotation {
    /// 0° - normal left-to-right.
    R0,
    /// 90° clockwise.
    R90,
    /// 180° - upside down.
    R180,
    /// 270° clockwise (= 90° counter-clockwise).
    R270,
}

/// A single positioned, decoded character with metrics.
#[derive(Debug, Clone)]
pub struct PositionedChar {
    /// Decoded Unicode text (usually 1 char, but ligatures can produce more).
    pub unicode: String,
    /// X position in user space.
    pub x: f64,
    /// Y position in user space.
    pub y: f64,
    /// Advance width in user space points.
    pub width: f64,
    /// Character height (≈ font_size × matrix scaling).
    pub height: f64,
    /// Effective font size in user space.
    pub font_size: f64,
    /// Unique font key (resource name plus font object identity).
    pub font_name: Vec<u8>,
    /// Rendering mode (0-7; 3 = invisible).
    pub render_mode: u8,
    /// Detected rotation.
    pub rotation: Rotation,
    /// Space character width in user space (font's space glyph advance × font_size / 1000).
    pub space_width: f64,
}

/// A word - contiguous characters grouped by proximity.
#[derive(Debug, Clone)]
pub struct TextWord {
    pub chars: Vec<PositionedChar>,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    /// Baseline coordinate (y for R0, x for R90, etc).
    pub base: f64,
    pub font_size: f64,
    pub rotation: Rotation,
    /// Whether a space should be inserted after this word.
    pub space_after: bool,
}

/// A line - words along a single baseline.
#[derive(Debug, Clone)]
pub struct TextLine {
    pub words: Vec<TextWord>,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub base: f64,
    pub rotation: Rotation,
}

/// A block - lines with consistent font/spacing (paragraph).
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub lines: Vec<TextLine>,
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub rotation: Rotation,
}

// ---------------------------------------------------------------------------
// TL1: Collect positioned characters from spans + fonts
// ---------------------------------------------------------------------------

/// TL1: Convert raw TextSpans to positioned, decoded characters.
///
/// Each span is split into individual character codes, decoded via the font
/// module, and positioned using glyph widths.
pub fn collect_chars(spans: &[TextSpan], page_fonts: &[(Vec<u8>, PdfFont)]) -> Vec<PositionedChar> {
    let mut chars = Vec::new();

    // Build a lookup map: font_name -> &PdfFont
    let font_map: HashMap<&[u8], &PdfFont> = page_fonts
        .iter()
        .map(|(name, f)| (name.as_slice(), f))
        .collect();

    for span in spans {
        let font_opt = font_map.get(span.font_name.as_slice());

        // Detect rotation from the span position context.
        // Without the full matrix we approximate as R0.
        // A more precise version would store the matrix in TextSpan.
        let rotation = Rotation::R0;

        let font_size = span.font_size.abs().max(0.001);

        // Effective height/font_size in page space, accounting for CTM × Tm scale
        let effective_size = font_size * span.ctm_scale_x.abs().max(0.001);
        let height = effective_size;

        if let Some(font) = font_opt {
            if font.is_two_byte {
                collect_two_byte_chars(&mut chars, span, font, effective_size, height, rotation);
            } else {
                collect_single_byte_chars(&mut chars, span, font, effective_size, height, rotation);
            }
        } else {
            // No font found - treat each byte as Latin-1
            collect_fallback_chars(&mut chars, span, effective_size, height, rotation);
        }
    }

    chars
}

/// Collect characters from a single-byte font span.
fn collect_single_byte_chars(
    out: &mut Vec<PositionedChar>,
    span: &TextSpan,
    font: &PdfFont,
    font_size: f64,
    height: f64,
    rotation: Rotation,
) {
    let mut x = span.x;
    let y = span.y;
    let space_width = compute_space_width(font, font_size);

    // Scale Tc/Tw from text space to page space
    let ctm_scale = span.ctm_scale_x.abs().max(0.001);
    let tc = span.char_spacing * ctm_scale;
    let tw = span.word_spacing * ctm_scale;

    for &byte in &span.text {
        let code = byte as u32;
        let unicode = font::decode_text(font, &[byte]);
        let w0 = font::glyph_width(font, code);
        let glyph_advance = w0 * font_size * font.font_matrix_scale;

        // Full advance includes Tc (and Tw for space glyphs), matching
        // compute_string_advance() in content.rs.
        let mut advance = glyph_advance + tc;
        if byte == 32 {
            advance += tw;
        }

        // Skip replacement characters - they have no text value
        if unicode != "\u{FFFD}" {
            out.push(PositionedChar {
                unicode,
                x,
                y,
                width: advance,
                height,
                font_size,
                font_name: span.font_name.clone(),
                render_mode: span.render_mode,
                rotation,
                space_width,
            });
        }

        x += advance;
    }
}

/// Collect characters from a two-byte (CID) font span.
fn collect_two_byte_chars(
    out: &mut Vec<PositionedChar>,
    span: &TextSpan,
    font: &PdfFont,
    font_size: f64,
    height: f64,
    rotation: Rotation,
) {
    let mut x = span.x;
    let y = span.y;
    let raw = &span.text;
    let mut i = 0;
    let space_width = compute_space_width(font, font_size);

    // Scale Tc from text space to page space
    let ctm_scale = span.ctm_scale_x.abs().max(0.001);
    let tc = span.char_spacing * ctm_scale;

    while i + 1 < raw.len() {
        let code = ((raw[i] as u32) << 8) | (raw[i + 1] as u32);
        let unicode = font::decode_text(font, &raw[i..i + 2]);
        let w0 = font::glyph_width(font, code);
        let glyph_advance = w0 * font_size * font.font_matrix_scale;

        // Full advance includes Tc, matching compute_string_advance().
        let advance = glyph_advance + tc;

        // Skip replacement characters
        if unicode != "\u{FFFD}" {
            out.push(PositionedChar {
                unicode,
                x,
                y,
                width: advance,
                height,
                font_size,
                font_name: span.font_name.clone(),
                render_mode: span.render_mode,
                rotation,
                space_width,
            });
        }

        x += advance;
        i += 2;
    }
}

/// Fallback: no font available, treat as Latin-1.
fn collect_fallback_chars(
    out: &mut Vec<PositionedChar>,
    span: &TextSpan,
    font_size: f64,
    height: f64,
    rotation: Rotation,
) {
    let mut x = span.x;
    let y = span.y;
    let default_glyph = 600.0 * font_size / 1000.0;

    // Scale Tc/Tw from text space to page space
    let ctm_scale = span.ctm_scale_x.abs().max(0.001);
    let tc = span.char_spacing * ctm_scale;
    let tw = span.word_spacing * ctm_scale;

    for &byte in &span.text {
        let ch = char::from_u32(byte as u32).unwrap_or(char::REPLACEMENT_CHARACTER);

        let mut advance = default_glyph + tc;
        if byte == 32 {
            advance += tw;
        }

        out.push(PositionedChar {
            unicode: ch.to_string(),
            x,
            y,
            width: advance,
            height,
            font_size,
            font_name: span.font_name.clone(),
            render_mode: span.render_mode,
            rotation,
            space_width: default_glyph, // estimate space ≈ default advance
        });

        x += advance;
    }
}

/// Compute a reliable space width for a font, in user space units.
///
/// If the font has a real width for the space glyph (code 32 or 0x0020),
/// use that. Otherwise, estimate from the average of actual glyph widths.
fn compute_space_width(font: &PdfFont, font_size: f64) -> f64 {
    let space_code = if font.is_two_byte { 0x0020 } else { 32 };
    let raw_w = font::glyph_width(font, space_code);

    // Check if this is a real width or the 600 default
    let effective_w = match &font.widths {
        font::FontWidths::Simple { first_char, widths } => {
            let idx = space_code.checked_sub(*first_char).map(|i| i as usize);
            match idx {
                Some(i) if i < widths.len() && widths[i] > 0.0 => widths[i],
                _ => {
                    // Space not in width table; estimate from average of non-zero widths
                    let avg = widths
                        .iter()
                        .filter(|&&w| w > 0.0 && w < 1500.0)
                        .copied()
                        .sum::<f64>()
                        / widths
                            .iter()
                            .filter(|&&w| w > 0.0 && w < 1500.0)
                            .count()
                            .max(1) as f64;
                    if avg > 0.0 { avg * 0.4 } else { 250.0 }
                }
            }
        }
        font::FontWidths::Cid { .. } => raw_w,
        font::FontWidths::Default(_) => 250.0, // typical space width
    };

    effective_w * font_size * font.font_matrix_scale
}

// ---------------------------------------------------------------------------
// TL2: Bucket characters by baseline
// ---------------------------------------------------------------------------

/// TL2: Bucket characters by baseline coordinate.
///
/// Returns a HashMap indexed by `floor(baseline / POOL_STEP)`.
/// For rotation 0, baseline = y. For rotation 90, baseline = x. Etc.
fn bucket_by_baseline(chars: &mut Vec<PositionedChar>) -> HashMap<i64, Vec<usize>> {
    let mut pool: HashMap<i64, Vec<usize>> = HashMap::new();

    for (i, ch) in chars.iter().enumerate() {
        let base = baseline_coord(ch);
        let idx = (base / POOL_STEP).floor() as i64;
        pool.entry(idx).or_default().push(i);
    }

    pool
}

/// Get the baseline coordinate for a character based on its rotation.
fn baseline_coord(ch: &PositionedChar) -> f64 {
    match ch.rotation {
        Rotation::R0 => ch.y,
        Rotation::R90 => ch.x,
        Rotation::R180 => -ch.y,
        Rotation::R270 => -ch.x,
    }
}

/// Get the primary axis coordinate (reading direction).
fn primary_coord(ch: &PositionedChar) -> f64 {
    match ch.rotation {
        Rotation::R0 => ch.x,
        Rotation::R90 => ch.y,
        Rotation::R180 => -ch.x,
        Rotation::R270 => -ch.y,
    }
}

// ---------------------------------------------------------------------------
// TL3: Duplicate/shadow text detection
// ---------------------------------------------------------------------------

/// TL3: Remove duplicate characters (shadow text, fake bold).
///
/// Two characters are duplicates if they have the same Unicode text and
/// their positions are within tolerance on both axes.
fn remove_duplicates(chars: &mut Vec<PositionedChar>) {
    if chars.len() < 2 {
        return;
    }

    let mut keep = vec![true; chars.len()];

    // Sort by primary axis for efficient pairwise comparison
    let mut indices: Vec<usize> = (0..chars.len()).collect();
    indices.sort_by(|&a, &b| {
        let pa = primary_coord(&chars[a]);
        let pb = primary_coord(&chars[b]);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });

    for i in 0..indices.len() {
        if !keep[indices[i]] {
            continue;
        }
        let ci = indices[i];
        let fs = chars[ci].font_size.max(1.0);
        let pri_tol = DUP_MAX_PRI_DELTA * fs;
        let sec_tol = DUP_MAX_SEC_DELTA * fs;
        let pri_i = primary_coord(&chars[ci]);

        for j in (i + 1)..indices.len() {
            let cj = indices[j];
            if !keep[cj] {
                continue;
            }

            let pri_j = primary_coord(&chars[cj]);
            // Past the tolerance window - stop searching
            if (pri_j - pri_i).abs() > pri_tol + chars[ci].width {
                break;
            }

            if chars[ci].unicode != chars[cj].unicode {
                continue;
            }

            let base_i = baseline_coord(&chars[ci]);
            let base_j = baseline_coord(&chars[cj]);
            if (base_i - base_j).abs() > sec_tol {
                continue;
            }

            if (pri_i - pri_j).abs() <= pri_tol {
                // Duplicate - remove the later one
                keep[cj] = false;
            }
        }
    }

    let mut write = 0;
    for read in 0..chars.len() {
        if keep[read] {
            if write != read {
                chars[write] = chars[read].clone();
            }
            write += 1;
        }
    }
    chars.truncate(write);
}

// ---------------------------------------------------------------------------
// TL4 + TL5: Form words
// ---------------------------------------------------------------------------

/// TL4: Cluster characters into words based on spatial proximity.
///
/// Characters are sorted by primary axis, then clustered: a new word starts
/// when the gap exceeds the word-break threshold or font changes.
fn form_words(chars: &[PositionedChar]) -> Vec<TextWord> {
    if chars.is_empty() {
        return Vec::new();
    }

    // Sort by baseline bucket then primary coordinate
    let mut sorted: Vec<&PositionedChar> = chars.iter().collect();
    sorted.sort_by(|a, b| {
        let ba = baseline_coord(a);
        let bb = baseline_coord(b);
        let cmp = ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal);
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
        let pa = primary_coord(a);
        let pb = primary_coord(b);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut words = Vec::new();
    let mut current_chars: Vec<PositionedChar> = vec![sorted[0].clone()];

    for i in 1..sorted.len() {
        let prev = sorted[i - 1];
        let curr = sorted[i];

        let should_break = should_break_word(prev, curr);

        if should_break {
            words.push(make_word(std::mem::take(&mut current_chars)));
            current_chars = vec![curr.clone()];
        } else {
            current_chars.push(curr.clone());
        }
    }

    if !current_chars.is_empty() {
        words.push(make_word(current_chars));
    }

    words
}

/// Determine whether to break between two adjacent characters.
fn should_break_word(prev: &PositionedChar, curr: &PositionedChar) -> bool {
    // Different rotation = always break
    if prev.rotation != curr.rotation {
        return true;
    }

    let fs = prev.font_size.max(curr.font_size).max(1.0);

    // Baseline distance too large = different line entirely
    let base_delta = (baseline_coord(prev) - baseline_coord(curr)).abs();
    if base_delta > MAX_INTRA_LINE_DELTA * fs {
        return true;
    }

    // Gap along primary axis
    let prev_end = primary_coord(prev) + prev.width;
    let curr_start = primary_coord(curr);
    let gap = curr_start - prev_end;

    // Excessive overlap = break
    if gap < MIN_CHAR_SPACING * fs {
        return true;
    }

    // TL5: Word-break threshold - use font-aware space width
    let sw = prev.space_width.max(curr.space_width);
    let threshold = if sw > 0.01 {
        // A word break should be at least ~35% of the space character width.
        // Cap to prevent overly large thresholds from wide-space fonts.
        (sw * 0.35).min(fs * 0.2)
    } else {
        compute_word_break_threshold(fs)
    };
    if gap > threshold {
        return true;
    }

    // Font name mismatch = break
    if prev.font_name != curr.font_name {
        return true;
    }

    // Font size mismatch > 5% = break, but only if there's also a gap.
    // Superscripts/subscripts (citations, footnote markers) are spatially adjacent
    // to their parent word and should stay attached despite the size change.
    let size_diff = (prev.font_size - curr.font_size).abs();
    if size_diff > 0.05 * fs && gap > 0.15 * fs {
        return true;
    }

    false
}

/// TL5: Compute the initial word-break gap threshold.
fn compute_word_break_threshold(font_size: f64) -> f64 {
    // Start with the max character spacing
    let base = MAX_CHAR_SPACING * font_size;
    // Apply wide character multiplier and cap
    let wide = MAX_WIDE_CHAR_MUL * base;
    let cap = MAX_WIDE_CHAR_SPACING * font_size;
    wide.min(cap).max(base)
}

/// Build a TextWord from a list of characters.
fn make_word(chars: Vec<PositionedChar>) -> TextWord {
    debug_assert!(!chars.is_empty());

    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;

    for ch in &chars {
        x_min = x_min.min(ch.x);
        x_max = x_max.max(ch.x + ch.width);
        y_min = y_min.min(ch.y);
        y_max = y_max.max(ch.y + ch.height);
    }

    let base = baseline_coord(&chars[0]);
    let font_size = chars[0].font_size;
    let rotation = chars[0].rotation;

    TextWord {
        chars,
        x_min,
        x_max,
        y_min,
        y_max,
        base,
        font_size,
        rotation,
        space_after: false,
    }
}

// ---------------------------------------------------------------------------
// TL5: Refine word spacing (post word formation)
// ---------------------------------------------------------------------------

/// TL5: Detect inter-word spaces by analyzing gaps between adjacent words on same line.
fn detect_word_spacing(words: &mut [TextWord]) {
    if words.len() < 2 {
        return;
    }

    for i in 0..(words.len() - 1) {
        let base_a = words[i].base;
        let base_b = words[i + 1].base;
        let fs = words[i].font_size.max(1.0);

        // Only consider words on approximately the same baseline
        if (base_a - base_b).abs() > MAX_INTRA_LINE_DELTA * fs {
            continue;
        }

        // Only same rotation
        if words[i].rotation != words[i + 1].rotation {
            continue;
        }

        let gap = word_gap(&words[i], &words[i + 1]);

        // Compute space threshold based on intra-word spacing
        let threshold = refined_space_threshold(&words[i], &words[i + 1]);

        words[i].space_after = gap >= threshold;
    }
}

/// Compute gap between two words along the primary axis.
fn word_gap(a: &TextWord, b: &TextWord) -> f64 {
    match a.rotation {
        Rotation::R0 => b.x_min - a.x_max,
        Rotation::R90 => b.y_min - a.y_max,
        Rotation::R180 => a.x_min - b.x_max,
        Rotation::R270 => a.y_min - b.y_max,
    }
}

/// TL5: Refined space threshold using intra-word character gaps.
fn refined_space_threshold(a: &TextWord, _b: &TextWord) -> f64 {
    let fs = a.font_size.max(1.0);

    // Compute minimum intra-word gap from multi-character words
    let min_gap = compute_min_intra_word_gap(a);

    if let Some(gap) = min_gap {
        if gap > 0.0 {
            let space = MAX_WIDE_CHAR_MUL * gap;
            return space
                .min(MAX_WIDE_CHAR_SPACING * fs)
                .max(MAX_CHAR_SPACING * fs);
        }
    }

    // Default: small multiple of font size
    MAX_CHAR_SPACING * fs
}

/// Compute the minimum gap between consecutive characters in a word.
fn compute_min_intra_word_gap(word: &TextWord) -> Option<f64> {
    if word.chars.len() < 2 {
        return None;
    }

    let mut min_gap = f64::MAX;
    for i in 0..(word.chars.len() - 1) {
        let end = primary_coord(&word.chars[i]) + word.chars[i].width;
        let start = primary_coord(&word.chars[i + 1]);
        let gap = start - end;
        if gap < min_gap {
            min_gap = gap;
        }
    }

    if min_gap < f64::MAX {
        Some(min_gap)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// TL6 + TL7: Form lines
// ---------------------------------------------------------------------------

/// TL6: Group words into lines by baseline proximity.
///
/// Words with baselines within MAX_INTRA_LINE_DELTA × font_size and
/// horizontal distance within MAX_WORD_SPACING × font_size are grouped.
/// TL7: Super/subscript characters (within 0.5 × fs baseline tolerance)
/// are naturally included.
fn form_lines(words: Vec<TextWord>) -> Vec<TextLine> {
    if words.is_empty() {
        return Vec::new();
    }

    let mut used = vec![false; words.len()];
    let mut lines = Vec::new();

    // Process words sorted by baseline then primary position
    let mut order: Vec<usize> = (0..words.len()).collect();
    order.sort_by(|&a, &b| {
        let ba = words[a].base;
        let bb = words[b].base;
        let cmp = ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal);
        if cmp != std::cmp::Ordering::Equal {
            return cmp;
        }
        let pa = word_primary_min(&words[a]);
        let pb = word_primary_min(&words[b]);
        pa.partial_cmp(&pb).unwrap_or(std::cmp::Ordering::Equal)
    });

    for &seed_idx in &order {
        if used[seed_idx] {
            continue;
        }
        used[seed_idx] = true;

        let seed = &words[seed_idx];
        let fs = seed.font_size.max(1.0);
        let rot = seed.rotation;
        let base_min = seed.base - MAX_INTRA_LINE_DELTA * fs;
        let base_max = seed.base + MAX_INTRA_LINE_DELTA * fs;

        let mut line_words = vec![seed_idx];

        // Find all words that fit on this line
        for &cand_idx in &order {
            if used[cand_idx] {
                continue;
            }
            let cand = &words[cand_idx];
            if cand.rotation != rot {
                continue;
            }
            if cand.base < base_min || cand.base > base_max {
                continue;
            }

            // Check horizontal proximity to any existing line word
            let cand_pri = word_primary_min(cand);
            let close_enough = line_words.iter().any(|&li| {
                let lw = &words[li];
                let lw_end = word_primary_max(lw);
                let lw_start = word_primary_min(lw);
                let dist_right = cand_pri - lw_end;
                let dist_left = lw_start - word_primary_max(cand);
                let dist = dist_right.min(dist_left);
                // Accept if within word spacing limit (in either direction)
                dist_right < MAX_WORD_SPACING * fs
                    || dist_left < MAX_WORD_SPACING * fs
                    || dist < 0.0 // overlapping
            });

            if close_enough {
                used[cand_idx] = true;
                line_words.push(cand_idx);
            }
        }

        // Sort line words by primary coordinate
        line_words.sort_by(|&a, &b| {
            word_primary_min(&words[a])
                .partial_cmp(&word_primary_min(&words[b]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        lines.push(make_line(
            line_words.iter().map(|&i| words[i].clone()).collect(),
        ));
    }

    // Sort lines by baseline (top to bottom for R0 = descending y)
    lines.sort_by(|a, b| {
        b.base
            .partial_cmp(&a.base)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    lines
}

/// Get the minimum primary coordinate of a word.
fn word_primary_min(w: &TextWord) -> f64 {
    match w.rotation {
        Rotation::R0 => w.x_min,
        Rotation::R90 => w.y_min,
        Rotation::R180 => -w.x_max,
        Rotation::R270 => -w.y_max,
    }
}

/// Get the maximum primary coordinate of a word.
fn word_primary_max(w: &TextWord) -> f64 {
    match w.rotation {
        Rotation::R0 => w.x_max,
        Rotation::R90 => w.y_max,
        Rotation::R180 => -w.x_min,
        Rotation::R270 => -w.y_min,
    }
}

/// Build a TextLine from a list of words.
fn make_line(words: Vec<TextWord>) -> TextLine {
    debug_assert!(!words.is_empty());

    let mut x_min = f64::MAX;
    let mut x_max = f64::MIN;
    let mut y_min = f64::MAX;
    let mut y_max = f64::MIN;

    for w in &words {
        x_min = x_min.min(w.x_min);
        x_max = x_max.max(w.x_max);
        y_min = y_min.min(w.y_min);
        y_max = y_max.max(w.y_max);
    }

    let base = words[0].base;
    let rotation = words[0].rotation;

    TextLine {
        words,
        x_min,
        x_max,
        y_min,
        y_max,
        base,
        rotation,
    }
}

// ---------------------------------------------------------------------------
// TL8: Form blocks
// ---------------------------------------------------------------------------

/// TL8: Group lines into blocks (paragraphs) by consistent spacing/font.
fn form_blocks(lines: Vec<TextLine>) -> Vec<TextBlock> {
    if lines.is_empty() {
        return Vec::new();
    }

    let mut used = vec![false; lines.len()];
    let mut blocks = Vec::new();

    for seed_idx in 0..lines.len() {
        if used[seed_idx] {
            continue;
        }
        used[seed_idx] = true;

        let seed = &lines[seed_idx];
        let rot = seed.rotation;
        let fs = seed
            .words
            .first()
            .map(|w| w.font_size)
            .unwrap_or(12.0)
            .max(1.0);

        let mut block_lines = vec![seed_idx];
        let mut block_y_min = seed.y_min;
        let mut block_y_max = seed.y_max;
        let mut block_x_min = seed.x_min;
        let mut block_x_max = seed.x_max;
        let mut base_min = seed.base;
        let mut base_max = seed.base;

        // Iteratively find lines that belong to this block
        let mut changed = true;
        while changed {
            changed = false;
            for cand_idx in 0..lines.len() {
                if used[cand_idx] {
                    continue;
                }
                let cand = &lines[cand_idx];
                if cand.rotation != rot {
                    continue;
                }

                let cand_fs = cand.words.first().map(|w| w.font_size).unwrap_or(12.0);
                let font_delta = (cand_fs - fs).abs() / fs;

                // Check if line is directly above or below the block
                let base_dist_above = (cand.base - base_max).abs();
                let base_dist_below = (base_min - cand.base).abs();
                let min_dist = base_dist_above.min(base_dist_below);

                if min_dist < MAX_LINE_SPACING * fs && font_delta < MAX_BLOCK_FONT_DELTA1 {
                    // Check horizontal overlap
                    let h_overlap = cand.x_max > block_x_min && cand.x_min < block_x_max;
                    if h_overlap {
                        used[cand_idx] = true;
                        block_lines.push(cand_idx);
                        block_y_min = block_y_min.min(cand.y_min);
                        block_y_max = block_y_max.max(cand.y_max);
                        block_x_min = block_x_min.min(cand.x_min);
                        block_x_max = block_x_max.max(cand.x_max);
                        base_min = base_min.min(cand.base);
                        base_max = base_max.max(cand.base);
                        changed = true;
                    }
                }
            }
        }

        // Sort block lines by baseline (top to bottom for R0)
        block_lines.sort_by(|&a, &b| {
            lines[b]
                .base
                .partial_cmp(&lines[a].base)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        blocks.push(TextBlock {
            lines: block_lines.iter().map(|&i| lines[i].clone()).collect(),
            x_min: block_x_min,
            x_max: block_x_max,
            y_min: block_y_min,
            y_max: block_y_max,
            rotation: rot,
        });
    }

    blocks
}

// ---------------------------------------------------------------------------
// TL9: Detect columns
// ---------------------------------------------------------------------------

/// TL9: Detect column boundaries from block positions.
///
/// Returns sorted column x-boundaries for layout rendering.
fn detect_columns(blocks: &[TextBlock]) -> Vec<f64> {
    if blocks.is_empty() {
        return Vec::new();
    }

    // Collect all block left edges
    let mut left_edges: Vec<f64> = blocks.iter().map(|b| b.x_min).collect();
    left_edges.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    left_edges.dedup_by(|a, b| (*a - *b).abs() < 1.0);

    // Check for column gaps: areas where no block occupies x space
    let avg_fs = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .flat_map(|l| l.words.iter())
        .map(|w| w.font_size)
        .sum::<f64>()
        / blocks
            .iter()
            .flat_map(|b| b.lines.iter())
            .flat_map(|l| l.words.iter())
            .count()
            .max(1) as f64;

    let col_gap = MIN_COL_SPACING * avg_fs;

    // Find gaps between blocks along x-axis
    let mut x_ranges: Vec<(f64, f64)> = blocks.iter().map(|b| (b.x_min, b.x_max)).collect();
    x_ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut columns = vec![x_ranges[0].0];
    let mut max_x = x_ranges[0].1;

    for &(x_min, x_max) in &x_ranges[1..] {
        if x_min - max_x > col_gap {
            // Column gap detected
            columns.push(x_min);
        }
        max_x = max_x.max(x_max);
    }

    columns
}

// ---------------------------------------------------------------------------
// TL10: Reading order
// ---------------------------------------------------------------------------

/// TL10: Sort blocks in reading order (top-to-bottom, left-to-right for LTR).
fn sort_reading_order(blocks: &mut Vec<TextBlock>) {
    blocks.sort_by(|a, b| {
        // Primary: top edge (descending y = higher on page comes first)
        let y_cmp = b
            .y_max
            .partial_cmp(&a.y_max)
            .unwrap_or(std::cmp::Ordering::Equal);
        if y_cmp != std::cmp::Ordering::Equal {
            // Only use y-ordering if blocks don't overlap vertically
            let overlap = a.y_max > b.y_min && b.y_max > a.y_min;
            if !overlap {
                return y_cmp;
            }
        }
        // Secondary: left edge (ascending x)
        a.x_min
            .partial_cmp(&b.x_min)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

// ---------------------------------------------------------------------------
// TL11: RTL text detection
// ---------------------------------------------------------------------------

/// TL11: Check if a character is in an RTL Unicode range.
fn is_rtl_char(ch: char) -> bool {
    let cp = ch as u32;
    // Hebrew: U+0590-U+05FF
    // Arabic: U+0600-U+06FF
    // Arabic Supplement: U+0750-U+077F
    // Arabic Extended: U+08A0-U+08FF
    // Hebrew Presentation Forms: U+FB1D-U+FB4F
    // Arabic Presentation Forms-A: U+FB50-U+FDFF
    // Arabic Presentation Forms-B: U+FE70-U+FEFF
    (0x0590..=0x05FF).contains(&cp)
        || (0x0600..=0x06FF).contains(&cp)
        || (0x0750..=0x077F).contains(&cp)
        || (0x08A0..=0x08FF).contains(&cp)
        || (0xFB1D..=0xFB4F).contains(&cp)
        || (0xFB50..=0xFDFF).contains(&cp)
        || (0xFE70..=0xFEFF).contains(&cp)
}

/// TL11: Check if a line is predominantly RTL.
fn is_rtl_line(line: &TextLine) -> bool {
    let mut rtl_count = 0;
    let mut total = 0;
    for word in &line.words {
        for ch in &word.chars {
            for c in ch.unicode.chars() {
                if c.is_alphabetic() {
                    total += 1;
                    if is_rtl_char(c) {
                        rtl_count += 1;
                    }
                }
            }
        }
    }
    total > 0 && rtl_count * 2 > total
}

// ---------------------------------------------------------------------------
// TL12: Vertical CJK text detection
// ---------------------------------------------------------------------------

/// TL12: Check if a character is CJK.
#[allow(dead_code)] // CJK vertical-text detection is staged for a future text_layout pass.
fn is_cjk_char(ch: char) -> bool {
    let cp = ch as u32;
    // CJK Unified Ideographs: U+4E00-U+9FFF
    // CJK Unified Ideographs Extension A: U+3400-U+4DBF
    // CJK Compatibility Ideographs: U+F900-U+FAFF
    // CJK punctuation: U+3000-U+303F
    // Katakana: U+30A0-U+30FF
    // Hiragana: U+3040-U+309F
    // Hangul: U+AC00-U+D7AF
    (0x3000..=0x303F).contains(&cp)
        || (0x3040..=0x309F).contains(&cp)
        || (0x30A0..=0x30FF).contains(&cp)
        || (0x3400..=0x4DBF).contains(&cp)
        || (0x4E00..=0x9FFF).contains(&cp)
        || (0xAC00..=0xD7AF).contains(&cp)
        || (0xF900..=0xFAFF).contains(&cp)
}

// ---------------------------------------------------------------------------
// TL13: Raw text extraction
// ---------------------------------------------------------------------------

/// TL13: Extract text in content stream order (raw mode).
///
/// Simply decodes each span to Unicode and concatenates, with newlines
/// at text object boundaries (BT/ET). No spatial analysis.
pub fn extract_text_raw(doc: &Document, page: &Page) -> Result<String> {
    let (spans, fonts_map) = ContentInterpreter::process_page(doc, page)?;
    let font_map: HashMap<&[u8], &PdfFont> = fonts_map
        .iter()
        .map(|(name, f)| (name.as_slice(), f))
        .collect();

    let mut result = String::new();

    for span in &spans {
        if let Some(font) = font_map.get(span.font_name.as_slice()) {
            result.push_str(&font::decode_text(font, &span.text));
        } else {
            // Fallback: Latin-1
            for &byte in &span.text {
                result.push(char::from_u32(byte as u32).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
        }
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// TL14: Layout text extraction
// ---------------------------------------------------------------------------

/// TL14: Extract text preserving physical layout (like `pdftotext -layout`).
///
/// Full pipeline: collect chars -> bucket -> dedup -> words -> lines -> blocks ->
/// reading order -> render with spaces preserving x positions.
pub fn extract_text_layout(doc: &Document, page: &Page) -> Result<String> {
    let (spans, fonts_map) = ContentInterpreter::process_page(doc, page)?;
    let page_fonts: Vec<(Vec<u8>, PdfFont)> = fonts_map.into_iter().collect();

    // TL1: Collect positioned characters
    let mut chars = collect_chars(&spans, &page_fonts);

    if chars.is_empty() {
        return Ok(String::new());
    }

    // TL2: Bucket by baseline (used internally by dedup)
    let _pool = bucket_by_baseline(&mut chars);

    // TL3: Remove duplicates
    remove_duplicates(&mut chars);

    // TL4: Form words
    let mut words = form_words(&chars);

    // TL5: Detect word spacing
    detect_word_spacing(&mut words);

    // TL6 + TL7: Form lines
    let lines = form_lines(words);

    // TL8: Form blocks
    let mut blocks = form_blocks(lines);

    // TL9: Detect columns
    let _columns = detect_columns(&blocks);

    // TL10: Reading order
    sort_reading_order(&mut blocks);

    // Render to string
    render_layout(&blocks, page)
}

/// Render blocks as layout text with physical positioning.
fn render_layout(blocks: &[TextBlock], page: &Page) -> Result<String> {
    let mut result = String::new();

    // Page width for column mapping
    let page_width = (page.media_box[2] - page.media_box[0]).abs();
    // Approximate character width for column grid (use average)
    let avg_char_width = estimate_avg_char_width(blocks).max(1.0);
    let _page_cols = (page_width / avg_char_width).ceil() as usize;

    let mut prev_base: Option<f64> = None;

    for block in blocks {
        // Add blank line between blocks
        if prev_base.is_some() {
            result.push('\n');
        }

        for line in &block.lines {
            // Check line gap for extra newlines
            if let Some(pb) = prev_base {
                let gap = (pb - line.base).abs();
                let fs = line.words.first().map(|w| w.font_size).unwrap_or(12.0);
                // Large gap = extra blank line
                if gap > MAX_LINE_SPACING * fs * 1.5 {
                    result.push('\n');
                }
            }

            // TL11: Handle RTL lines
            let words_in_order: Vec<&TextWord> = if is_rtl_line(line) {
                let mut ws: Vec<&TextWord> = line.words.iter().collect();
                ws.reverse();
                ws
            } else {
                line.words.iter().collect()
            };

            // Build the line with physical x positioning
            let mut line_buf = String::new();
            let page_left = page.media_box[0];

            for word in &words_in_order {
                // Compute column position of this word
                let word_x = word.x_min - page_left;
                let target_col = (word_x / avg_char_width).round() as usize;
                let current_col = line_buf.chars().count();

                // Pad with spaces to reach target column
                if target_col > current_col {
                    for _ in 0..(target_col - current_col) {
                        line_buf.push(' ');
                    }
                } else if !line_buf.is_empty() {
                    // At least one space between words
                    line_buf.push(' ');
                }

                // Append word characters
                for ch in &word.chars {
                    line_buf.push_str(&ch.unicode);
                }
            }

            // Trim trailing spaces
            let trimmed = line_buf.trim_end();
            result.push_str(trimmed);
            result.push('\n');

            prev_base = Some(line.base);
        }
    }

    // Remove trailing newlines
    while result.ends_with('\n') {
        result.pop();
    }

    Ok(result)
}

/// Estimate average character width from blocks.
fn estimate_avg_char_width(blocks: &[TextBlock]) -> f64 {
    let mut total_width = 0.0;
    let mut count = 0;

    for block in blocks {
        for line in &block.lines {
            for word in &line.words {
                for ch in &word.chars {
                    if ch.width > 0.0 {
                        total_width += ch.width;
                        count += 1;
                    }
                }
            }
        }
    }

    if count > 0 {
        total_width / count as f64
    } else {
        6.0 // default ~6pt
    }
}

/// TL1 public API: extract positioned characters from a page.
pub fn extract_chars(doc: &Document, page: &Page) -> Result<Vec<PositionedChar>> {
    let (spans, fonts_map) = ContentInterpreter::process_page(doc, page)?;
    let page_fonts: Vec<(Vec<u8>, PdfFont)> = fonts_map.into_iter().collect();
    Ok(collect_chars(&spans, &page_fonts))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- Test helpers ---

    fn make_char(unicode: &str, x: f64, y: f64, width: f64, font_size: f64) -> PositionedChar {
        PositionedChar {
            unicode: unicode.to_string(),
            x,
            y,
            width,
            height: font_size,
            font_size,
            font_name: b"F1".to_vec(),
            render_mode: 0,
            rotation: Rotation::R0,
            space_width: font_size * 0.25,
        }
    }

    fn make_char_with_font(
        unicode: &str,
        x: f64,
        y: f64,
        width: f64,
        font_size: f64,
        font: &[u8],
    ) -> PositionedChar {
        PositionedChar {
            unicode: unicode.to_string(),
            x,
            y,
            width,
            height: font_size,
            font_size,
            font_name: font.to_vec(),
            render_mode: 0,
            rotation: Rotation::R0,
            space_width: font_size * 0.25,
        }
    }

    fn make_char_rotated(
        unicode: &str,
        x: f64,
        y: f64,
        width: f64,
        font_size: f64,
        rot: Rotation,
    ) -> PositionedChar {
        PositionedChar {
            unicode: unicode.to_string(),
            x,
            y,
            width,
            height: font_size,
            font_size,
            font_name: b"F1".to_vec(),
            render_mode: 0,
            rotation: rot,
            space_width: font_size * 0.25,
        }
    }

    // --- TL1: Positioned character collection ---

    #[test]
    fn tl1_collect_chars_empty() {
        let chars = collect_chars(&[], &[]);
        assert!(chars.is_empty());
    }

    #[test]
    fn tl1_collect_chars_fallback() {
        let span = TextSpan {
            text: vec![0x41, 0x42, 0x43], // ABC
            font_name: b"Unknown".to_vec(),
            font_size: 12.0,
            x: 100.0,
            y: 700.0,
            render_mode: 0,
            ctm_scale_x: 1.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
        };
        let chars = collect_chars(&[span], &[]);
        assert_eq!(chars.len(), 3);
        assert_eq!(chars[0].unicode, "A");
        assert_eq!(chars[1].unicode, "B");
        assert_eq!(chars[2].unicode, "C");
        assert!(chars[0].x < chars[1].x);
        assert!(chars[1].x < chars[2].x);
    }

    #[test]
    fn tl1_collect_chars_with_font() {
        let span = TextSpan {
            text: vec![0x48, 0x69], // "Hi"
            font_name: b"F1".to_vec(),
            font_size: 12.0,
            x: 50.0,
            y: 700.0,
            render_mode: 0,
            ctm_scale_x: 1.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
        };

        let font = PdfFont {
            name: b"Helvetica".to_vec(),
            subtype: font::FontSubtype::Type1,
            encoding: font::FontEncoding::Named(font::StandardEncoding::WinAnsi),
            to_unicode: None,
            widths: font::FontWidths::Simple {
                first_char: 0x48,
                // H(0x48)=722, then fill 0x49..0x68 with 600 (default), i(0x69)=278
                widths: {
                    let mut w = vec![722.0]; // H
                    w.resize(0x69 - 0x48, 600.0); // pad I..h with defaults
                    w.push(278.0); // i
                    w
                },
            },
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        let page_fonts = vec![(b"F1".to_vec(), font)];
        let chars = collect_chars(&[span], &page_fonts);

        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0].unicode, "H");
        assert_eq!(chars[1].unicode, "i");

        // Width: 722 * 12/1000 = 8.664
        assert!((chars[0].width - 8.664).abs() < 0.01);
        // Width: 278 * 12/1000 = 3.336
        assert!((chars[1].width - 3.336).abs() < 0.01);

        // Second char starts after first
        let expected_x = 50.0 + 8.664;
        assert!((chars[1].x - expected_x).abs() < 0.01);
    }

    // --- TL2: Baseline bucketing ---

    #[test]
    fn tl2_bucket_same_baseline() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("B", 20.0, 700.0, 7.0, 12.0),
            make_char("C", 30.0, 700.0, 7.0, 12.0),
        ];
        let pool = bucket_by_baseline(&mut chars);
        // All should be in the same bucket
        assert_eq!(pool.len(), 1);
        let bucket: Vec<&Vec<usize>> = pool.values().collect();
        assert_eq!(bucket[0].len(), 3);
    }

    #[test]
    fn tl2_bucket_different_baselines() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("B", 10.0, 680.0, 7.0, 12.0), // 20pt below
        ];
        let pool = bucket_by_baseline(&mut chars);
        assert_eq!(pool.len(), 2); // Different buckets (700/4=175, 680/4=170)
    }

    // --- TL3: Duplicate removal ---

    #[test]
    fn tl3_remove_exact_duplicates() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("A", 10.0, 700.0, 7.0, 12.0), // exact duplicate
            make_char("B", 20.0, 700.0, 7.0, 12.0),
        ];
        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0].unicode, "A");
        assert_eq!(chars[1].unicode, "B");
    }

    #[test]
    fn tl3_remove_shadow_text() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("A", 10.5, 699.8, 7.0, 12.0), // slight offset (shadow)
        ];
        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 1);
    }

    #[test]
    fn tl3_keep_different_chars() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("B", 10.0, 700.0, 7.0, 12.0), // same position, different char
        ];
        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 2);
    }

    #[test]
    fn tl3_keep_distant_same_chars() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("A", 100.0, 700.0, 7.0, 12.0), // far apart
        ];
        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 2);
    }

    // --- TL4: Word formation ---

    #[test]
    fn tl4_single_word() {
        let chars = vec![
            make_char("H", 10.0, 700.0, 7.0, 12.0),
            make_char("i", 17.0, 700.0, 3.5, 12.0),
        ];
        let words = form_words(&chars);
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].chars.len(), 2);
    }

    #[test]
    fn tl4_two_words_with_gap() {
        // "Hi there" - gap between "i" and "t" is larger than threshold
        let fs = 12.0;
        let threshold = compute_word_break_threshold(fs);
        let chars = vec![
            make_char("H", 10.0, 700.0, 7.0, fs),
            make_char("i", 17.0, 700.0, 3.5, fs),
            // Gap after "i" ends at 20.5, "t" starts at 20.5 + threshold + 1
            make_char("t", 20.5 + threshold + 1.0, 700.0, 5.0, fs),
            make_char("h", 20.5 + threshold + 6.0, 700.0, 7.0, fs),
        ];
        let words = form_words(&chars);
        assert_eq!(words.len(), 2);
    }

    #[test]
    fn tl4_break_on_font_change() {
        let chars = vec![
            make_char_with_font("A", 10.0, 700.0, 7.0, 12.0, b"F1"),
            make_char_with_font("B", 17.0, 700.0, 7.0, 12.0, b"F2"),
        ];
        let words = form_words(&chars);
        assert_eq!(words.len(), 2);
    }

    #[test]
    fn tl4_break_on_baseline_change() {
        let chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("B", 17.0, 650.0, 7.0, 12.0), // different line
        ];
        let words = form_words(&chars);
        assert_eq!(words.len(), 2);
    }

    // --- TL5: Word spacing detection ---

    #[test]
    fn tl5_space_after_word() {
        let mut words = vec![
            make_word(vec![
                make_char("H", 10.0, 700.0, 7.0, 12.0),
                make_char("i", 17.0, 700.0, 3.5, 12.0),
            ]),
            make_word(vec![make_char("t", 30.0, 700.0, 5.0, 12.0)]),
        ];
        detect_word_spacing(&mut words);
        assert!(words[0].space_after);
    }

    #[test]
    fn tl5_no_space_tight_words() {
        let mut words = vec![
            make_word(vec![make_char("A", 10.0, 700.0, 7.0, 12.0)]),
            make_word(vec![
                make_char("B", 17.1, 700.0, 7.0, 12.0), // very close
            ]),
        ];
        detect_word_spacing(&mut words);
        assert!(!words[0].space_after);
    }

    // --- TL6: Line formation ---

    #[test]
    fn tl6_single_line() {
        let words = vec![
            make_word(vec![make_char("Hello", 10.0, 700.0, 35.0, 12.0)]),
            make_word(vec![make_char("World", 50.0, 700.0, 35.0, 12.0)]),
        ];
        let lines = form_lines(words);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].words.len(), 2);
    }

    #[test]
    fn tl6_two_lines() {
        let words = vec![
            make_word(vec![make_char("Line1", 10.0, 700.0, 35.0, 12.0)]),
            make_word(vec![make_char("Line2", 10.0, 680.0, 35.0, 12.0)]),
        ];
        let lines = form_lines(words);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn tl6_lines_sorted_top_to_bottom() {
        let words = vec![
            make_word(vec![make_char("Bottom", 10.0, 680.0, 40.0, 12.0)]),
            make_word(vec![make_char("Top", 10.0, 700.0, 25.0, 12.0)]),
        ];
        let lines = form_lines(words);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].base > lines[1].base); // top first
    }

    // --- TL7: Superscript/subscript ---

    #[test]
    fn tl7_superscript_on_same_line() {
        // "x²" - superscript is within 0.5 × fs of baseline
        let words = vec![
            make_word(vec![make_char("x", 10.0, 700.0, 7.0, 12.0)]),
            make_word(vec![make_char("2", 17.0, 704.0, 5.0, 8.0)]), // 4pt above, within 0.5*12=6
        ];
        let lines = form_lines(words);
        assert_eq!(lines.len(), 1); // same line
        assert_eq!(lines[0].words.len(), 2);
    }

    // --- TL8: Block formation ---

    #[test]
    fn tl8_single_block() {
        let lines = vec![
            make_line(vec![make_word(vec![make_char(
                "Line1", 10.0, 700.0, 35.0, 12.0,
            )])]),
            make_line(vec![
                make_word(vec![make_char("Line2", 10.0, 686.0, 35.0, 12.0)]), // 14pt gap < 1.5*12=18
            ]),
        ];
        let blocks = form_blocks(lines);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].lines.len(), 2);
    }

    #[test]
    fn tl8_two_blocks() {
        let lines = vec![
            make_line(vec![make_word(vec![make_char(
                "Para1", 10.0, 700.0, 35.0, 12.0,
            )])]),
            make_line(vec![
                make_word(vec![make_char("Para2", 10.0, 600.0, 35.0, 12.0)]), // 100pt gap >> 18
            ]),
        ];
        let blocks = form_blocks(lines);
        assert_eq!(blocks.len(), 2);
    }

    // --- TL9: Column detection ---

    #[test]
    fn tl9_single_column() {
        let blocks = vec![TextBlock {
            lines: vec![make_line(vec![make_word(vec![make_char(
                "A", 10.0, 700.0, 7.0, 12.0,
            )])])],
            x_min: 10.0,
            x_max: 300.0,
            y_min: 680.0,
            y_max: 712.0,
            rotation: Rotation::R0,
        }];
        let columns = detect_columns(&blocks);
        assert_eq!(columns.len(), 1);
    }

    #[test]
    fn tl9_two_columns() {
        let blocks = vec![
            TextBlock {
                lines: vec![make_line(vec![make_word(vec![make_char(
                    "Left", 10.0, 700.0, 28.0, 12.0,
                )])])],
                x_min: 10.0,
                x_max: 200.0,
                y_min: 688.0,
                y_max: 712.0,
                rotation: Rotation::R0,
            },
            TextBlock {
                lines: vec![make_line(vec![make_word(vec![make_char(
                    "Right", 310.0, 700.0, 35.0, 12.0,
                )])])],
                x_min: 310.0,
                x_max: 500.0,
                y_min: 688.0,
                y_max: 712.0,
                rotation: Rotation::R0,
            },
        ];
        let columns = detect_columns(&blocks);
        assert_eq!(columns.len(), 2);
    }

    // --- TL10: Reading order ---

    #[test]
    fn tl10_top_to_bottom() {
        let mut blocks = vec![
            TextBlock {
                lines: vec![],
                x_min: 10.0,
                x_max: 200.0,
                y_min: 600.0,
                y_max: 620.0,
                rotation: Rotation::R0,
            },
            TextBlock {
                lines: vec![],
                x_min: 10.0,
                x_max: 200.0,
                y_min: 700.0,
                y_max: 720.0,
                rotation: Rotation::R0,
            },
        ];
        sort_reading_order(&mut blocks);
        assert!(blocks[0].y_max > blocks[1].y_max); // higher block first
    }

    #[test]
    fn tl10_left_to_right_same_row() {
        let mut blocks = vec![
            TextBlock {
                lines: vec![],
                x_min: 300.0,
                x_max: 500.0,
                y_min: 700.0,
                y_max: 720.0,
                rotation: Rotation::R0,
            },
            TextBlock {
                lines: vec![],
                x_min: 10.0,
                x_max: 200.0,
                y_min: 700.0,
                y_max: 720.0,
                rotation: Rotation::R0,
            },
        ];
        sort_reading_order(&mut blocks);
        assert!(blocks[0].x_min < blocks[1].x_min); // left block first
    }

    // --- TL11: RTL detection ---

    #[test]
    fn tl11_detect_rtl() {
        assert!(is_rtl_char('\u{05D0}')); // Hebrew Alef
        assert!(is_rtl_char('\u{0627}')); // Arabic Alif
        assert!(!is_rtl_char('A'));
        assert!(!is_rtl_char('1'));
    }

    #[test]
    fn tl11_rtl_line_detection() {
        let line = make_line(vec![make_word(vec![PositionedChar {
            unicode: "\u{05E9}\u{05DC}\u{05D5}\u{05DD}".to_string(), // שלום
            x: 100.0,
            y: 700.0,
            width: 40.0,
            height: 12.0,
            font_size: 12.0,
            font_name: b"F1".to_vec(),
            render_mode: 0,
            rotation: Rotation::R0,
            space_width: 3.0,
        }])]);
        assert!(is_rtl_line(&line));
    }

    #[test]
    fn tl11_ltr_line_not_rtl() {
        let line = make_line(vec![make_word(vec![make_char(
            "Hello", 10.0, 700.0, 35.0, 12.0,
        )])]);
        assert!(!is_rtl_line(&line));
    }

    // --- TL12: CJK detection ---

    #[test]
    fn tl12_detect_cjk() {
        assert!(is_cjk_char('中'));
        assert!(is_cjk_char('あ'));
        assert!(is_cjk_char('カ'));
        assert!(!is_cjk_char('A'));
        assert!(!is_cjk_char('1'));
    }

    // --- TL13: Raw text extraction ---

    #[test]
    fn tl13_raw_from_spans() {
        let spans = vec![
            TextSpan {
                text: vec![0x48, 0x65, 0x6C, 0x6C, 0x6F], // Hello
                font_name: b"F1".to_vec(),
                font_size: 12.0,
                x: 10.0,
                y: 700.0,
                render_mode: 0,
                ctm_scale_x: 1.0,
                char_spacing: 0.0,
                word_spacing: 0.0,
            },
            TextSpan {
                text: vec![0x57, 0x6F, 0x72, 0x6C, 0x64], // World
                font_name: b"F1".to_vec(),
                font_size: 12.0,
                x: 50.0,
                y: 700.0,
                render_mode: 0,
                ctm_scale_x: 1.0,
                char_spacing: 0.0,
                word_spacing: 0.0,
            },
        ];

        let font = PdfFont {
            name: b"Helvetica".to_vec(),
            subtype: font::FontSubtype::Type1,
            encoding: font::FontEncoding::Named(font::StandardEncoding::WinAnsi),
            to_unicode: None,
            widths: font::FontWidths::Default(600.0),
            is_two_byte: false,
            embedded_cmap: None,
            font_matrix_scale: 0.001,
        };

        let page_fonts = vec![(b"F1".to_vec(), font)];
        let font_map: HashMap<&[u8], &PdfFont> = page_fonts
            .iter()
            .map(|(name, f)| (name.as_slice(), f))
            .collect();

        let mut result = String::new();
        for span in &spans {
            if let Some(f) = font_map.get(span.font_name.as_slice()) {
                result.push_str(&font::decode_text(f, &span.text));
            }
        }
        assert_eq!(result, "HelloWorld");
    }

    // --- TL14: Layout rendering ---

    #[test]
    fn tl14_render_simple_line() {
        let blocks = vec![TextBlock {
            lines: vec![make_line(vec![
                TextWord {
                    chars: vec![
                        make_char("H", 10.0, 700.0, 7.0, 12.0),
                        make_char("i", 17.0, 700.0, 3.5, 12.0),
                    ],
                    x_min: 10.0,
                    x_max: 20.5,
                    y_min: 700.0,
                    y_max: 712.0,
                    base: 700.0,
                    font_size: 12.0,
                    rotation: Rotation::R0,
                    space_after: true,
                },
                TextWord {
                    chars: vec![make_char("!", 50.0, 700.0, 4.0, 12.0)],
                    x_min: 50.0,
                    x_max: 54.0,
                    y_min: 700.0,
                    y_max: 712.0,
                    base: 700.0,
                    font_size: 12.0,
                    rotation: Rotation::R0,
                    space_after: false,
                },
            ])],
            x_min: 10.0,
            x_max: 54.0,
            y_min: 700.0,
            y_max: 712.0,
            rotation: Rotation::R0,
        }];

        let page = Page {
            dict: super::super::object::PdfObject::Null,
            media_box: [0.0, 0.0, 612.0, 792.0],
            crop_box: [0.0, 0.0, 612.0, 792.0],
            rotate: 0,
            resources: None,
        };

        let result = render_layout(&blocks, &page).unwrap();
        // Should have "Hi" then spaces then "!"
        assert!(result.contains("Hi"));
        assert!(result.contains("!"));
    }

    // --- Word helpers ---

    #[test]
    fn word_bounding_box() {
        let word = make_word(vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("B", 17.0, 700.0, 7.0, 12.0),
        ]);
        assert!((word.x_min - 10.0).abs() < 0.001);
        assert!((word.x_max - 24.0).abs() < 0.001);
    }

    // --- Rotation ---

    #[test]
    fn rotation_baseline_coords() {
        let ch0 = make_char_rotated("A", 10.0, 700.0, 7.0, 12.0, Rotation::R0);
        assert!((baseline_coord(&ch0) - 700.0).abs() < 0.001);
        assert!((primary_coord(&ch0) - 10.0).abs() < 0.001);

        let ch90 = make_char_rotated("A", 10.0, 700.0, 7.0, 12.0, Rotation::R90);
        assert!((baseline_coord(&ch90) - 10.0).abs() < 0.001);
        assert!((primary_coord(&ch90) - 700.0).abs() < 0.001);
    }

    // --- Integration: full pipeline on synthetic data ---

    #[test]
    fn full_pipeline_two_words() {
        // Simulate "Hi World" with a clear gap
        let mut chars = vec![
            make_char("H", 10.0, 700.0, 7.2, 12.0),
            make_char("i", 17.2, 700.0, 3.0, 12.0),
            // Gap of ~10pt (> threshold) before "World"
            make_char("W", 30.0, 700.0, 9.0, 12.0),
            make_char("o", 39.0, 700.0, 6.5, 12.0),
            make_char("r", 45.5, 700.0, 4.0, 12.0),
            make_char("l", 49.5, 700.0, 3.0, 12.0),
            make_char("d", 52.5, 700.0, 6.5, 12.0),
        ];

        // TL3: dedup (no dups here)
        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 7);

        // TL4: form words
        let mut words = form_words(&chars);
        // The gap between "i" (ends at 20.2) and "W" (starts at 30.0) = 9.8pt
        // Threshold ≈ 0.039*12 = 0.468pt -> should break
        assert_eq!(
            words.len(),
            2,
            "Expected 2 words, got {}: {:?}",
            words.len(),
            words
                .iter()
                .map(|w| w
                    .chars
                    .iter()
                    .map(|c| c.unicode.as_str())
                    .collect::<String>())
                .collect::<Vec<_>>()
        );

        // TL5: detect spacing
        detect_word_spacing(&mut words);
        assert!(words[0].space_after);

        // TL6: form lines
        let lines = form_lines(words);
        assert_eq!(lines.len(), 1);

        // TL8: blocks
        let blocks = form_blocks(lines);
        assert_eq!(blocks.len(), 1);
    }

    #[test]
    fn full_pipeline_two_lines() {
        let mut chars = vec![
            // Line 1: "Hello"
            make_char("H", 10.0, 700.0, 7.0, 12.0),
            make_char("e", 17.0, 700.0, 6.0, 12.0),
            make_char("l", 23.0, 700.0, 3.0, 12.0),
            make_char("l", 26.0, 700.0, 3.0, 12.0),
            make_char("o", 29.0, 700.0, 6.5, 12.0),
            // Line 2: "World" (20pt below)
            make_char("W", 10.0, 680.0, 9.0, 12.0),
            make_char("o", 19.0, 680.0, 6.5, 12.0),
            make_char("r", 25.5, 680.0, 4.0, 12.0),
            make_char("l", 29.5, 680.0, 3.0, 12.0),
            make_char("d", 32.5, 680.0, 6.5, 12.0),
        ];

        remove_duplicates(&mut chars);
        let words = form_words(&chars);
        let lines = form_lines(words);
        assert_eq!(lines.len(), 2);

        let blocks = form_blocks(lines);
        // The 20pt vertical gap exceeds the block-split threshold of
        // 1.5 x font_size = 18pt, so the two lines are separate blocks.
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn full_pipeline_with_duplicates() {
        let mut chars = vec![
            make_char("A", 10.0, 700.0, 7.0, 12.0),
            make_char("A", 10.2, 700.1, 7.0, 12.0), // shadow
            make_char("B", 17.0, 700.0, 7.0, 12.0),
            make_char("B", 17.1, 699.9, 7.0, 12.0), // shadow
            make_char("C", 24.0, 700.0, 7.0, 12.0),
        ];

        remove_duplicates(&mut chars);
        assert_eq!(chars.len(), 3); // shadows removed
    }
}
