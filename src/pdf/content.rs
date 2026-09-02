//! PDF content stream interpreter (CS1-CS21).
//!
//! Interprets page content streams to extract positioned text spans.
//! Per ISO 32000-2 §7.8.2, §8.4, §9.3-9.4.

use super::decode;
use super::document::{Document, Page};
use super::font::{self, PdfFont};
use super::object::PdfObject;
use crate::core::Result;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// CS19: Default glyph width (used until font metrics are available)
// ---------------------------------------------------------------------------

/// Default glyph width in 1/1000 units. Assumes monospace-like spacing.
/// Replaced with actual font metrics in the font/encoding phase.
const DEFAULT_GLYPH_WIDTH: f64 = 600.0;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A positioned text span extracted from the content stream.
#[derive(Debug, Clone)]
pub struct TextSpan {
    /// Raw bytes (not yet Unicode - font decoding is a later phase).
    pub text: Vec<u8>,
    /// Unique font key (resource name plus font object identity), resolving
    /// into the font map returned by `process_page`. Not the bare Tf name:
    /// two scopes may bind the same name to different fonts.
    pub font_name: Vec<u8>,
    /// Font size from Tf.
    pub font_size: f64,
    /// X position in user space (CTM × Tm applied).
    pub x: f64,
    /// Y position in user space (CTM × Tm applied).
    pub y: f64,
    /// Text rendering mode (0-7). Mode 3 = invisible.
    pub render_mode: u8,
    /// Horizontal scale factor from CTM × Tm (for computing glyph widths in page space).
    pub ctm_scale_x: f64,
    /// Character spacing (Tc) - extra advance added after each glyph.
    pub char_spacing: f64,
    /// Word spacing (Tw) - extra advance added after space glyphs (code 32).
    pub word_spacing: f64,
}

// ---------------------------------------------------------------------------
// CS9-CS14: Text state
// ---------------------------------------------------------------------------

/// Text state parameters (ISO 32000-2 §9.3).
#[derive(Debug, Clone)]
struct TextState {
    /// CS5: Current font resource name.
    font_name: Vec<u8>,
    /// CS5: Current font size.
    font_size: f64,
    /// CS10: Character spacing (Tc).
    char_spacing: f64,
    /// CS11: Word spacing (Tw).
    word_spacing: f64,
    /// CS12: Horizontal scaling (Tz) - percentage, default 100.
    horiz_scaling: f64,
    /// CS9: Text leading (TL).
    leading: f64,
    /// CS14: Text rise (Ts).
    rise: f64,
    /// CS13: Text rendering mode (Tr).
    render_mode: u8,
    /// CS6: Text matrix - set by Tm, modified by Td/TD/T*/Tj/TJ.
    tm: [f64; 6],
    /// Text line matrix - tracks line start for Td/TD/T*.
    tlm: [f64; 6],
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font_name: Vec::new(),
            font_size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horiz_scaling: 100.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
            tm: IDENTITY,
            tlm: IDENTITY,
        }
    }
}

// ---------------------------------------------------------------------------
// CS2-CS3: Graphics state
// ---------------------------------------------------------------------------

/// Full graphics state (ISO 32000-2 §8.4).
#[derive(Debug, Clone)]
struct GraphicsState {
    /// CS3: Current transformation matrix.
    ctm: [f64; 6],
    /// Text state parameters.
    text: TextState,
}

impl Default for GraphicsState {
    fn default() -> Self {
        Self {
            ctm: IDENTITY,
            text: TextState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Matrix helpers - [a b c d e f] in row-major order
// ---------------------------------------------------------------------------

/// Identity matrix.
const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Multiply two 3×3 affine matrices stored as [a b c d e f].
///
/// Matrix layout:
/// ```text
///   | a  b  0 |
///   | c  d  0 |
///   | e  f  1 |
/// ```
fn mat_multiply(m1: &[f64; 6], m2: &[f64; 6]) -> [f64; 6] {
    [
        m1[0] * m2[0] + m1[1] * m2[2],
        m1[0] * m2[1] + m1[1] * m2[3],
        m1[2] * m2[0] + m1[3] * m2[2],
        m1[2] * m2[1] + m1[3] * m2[3],
        m1[4] * m2[0] + m1[5] * m2[2] + m2[4],
        m1[4] * m2[1] + m1[5] * m2[3] + m2[5],
    ]
}

/// Create a translation matrix.
fn mat_translate(tx: f64, ty: f64) -> [f64; 6] {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

/// Transform a point (x, y) by an affine matrix.
fn mat_transform(m: &[f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

// ---------------------------------------------------------------------------
// Content stream interpreter
// ---------------------------------------------------------------------------

/// Maximum Form XObject recursion depth.
const MAX_XOBJECT_DEPTH: u32 = 16;

/// CS1-CS21: Content stream interpreter.
///
/// Walks a page's content stream(s), maintains graphics state, and collects
/// positioned text spans.
pub struct ContentInterpreter<'a> {
    doc: &'a Document<'a>,
    /// CS2: Graphics state stack (q/Q).
    state_stack: Vec<GraphicsState>,
    /// Current graphics state.
    current: GraphicsState,
    /// Operand accumulator for postfix notation.
    operands: Vec<PdfObject>,
    /// Collected text spans.
    spans: Vec<TextSpan>,
    /// CS21: Form XObject recursion depth.
    xobject_depth: u32,
    /// Every font seen on the page, keyed by a unique font key (accumulate
    /// only, never restored). Spans reference these keys, so the layout
    /// engine can decode text after interpretation without name collisions.
    fonts: HashMap<Vec<u8>, PdfFont>,
    /// Resource-name scope: font name as written in the content stream
    /// (e.g. b"F5") to unique key in `fonts`. Saved and restored around
    /// nested content streams, which may rebind a name to another font.
    scope: HashMap<Vec<u8>, Vec<u8>>,
    /// Uniquifier for fonts defined as direct objects (no object ref).
    direct_font_seq: u32,
    /// OCG object references that are off by default (for filtering).
    off_ocgs: std::collections::HashSet<(u32, u16)>,
}

// Content stream operators are bare words (BT, Tf, cm, etc.) that the PDF
// file-level tokenizer can't handle. We use a dedicated content stream tokenizer.

/// A content stream token - either an operand or an operator.
#[derive(Debug, Clone, PartialEq)]
pub enum CsToken {
    /// Operand: a PDF object (number, string, name, array, dict).
    Operand(PdfObject),
    /// Operator: a bare keyword like "BT", "Tf", "cm", "Tj", etc.
    Operator(Vec<u8>),
}

/// A content stream operation relevant for image extraction.
#[derive(Debug)]
pub enum ContentStreamOp {
    /// A named reference: Do (XObject) or gs (ExtGState).
    Ref { op: &'static [u8], name: Vec<u8> },
    /// An inline image (BI..ID..EI), with the tokens between BI and EI.
    InlineImage(Vec<CsToken>),
}

/// CS1: Tokenize a content stream into operands and operators.
///
/// Content streams use postfix notation: operands appear before their operator.
/// Operators are bare alphabetic keywords not recognized by the PDF tokenizer.
fn tokenize_content_stream(data: &[u8]) -> Vec<CsToken> {
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Skip whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }

        // Skip comments
        if data[pos] == b'%' {
            while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
                pos += 1;
            }
            continue;
        }

        let b = data[pos];

        // Name
        if b == b'/' {
            pos += 1;
            let start = pos;
            while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
                pos += 1;
            }
            tokens.push(CsToken::Operand(PdfObject::Name(data[start..pos].to_vec())));
            continue;
        }

        // Literal string
        if b == b'(' {
            if let Some((s, end)) = read_literal_string(data, pos) {
                tokens.push(CsToken::Operand(PdfObject::String(s)));
                pos = end;
            } else {
                pos += 1;
            }
            continue;
        }

        // Hex string or dict start
        if b == b'<' {
            if pos + 1 < data.len() && data[pos + 1] == b'<' {
                // Dict start - read until >>
                pos += 2;
                if let Some((dict, end)) = read_inline_dict_raw(data, pos) {
                    tokens.push(CsToken::Operand(dict));
                    pos = end;
                }
            } else {
                // Hex string
                pos += 1;
                let start = pos;
                while pos < data.len() && data[pos] != b'>' {
                    pos += 1;
                }
                let hex = decode_hex_bytes(&data[start..pos]);
                tokens.push(CsToken::Operand(PdfObject::String(hex)));
                if pos < data.len() {
                    pos += 1; // skip >
                }
            }
            continue;
        }

        // Array
        if b == b'[' {
            pos += 1;
            if let Some((arr, end)) = read_inline_array_raw(data, pos) {
                tokens.push(CsToken::Operand(arr));
                pos = end;
            }
            continue;
        }

        // Number (digit, sign, or decimal point)
        if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() {
            let start = pos;
            if b == b'+' || b == b'-' {
                pos += 1;
            }
            let mut has_dot = false;
            while pos < data.len() {
                if data[pos].is_ascii_digit() {
                    pos += 1;
                } else if data[pos] == b'.' && !has_dot {
                    has_dot = true;
                    pos += 1;
                } else {
                    break;
                }
            }
            let s = &data[start..pos];
            if let Ok(text) = std::str::from_utf8(s) {
                if has_dot {
                    if let Ok(v) = text.parse::<f64>() {
                        tokens.push(CsToken::Operand(PdfObject::Real(v)));
                        continue;
                    }
                } else if let Ok(v) = text.parse::<i64>() {
                    tokens.push(CsToken::Operand(PdfObject::Int(v)));
                    continue;
                }
            }
            // Fallback - treat as operator
            tokens.push(CsToken::Operator(s.to_vec()));
            continue;
        }

        // Alphabetic keyword - either an operator or true/false/null
        if b.is_ascii_alphabetic() || b == b'\'' || b == b'"' {
            let start = pos;
            if b == b'\'' || b == b'"' {
                pos += 1;
            } else {
                while pos < data.len()
                    && (data[pos].is_ascii_alphabetic()
                        || data[pos] == b'*'
                        || data[pos] == b'0'
                        || data[pos] == b'1')
                {
                    pos += 1;
                }
            }
            let word = &data[start..pos];
            match word {
                b"true" => tokens.push(CsToken::Operand(PdfObject::Bool(true))),
                b"false" => tokens.push(CsToken::Operand(PdfObject::Bool(false))),
                b"null" => tokens.push(CsToken::Operand(PdfObject::Null)),
                _ => tokens.push(CsToken::Operator(word.to_vec())),
            }
            continue;
        }

        // Skip unrecognized byte
        pos += 1;
    }

    tokens
}

/// Tokenize a content stream, handling inline images (BI/ID/EI).
///
/// Like `tokenize_content_stream`, but when `BI` is encountered:
/// 1. Emits `BI` as an operator
/// 2. Continues tokenizing key/value pairs normally until `ID`
/// 3. After `ID`, reads raw binary data until `EI` is found at a proper boundary
/// 4. Emits the image data as a String operand, then `EI` as operator
pub fn tokenize_content_stream_with_inline(data: &[u8]) -> Vec<CsToken> {
    let mut tokens = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Skip whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }

        // Skip comments
        if data[pos] == b'%' {
            while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
                pos += 1;
            }
            continue;
        }

