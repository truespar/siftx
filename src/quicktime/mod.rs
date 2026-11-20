//! QuickTime/MP4/MOV metadata parser.
//!
//! Extracts metadata from ISOBMFF-based video containers (MP4, MOV, M4A, M4V, 3GP).
//! Reuses the box parser from the HEIF module since both share the ISOBMFF structure.

use crate::core::{Error, Reader, Result};

/// Parsed QuickTime/MP4 metadata.
#[derive(Debug)]
pub struct QuickTimeInfo<'a> {
    /// File type box: major brand, minor version, compatible brands.
    pub major_brand: [u8; 4],
    pub minor_version: u32,
    pub compatible_brands: Vec<[u8; 4]>,
    /// Movie header (mvhd) metadata.
    pub creation_time: Option<u64>,
    pub modification_time: Option<u64>,
    pub time_scale: Option<u32>,
    pub duration: Option<u64>,
    /// A fragmented file's whole length (mvex->mehd), in the movie's time scale: its movie
    /// header's duration is 0 or the first fragment's.
    pub fragment_duration: Option<u64>,
    /// Tracks.
    pub tracks: Vec<Track>,
    /// Raw XMP data from udta->meta or uuid box.
    pub xmp_data: Option<&'a [u8]>,
    /// GPS coordinates from udta->©xyz.
    pub gps_string: Option<String>,
}

/// A media track (video or audio).
#[derive(Debug, Clone)]
pub struct Track {
    pub track_id: u32,
    pub track_type: TrackType,
    pub duration_secs: f64,
    pub creation_time: Option<u64>,
    pub modification_time: Option<u64>,
    /// Video width (0 for audio tracks).
    pub width: u32,
    /// Video height (0 for audio tracks).
    pub height: u32,
    /// Codec identifier (e.g., "avc1", "mp4a", "hvc1").
    pub codec: [u8; 4],
    /// Human-readable codec name from stsd.
    pub codec_name: String,
    /// For video: frame rate (0 for audio).
    pub frame_rate: f64,
    /// For audio: sample rate.
    pub audio_sample_rate: u32,
    /// For audio: number of channels.
    pub audio_channels: u16,
    /// For audio: bits per sample.
    pub audio_bps: u16,
    /// Handler description string.
    pub handler_description: String,
    /// Media language code.
    pub language: String,
    /// Media time scale.
    pub media_time_scale: u32,
}

/// Track type based on handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackType {
    Video,
    Audio,
    Subtitle,
    Metadata,
    Other,
}

impl std::fmt::Display for TrackType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrackType::Video => write!(f, "Video"),
            TrackType::Audio => write!(f, "Audio"),
            TrackType::Subtitle => write!(f, "Subtitle"),
            TrackType::Metadata => write!(f, "Metadata"),
            TrackType::Other => write!(f, "Other"),
        }
    }
}

/// Check if this ISOBMFF brand indicates a QuickTime/MP4/MOV container.
pub fn is_quicktime_brand(brand: &[u8; 4]) -> bool {
    matches!(
        brand,
        b"isom"
            | b"iso2"
            | b"iso3"
            | b"iso4"
            | b"iso5"
            | b"iso6"
            | b"mp41"
            | b"mp42"
            | b"mp71"
            | b"M4A "
            | b"M4B "
            | b"M4P "
            | b"M4V "
            | b"qt  "
            | b"MSNV"
            | b"avc1"
            | b"hvc1"
            | b"av01"
            | b"3gp4"
            | b"3gp5"
            | b"3gp6"
            | b"3gp7"
            | b"3gs7"
            | b"3ge6"
            | b"3ge7"
            | b"NDAS"
            | b"NDSC"
            | b"NDSH"
            | b"NDSM"
            | b"NDSP"
            | b"NDSS"
            | b"NDXH"
            | b"NDXM"
            | b"NDXP"
            | b"NDXS"
            | b"dash"
            | b"f4v "
    )
}

/// Parse QuickTime/MP4 metadata from raw file data.
pub fn parse_quicktime<'a>(data: &'a [u8]) -> Result<QuickTimeInfo<'a>> {
    let boxes = crate::heif::parse_boxes(data)?;

    // ftyp
    let ftyp_box = boxes
        .iter()
        .find(|b| &b.box_type == b"ftyp")
        .ok_or_else(|| Error::Format("MP4: no ftyp box".into()))?;
    let ftyp = crate::heif::parse_ftyp(ftyp_box.data)?;

    let mut info = QuickTimeInfo {
        major_brand: ftyp.major_brand,
        minor_version: ftyp.minor_version,
        compatible_brands: ftyp.compatible_brands,
        creation_time: None,
        modification_time: None,
        time_scale: None,
        duration: None,
        tracks: Vec::new(),
        xmp_data: None,
        gps_string: None,
        fragment_duration: None,
    };

    // moov box contains all metadata
    if let Some(moov) = boxes.iter().find(|b| &b.box_type == b"moov") {
        let moov_children = crate::heif::parse_boxes(moov.data)?;

        // mvhd - movie header
        if let Some(mvhd) = moov_children.iter().find(|b| &b.box_type == b"mvhd") {
            parse_mvhd(mvhd.data, &mut info);
        }

        // mvex -> mehd: a fragmented file's length
        if let Some(mvex) = moov_children.iter().find(|b| &b.box_type == b"mvex")
            && let Ok(kids) = crate::heif::parse_boxes(mvex.data)
            && let Some(mehd) = kids.iter().find(|b| &b.box_type == b"mehd")
        {
            info.fragment_duration = parse_mehd(mehd.data);
        }

        // trak boxes - one per track
        for trak in moov_children.iter().filter(|b| &b.box_type == b"trak") {
            if let Some(track) = parse_trak(trak.data, &info) {
                info.tracks.push(track);
            }
        }

        // udta -> metadata
        if let Some(udta) = moov_children.iter().find(|b| &b.box_type == b"udta") {
            parse_udta(udta.data, data, &mut info);
        }
    }

    // XMP in uuid box (fallback)
    if info.xmp_data.is_none() {
        if let Some(xmp) = find_xmp_uuid(data) {
            info.xmp_data = Some(xmp);
        }
    }

    Ok(info)
}

/// Parse mvhd (movie header) box.
fn parse_mvhd(data: &[u8], info: &mut QuickTimeInfo) {
    if data.is_empty() {
        return;
    }
    let version = data[0];
    let mut r = Reader::new(&data[4..]); // skip version(1) + flags(3)

    if version == 1 {
        // 64-bit times
        info.creation_time = r.read_u64_be().ok();
        info.modification_time = r.read_u64_be().ok();
        info.time_scale = r.read_u32_be().ok();
        info.duration = r.read_u64_be().ok();
    } else {
        // 32-bit times
        info.creation_time = r.read_u32_be().ok().map(|v| v as u64);
        info.modification_time = r.read_u32_be().ok().map(|v| v as u64);
        info.time_scale = r.read_u32_be().ok();
        info.duration = r.read_u32_be().ok().map(|v| v as u64);
    }
}

/// Parse mehd (movie extends header): the fragment duration, 32 or 64 bits by version.
fn parse_mehd(data: &[u8]) -> Option<u64> {
    let version = *data.first()?;
    let mut r = Reader::new(data.get(4..)?);
    if version == 1 {
        r.read_u64_be().ok()
    } else {
        r.read_u32_be().ok().map(|v| v as u64)
    }
}