        let b = data[pos];

        // Name
        if b == b'/' {
            pos += 1;
            let start = pos;
            while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
                pos += 1;
            }
            tokens.push(CsToken::Operand(PdfObject::Name(data[start..pos].to_vec())));
            continue;
        }

        // Literal string
        if b == b'(' {
            if let Some((s, end)) = read_literal_string(data, pos) {
                tokens.push(CsToken::Operand(PdfObject::String(s)));
                pos = end;
            } else {
                pos += 1;
            }
            continue;
        }

        // Hex string or dict start
        if b == b'<' {
            if pos + 1 < data.len() && data[pos + 1] == b'<' {
                pos += 2;
                if let Some((dict, end)) = read_inline_dict_raw(data, pos) {
                    tokens.push(CsToken::Operand(dict));
                    pos = end;
                }
            } else {
                pos += 1;
                let start = pos;
                while pos < data.len() && data[pos] != b'>' {
                    pos += 1;
                }
                let hex = decode_hex_bytes(&data[start..pos]);
                tokens.push(CsToken::Operand(PdfObject::String(hex)));
                if pos < data.len() {
                    pos += 1;
                }
            }
            continue;
        }

        // Array
        if b == b'[' {
            pos += 1;
            if let Some((arr, end)) = read_inline_array_raw(data, pos) {
                tokens.push(CsToken::Operand(arr));
                pos = end;
            }
            continue;
        }

        // Number
        if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() {
            let start = pos;
            if b == b'+' || b == b'-' {
                pos += 1;
            }
            let mut has_dot = false;
            while pos < data.len() {
                if data[pos].is_ascii_digit() {
                    pos += 1;
                } else if data[pos] == b'.' && !has_dot {
                    has_dot = true;
                    pos += 1;
                } else {
                    break;
                }
            }
            let s = &data[start..pos];
            if let Ok(text) = std::str::from_utf8(s) {
                if has_dot {
                    if let Ok(v) = text.parse::<f64>() {
                        tokens.push(CsToken::Operand(PdfObject::Real(v)));
                        continue;
                    }
                } else if let Ok(v) = text.parse::<i64>() {
                    tokens.push(CsToken::Operand(PdfObject::Int(v)));
                    continue;
                }
            }
            tokens.push(CsToken::Operator(s.to_vec()));
            continue;
        }

        // Alphabetic keyword
        if b.is_ascii_alphabetic() || b == b'\'' || b == b'"' {
            let start = pos;
            if b == b'\'' || b == b'"' {
                pos += 1;
            } else {
                while pos < data.len()
                    && (data[pos].is_ascii_alphabetic()
                        || data[pos] == b'*'
                        || data[pos] == b'0'
                        || data[pos] == b'1')
                {
                    pos += 1;
                }
            }
            let word = &data[start..pos];
            match word {
                b"true" => tokens.push(CsToken::Operand(PdfObject::Bool(true))),
                b"false" => tokens.push(CsToken::Operand(PdfObject::Bool(false))),
                b"null" => tokens.push(CsToken::Operand(PdfObject::Null)),
                b"BI" => {
                    tokens.push(CsToken::Operator(b"BI".to_vec()));
                    // Parse inline image: tokenize key/value pairs until ID,
                    // then read binary data until EI
                    pos = tokenize_inline_image(data, pos, &mut tokens);
                }
                _ => tokens.push(CsToken::Operator(word.to_vec())),
            }
            continue;
        }

        pos += 1;
    }

    tokens
}

/// Parse inline image after BI has been emitted.
/// Tokenizes key/value pairs, then reads binary data between ID and EI.
/// Returns the position after EI.
fn tokenize_inline_image(data: &[u8], mut pos: usize, tokens: &mut Vec<CsToken>) -> usize {
    // Phase 1: Tokenize key/value pairs until ID operator
    loop {
        // Skip whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            break;
        }

        // Check for ID keyword
        if pos + 1 < data.len()
            && data[pos] == b'I'
            && data[pos + 1] == b'D'
            && (pos + 2 >= data.len() || !data[pos + 2].is_ascii_alphabetic())
        {
            tokens.push(CsToken::Operator(b"ID".to_vec()));
            pos += 2;
            // ID must be followed by exactly one whitespace byte (per spec)
            if pos < data.len() && is_whitespace(data[pos]) {
                pos += 1;
            }
            break;
        }

        let b = data[pos];

        // Comment (skip to end of line)
        if b == b'%' {
            pos += 1;
            while pos < data.len() && data[pos] != b'\n' && data[pos] != b'\r' {
                pos += 1;
            }
            continue;
        }

        // Name (key)
        if b == b'/' {
            pos += 1;
            let start = pos;
            while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
                pos += 1;
            }
            tokens.push(CsToken::Operand(PdfObject::Name(data[start..pos].to_vec())));
            continue;
        }

        // Number
        if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() {
            let start = pos;
            if b == b'+' || b == b'-' {
                pos += 1;
            }
            let mut has_dot = false;
            while pos < data.len() {
                if data[pos].is_ascii_digit() {
                    pos += 1;
                } else if data[pos] == b'.' && !has_dot {
                    has_dot = true;
                    pos += 1;
                } else {
                    break;
                }
            }
            let s = &data[start..pos];
            if let Ok(text) = std::str::from_utf8(s) {
                if has_dot {
                    if let Ok(v) = text.parse::<f64>() {
                        tokens.push(CsToken::Operand(PdfObject::Real(v)));
                        continue;
                    }
                } else if let Ok(v) = text.parse::<i64>() {
                    tokens.push(CsToken::Operand(PdfObject::Int(v)));
                    continue;
                }
            }
            tokens.push(CsToken::Operator(s.to_vec()));
            continue;
        }

        // Array (e.g. filter array)
        if b == b'[' {
            pos += 1;
            if let Some((arr, end)) = read_inline_array_raw(data, pos) {
                tokens.push(CsToken::Operand(arr));
                pos = end;
            }
            continue;
        }

        // Hex string
        if b == b'<' {
            if pos + 1 < data.len() && data[pos + 1] == b'<' {
                pos += 2;
                if let Some((dict, end)) = read_inline_dict_raw(data, pos) {
                    tokens.push(CsToken::Operand(dict));
                    pos = end;
                }
            } else {
                pos += 1;
                let start = pos;
                while pos < data.len() && data[pos] != b'>' {
                    pos += 1;
                }
                let hex = decode_hex_bytes(&data[start..pos]);
                tokens.push(CsToken::Operand(PdfObject::String(hex)));
                if pos < data.len() {
                    pos += 1;
                }
            }
            continue;
        }

        // Alphabetic value (color space abbreviation like G, RGB, CMYK, or true/false)
        if b.is_ascii_alphabetic() {
            let start = pos;
            while pos < data.len() && data[pos].is_ascii_alphabetic() {
                pos += 1;
            }
            let word = &data[start..pos];
            match word {
                b"true" => tokens.push(CsToken::Operand(PdfObject::Bool(true))),
                b"false" => tokens.push(CsToken::Operand(PdfObject::Bool(false))),
                b"null" => tokens.push(CsToken::Operand(PdfObject::Null)),
                _ => tokens.push(CsToken::Operand(PdfObject::Name(word.to_vec()))),
            }
            continue;
        }

        pos += 1;
    }

    // Phase 2: Read binary image data until EI
    let data_start = pos;
    // Search for EI preceded by whitespace and followed by whitespace/delimiter/EOF
    while pos < data.len() {
        if data[pos] == b'E' && pos + 1 < data.len() && data[pos + 1] == b'I' {
            // EI must be preceded by whitespace
            let preceded_by_ws = pos > data_start && is_whitespace(data[pos - 1]);
            // EI must be followed by whitespace, delimiter, or EOF
            let followed_ok = pos + 2 >= data.len()
                || is_whitespace(data[pos + 2])
                || is_delimiter(data[pos + 2]);
            if preceded_by_ws && followed_ok {
                // Don't include the preceding whitespace in image data
                let img_data = &data[data_start..pos - 1];
                tokens.push(CsToken::Operand(PdfObject::String(img_data.to_vec())));
                tokens.push(CsToken::Operator(b"EI".to_vec()));
                pos += 2;
                return pos;
            }
        }
        pos += 1;
    }

    // If we never found EI, emit whatever we have
    if pos > data_start {
        tokens.push(CsToken::Operand(PdfObject::String(
            data[data_start..pos].to_vec(),
        )));
    }

    pos
}

/// PDF whitespace check (matches tokenizer).
fn is_whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

/// PDF delimiter check.
fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// Read a literal string starting at `(`, returning (decoded_bytes, end_position).
fn read_literal_string(data: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    let mut pos = start + 1; // skip (
    let mut result = Vec::new();
    let mut depth = 1u32;

    while pos < data.len() && depth > 0 {
        match data[pos] {
            b'(' => {
                depth += 1;
                result.push(b'(');
                pos += 1;
            }
            b')' => {
                depth -= 1;
                if depth > 0 {
                    result.push(b')');
                }
                pos += 1;
            }
            b'\\' => {
                pos += 1;
                if pos >= data.len() {
                    break;
                }
                match data[pos] {
                    b'n' => {
                        result.push(b'\n');
                        pos += 1;
                    }
                    b'r' => {
                        result.push(b'\r');
                        pos += 1;
                    }
                    b't' => {
                        result.push(b'\t');
                        pos += 1;
                    }
                    b'b' => {
                        result.push(8);
                        pos += 1;
                    }
                    b'f' => {
                        result.push(12);
                        pos += 1;
                    }
                    b'(' => {
                        result.push(b'(');
                        pos += 1;
                    }
                    b')' => {
                        result.push(b')');
                        pos += 1;
                    }
                    b'\\' => {
                        result.push(b'\\');
                        pos += 1;
                    }
                    b'\r' => {
                        pos += 1;
                        if pos < data.len() && data[pos] == b'\n' {
                            pos += 1;
                        }
                    }
                    b'\n' => {
                        pos += 1;
                    }
                    b'0'..=b'7' => {
                        let mut val = (data[pos] - b'0') as u16;
                        pos += 1;
                        if pos < data.len() && data[pos] >= b'0' && data[pos] <= b'7' {
                            val = val * 8 + (data[pos] - b'0') as u16;
                            pos += 1;
                            if pos < data.len() && data[pos] >= b'0' && data[pos] <= b'7' {
                                val = val * 8 + (data[pos] - b'0') as u16;
                                pos += 1;
                            }
                        }
                        result.push(val as u8);
                    }
                    other => {
                        result.push(other);
                        pos += 1;
                    }
                }
            }
            other => {
                result.push(other);
                pos += 1;
            }
        }
    }

    Some((result, pos))
}