impl QuickTimeInfo<'_> {
    /// The movie's length in seconds: the movie header's, else a fragmented file's (mehd),
    /// else its longest track's. None when the file says none.
    pub fn duration_secs(&self) -> Option<f64> {
        let scaled = |d: Option<u64>| match (d, self.time_scale) {
            (Some(d), Some(ts)) if d > 0 && ts > 0 => Some(d as f64 / ts as f64),
            _ => None,
        };
        scaled(self.fragment_duration)
            .filter(|f| scaled(self.duration).is_none_or(|m| *f > m))
            .or_else(|| scaled(self.duration))
            .or_else(|| {
                self.tracks
                    .iter()
                    .map(|t| t.duration_secs)
                    .filter(|d| d.is_finite() && *d > 0.0)
                    .reduce(f64::max)
            })
    }
}

/// Parse a trak box into a Track.
fn parse_trak(data: &[u8], info: &QuickTimeInfo) -> Option<Track> {
    let children = crate::heif::parse_boxes(data).ok()?;

    let mut track = Track {
        track_id: 0,
        track_type: TrackType::Other,
        duration_secs: 0.0,
        creation_time: None,
        modification_time: None,
        width: 0,
        height: 0,
        codec: [0; 4],
        codec_name: String::new(),
        frame_rate: 0.0,
        audio_sample_rate: 0,
        audio_channels: 0,
        audio_bps: 0,
        handler_description: String::new(),
        language: String::new(),
        media_time_scale: 0,
    };

    // tkhd - track header
    if let Some(tkhd) = children.iter().find(|b| &b.box_type == b"tkhd") {
        parse_tkhd(tkhd.data, &mut track, info);
    }

    // mdia - media information
    if let Some(mdia) = children.iter().find(|b| &b.box_type == b"mdia") {
        parse_mdia(mdia.data, &mut track);
    }

    Some(track)
}

/// Parse tkhd (track header).
fn parse_tkhd(data: &[u8], track: &mut Track, info: &QuickTimeInfo) {
    if data.is_empty() {
        return;
    }
    let version = data[0];
    let mut r = Reader::new(&data[4..]); // skip version + flags

    if version == 1 {
        track.creation_time = r.read_u64_be().ok();
        track.modification_time = r.read_u64_be().ok();
        track.track_id = r.read_u32_be().unwrap_or(0);
        let _ = r.read_u32_be(); // reserved
        let duration = r.read_u64_be().unwrap_or(0);
        if let Some(ts) = info.time_scale {
            if ts > 0 {
                track.duration_secs = duration as f64 / ts as f64;
            }
        }
    } else {
        track.creation_time = r.read_u32_be().ok().map(|v| v as u64);
        track.modification_time = r.read_u32_be().ok().map(|v| v as u64);
        track.track_id = r.read_u32_be().unwrap_or(0);
        let _ = r.read_u32_be(); // reserved
        let duration = r.read_u32_be().unwrap_or(0);
        if let Some(ts) = info.time_scale {
            if ts > 0 {
                track.duration_secs = duration as f64 / ts as f64;
            }
        }
    }

    // Skip: reserved(8), layer(2), alternate_group(2), volume(2), reserved(2), matrix(36)
    let skip = 8 + 2 + 2 + 2 + 2 + 36;
    if r.remaining() >= skip + 8 {
        let _ = r.read_bytes(skip);
        // Width and height are 16.16 fixed point
        let w_fixed = r.read_u32_be().unwrap_or(0);
        let h_fixed = r.read_u32_be().unwrap_or(0);
        track.width = w_fixed >> 16;
        track.height = h_fixed >> 16;
    }
}