/// Decode hex bytes from raw hex character data.
fn decode_hex_bytes(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(data.len() / 2);
    let mut high: Option<u8> = None;

    for &b in data {
        let nibble = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => continue, // skip whitespace
        };
        match high {
            None => high = Some(nibble),
            Some(hi) => {
                result.push((hi << 4) | nibble);
                high = None;
            }
        }
    }
    if let Some(hi) = high {
        result.push(hi << 4);
    }
    result
}

/// Read an inline array from content stream data, starting after `[`.
/// Returns (PdfObject::Array, position after `]`).
fn read_inline_array_raw(data: &[u8], start: usize) -> Option<(PdfObject, usize)> {
    // Re-tokenize from start position to find matching ]
    let sub = &data[start..];
    let sub_tokens = tokenize_array_contents(sub)?;
    Some((PdfObject::Array(sub_tokens.0), start + sub_tokens.1))
}

/// Tokenize array contents until `]`. Returns (items, bytes_consumed).
fn tokenize_array_contents(data: &[u8]) -> Option<(Vec<PdfObject>, usize)> {
    let mut items = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Skip whitespace
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            return None;
        }

        if data[pos] == b']' {
            return Some((items, pos + 1));
        }

        // Parse one operand
        let b = data[pos];

        if b == b'(' {
            let (s, end) = read_literal_string(data, pos)?;
            items.push(PdfObject::String(s));
            pos = end;
        } else if b == b'<' {
            pos += 1;
            let start = pos;
            while pos < data.len() && data[pos] != b'>' {
                pos += 1;
            }
            let hex = decode_hex_bytes(&data[start..pos]);
            items.push(PdfObject::String(hex));
            if pos < data.len() {
                pos += 1;
            }
        } else if b == b'/' {
            pos += 1;
            let start = pos;
            while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
                pos += 1;
            }
            items.push(PdfObject::Name(data[start..pos].to_vec()));
        } else if b == b'+' || b == b'-' || b == b'.' || b.is_ascii_digit() {
            let start = pos;
            if b == b'+' || b == b'-' {
                pos += 1;
            }
            let mut has_dot = false;
            while pos < data.len() {
                if data[pos].is_ascii_digit() {
                    pos += 1;
                } else if data[pos] == b'.' && !has_dot {
                    has_dot = true;
                    pos += 1;
                } else {
                    break;
                }
            }
            if let Ok(text) = std::str::from_utf8(&data[start..pos]) {
                if has_dot {
                    if let Ok(v) = text.parse::<f64>() {
                        items.push(PdfObject::Real(v));
                    }
                } else if let Ok(v) = text.parse::<i64>() {
                    items.push(PdfObject::Int(v));
                }
            }
        } else if b.is_ascii_alphabetic() {
            let start = pos;
            while pos < data.len() && data[pos].is_ascii_alphabetic() {
                pos += 1;
            }
            let word = &data[start..pos];
            match word {
                b"true" => items.push(PdfObject::Bool(true)),
                b"false" => items.push(PdfObject::Bool(false)),
                b"null" => items.push(PdfObject::Null),
                _ => { /* skip operators inside arrays - shouldn't happen */ }
            }
        } else {
            pos += 1; // skip unrecognized
        }
    }

    None // unterminated
}