/// Parse mdia (media) box.
fn parse_mdia(data: &[u8], track: &mut Track) {
    let children = match crate::heif::parse_boxes(data) {
        Ok(c) => c,
        Err(_) => return,
    };

    // mdhd - media header
    if let Some(mdhd) = children.iter().find(|b| &b.box_type == b"mdhd") {
        parse_mdhd(mdhd.data, track);
    }

    // hdlr - handler reference (determines track type)
    if let Some(hdlr) = children.iter().find(|b| &b.box_type == b"hdlr") {
        parse_hdlr(hdlr.data, track);
    }

    // minf -> stbl -> stsd (sample description)
    if let Some(minf) = children.iter().find(|b| &b.box_type == b"minf") {
        if let Ok(minf_children) = crate::heif::parse_boxes(minf.data) {
            if let Some(stbl) = minf_children.iter().find(|b| &b.box_type == b"stbl") {
                if let Ok(stbl_children) = crate::heif::parse_boxes(stbl.data) {
                    if let Some(stsd) = stbl_children.iter().find(|b| &b.box_type == b"stsd") {
                        parse_stsd(stsd.data, track);
                    }
                    // stts for frame rate estimation
                    if let Some(stts) = stbl_children.iter().find(|b| &b.box_type == b"stts") {
                        estimate_frame_rate(stts.data, track);
                    }
                }
            }
        }
    }
}

/// Parse mdhd (media header).
fn parse_mdhd(data: &[u8], track: &mut Track) {
    if data.is_empty() {
        return;
    }
    let version = data[0];
    let mut r = Reader::new(&data[4..]);

    let media_duration;
    if version == 1 {
        let _ = r.read_u64_be(); // creation time
        let _ = r.read_u64_be(); // modification time
        track.media_time_scale = r.read_u32_be().unwrap_or(0);
        media_duration = r.read_u64_be().unwrap_or(0);
    } else {
        let _ = r.read_u32_be(); // creation time
        let _ = r.read_u32_be(); // modification time
        track.media_time_scale = r.read_u32_be().unwrap_or(0);
        media_duration = r.read_u32_be().unwrap_or(0) as u64;
    }

    // Better duration from media header
    if track.media_time_scale > 0 {
        track.duration_secs = media_duration as f64 / track.media_time_scale as f64;
    }

    // Language code (packed ISO-639-2/T)
    if let Ok(lang_raw) = r.read_u16_be() {
        if lang_raw != 0 && lang_raw != 0x7FFF {
            let c1 = ((lang_raw >> 10) & 0x1F) as u8 + 0x60;
            let c2 = ((lang_raw >> 5) & 0x1F) as u8 + 0x60;
            let c3 = (lang_raw & 0x1F) as u8 + 0x60;
            if c1.is_ascii_lowercase() && c2.is_ascii_lowercase() && c3.is_ascii_lowercase() {
                track.language = format!("{}{}{}", c1 as char, c2 as char, c3 as char);
            }
        }
    }
}

/// Parse hdlr (handler reference).
fn parse_hdlr(data: &[u8], track: &mut Track) {
    if data.len() < 12 {
        return;
    }
    // version(1) + flags(3) + pre_defined(4) + handler_type(4)
    let handler_type = &data[8..12];
    track.track_type = match handler_type {
        b"vide" => TrackType::Video,
        b"soun" => TrackType::Audio,
        b"subt" | b"sbtl" | b"text" => TrackType::Subtitle,
        b"meta" => TrackType::Metadata,
        _ => TrackType::Other,
    };

    // Handler description: skip version(1)+flags(3)+pre_defined(4)+handler_type(4)+reserved(12) = 24 bytes
    if data.len() > 24 {
        let desc = &data[24..];
        // May be pascal string (first byte = length) or C string
        if !desc.is_empty() {
            let s = if desc[0] as usize <= desc.len() - 1 && desc[0] > 0 && desc[0] < 128 {
                // Check if it's a pascal string
                let len = desc[0] as usize;
                if len < desc.len() {
                    String::from_utf8_lossy(&desc[1..1 + len]).into_owned()
                } else {
                    String::from_utf8_lossy(desc)
                        .trim_end_matches('\0')
                        .to_string()
                }
            } else {
                String::from_utf8_lossy(desc)
                    .trim_end_matches('\0')
                    .to_string()
            };
            if !s.is_empty() {
                track.handler_description = s;
            }
        }
    }
}

/// Parse stsd (sample description) - extract codec info.
fn parse_stsd(data: &[u8], track: &mut Track) {
    if data.len() < 16 {
        return;
    }
    // version(1) + flags(3) + entry_count(4)
    let mut r = Reader::new(&data[8..]);

    // First sample entry: size(4) + format(4) + reserved(6) + data_ref_index(2) = 16
    let _ = r.read_u32_be(); // size
    let format = match r.read_bytes(4) {
        Ok(b) => [b[0], b[1], b[2], b[3]],
        Err(_) => return,
    };
    track.codec = format;

    let _ = r.read_bytes(6); // reserved
    let _ = r.read_u16_be(); // data_ref_index

    match track.track_type {
        TrackType::Video => {
            // Video sample entry: skip pre_defined(2)+reserved(2)+pre_defined(12)=16
            if r.remaining() >= 16 + 4 + 4 + 4 + 2 + 32 + 2 + 2 {
                let _ = r.read_bytes(16);
                let w = r.read_u16_be().unwrap_or(0);
                let h = r.read_u16_be().unwrap_or(0);
                if w > 0 && h > 0 {
                    // Source dimensions from stsd (may differ from tkhd)
                    // Don't overwrite tkhd values unless they're 0
                    if track.width == 0 {
                        track.width = w as u32;
                    }
                    if track.height == 0 {
                        track.height = h as u32;
                    }
                }
                let _ = r.read_u32_be(); // horiz_resolution
                let _ = r.read_u32_be(); // vert_resolution
                let _ = r.read_u32_be(); // data_size
                let _frame_count = r.read_u16_be().unwrap_or(1);
                // Compressor name: 32 bytes, pascal string
                if let Ok(name_bytes) = r.read_bytes(32) {
                    let name_len = name_bytes[0] as usize;
                    if name_len > 0 && name_len < 32 {
                        let name = String::from_utf8_lossy(&name_bytes[1..1 + name_len]);
                        track.codec_name = name.trim().to_string();
                    }
                }
            }
        }
        TrackType::Audio => {
            // Audio sample entry
            if r.remaining() >= 20 {
                let _ = r.read_bytes(8); // reserved (version(2) + revision(2) + vendor(4))
                track.audio_channels = r.read_u16_be().unwrap_or(0);
                track.audio_bps = r.read_u16_be().unwrap_or(0);
                let _ = r.read_u16_be(); // compression_id
                let _ = r.read_u16_be(); // packet_size
                let sr_fixed = r.read_u32_be().unwrap_or(0);
                track.audio_sample_rate = sr_fixed >> 16; // 16.16 fixed point
            }
        }
        _ => {}
    }
}

/// Estimate frame rate from stts (time-to-sample) box.
fn estimate_frame_rate(data: &[u8], track: &mut Track) {
    if track.track_type != TrackType::Video || data.len() < 16 {
        return;
    }
    // version(1) + flags(3) + entry_count(4) + first_entry(count(4) + delta(4))
    let mut r = Reader::new(&data[4..]); // skip version + flags
    let entry_count = r.read_u32_be().unwrap_or(0);
    if entry_count == 0 {
        return;
    }

    // Use the first entry's delta (most common case for constant frame rate)
    let _sample_count = r.read_u32_be().unwrap_or(0);
    let sample_delta = r.read_u32_be().unwrap_or(0);

    if sample_delta > 0 && track.media_time_scale > 0 {
        track.frame_rate = track.media_time_scale as f64 / sample_delta as f64;
    }
}