/// Read an inline dict from raw data, starting after `<<`.
fn read_inline_dict_raw(data: &[u8], start: usize) -> Option<(PdfObject, usize)> {
    let mut entries = Vec::new();
    let mut pos = start;

    while pos < data.len() {
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            return None;
        }

        // Check for >>
        if pos + 1 < data.len() && data[pos] == b'>' && data[pos + 1] == b'>' {
            return Some((PdfObject::Dict(entries), pos + 2));
        }

        // Key must be a name
        if data[pos] != b'/' {
            pos += 1;
            continue;
        }
        pos += 1;
        let name_start = pos;
        while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
            pos += 1;
        }
        let key = data[name_start..pos].to_vec();

        // Skip whitespace before value
        while pos < data.len() && is_whitespace(data[pos]) {
            pos += 1;
        }
        if pos >= data.len() {
            return None;
        }

        // Simple value parsing for inline dicts
        let b = data[pos];
        if b == b'/' {
            pos += 1;
            let vs = pos;
            while pos < data.len() && !is_whitespace(data[pos]) && !is_delimiter(data[pos]) {
                pos += 1;
            }
            entries.push((key, PdfObject::Name(data[vs..pos].to_vec())));
        } else if b.is_ascii_digit() || b == b'+' || b == b'-' || b == b'.' {
            let ns = pos;
            if b == b'+' || b == b'-' {
                pos += 1;
            }
            let mut dot = false;
            while pos < data.len() && (data[pos].is_ascii_digit() || (data[pos] == b'.' && !dot)) {
                if data[pos] == b'.' {
                    dot = true;
                }
                pos += 1;
            }
            if let Ok(t) = std::str::from_utf8(&data[ns..pos]) {
                if dot {
                    if let Ok(v) = t.parse::<f64>() {
                        entries.push((key, PdfObject::Real(v)));
                    }
                } else if let Ok(v) = t.parse::<i64>() {
                    entries.push((key, PdfObject::Int(v)));
                }
            }
        } else if b.is_ascii_alphabetic() {
            let ws = pos;
            while pos < data.len() && data[pos].is_ascii_alphabetic() {
                pos += 1;
            }
            let w = &data[ws..pos];
            match w {
                b"true" => entries.push((key, PdfObject::Bool(true))),
                b"false" => entries.push((key, PdfObject::Bool(false))),
                b"null" => entries.push((key, PdfObject::Null)),
                _ => entries.push((key, PdfObject::Name(w.to_vec()))),
            }
        } else {
            pos += 1;
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Operator dispatch - the real interpreter
// ---------------------------------------------------------------------------

impl<'a> ContentInterpreter<'a> {
    /// Process a page's content, using the dedicated content stream tokenizer.
    pub fn process_page(
        doc: &'a Document<'a>,
        page: &Page,
    ) -> Result<(Vec<TextSpan>, HashMap<Vec<u8>, PdfFont>)> {
        let off_ocgs = doc.off_ocg_refs();

        let mut interp = Self {
            doc,
            state_stack: Vec::new(),
            current: GraphicsState::default(),
            operands: Vec::new(),
            spans: Vec::new(),
            xobject_depth: 0,
            fonts: HashMap::new(),
            scope: HashMap::new(),
            direct_font_seq: 0,
            off_ocgs,
        };
        interp.register_fonts(font::load_page_fonts(doc, page.resources.as_ref()), None);

        let content_data = interp.get_page_content(page)?;
        let resources = page.resources.as_ref();

        interp.run(&content_data, resources)?;

        // Process annotation appearance streams (/Annots -> /AP -> /N)
        // Poppler extracts text from FreeText annotations, widget appearances, etc.
        interp.process_annotation_appearances(page);

        let fonts = interp.fonts;
        Ok((interp.spans, fonts))
    }

    /// Bind fonts from one resource dictionary into the current name scope.
    /// The key is unique per font object, so same-named fonts from different
    /// resource scopes (page vs form XObject) never collide in `self.fonts`.
    ///
    /// `host_ref` identifies the object owning the resource dictionary (a
    /// form XObject or appearance stream). A font defined as a direct object
    /// has no ref of its own, so it is keyed by its host: stable across
    /// repeated `Do` of the same form, which keeps `self.fonts` bounded and
    /// keeps span grouping intact. Only a host without a ref (the page
    /// itself, registered once) falls back to a sequence number.
    fn register_fonts(&mut self, loaded: Vec<font::LoadedFont>, host_ref: Option<(u32, u16)>) {
        for (name, obj_ref, font) in loaded {
            let mut key = name.clone();
            match (obj_ref, host_ref) {
                (Some((num, generation)), _) => {
                    key.extend_from_slice(format!("#{num}_{generation}").as_bytes());
                }
                (None, Some((num, generation))) => {
                    key.extend_from_slice(format!("#r{num}_{generation}").as_bytes());
                }
                (None, None) => {
                    key.extend_from_slice(format!("#d{}", self.direct_font_seq).as_bytes());
                    self.direct_font_seq += 1;
                }
            }
            self.fonts.entry(key.clone()).or_insert(font);
            self.scope.insert(name, key);
        }
    }

    /// Process annotation appearance streams to extract text from FreeText
    /// annotations, form widget appearances, etc.
    fn process_annotation_appearances(&mut self, page: &Page) {
        let annots = match page.dict.dict_get(b"Annots") {
            Some(a) => match self.doc.resolve_obj(a) {
                Ok(resolved) => resolved,
                Err(_) => return,
            },
            None => return,
        };

        let annot_array = match annots.as_array() {
            Some(arr) => arr,
            None => return,
        };

        for annot_ref in annot_array {
            let annot = match self.doc.resolve_obj(annot_ref) {
                Ok(a) => a,
                Err(_) => continue,
            };

            // Get /AP (appearance dict)
            let ap = match annot.dict_get(b"AP") {
                Some(ap) => match self.doc.resolve_obj(ap) {
                    Ok(a) => a,
                    Err(_) => continue,
                },
                None => continue,
            };

            // Process /N (normal appearance) - may be a stream or a dict of streams
            // Pass the annotation's /Rect for coordinate mapping
            let rect = annot
                .dict_get(b"Rect")
                .and_then(|r| r.as_array())
                .and_then(|arr| {
                    if arr.len() == 4 {
                        Some([
                            arr[0].as_f64().unwrap_or(0.0),
                            arr[1].as_f64().unwrap_or(0.0),
                            arr[2].as_f64().unwrap_or(0.0),
                            arr[3].as_f64().unwrap_or(0.0),
                        ])
                    } else {
                        None
                    }
                });

            if let Some(n) = ap.dict_get(b"N") {
                let appearance_ref = n.as_ref().map(|r| (r.num, r.generation));
                self.process_appearance_text(n, appearance_ref, rect.as_ref());
            }
        }
    }

    /// Process a single appearance stream (Form XObject) for text extraction.
    /// `annot_rect` is the annotation's /Rect on the page, used to map
    /// from BBox space to page space per PDF spec §12.5.5.
    fn process_appearance_text(
        &mut self,
        appearance: &PdfObject,
        appearance_ref: Option<(u32, u16)>,
        annot_rect: Option<&[f64; 4]>,
    ) {
        let form_obj = match self.doc.resolve_obj(appearance) {
            Ok(obj) => obj,
            Err(_) => return,
        };

        // If it's a dict of sub-appearances (e.g., /Yes, /Off for checkboxes),
        // try the /AS (appearance state) to pick the right one, or process all
        if form_obj.stream_data().is_none() {
            // Could be a dict of appearance states - just skip for now
            // (checkbox/radio button visual states are not useful text)
            return;
        }

        let raw = match form_obj.stream_data() {
            Some(r) => r,
            None => return,
        };

        let content_data = match decode::decode_stream(&form_obj, raw) {
            Ok(d) => d,
            Err(_) => return,
        };

        // Apply the Form XObject's /Matrix
        let form_matrix = form_obj
            .dict_get(b"Matrix")
            .and_then(|m| m.as_array())
            .and_then(|arr| {
                if arr.len() == 6 {
                    Some([
                        arr[0].as_f64().unwrap_or(1.0),
                        arr[1].as_f64().unwrap_or(0.0),
                        arr[2].as_f64().unwrap_or(0.0),
                        arr[3].as_f64().unwrap_or(1.0),
                        arr[4].as_f64().unwrap_or(0.0),
                        arr[5].as_f64().unwrap_or(0.0),
                    ])
                } else {
                    None
                }
            })
            .unwrap_or(IDENTITY);

        // Compute the Rect-to-BBox mapping matrix per PDF spec §12.5.5:
        // Maps from appearance BBox coordinates to the annotation Rect on page.
        let bbox_to_rect = if let Some(rect) = annot_rect {
            let bbox = form_obj
                .dict_get(b"BBox")
                .and_then(|b| b.as_array())
                .and_then(|arr| {
                    if arr.len() == 4 {
                        Some([
                            arr[0].as_f64().unwrap_or(0.0),
                            arr[1].as_f64().unwrap_or(0.0),
                            arr[2].as_f64().unwrap_or(0.0),
                            arr[3].as_f64().unwrap_or(0.0),
                        ])
                    } else {
                        None
                    }
                });

            if let Some(bbox) = bbox {
                let bw = (bbox[2] - bbox[0]).abs();
                let bh = (bbox[3] - bbox[1]).abs();
                let rw = (rect[2] - rect[0]).abs();
                let rh = (rect[3] - rect[1]).abs();

                if bw > 0.001 && bh > 0.001 {
                    let sx = rw / bw;
                    let sy = rh / bh;
                    let tx = rect[0].min(rect[2]) - bbox[0].min(bbox[2]) * sx;
                    let ty = rect[1].min(rect[3]) - bbox[1].min(bbox[3]) * sy;
                    [sx, 0.0, 0.0, sy, tx, ty]
                } else {
                    IDENTITY
                }
            } else {
                IDENTITY
            }
        } else {
            IDENTITY
        };

        // Final CTM = Form Matrix * BBox-to-Rect * page CTM
        let combined = mat_multiply(&form_matrix, &bbox_to_rect);

        // Save state, apply matrix, load fonts, process, restore
        self.state_stack.push(self.current.clone());
        self.current.ctm = combined;

        let form_resources = form_obj
            .dict_get(b"Resources")
            .and_then(|r| self.doc.resolve_obj(r).ok());
        let resources_ref = form_resources.as_ref();

        // Scope the appearance stream's font-name bindings like a form
        // XObject. The fonts accumulate in self.fonts under unique keys, so
        // they stay available for text decoding by the layout engine.
        let saved_scope = self.scope.clone();
        if form_resources.is_some() {
            self.register_fonts(
                font::load_page_fonts(self.doc, resources_ref),
                appearance_ref,
            );
        }

        let _ = self.run(&content_data, resources_ref);

        self.scope = saved_scope;

        self.current = self.state_stack.pop().unwrap_or_default();
    }

    /// Find all XObject `Do` operator targets in content stream order.
    /// Returns the XObject name (e.g., b"Im0") for each `Do` invocation.
    /// Duplicates are preserved - if the same image is Do'd 16 times, it appears 16 times.
    pub fn find_do_targets(doc: &'a Document<'a>, page: &Page) -> Result<Vec<Vec<u8>>> {
        let interp = ContentInterpreter {
            doc,
            state_stack: Vec::new(),
            current: GraphicsState::default(),
            operands: Vec::new(),
            spans: Vec::new(),
            xobject_depth: 0,
            fonts: HashMap::new(),
            scope: HashMap::new(),
            direct_font_seq: 0,
            off_ocgs: std::collections::HashSet::new(),
        };
        let content_data = interp.get_page_content(page)?;
        let tokens = tokenize_content_stream(&content_data);

        let mut targets = Vec::new();
        let mut operands: Vec<PdfObject> = Vec::new();
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    if op == b"Do" {
                        if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                            targets.push(name.to_vec());
                        }
                    }
                    operands.clear();
                }
            }
        }
        Ok(targets)
    }

    /// Find all Do, gs, and BI operators in content stream order.
    /// Returns a list of ContentStreamOp entries preserving content stream order,
    /// so that inline images are correctly interleaved with Do/gs targets.
    pub fn find_operators_in_order(data: &[u8]) -> Vec<ContentStreamOp> {
        let tokens = tokenize_content_stream_with_inline(data);
        let mut ops = Vec::new();
        let mut operands: Vec<PdfObject> = Vec::new();
        let mut i = 0;
        while i < tokens.len() {
            match &tokens[i] {
                CsToken::Operand(obj) => {
                    operands.push(obj.clone());
                    i += 1;
                }
                CsToken::Operator(op) => {
                    if op == b"Do" || op == b"gs" {
                        if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                            ops.push(ContentStreamOp::Ref {
                                op: if op == b"Do" { b"Do" } else { b"gs" },
                                name: name.to_vec(),
                            });
                        }
                    } else if op == b"BI" {
                        // Inline image: collect tokens until EI
                        i += 1;
                        let start = i;
                        // Skip to EI
                        while i < tokens.len() {
                            if let CsToken::Operator(ref o) = tokens[i] {
                                if o == b"EI" {
                                    break;
                                }
                            }
                            i += 1;
                        }
                        // Collect the BI..EI token range (inclusive of what's between)
                        let inline_tokens: Vec<CsToken> = tokens[start..i].to_vec();
                        ops.push(ContentStreamOp::InlineImage(inline_tokens));
                        // Skip EI
                    }
                    operands.clear();
                    i += 1;
                }
            }
        }
        ops
    }

    pub fn find_do_targets_in_bytes(data: &[u8]) -> Vec<Vec<u8>> {
        let tokens = tokenize_content_stream(data);
        let mut targets = Vec::new();
        let mut operands: Vec<PdfObject> = Vec::new();
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    if op == b"Do" {
                        if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                            targets.push(name.to_vec());
                        }
                    }
                    operands.clear();
                }
            }
        }
        targets
    }

    /// Find all gs operator targets from raw content stream bytes.
    /// Returns ExtGState resource names used in the content stream.
    pub fn find_gs_targets_in_bytes(data: &[u8]) -> Vec<Vec<u8>> {
        let tokens = tokenize_content_stream(data);
        let mut targets = Vec::new();
        let mut operands: Vec<PdfObject> = Vec::new();
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    if op == b"gs" {
                        if let Some(name) = operands.last().and_then(|o| o.as_name()) {
                            targets.push(name.to_vec());
                        }
                    }
                    operands.clear();
                }
            }
        }
        targets
    }

    /// Get the concatenated content stream bytes for a page.
    fn get_page_content(&self, page: &Page) -> Result<Vec<u8>> {
        let contents = match page.dict.dict_get(b"Contents") {
            Some(c) => c.clone(),
            None => return Ok(Vec::new()),
        };

        let contents = self.doc.resolve_obj(&contents)?;

        match &contents {
            PdfObject::Stream { .. } => {
                let raw = contents.stream_data().unwrap();
                decode::decode_stream(&contents, raw)
            }
            PdfObject::Array(refs) => {
                let mut data = Vec::new();
                for item in refs {
                    let resolved = self.doc.resolve_obj(item)?;
                    if let Some(raw) = resolved.stream_data() {
                        let decoded = decode::decode_stream(&resolved, raw)?;
                        if !data.is_empty() {
                            data.push(b' ');
                        }
                        data.extend_from_slice(&decoded);
                    }
                }
                Ok(data)
            }
            _ => Ok(Vec::new()),
        }
    }

    /// CS1: Run the content stream interpreter on tokenized data.
    fn run(&mut self, data: &[u8], resources: Option<&PdfObject>) -> Result<()> {
        let tokens = tokenize_content_stream(data);

        for token in &tokens {
            match token {
                CsToken::Operand(obj) => {
                    self.operands.push(obj.clone());
                }
                CsToken::Operator(op) => {
                    self.dispatch(op, resources)?;
                    self.operands.clear();
                }
            }
        }

        Ok(())
    }

    /// Dispatch a content stream operator.
    fn dispatch(&mut self, op: &[u8], resources: Option<&PdfObject>) -> Result<()> {
        match op {
            // CS2: Graphics state stack
            b"q" => self.op_q(),
            b"Q" => self.op_big_q(),

            // CS3: CTM
            b"cm" => self.op_cm(),

            // CS4: Text object
            b"BT" => self.op_bt(),
            b"ET" => self.op_et(),

            // CS5: Font
            b"Tf" => self.op_tf(),

            // CS6: Text matrix (absolute)
            b"Tm" => self.op_tm(),

            // CS7: Text position (relative)
            b"Td" => self.op_td(),
            b"TD" => self.op_big_td(),

            // CS8: Next line
            b"T*" => self.op_t_star(),

            // CS9: Text leading
            b"TL" => self.op_tl(),

            // CS10: Character spacing
            b"Tc" => self.op_tc(),

            // CS11: Word spacing
            b"Tw" => self.op_tw(),

            // CS12: Horizontal scaling
            b"Tz" => self.op_tz(),

            // CS13: Text rendering mode
            b"Tr" => self.op_tr(),

            // CS14: Text rise
            b"Ts" => self.op_ts(),

            // CS15: Show string
            b"Tj" => self.op_tj(),

            // CS16: Show string with positioning
            b"TJ" => self.op_big_tj(),

            // CS17: Next line + show string
            b"'" => self.op_quote(),

            // CS18: Set spacing + next line + show string
            b"\"" => self.op_double_quote(),

            // CS20: XObject reference
            b"Do" => self.op_do(resources),

            // All other operators - ignore (graphics, color, path, etc.)
            _ => Ok(()),
        }
    }

    // --- CS2: Graphics state stack ---

    fn op_q(&mut self) -> Result<()> {
        self.state_stack.push(self.current.clone());
        Ok(())
    }

    fn op_big_q(&mut self) -> Result<()> {
        if let Some(state) = self.state_stack.pop() {
            self.current = state;
        }
        Ok(())
    }

    // --- CS3: CTM ---

    fn op_cm(&mut self) -> Result<()> {
        if self.operands.len() < 6 {
            return Ok(());
        }
        let n = self.operands.len();
        let a = self.operands[n - 6].as_f64().unwrap_or(1.0);
        let b = self.operands[n - 5].as_f64().unwrap_or(0.0);
        let c = self.operands[n - 4].as_f64().unwrap_or(0.0);
        let d = self.operands[n - 3].as_f64().unwrap_or(1.0);
        let e = self.operands[n - 2].as_f64().unwrap_or(0.0);
        let f = self.operands[n - 1].as_f64().unwrap_or(0.0);

        let m = [a, b, c, d, e, f];
        self.current.ctm = mat_multiply(&m, &self.current.ctm);
        Ok(())
    }

    // --- CS4: Text object ---

    fn op_bt(&mut self) -> Result<()> {
        self.current.text.tm = IDENTITY;
        self.current.text.tlm = IDENTITY;
        Ok(())
    }

    fn op_et(&mut self) -> Result<()> {
        // End text - nothing to do structurally
        Ok(())
    }

    // --- CS5: Font ---

    fn op_tf(&mut self) -> Result<()> {
        if self.operands.len() < 2 {
            return Ok(());
        }
        let n = self.operands.len();
        if let Some(name) = self.operands[n - 2].as_name() {
            // Resolve the resource name through the current scope to the
            // unique font key. Fall back to the bare name for fonts that
            // never went through register_fonts (missing resources).
            self.current.text.font_name = self
                .scope
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_vec());
        }
        self.current.text.font_size = self.operands[n - 1].as_f64().unwrap_or(12.0);
        Ok(())
    }

    // --- CS6: Set text matrix (absolute) ---

    fn op_tm(&mut self) -> Result<()> {
        if self.operands.len() < 6 {
            return Ok(());
        }
        let n = self.operands.len();
        let tm = [
            self.operands[n - 6].as_f64().unwrap_or(1.0),
            self.operands[n - 5].as_f64().unwrap_or(0.0),
            self.operands[n - 4].as_f64().unwrap_or(0.0),
            self.operands[n - 3].as_f64().unwrap_or(1.0),
            self.operands[n - 2].as_f64().unwrap_or(0.0),
            self.operands[n - 1].as_f64().unwrap_or(0.0),
        ];
        self.current.text.tm = tm;
        self.current.text.tlm = tm;
        Ok(())
    }

    // --- CS7: Move text position (relative) ---

    fn op_td(&mut self) -> Result<()> {
        if self.operands.len() < 2 {
            return Ok(());
        }
        let n = self.operands.len();
        let tx = self.operands[n - 2].as_f64().unwrap_or(0.0);
        let ty = self.operands[n - 1].as_f64().unwrap_or(0.0);
        self.td(tx, ty);
        Ok(())
    }

    fn op_big_td(&mut self) -> Result<()> {
        if self.operands.len() < 2 {
            return Ok(());
        }
        let n = self.operands.len();
        let tx = self.operands[n - 2].as_f64().unwrap_or(0.0);
        let ty = self.operands[n - 1].as_f64().unwrap_or(0.0);
        // TD sets TL = -ty, then does Td
        self.current.text.leading = -ty;
        self.td(tx, ty);
        Ok(())
    }

    /// Shared Td implementation: Tlm = translate(tx,ty) × Tlm; Tm = Tlm.
    fn td(&mut self, tx: f64, ty: f64) {
        let translate = mat_translate(tx, ty);
        self.current.text.tlm = mat_multiply(&translate, &self.current.text.tlm);
        self.current.text.tm = self.current.text.tlm;
    }

    // --- CS8: Next line ---

    fn op_t_star(&mut self) -> Result<()> {
        let leading = self.current.text.leading;
        self.td(0.0, -leading);
        Ok(())
    }

    // --- CS9-CS14: Text state parameters ---

    fn op_tl(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_f64()) {
            self.current.text.leading = v;
        }
        Ok(())
    }

    fn op_tc(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_f64()) {
            self.current.text.char_spacing = v;
        }
        Ok(())
    }

    fn op_tw(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_f64()) {
            self.current.text.word_spacing = v;
        }
        Ok(())
    }

    fn op_tz(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_f64()) {
            self.current.text.horiz_scaling = v;
        }
        Ok(())
    }

    fn op_tr(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_int()) {
            self.current.text.render_mode = v as u8;
        }
        Ok(())
    }

    fn op_ts(&mut self) -> Result<()> {
        if let Some(v) = self.operands.last().and_then(|o| o.as_f64()) {
            self.current.text.rise = v;
        }
        Ok(())
    }

    // --- CS15: Show string (Tj) ---

    fn op_tj(&mut self) -> Result<()> {
        if let Some(obj) = self.operands.last() {
            if let Some(bytes) = obj.as_string() {
                self.show_string(bytes.to_vec());
            }
        }
        Ok(())
    }

    // --- CS16: Show string with positioning (TJ) ---

    fn op_big_tj(&mut self) -> Result<()> {
        if let Some(obj) = self.operands.last().cloned() {
            if let Some(arr) = obj.as_array() {
                for item in arr {
                    match item {
                        PdfObject::String(bytes) => {
                            self.show_string(bytes.clone());
                        }
                        PdfObject::Int(n) => {
                            // Negative number = move right (advance), positive = move left (kern)
                            // Displacement is in thousandths of a unit of text space
                            self.adjust_text_position(*n as f64);
                        }
                        PdfObject::Real(n) => {
                            self.adjust_text_position(*n);
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }

    // --- CS17: Quote operator (') ---

    fn op_quote(&mut self) -> Result<()> {
        // T* then Tj
        self.op_t_star()?;
        self.op_tj()
    }

    // --- CS18: Double-quote operator (") ---

    fn op_double_quote(&mut self) -> Result<()> {
        if self.operands.len() < 3 {
            return Ok(());
        }
        let n = self.operands.len();
        // Set word spacing
        if let Some(v) = self.operands[n - 3].as_f64() {
            self.current.text.word_spacing = v;
        }
        // Set character spacing
        if let Some(v) = self.operands[n - 2].as_f64() {
            self.current.text.char_spacing = v;
        }
        // Move the string operand to be last, then T* + Tj
        self.op_t_star()?;
        // The string is the last operand
        if let Some(bytes) = self.operands[n - 1].as_string() {
            self.show_string(bytes.to_vec());
        }
        Ok(())
    }

    // --- CS19: Glyph width and text advance ---

    /// Show a string: emit a TextSpan and advance the text matrix.
    fn show_string(&mut self, text: Vec<u8>) {
        if text.is_empty() {
            return;
        }

        let ts = &self.current.text;

        // Compute position in user space: CTM × Tm × (0, rise)
        let (x, y) = self.text_position();

        // Compute the effective horizontal scale from CTM × Tm
        // For matrix [a b c d e f], horizontal scale = sqrt(a² + b²)
        let combined_tm = mat_multiply(&ts.tm, &self.current.ctm);
        let ctm_scale_x =
            (combined_tm[0] * combined_tm[0] + combined_tm[1] * combined_tm[1]).sqrt();

        self.spans.push(TextSpan {
            text: text.clone(),
            font_name: ts.font_name.clone(),
            font_size: ts.font_size,
            x,
            y,
            render_mode: ts.render_mode,
            ctm_scale_x,
            char_spacing: ts.char_spacing,
            word_spacing: ts.word_spacing,
        });

        // CS19: Advance text position by string width
        let advance = self.compute_string_advance(&text);
        self.advance_tm(advance);
    }

    /// Compute the current text position in user space.
    fn text_position(&self) -> (f64, f64) {
        let ts = &self.current.text;
        // Apply text rise
        let rise_matrix = mat_translate(0.0, ts.rise);
        let effective_tm = mat_multiply(&rise_matrix, &ts.tm);
        // Transform through CTM
        let combined = mat_multiply(&effective_tm, &self.current.ctm);
        mat_transform(&combined, 0.0, 0.0)
    }

    /// CS19: Compute the total horizontal advance for a string.
    fn compute_string_advance(&self, text: &[u8]) -> f64 {
        let ts = &self.current.text;
        let font_size = ts.font_size;
        let char_space = ts.char_spacing;
        let word_space = ts.word_spacing;
        let hz = ts.horiz_scaling / 100.0;

        let current_font = self.fonts.get(&ts.font_name);
        let is_two_byte = current_font.map_or(false, |f| f.is_two_byte);
        let fm_scale = current_font.map_or(0.001, |f| f.font_matrix_scale);

        let mut total = 0.0;

        if is_two_byte {
            let mut i = 0;
            while i + 1 < text.len() {
                let code = ((text[i] as u32) << 8) | (text[i + 1] as u32);
                let w0 = current_font
                    .map(|f| font::glyph_width(f, code))
                    .unwrap_or(DEFAULT_GLYPH_WIDTH);
                let tx = (w0 * font_size * fm_scale + char_space) * hz;
                total += tx;
                i += 2;
            }
        } else {
            for &byte in text {
                let w0 = current_font
                    .map(|f| font::glyph_width(f, byte as u32))
                    .unwrap_or(DEFAULT_GLYPH_WIDTH);
                let mut tx = w0 * font_size * fm_scale + char_space;
                if byte == 32 {
                    tx += word_space;
                }
                tx *= hz;
                total += tx;
            }
        }

        total
    }

    /// Adjust text position by TJ numeric adjustment (in thousandths of text space).
    fn adjust_text_position(&mut self, amount: f64) {
        let ts = &self.current.text;
        let font_size = ts.font_size;
        let hz = ts.horiz_scaling / 100.0;
        // TJ adjustments are always in thousandths of text space, regardless of FontMatrix
        let tx = -amount * font_size / 1000.0 * hz;
        self.advance_tm(tx);
    }

    /// Advance the text matrix horizontally.
    fn advance_tm(&mut self, tx: f64) {
        let translate = mat_translate(tx, 0.0);
        self.current.text.tm = mat_multiply(&translate, &self.current.text.tm);
    }

    // --- CS20-CS21: XObject handling ---

    fn op_do(&mut self, resources: Option<&PdfObject>) -> Result<()> {
        let name = match self.operands.last().and_then(|o| o.as_name()) {
            Some(n) => n.to_vec(),
            None => return Ok(()),
        };

        // Look up in page resources: /XObject dictionary
        let xobj_dict = resources
            .and_then(|r| self.doc.resolve_obj(r).ok())
            .and_then(|r| r.dict_get(b"XObject").cloned())
            .and_then(|xo| self.doc.resolve_obj(&xo).ok());

        let xobj_dict = match xobj_dict {
            Some(d) => d,
            None => return Ok(()),
        };

        let xobj_ref = match xobj_dict.dict_get(&name) {
            Some(r) => r.clone(),
            None => return Ok(()),
        };

        let xobj = self.doc.resolve_obj(&xobj_ref)?;

        // Check subtype
        let subtype = xobj
            .dict_get(b"Subtype")
            .and_then(|s| s.as_name_str())
            .unwrap_or("");

        match subtype {
            "Form" => {
                let xobj_id = xobj_ref.as_ref().map(|r| (r.num, r.generation));
                self.process_form_xobject(&xobj, xobj_id)?
            }
            // "Image" - handled in image extraction phase, skip here
            _ => {}
        }

        Ok(())
    }

    /// Check if an OCG or OCMD reference points to a hidden content group.
    fn is_ocg_hidden(&self, oc: &PdfObject) -> bool {
        // Resolve the /OC value to get the actual dict
        let oc_ref = oc.as_ref(); // save ref before resolving
        let oc_dict = match self.doc.resolve_obj(oc) {
            Ok(d) => d,
            Err(_) => return false,
        };

        let oc_type = oc_dict
            .dict_get(b"Type")
            .and_then(|t| t.as_name_str())
            .unwrap_or("");

        match oc_type {
            "OCG" => {
                // Direct OCG reference - check if its obj ref is in the off set
                if let Some(r) = oc_ref {
                    return self.off_ocgs.contains(&(r.num, r.generation));
                }
                false
            }
            "OCMD" => {
                // Optional Content Membership Dictionary
                let policy = oc_dict
                    .dict_get(b"P")
                    .and_then(|p| p.as_name_str())
                    .unwrap_or("AnyOn");

                if let Some(ocgs) = oc_dict.dict_get(b"OCGs") {
                    let refs: Vec<(u32, u16)> = if let Some(r) = ocgs.as_ref() {
                        vec![(r.num, r.generation)]
                    } else if let Some(arr) = ocgs.as_array() {
                        arr.iter()
                            .filter_map(|item| item.as_ref())
                            .map(|r| (r.num, r.generation))
                            .collect()
                    } else {
                        return false;
                    };

                    match policy {
                        "AnyOn" => {
                            // Visible if ANY referenced OCG is on
                            // Hidden if ALL are off
                            refs.iter().all(|r| self.off_ocgs.contains(r))
                        }
                        "AllOn" => {
                            // Visible if ALL are on; hidden if any is off
                            refs.iter().any(|r| self.off_ocgs.contains(r))
                        }
                        "AnyOff" => {
                            // Visible if ANY is off; hidden if all are on
                            !refs.iter().any(|r| self.off_ocgs.contains(r))
                        }
                        "AllOff" => {
                            // Visible if ALL are off; hidden if any is on
                            !refs.iter().all(|r| self.off_ocgs.contains(r))
                        }
                        _ => false,
                    }
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// CS21: Recursively process a Form XObject's content stream.
    fn process_form_xobject(
        &mut self,
        xobj: &PdfObject,
        xobj_id: Option<(u32, u16)>,
    ) -> Result<()> {
        if self.xobject_depth >= MAX_XOBJECT_DEPTH {
            return Ok(()); // Prevent infinite recursion
        }

        // Check OCG visibility - skip form XObjects from hidden layers
        if !self.off_ocgs.is_empty() {
            if let Some(oc) = xobj.dict_get(b"OC") {
                if self.is_ocg_hidden(oc) {
                    return Ok(());
                }
            }
        }

        let stream_data = match xobj.stream_data() {
            Some(raw) => decode::decode_stream(xobj, raw)?,
            None => return Ok(()),
        };

        // CS21: Apply the Form XObject's /Matrix to CTM
        let matrix = xobj
            .dict_get(b"Matrix")
            .and_then(|m| m.as_array())
            .and_then(|arr| {
                if arr.len() == 6 {
                    Some([
                        arr[0].as_f64().unwrap_or(1.0),
                        arr[1].as_f64().unwrap_or(0.0),
                        arr[2].as_f64().unwrap_or(0.0),
                        arr[3].as_f64().unwrap_or(1.0),
                        arr[4].as_f64().unwrap_or(0.0),
                        arr[5].as_f64().unwrap_or(0.0),
                    ])
                } else {
                    None
                }
            })
            .unwrap_or(IDENTITY);

        // Save state, apply matrix, process, restore
        self.state_stack.push(self.current.clone());
        self.current.ctm = mat_multiply(&matrix, &self.current.ctm);

        // Use the Form XObject's own /Resources if present, else inherit
        let form_resources = xobj
            .dict_get(b"Resources")
            .and_then(|r| self.doc.resolve_obj(r).ok());

        let resources_ref = form_resources.as_ref();

        // The form's own resources may rebind a font name the page also uses
        // (e.g. both call different fonts /F5), so scope the name bindings to
        // this stream. The fonts themselves accumulate in self.fonts under
        // unique keys and stay available for span decoding.
        let saved_scope = self.scope.clone();
        if form_resources.is_some() {
            self.register_fonts(font::load_page_fonts(self.doc, resources_ref), xobj_id);
        }

        self.xobject_depth += 1;
        let run_result = self.run(&stream_data, resources_ref);
        self.xobject_depth -= 1;

        self.scope = saved_scope;

        if let Some(state) = self.state_stack.pop() {
            self.current = state;
        }

        run_result
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- CS1: Content stream tokenizer ---

    #[test]
    fn cs1_tokenize_basic() {
        let data = b"BT /F1 12 Tf (Hello) Tj ET";
        let tokens = tokenize_content_stream(data);
        assert_eq!(
            tokens,
            vec![
                CsToken::Operator(b"BT".to_vec()),
                CsToken::Operand(PdfObject::Name(b"F1".to_vec())),
                CsToken::Operand(PdfObject::Int(12)),
                CsToken::Operator(b"Tf".to_vec()),
                CsToken::Operand(PdfObject::String(b"Hello".to_vec())),
                CsToken::Operator(b"Tj".to_vec()),
                CsToken::Operator(b"ET".to_vec()),
            ]
        );
    }

    #[test]
    fn cs1_tokenize_numbers() {
        let data = b"1 0 0 1 72 700 cm";
        let tokens = tokenize_content_stream(data);
        assert_eq!(
            tokens,
            vec![
                CsToken::Operand(PdfObject::Int(1)),
                CsToken::Operand(PdfObject::Int(0)),
                CsToken::Operand(PdfObject::Int(0)),
                CsToken::Operand(PdfObject::Int(1)),
                CsToken::Operand(PdfObject::Int(72)),
                CsToken::Operand(PdfObject::Int(700)),
                CsToken::Operator(b"cm".to_vec()),
            ]
        );
    }

    #[test]
    fn cs1_tokenize_real_numbers() {
        let data = b"0.5 0.0 Td";
        let tokens = tokenize_content_stream(data);
        assert_eq!(
            tokens,
            vec![
                CsToken::Operand(PdfObject::Real(0.5)),
                CsToken::Operand(PdfObject::Real(0.0)),
                CsToken::Operator(b"Td".to_vec()),
            ]
        );
    }

    #[test]
    fn cs1_tokenize_hex_string() {
        let data = b"<48656C6C6F> Tj";
        let tokens = tokenize_content_stream(data);
        assert_eq!(
            tokens,
            vec![
                CsToken::Operand(PdfObject::String(b"Hello".to_vec())),
                CsToken::Operator(b"Tj".to_vec()),
            ]
        );
    }

    #[test]
    fn cs1_tokenize_array() {
        let data = b"[(Hello) -100 (World)] TJ";
        let tokens = tokenize_content_stream(data);
        assert_eq!(tokens.len(), 2);
        match &tokens[0] {
            CsToken::Operand(PdfObject::Array(arr)) => {
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0], PdfObject::String(b"Hello".to_vec()));
                assert_eq!(arr[1], PdfObject::Int(-100));
                assert_eq!(arr[2], PdfObject::String(b"World".to_vec()));
            }
            _ => panic!("expected array operand"),
        }
        assert_eq!(tokens[1], CsToken::Operator(b"TJ".to_vec()));
    }

    #[test]
    fn cs1_tokenize_comments() {
        let data = b"BT\n% this is a comment\n/F1 12 Tf ET";
        let tokens = tokenize_content_stream(data);
        assert_eq!(tokens[0], CsToken::Operator(b"BT".to_vec()));
        assert_eq!(tokens[1], CsToken::Operand(PdfObject::Name(b"F1".to_vec())));
    }

    #[test]
    fn cs1_tokenize_quote_operators() {
        let data = b"(Hello) '";
        let tokens = tokenize_content_stream(data);
        assert_eq!(
            tokens,
            vec![
                CsToken::Operand(PdfObject::String(b"Hello".to_vec())),
                CsToken::Operator(b"'".to_vec()),
            ]
        );
    }

    #[test]
    fn cs1_tokenize_double_quote() {
        let data = b"0 0 (Hello) \"";
        let tokens = tokenize_content_stream(data);
        assert_eq!(tokens.len(), 4);
        assert_eq!(tokens[3], CsToken::Operator(b"\"".to_vec()));
    }

    #[test]
    fn cs1_tokenize_t_star() {
        let data = b"T*";
        let tokens = tokenize_content_stream(data);
        assert_eq!(tokens, vec![CsToken::Operator(b"T*".to_vec())]);
    }

    #[test]
    fn cs1_tokenize_negative_numbers() {
        let data = b"-100 Tj";
        let tokens = tokenize_content_stream(data);
        assert_eq!(tokens[0], CsToken::Operand(PdfObject::Int(-100)));
    }

    // --- CS2: Graphics state stack ---

    #[test]
    fn cs2_q_saves_state() {
        let data = b"1 0 0 1 100 200 cm q 1 0 0 1 50 50 cm Q";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        // Manually dispatch
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        // After Q, CTM should be restored to the first cm value
        assert!((interp.current.ctm[4] - 100.0).abs() < 0.001);
        assert!((interp.current.ctm[5] - 200.0).abs() < 0.001);
    }

    // --- CS3: CTM ---

    #[test]
    fn cs3_cm_concatenation() {
        let data = b"1 0 0 1 72 700 cm";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        assert!((interp.current.ctm[4] - 72.0).abs() < 0.001);
        assert!((interp.current.ctm[5] - 700.0).abs() < 0.001);
    }

    // --- CS4: BT/ET ---

    #[test]
    fn cs4_bt_resets_text_matrix() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.tm = [2.0, 0.0, 0.0, 2.0, 100.0, 200.0];
        interp.dispatch(b"BT", None).unwrap();
        assert_eq!(interp.current.text.tm, IDENTITY);
        assert_eq!(interp.current.text.tlm, IDENTITY);
    }

    // --- CS5: Tf ---

    #[test]
    fn cs5_set_font() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Name(b"F1".to_vec()));
        interp.operands.push(PdfObject::Real(14.0));
        interp.dispatch(b"Tf", None).unwrap();
        assert_eq!(interp.current.text.font_name, b"F1");
        assert!((interp.current.text.font_size - 14.0).abs() < 0.001);
    }

    // --- CS6: Tm ---

    #[test]
    fn cs6_set_text_matrix() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for v in [1.0, 0.0, 0.0, 1.0, 100.0, 700.0] {
            interp.operands.push(PdfObject::Real(v));
        }
        interp.dispatch(b"Tm", None).unwrap();
        assert!((interp.current.text.tm[4] - 100.0).abs() < 0.001);
        assert!((interp.current.text.tm[5] - 700.0).abs() < 0.001);
        // Tlm should also be set
        assert_eq!(interp.current.text.tm, interp.current.text.tlm);
    }

    // --- CS7: Td / TD ---

    #[test]
    fn cs7_td_relative_move() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        // Start at identity, Td(72, -14)
        interp.operands.push(PdfObject::Real(72.0));
        interp.operands.push(PdfObject::Real(-14.0));
        interp.dispatch(b"Td", None).unwrap();
        assert!((interp.current.text.tm[4] - 72.0).abs() < 0.001);
        assert!((interp.current.text.tm[5] - -14.0).abs() < 0.001);
    }

    #[test]
    fn cs7_big_td_sets_leading() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(0.0));
        interp.operands.push(PdfObject::Real(-14.0));
        interp.dispatch(b"TD", None).unwrap();
        // TL should be set to -(-14) = 14
        assert!((interp.current.text.leading - 14.0).abs() < 0.001);
    }

    // --- CS8: T* ---

    #[test]
    fn cs8_t_star_uses_leading() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.leading = 14.0;
        interp.dispatch(b"T*", None).unwrap();
        // Should move by (0, -14)
        assert!((interp.current.text.tm[5] - -14.0).abs() < 0.001);
    }

    // --- CS9-CS14: Text state parameters ---

    #[test]
    fn cs9_tl() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(14.0));
        interp.dispatch(b"TL", None).unwrap();
        assert!((interp.current.text.leading - 14.0).abs() < 0.001);
    }

    #[test]
    fn cs10_tc() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(0.5));
        interp.dispatch(b"Tc", None).unwrap();
        assert!((interp.current.text.char_spacing - 0.5).abs() < 0.001);
    }

    #[test]
    fn cs11_tw() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(1.0));
        interp.dispatch(b"Tw", None).unwrap();
        assert!((interp.current.text.word_spacing - 1.0).abs() < 0.001);
    }

    #[test]
    fn cs12_tz() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(150.0));
        interp.dispatch(b"Tz", None).unwrap();
        assert!((interp.current.text.horiz_scaling - 150.0).abs() < 0.001);
    }

    #[test]
    fn cs13_tr() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Int(3));
        interp.dispatch(b"Tr", None).unwrap();
        assert_eq!(interp.current.text.render_mode, 3);
    }

    #[test]
    fn cs14_ts() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Real(5.0));
        interp.dispatch(b"Ts", None).unwrap();
        assert!((interp.current.text.rise - 5.0).abs() < 0.001);
    }

    // --- CS15: Tj ---

    #[test]
    fn cs15_show_string() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_name = b"F1".to_vec();
        interp.current.text.font_size = 12.0;
        interp.operands.push(PdfObject::String(b"Hello".to_vec()));
        interp.dispatch(b"Tj", None).unwrap();
        assert_eq!(interp.spans.len(), 1);
        assert_eq!(interp.spans[0].text, b"Hello");
        assert_eq!(interp.spans[0].font_name, b"F1");
        assert!((interp.spans[0].font_size - 12.0).abs() < 0.001);
    }

    // --- CS16: TJ ---

    #[test]
    fn cs16_show_string_with_positioning() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_name = b"F1".to_vec();
        interp.current.text.font_size = 12.0;
        interp.operands.push(PdfObject::Array(vec![
            PdfObject::String(b"H".to_vec()),
            PdfObject::Int(-100),
            PdfObject::String(b"ello".to_vec()),
        ]));
        interp.dispatch(b"TJ", None).unwrap();
        assert_eq!(interp.spans.len(), 2);
        assert_eq!(interp.spans[0].text, b"H");
        assert_eq!(interp.spans[1].text, b"ello");
        // Second span should be offset from first
        assert!(interp.spans[1].x > interp.spans[0].x);
    }

    // --- CS17: ' operator ---

    #[test]
    fn cs17_quote_operator() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_name = b"F1".to_vec();
        interp.current.text.font_size = 12.0;
        interp.current.text.leading = 14.0;
        interp.operands.push(PdfObject::String(b"Line2".to_vec()));
        interp.dispatch(b"'", None).unwrap();
        assert_eq!(interp.spans.len(), 1);
        assert_eq!(interp.spans[0].text, b"Line2");
        // Y position should be -14 (moved down by leading)
        assert!((interp.spans[0].y - -14.0).abs() < 0.001);
    }

    // --- CS18: " operator ---

    #[test]
    fn cs18_double_quote_operator() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_name = b"F1".to_vec();
        interp.current.text.font_size = 12.0;
        interp.current.text.leading = 14.0;
        interp.operands.push(PdfObject::Real(1.0)); // word spacing
        interp.operands.push(PdfObject::Real(0.5)); // char spacing
        interp.operands.push(PdfObject::String(b"Text".to_vec()));
        interp.dispatch(b"\"", None).unwrap();
        assert_eq!(interp.spans.len(), 1);
        assert_eq!(interp.spans[0].text, b"Text");
        assert!((interp.current.text.word_spacing - 1.0).abs() < 0.001);
        assert!((interp.current.text.char_spacing - 0.5).abs() < 0.001);
    }

    // --- CS19: Glyph width calculation ---

    #[test]
    fn cs19_string_advance() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_size = 12.0;
        // "AB" = 2 chars × (600 * 12/1000 + 0) × 1.0 = 2 × 7.2 = 14.4
        let advance = interp.compute_string_advance(b"AB");
        assert!((advance - 14.4).abs() < 0.001);
    }

    #[test]
    fn cs19_word_spacing() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_size = 12.0;
        interp.current.text.word_spacing = 2.0;
        // "A " = (7.2) + (7.2 + 2.0) = 16.4
        let advance = interp.compute_string_advance(b"A ");
        assert!((advance - 16.4).abs() < 0.001);
    }

    #[test]
    fn cs19_horiz_scaling() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.font_size = 10.0;
        interp.current.text.horiz_scaling = 200.0;
        // "A" = (600 * 10/1000 + 0) × 2.0 = 6.0 × 2.0 = 12.0
        let advance = interp.compute_string_advance(b"A");
        assert!((advance - 12.0).abs() < 0.001);
    }

    #[test]
    fn cs19_text_position_with_ctm() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.ctm = [1.0, 0.0, 0.0, 1.0, 50.0, 100.0];
        interp.current.text.tm = [1.0, 0.0, 0.0, 1.0, 10.0, 20.0];
        let (x, y): (f64, f64) = interp.text_position();
        // CTM × Tm × (0,0) = (50+10, 100+20) = (60, 120)
        assert!((x - 60.0_f64).abs() < 0.001);
        assert!((y - 120.0_f64).abs() < 0.001);
    }

    #[test]
    fn cs19_text_rise_affects_position() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.current.text.rise = 5.0;
        let (_, y): (f64, f64) = interp.text_position();
        assert!((y - 5.0_f64).abs() < 0.001);
    }

    // --- CS20: Do operator (basic) ---

    #[test]
    fn cs20_do_ignores_missing_resource() {
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        interp.operands.push(PdfObject::Name(b"Im1".to_vec()));
        // No resources - should not error
        assert!(interp.dispatch(b"Do", None).is_ok());
    }

    // --- Matrix helpers ---

    #[test]
    fn matrix_identity() {
        let result = mat_multiply(&IDENTITY, &IDENTITY);
        assert_eq!(result, IDENTITY);
    }

    #[test]
    fn matrix_translate() {
        let t = mat_translate(10.0, 20.0);
        let (x, y) = mat_transform(&t, 0.0, 0.0);
        assert!((x - 10.0).abs() < 0.001);
        assert!((y - 20.0).abs() < 0.001);
    }

    #[test]
    fn matrix_concatenation() {
        let t1 = mat_translate(10.0, 0.0);
        let t2 = mat_translate(0.0, 20.0);
        let combined = mat_multiply(&t1, &t2);
        let (x, y) = mat_transform(&combined, 0.0, 0.0);
        assert!((x - 10.0).abs() < 0.001);
        assert!((y - 20.0).abs() < 0.001);
    }

    #[test]
    fn matrix_scale_then_translate() {
        // Scale by 2, then translate by (10, 10)
        let scale = [2.0, 0.0, 0.0, 2.0, 0.0, 0.0];
        let translate = mat_translate(10.0, 10.0);
        let combined = mat_multiply(&scale, &translate);
        let (x, y) = mat_transform(&combined, 5.0, 5.0);
        // (5*2, 5*2) then translate by (10, 10) = (20, 20)
        assert!((x - 20.0).abs() < 0.001);
        assert!((y - 20.0).abs() < 0.001);
    }

    // --- Integration: full content stream ---

    #[test]
    fn integration_simple_text() {
        let data = b"BT /F1 12 Tf 72 700 Td (Hello World) Tj ET";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        assert_eq!(interp.spans.len(), 1);
        assert_eq!(interp.spans[0].text, b"Hello World");
        assert!((interp.spans[0].x - 72.0).abs() < 0.001);
        assert!((interp.spans[0].y - 700.0).abs() < 0.001);
    }

    #[test]
    fn integration_multiline_text() {
        let data = b"BT /F1 12 Tf 14 TL 72 700 Td (Line 1) Tj T* (Line 2) Tj ET";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        assert_eq!(interp.spans.len(), 2);
        assert_eq!(interp.spans[0].text, b"Line 1");
        assert_eq!(interp.spans[1].text, b"Line 2");
        // Second line should be 14 units below
        assert!((interp.spans[0].y - 700.0).abs() < 0.001);
        assert!((interp.spans[1].y - 686.0).abs() < 0.001);
    }

    #[test]
    fn integration_tj_array() {
        let data = b"BT /F1 12 Tf 72 700 Td [(H) -200 (ello)] TJ ET";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        assert_eq!(interp.spans.len(), 2);
        assert_eq!(interp.spans[0].text, b"H");
        assert_eq!(interp.spans[1].text, b"ello");
    }

    #[test]
    fn integration_invisible_text() {
        let data = b"BT /F1 12 Tf 3 Tr (Hidden) Tj ET";
        let tokens = tokenize_content_stream(data);
        let doc = Document::parse(MINIMAL_PDF).unwrap();
        let mut interp = make_test_interp_with_doc(&doc);
        for token in &tokens {
            match token {
                CsToken::Operand(obj) => interp.operands.push(obj.clone()),
                CsToken::Operator(op) => {
                    interp.dispatch(op, None).unwrap();
                    interp.operands.clear();
                }
            }
        }
        assert_eq!(interp.spans.len(), 1);
        assert_eq!(interp.spans[0].render_mode, 3);
    }

    /// Minimal valid PDF for creating a real Document in tests.
    static MINIMAL_PDF: &[u8] = b"%PDF-1.7\n\
        1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n\
        2 0 obj\n<< /Type /Pages /Kids [] /Count 0 >>\nendobj\n\
        xref\n0 3\n\
        0000000000 65535 f \n\
        0000000009 00000 n \n\
        0000000058 00000 n \n\
        trailer\n<< /Size 3 /Root 1 0 R >>\n\
        startxref\n109\n%%EOF";

    /// Helper: create a test interpreter with a real (minimal) Document.
    fn make_test_interp_with_doc<'a>(doc: &'a Document<'a>) -> ContentInterpreter<'a> {
        ContentInterpreter {
            doc,
            state_stack: Vec::new(),
            current: GraphicsState::default(),
            operands: Vec::new(),
            spans: Vec::new(),
            xobject_depth: 0,
            fonts: HashMap::new(),
            scope: HashMap::new(),
            direct_font_seq: 0,
            off_ocgs: std::collections::HashSet::new(),
        }
    }

    // --- CS22: font-name collision across resource scopes ---

    fn tounicode(map_c4: &str) -> Vec<u8> {
        format!(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
             /CMapName /T def\n/CMapType 2 def\n\
             1 begincodespacerange\n<00> <FF>\nendcodespacerange\n\
             1 beginbfrange\n<41> <5A> <0041>\nendbfrange\n\
             1 beginbfchar\n<C4> <{map_c4}>\nendbfchar\n\
             endcmap\nCMapName currentdict /CMap defineresource pop\nend end"
        )
        .into_bytes()
    }

    fn stream_obj(dict_extra: &str, data: &[u8]) -> Vec<u8> {
        let mut v = format!("<<{dict_extra}/Length {}>>\nstream\n", data.len()).into_bytes();
        v.extend_from_slice(data);
        v.extend_from_slice(b"\nendstream");
        v
    }

    fn assemble_pdf(objs: &[Vec<u8>]) -> Vec<u8> {
        let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, obj) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(obj);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_pos = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{}\n%%EOF\n",
                objs.len() + 1,
                xref_pos
            )
            .as_bytes(),
        );
        out
    }

    /// PDF where the page and a form XObject both bind /F5, to different
    /// fonts: the page's maps 0xC4 -> 'b', the form's maps 0xC4 -> U+00C4.
    /// The form draws (L\xC4SKOPIA).
    fn build_f5_collision_pdf() -> Vec<u8> {
        let objs: Vec<Vec<u8>> = vec![
            b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
            b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
              /Resources<</Font<</F5 6 0 R>>/XObject<</Fm1 8 0 R>>>>>>"
                .to_vec(),
            stream_obj("", b"BT /F5 12 Tf 50 700 Td (PAGETEXT) Tj ET\nq /Fm1 Do Q"),
            stream_obj("", &tounicode("0062")), // page /F5: 0xC4 -> 'b'
            b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica/ToUnicode 5 0 R>>".to_vec(),
            b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica/ToUnicode 9 0 R>>".to_vec(),
            stream_obj(
                "/Subtype/Form/BBox[0 0 612 792]/Resources<</Font<</F5 7 0 R>>>>",
                b"BT /F5 12 Tf 50 600 Td (L\xC4SKOPIA) Tj ET",
            ),
            stream_obj("", &tounicode("00C4")), // form /F5: 0xC4 -> U+00C4
        ];
        assemble_pdf(&objs)
    }

    /// PDF whose form XObject defines /F5 as a DIRECT font dict (no object
    /// ref of its own) and is `Do`'d twice. The direct font must get one
    /// stable key derived from the form's ref, not one key per invocation.
    fn build_direct_font_double_do_pdf() -> Vec<u8> {
        let objs: Vec<Vec<u8>> = vec![
            b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
            b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
              /Resources<</XObject<</Fm1 5 0 R>>>>>>"
                .to_vec(),
            stream_obj("", b"q /Fm1 Do Q q 1 0 0 1 200 0 cm /Fm1 Do Q"),
            stream_obj(
                "/Subtype/Form/BBox[0 0 612 792]/Resources<</Font<</F5\
                 <</Type/Font/Subtype/Type1/BaseFont/Helvetica/ToUnicode 6 0 R>>>>>>",
                b"BT /F5 12 Tf 50 600 Td (L\xC4SKOPIA) Tj ET",
            ),
            stream_obj("", &tounicode("00C4")),
        ];
        assemble_pdf(&objs)
    }

    fn build_annotation_font_collision_pdf() -> Vec<u8> {
        let objs: Vec<Vec<u8>> = vec![
            b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
            b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
              /Resources<</Font<</F5 6 0 R>>>>/Annots[8 0 R]>>"
                .to_vec(),
            stream_obj("", b"BT /F5 12 Tf 50 700 Td (P\xC4GE) Tj ET"),
            stream_obj("", &tounicode("0062")),
            b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica/ToUnicode 5 0 R>>".to_vec(),
            b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica/ToUnicode 10 0 R>>".to_vec(),
            b"<</Type/Annot/Subtype/FreeText/Rect[0 0 100 100]/AP<</N 9 0 R>>>>".to_vec(),
            stream_obj(
                "/Subtype/Form/BBox[0 0 100 100]/Resources<</Font<</F5 7 0 R>>>>",
                b"BT /F5 12 Tf 5 50 Td (L\xC4SKOPIA) Tj ET",
            ),
            stream_obj("", &tounicode("00C4")),
        ];
        assemble_pdf(&objs)
    }

    #[test]
    fn cs22_form_font_name_collision_keeps_scopes_apart() {
        let pdf = build_f5_collision_pdf();
        let doc = Document::parse(&pdf).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, fonts) = ContentInterpreter::process_page(&doc, &page).unwrap();

        // The form's span must decode through the form's /F5 (0xC4 -> U+00C4),
        // not the page's /F5 (0xC4 -> 'b').
        let form_span = spans
            .iter()
            .find(|s| s.text.contains(&0xC4))
            .expect("form span with 0xC4 byte");
        let font = fonts
            .get(&form_span.font_name)
            .expect("form span font key resolves");
        let decoded = font
            .to_unicode
            .as_ref()
            .expect("form font has ToUnicode")
            .lookup(0xC4)
            .expect("0xC4 mapped");
        assert_eq!(decoded, "\u{00C4}");

        // The page's own span still decodes through the page's /F5.
        let page_span = spans
            .iter()
            .find(|s| s.text.starts_with(b"PAGETEXT"))
            .expect("page span");
        let page_font = fonts.get(&page_span.font_name).expect("page font key");
        assert_eq!(
            page_font.to_unicode.as_ref().unwrap().lookup(0xC4).unwrap(),
            "b"
        );
    }

    #[test]
    fn cs22_direct_font_key_stable_across_repeated_do() {
        let pdf = build_direct_font_double_do_pdf();
        let doc = Document::parse(&pdf).unwrap();
        let page = doc.page(0).unwrap();
        let (spans, fonts) = ContentInterpreter::process_page(&doc, &page).unwrap();

        let form_spans: Vec<_> = spans.iter().filter(|s| s.text.contains(&0xC4)).collect();
        assert_eq!(form_spans.len(), 2, "form drawn twice");
        assert_eq!(
            form_spans[0].font_name, form_spans[1].font_name,
            "same direct font must keep one key across repeated Do"
        );
        let font = fonts
            .get(&form_spans[0].font_name)
            .expect("font key resolves");
        assert_eq!(
            font.to_unicode.as_ref().unwrap().lookup(0xC4).unwrap(),
            "\u{00C4}"
        );
    }

    #[test]
    fn cs22_annotation_font_name_collision_keeps_scopes_apart() {
        let pdf = build_annotation_font_collision_pdf();
        let doc = Document::parse(&pdf).unwrap();
        let page = doc.page(0).unwrap();
        let text = crate::pdf::text_layout::extract_text_raw(&doc, &page).unwrap();

        assert_eq!(text, "PbGEL\u{00C4}SKOPIA");
    }
}