/// Parse udta (user data) box.
fn parse_udta<'a>(data: &[u8], file_data: &'a [u8], info: &mut QuickTimeInfo<'a>) {
    let children = match crate::heif::parse_boxes(data) {
        Ok(c) => c,
        Err(_) => return,
    };

    // GPS string: ©xyz
    for child in &children {
        if child.box_type == [0xA9, b'x', b'y', b'z'] {
            // Format: "+DD.DDDD-DDD.DDDD/" or "+DD.DDDD-DDD.DDDD+DDD.DDD/"
            if child.data.len() > 4 {
                // Skip data size(2) + language(2)
                if let Ok(s) = std::str::from_utf8(&child.data[4..]) {
                    let s = s.trim().trim_end_matches('/');
                    if !s.is_empty() {
                        info.gps_string = Some(s.to_string());
                    }
                }
            }
        }
    }

    // meta box inside udta
    if let Some(meta) = children.iter().find(|b| &b.box_type == b"meta") {
        let meta_data = if meta.data.len() >= 4 {
            &meta.data[4..] // skip version + flags
        } else {
            meta.data
        };

        // Look for XMP in meta->ilst or as hdlr+XML
        if let Ok(meta_children) = crate::heif::parse_boxes(meta_data) {
            for child in &meta_children {
                // XMP can be in an 'XMP_' or 'xml ' handler
                if &child.box_type == b"xml " || &child.box_type == b"XMP_" {
                    if !child.data.is_empty() {
                        // Compute offset back into file_data
                        let ptr = child.data.as_ptr() as usize;
                        let base = file_data.as_ptr() as usize;
                        if ptr >= base && ptr + child.data.len() <= base + file_data.len() {
                            let offset = ptr - base;
                            info.xmp_data = Some(&file_data[offset..offset + child.data.len()]);
                        }
                    }
                }
            }
        }
    }
}

/// Look for XMP in a UUID box.
fn find_xmp_uuid<'a>(data: &'a [u8]) -> Option<&'a [u8]> {
    // XMP UUID: BE7ACFCB-97A9-42E8-9C71-999491E3AFAC
    const XMP_UUID: [u8; 16] = [
        0xBE, 0x7A, 0xCF, 0xCB, 0x97, 0xA9, 0x42, 0xE8, 0x9C, 0x71, 0x99, 0x94, 0x91, 0xE3, 0xAF,
        0xAC,
    ];

    let boxes = crate::heif::parse_boxes(data).ok()?;
    for b in &boxes {
        if &b.box_type == b"uuid" && b.data.len() > 16 {
            if b.data[..16] == XMP_UUID {
                return Some(&b.data[16..]);
            }
        }
    }
    None
}

/// Format a QuickTime timestamp (seconds since 1904-01-01) as "YYYY:MM:DD HH:MM:SS".
pub fn format_qt_date(secs: u64) -> String {
    if secs == 0 {
        return "0000:00:00 00:00:00".into();
    }
    // Epoch difference: 1904-01-01 to 1970-01-01 = 2082844800 seconds
    const EPOCH_DIFF: u64 = 2_082_844_800;
    if secs < EPOCH_DIFF {
        return format!("{secs}");
    }
    let unix = secs - EPOCH_DIFF;
    // Simple date calculation (no leap second precision needed)
    let days = unix / 86400;
    let time = unix % 86400;
    let h = time / 3600;
    let m = (time % 3600) / 60;
    let s = time % 60;

    // Days to Y-M-D
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}:{mo:02}:{d:02} {h:02}:{m:02}:{s:02}")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let era;
    let doe;
    let yoe;
    let doy;
    let mp;

    days += 719468;
    era = days / 146097;
    doe = days - era * 146097;
    yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// Format duration in seconds as "H:MM:SS" or "M:SS".
pub fn format_duration(secs: f64) -> String {
    let total_secs = secs.round() as u64;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Format major brand to human-readable string (matching ExifTool).
pub fn format_brand(brand: &[u8; 4]) -> String {
    match brand {
        b"isom" => "MP4 Base Media v1 [IS0 14496-12:2003]".into(),
        b"iso2" => "MP4 Base Media v2 [ISO 14496-12:2005]".into(),
        b"mp41" => "MP4 v1 [ISO 14496-1:ch13]".into(),
        b"mp42" => "MP4 v2 [ISO 14496-14]".into(),
        b"M4A " => "Apple iTunes AAC-LC (.M4A) Audio".into(),
        b"M4V " => "Apple iTunes Video (.M4V) Video".into(),
        b"qt  " => "Apple QuickTime (.MOV/QT)".into(),
        b"3gp4" => "3GPP Media (.3GP) Release 4".into(),
        b"3gp5" => "3GPP Media (.3GP) Release 5".into(),
        b"3gp6" => "3GPP Media (.3GP) Release 6".into(),
        b"avc1" => "MP4 Base w/ AVC ext [ISO 14496-12:2005]".into(),
        b"dash" => "MPEG-DASH".into(),
        b"f4v " => "Adobe Flash Video".into(),
        _ => {
            let s = String::from_utf8_lossy(brand);
            s.trim().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brand_detection() {
        assert!(is_quicktime_brand(b"isom"));
        assert!(is_quicktime_brand(b"mp42"));
        assert!(is_quicktime_brand(b"M4A "));
        assert!(is_quicktime_brand(b"qt  "));
        assert!(!is_quicktime_brand(b"heic"));
        assert!(!is_quicktime_brand(b"avif"));
    }

    #[test]
    fn format_qt_date_test() {
        // 2017-01-08 12:18:42 UTC
        // Unix: 1483877922
        // QT: 1483877922 + 2082844800 = 3566722722
        assert_eq!(format_qt_date(3566722722), "2017:01:08 12:18:42");
    }

    #[test]
    fn format_qt_date_zero() {
        assert_eq!(format_qt_date(0), "0000:00:00 00:00:00");
    }

    #[test]
    fn format_duration_test() {
        assert_eq!(format_duration(30.0), "0:30");
        assert_eq!(format_duration(90.0), "1:30");
        assert_eq!(format_duration(3661.0), "1:01:01");
    }

    /// one box: size, type, body
    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend_from_slice(body);
        b
    }

    /// a version 0 mvhd: times, time scale, duration (the rest of the header left zero)
    fn mvhd(time_scale: u32, duration: u32) -> Vec<u8> {
        let mut body = vec![0u8; 4];
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes());
        body.extend_from_slice(&time_scale.to_be_bytes());
        body.extend_from_slice(&duration.to_be_bytes());
        body.resize(100, 0);
        mp4_box(b"mvhd", &body)
    }

    fn ftyp() -> Vec<u8> {
        mp4_box(b"ftyp", b"isom\0\0\x02\0isomiso2mp41")
    }

    #[test]
    fn duration_from_the_movie_header_after_the_media() {
        // a recording as a meeting tool writes it: the media first, the moov at the end
        let mut f = ftyp();
        f.extend(mp4_box(b"mdat", &vec![0u8; 64 * 1024]));
        f.extend(mp4_box(b"moov", &mvhd(1000, 17_250)));
        let info = parse_quicktime(&f).unwrap();
        assert_eq!(info.duration_secs(), Some(17.25));
    }

    #[test]
    fn duration_of_a_fragmented_file_from_mehd() {
        // the movie header says the first fragment only; mehd the whole
        let mut mehd = vec![1u8, 0, 0, 0];
        mehd.extend_from_slice(&22_000u64.to_be_bytes());
        let mut moov = mvhd(1000, 0);
        moov.extend(mp4_box(b"mvex", &mp4_box(b"mehd", &mehd)));
        let mut f = ftyp();
        f.extend(mp4_box(b"moov", &moov));
        let info = parse_quicktime(&f).unwrap();
        assert_eq!(info.fragment_duration, Some(22_000));
        assert_eq!(info.duration_secs(), Some(22.0));
    }

    #[test]
    fn no_duration_when_the_file_says_none() {
        let mut f = ftyp();
        f.extend(mp4_box(b"moov", &mvhd(1000, 0)));
        assert_eq!(parse_quicktime(&f).unwrap().duration_secs(), None);
    }

    #[test]
    fn format_brand_test() {
        assert_eq!(format_brand(b"mp42"), "MP4 v2 [ISO 14496-14]");
        assert_eq!(format_brand(b"qt  "), "Apple QuickTime (.MOV/QT)");
    }
}
