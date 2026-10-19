//! TIFF/IFD parser (T1-T11), EXIF decoder (E1-E10), tag tables & value pipeline (V1-V10).

pub mod exif;
pub mod maker_notes;
pub mod tags;
pub mod value;

use crate::core::{Error, Reader, RecursionGuard, Result};

/// Maximum number of IFD entries we'll accept in a single IFD (DoS protection).
const MAX_IFD_ENTRIES: usize = 10_000;

/// Maximum number of IFDs to follow in a chain. Cycles are caught by the guard's visited set;
/// this bounds only the work a hostile file can demand. Scanned multi-page documents routinely
/// run to several hundred pages (one IFD each), so the bound must sit well above that: at 100
/// the walker silently returned the first 100 pages of a longer document.
const MAX_IFD_CHAIN: usize = 65_536;

/// TIFF data types with their sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum DataType {
    Byte = 1,
    Ascii = 2,
    Short = 3,
    Long = 4,
    Rational = 5,
    SByte = 6,
    Undefined = 7,
    SShort = 8,
    SLong = 9,
    SRational = 10,
    Float = 11,
    Double = 12,
    Ifd = 13,
    // BigTIFF types (T4)
    Long8 = 16,
    SLong8 = 17,
    Ifd8 = 18,
}

impl DataType {
    /// Try to convert a raw u16 to a DataType.
    pub fn from_u16(v: u16) -> Option<DataType> {
        match v {
            1 => Some(DataType::Byte),
            2 => Some(DataType::Ascii),
            3 => Some(DataType::Short),
            4 => Some(DataType::Long),
            5 => Some(DataType::Rational),
            6 => Some(DataType::SByte),
            7 => Some(DataType::Undefined),
            8 => Some(DataType::SShort),
            9 => Some(DataType::SLong),
            10 => Some(DataType::SRational),
            11 => Some(DataType::Float),
            12 => Some(DataType::Double),
            13 => Some(DataType::Ifd),
            16 => Some(DataType::Long8),
            17 => Some(DataType::SLong8),
            18 => Some(DataType::Ifd8),
            _ => None,
        }
    }

    /// Size of one element of this type in bytes.
    pub fn size(&self) -> usize {
        match self {
            DataType::Byte | DataType::SByte | DataType::Ascii | DataType::Undefined => 1,
            DataType::Short | DataType::SShort => 2,
            DataType::Long | DataType::SLong | DataType::Float | DataType::Ifd => 4,
            DataType::Rational
            | DataType::SRational
            | DataType::Double
            | DataType::Long8
            | DataType::SLong8
            | DataType::Ifd8 => 8,
        }
    }
}

/// A raw IFD entry before value interpretation.
#[derive(Debug, Clone)]
pub struct IfdEntry<'a> {
    /// Tag ID.
    pub tag: u16,
    /// Data type.
    pub data_type: DataType,
    /// Raw type value (preserved even if DataType is unknown).
    pub raw_type: u16,
    /// Number of values.
    pub count: u64,
    /// The raw bytes of the value (resolved from inline or offset).
    pub data: &'a [u8],
    /// Whether the value was stored inline in the offset field.
    pub inline: bool,
}

impl<'a> IfdEntry<'a> {
    /// Total byte size of the value data.
    pub fn value_size(&self) -> u64 {
        self.count * self.data_type.size() as u64
    }

    /// Read value as a single u16 (for SHORT type with count=1).
    pub fn as_u16(&self, big_endian: bool) -> Option<u16> {
        if self.count == 1 && self.data.len() >= 2 {
            Some(if big_endian {
                u16::from_be_bytes([self.data[0], self.data[1]])
            } else {
                u16::from_le_bytes([self.data[0], self.data[1]])
            })
        } else {
            None
        }
    }

    /// Read value as a single u32 (for LONG type with count=1).
    pub fn as_u32(&self, big_endian: bool) -> Option<u32> {
        if self.count == 1 && self.data.len() >= 4 {
            Some(if big_endian {
                u32::from_be_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            } else {
                u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            })
        } else {
            None
        }
    }

    /// Read value as ASCII string (strips trailing null bytes).
    pub fn as_ascii(&self) -> Option<&'a str> {
        if self.data_type == DataType::Ascii {
            let s = self.data;
            // Strip trailing nulls
            let end = s.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
            std::str::from_utf8(&s[..end]).ok()
        } else {
            None
        }
    }

    /// Read value as a rational (numerator, denominator).
    pub fn as_rational(&self, big_endian: bool) -> Option<(u32, u32)> {
        if self.count == 1 && self.data.len() >= 8 && (self.data_type == DataType::Rational) {
            let num = if big_endian {
                u32::from_be_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            } else {
                u32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            };
            let den = if big_endian {
                u32::from_be_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
            } else {
                u32::from_le_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
            };
            Some((num, den))
        } else {
            None
        }
    }

    /// Read value as a signed rational (numerator, denominator).
    pub fn as_srational(&self, big_endian: bool) -> Option<(i32, i32)> {
        if self.count == 1 && self.data.len() >= 8 && (self.data_type == DataType::SRational) {
            let num = if big_endian {
                i32::from_be_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            } else {
                i32::from_le_bytes([self.data[0], self.data[1], self.data[2], self.data[3]])
            };
            let den = if big_endian {
                i32::from_be_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
            } else {
                i32::from_le_bytes([self.data[4], self.data[5], self.data[6], self.data[7]])
            };
            Some((num, den))
        } else {
            None
        }
    }
}

/// A parsed IFD (Image File Directory).
#[derive(Debug, Clone)]
pub struct Ifd<'a> {
    /// Byte offset of this IFD in the TIFF data.
    pub offset: u64,
    /// The entries in this IFD.
    pub entries: Vec<IfdEntry<'a>>,
    /// Offset of the next IFD (0 = no next IFD).
    pub next_ifd_offset: u64,
}

impl<'a> Ifd<'a> {
    /// Find an entry by tag ID.
    pub fn entry(&self, tag: u16) -> Option<&IfdEntry<'a>> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    /// Get all sub-IFD offsets from tag 0x014A (SubIFDs array).
    /// Returns empty vec if tag not present.
    pub fn sub_ifd_offsets(&self, big_endian: bool) -> Vec<u64> {
        match self.entry(0x014A) {
            Some(entry) => read_u64_array(entry, big_endian),
            None => Vec::new(),
        }
    }

    /// Get a sub-IFD offset from a tag (e.g., ExifIFD 0x8769, GPS 0x8825).
    pub fn sub_ifd_offset(&self, tag: u16, big_endian: bool) -> Option<u64> {
        let entry = self.entry(tag)?;
        if entry.data_type == DataType::Long {
            entry.as_u32(big_endian).map(|v| v as u64)
        } else if entry.data_type == DataType::Long8 && entry.data.len() >= 8 {
            Some(if big_endian {
                u64::from_be_bytes(entry.data[..8].try_into().ok()?)
            } else {
                u64::from_le_bytes(entry.data[..8].try_into().ok()?)
            })
        } else {
            entry.as_u32(big_endian).map(|v| v as u64)
        }
    }
}

/// Parsed TIFF header information.
#[derive(Debug, Clone, Copy)]
pub struct TiffHeader {
    /// True if big-endian (Motorola byte order, `MM`), false if little-endian (Intel, `II`).
    pub big_endian: bool,
    /// True if this is a BigTIFF file (8-byte offsets).
    pub bigtiff: bool,
    /// Offset of IFD0.
    pub ifd0_offset: u64,
}

/// Parse a TIFF header (T1, T11).
pub fn parse_header(data: &[u8]) -> Result<TiffHeader> {
    if data.len() < 8 {
        return Err(Error::Truncated {
            needed: 8,
            available: data.len(),
        });
    }

    // T1: Byte order
    let big_endian = match (data[0], data[1]) {
        (b'I', b'I') => false,
        (b'M', b'M') => true,
        _ => return Err(Error::Format("invalid TIFF byte order marker".into())),
    };

    let magic = if big_endian {
        u16::from_be_bytes([data[2], data[3]])
    } else {
        u16::from_le_bytes([data[2], data[3]])
    };

    match magic {
        42 | 0x4F52 | 0x5352 | 0x55 => {
            // Standard TIFF (42), Olympus ORF (0x4F52 'OR', 0x5352 'SR'),
            // Panasonic RW2 (0x55)
            let ifd0_offset = if big_endian {
                u32::from_be_bytes([data[4], data[5], data[6], data[7]])
            } else {
                u32::from_le_bytes([data[4], data[5], data[6], data[7]])
            } as u64;

            Ok(TiffHeader {
                big_endian,
                bigtiff: false,
                ifd0_offset,
            })
        }
        43 => {
            // T11: BigTIFF
            if data.len() < 16 {
                return Err(Error::Truncated {
                    needed: 16,
                    available: data.len(),
                });
            }

            let bytesize = if big_endian {
                u16::from_be_bytes([data[4], data[5]])
            } else {
                u16::from_le_bytes([data[4], data[5]])
            };

            if bytesize != 8 {
                return Err(Error::Format(format!(
                    "BigTIFF offset size must be 8, got {bytesize}"
                )));
            }

            // Reserved bytes at offset 6-7 must be 0
            let ifd0_offset = if big_endian {
                u64::from_be_bytes(data[8..16].try_into().unwrap())
            } else {
                u64::from_le_bytes(data[8..16].try_into().unwrap())
            };

            Ok(TiffHeader {
                big_endian,
                bigtiff: true,
                ifd0_offset,
            })
        }
        _ => Err(Error::Format(format!(
            "invalid TIFF magic number: {magic} (expected 42 or 43)"
        ))),
    }
}

/// Parse a single IFD at the given offset (T2, T3, T5, T7, T9).
pub fn parse_ifd<'a>(
    data: &'a [u8],
    offset: u64,
    big_endian: bool,
    bigtiff: bool,
) -> Result<Ifd<'a>> {
    let reader = Reader::new(data);
    let off = offset as usize;

    // T7: Validate we have enough data for entry count
    let (entry_count, entries_start, entry_size, next_ifd_size) = if bigtiff {
        // BigTIFF: 8-byte entry count, 20-byte entries, 8-byte next IFD offset
        if off + 8 > data.len() {
            return Err(Error::Truncated {
                needed: 8,
                available: data.len().saturating_sub(off),
            });
        }
        let count = if big_endian {
            u64::from_be_bytes(data[off..off + 8].try_into().unwrap())
        } else {
            u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
        };
        (count as usize, off + 8, 20usize, 8usize)
    } else {
        // Standard TIFF: 2-byte entry count, 12-byte entries, 4-byte next IFD offset
        if off + 2 > data.len() {
            return Err(Error::Truncated {
                needed: 2,
                available: data.len().saturating_sub(off),
            });
        }
        let count = reader.peek_u16(off, big_endian)? as usize;
        (count, off + 2, 12usize, 4usize)
    };

    // T7: Sanity check entry count
    if entry_count > MAX_IFD_ENTRIES {
        return Err(Error::Format(format!(
            "IFD entry count {entry_count} exceeds maximum {MAX_IFD_ENTRIES}"
        )));
    }

    let entries_total_size = entry_count * entry_size;
    let ifd_end = entries_start + entries_total_size;

    // T7: Check we have enough data for all entries
    if ifd_end > data.len() {
        return Err(Error::Truncated {
            needed: entries_total_size,
            available: data.len().saturating_sub(entries_start),
        });
    }

    let mut entries = Vec::with_capacity(entry_count);

    for i in 0..entry_count {
        let entry_off = entries_start + i * entry_size;
        let entry = parse_ifd_entry(data, entry_off, big_endian, bigtiff)?;
        entries.push(entry);
    }

    // Next IFD offset (T6)
    let next_ifd_offset = if ifd_end + next_ifd_size <= data.len() {
        if bigtiff {
            if big_endian {
                u64::from_be_bytes(data[ifd_end..ifd_end + 8].try_into().unwrap())
            } else {
                u64::from_le_bytes(data[ifd_end..ifd_end + 8].try_into().unwrap())
            }
        } else {
            let v = if big_endian {
                u32::from_be_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
            } else {
                u32::from_le_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
            };
            v as u64
        }
    } else {
        0
    };

    Ok(Ifd {
        offset,
        entries,
        next_ifd_offset,
    })
}

/// Parse an IFD tolerantly - skip entries with out-of-bounds value offsets
/// instead of failing the entire IFD. Used for embedded TIFF structures (e.g.,
/// Nikon Type 3 MakerNotes) where some value offsets may exceed the slice bounds.
pub fn parse_ifd_tolerant<'a>(
    data: &'a [u8],
    offset: u64,
    big_endian: bool,
    bigtiff: bool,
) -> Option<Ifd<'a>> {
    let off = offset as usize;

    let (entry_count, entries_start, entry_size, next_ifd_size) = if bigtiff {
        if off + 8 > data.len() {
            return None;
        }
        let count = if big_endian {
            u64::from_be_bytes(data[off..off + 8].try_into().unwrap())
        } else {
            u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
        } as usize;
        (count, off + 8, 20usize, 8usize)
    } else {
        if off + 2 > data.len() {
            return None;
        }
        let count = if big_endian {
            u16::from_be_bytes([data[off], data[off + 1]])
        } else {
            u16::from_le_bytes([data[off], data[off + 1]])
        } as usize;
        (count, off + 2, 12usize, 4usize)
    };

    if entry_count > MAX_IFD_ENTRIES {
        return None;
    }

    let entries_total_size = entry_count * entry_size;
    let ifd_end = entries_start + entries_total_size;
    if ifd_end > data.len() {
        return None;
    }

    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let entry_off = entries_start + i * entry_size;
        if let Ok(entry) = parse_ifd_entry(data, entry_off, big_endian, bigtiff) {
            entries.push(entry);
        }
        // Skip entries that fail (out-of-bounds offsets, etc.)
    }

    let next_ifd_offset = if ifd_end + next_ifd_size <= data.len() {
        if bigtiff {
            if big_endian {
                u64::from_be_bytes(data[ifd_end..ifd_end + 8].try_into().unwrap())
            } else {
                u64::from_le_bytes(data[ifd_end..ifd_end + 8].try_into().unwrap())
            }
        } else {
            let v = if big_endian {
                u32::from_be_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
            } else {
                u32::from_le_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
            };
            v as u64
        }
    } else {
        0
    };

    Some(Ifd {
        offset,
        entries,
        next_ifd_offset,
    })
}

/// Parse an IFD with a base offset adjustment for relocated MakerNotes.
///
/// `base_adjust` is added to all external value offsets before resolving them.
/// A positive value shifts offsets forward, negative shifts backward.
pub fn parse_ifd_with_base<'a>(
    data: &'a [u8],
    offset: u64,
    big_endian: bool,
    base_adjust: i64,
) -> Result<Ifd<'a>> {
    let reader = Reader::new(data);
    let off = offset as usize;

    if off + 2 > data.len() {
        return Err(Error::Truncated {
            needed: 2,
            available: data.len().saturating_sub(off),
        });
    }
    let entry_count = reader.peek_u16(off, big_endian)? as usize;

    if entry_count > MAX_IFD_ENTRIES {
        return Err(Error::Format(format!(
            "IFD entry count {entry_count} exceeds maximum {MAX_IFD_ENTRIES}"
        )));
    }

    let entries_start = off + 2;
    let entries_total_size = entry_count * 12;
    let ifd_end = entries_start + entries_total_size;

    if ifd_end > data.len() {
        return Err(Error::Truncated {
            needed: entries_total_size,
            available: data.len().saturating_sub(entries_start),
        });
    }

    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let entry_off = entries_start + i * 12;
        let entry = parse_ifd_entry_with_base(data, entry_off, big_endian, base_adjust)?;
        entries.push(entry);
    }

    let next_ifd_offset = if ifd_end + 4 <= data.len() {
        let v = if big_endian {
            u32::from_be_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
        } else {
            u32::from_le_bytes(data[ifd_end..ifd_end + 4].try_into().unwrap())
        };
        v as u64
    } else {
        0
    };

    Ok(Ifd {
        offset,
        entries,
        next_ifd_offset,
    })
}

/// Parse a single IFD entry with a base offset adjustment.
fn parse_ifd_entry_with_base<'a>(
    data: &'a [u8],
    off: usize,
    big_endian: bool,
    base_adjust: i64,
) -> Result<IfdEntry<'a>> {
    let reader = Reader::new(data);

    let tag = reader.peek_u16(off, big_endian)?;
    let raw_type = reader.peek_u16(off + 2, big_endian)?;
    let data_type = DataType::from_u16(raw_type).unwrap_or(DataType::Undefined);

    let count = reader.peek_u32(off + 4, big_endian)? as u64;
    let value_size = count.saturating_mul(data_type.size() as u64);

    let (entry_data, inline) = if value_size <= 4 {
        let end = (off + 8 + value_size as usize).min(off + 12);
        (&data[off + 8..end], true)
    } else {
        let raw_offset = reader.peek_u32(off + 8, big_endian)? as i64;
        let value_offset = (raw_offset + base_adjust) as usize;

        if value_offset >= data.len() {
            return Err(Error::Truncated {
                needed: value_size as usize,
                available: 0,
            });
        }
        let end = value_offset
            .saturating_add(value_size as usize)
            .min(data.len());
        (&data[value_offset..end], false)
    };

    Ok(IfdEntry {
        tag,
        data_type,
        raw_type,
        count,
        data: entry_data,
        inline,
    })
}

/// Parse a single IFD entry (T2, T3, T5).
fn parse_ifd_entry<'a>(
    data: &'a [u8],
    off: usize,
    big_endian: bool,
    bigtiff: bool,
) -> Result<IfdEntry<'a>> {
    let reader = Reader::new(data);

    // T2: tag (u16), type (u16)
    let tag = reader.peek_u16(off, big_endian)?;
    let raw_type = reader.peek_u16(off + 2, big_endian)?;
    let data_type = DataType::from_u16(raw_type).unwrap_or(DataType::Undefined);

    if bigtiff {
        // BigTIFF entry: tag(2) + type(2) + count(8) + value/offset(8) = 20 bytes
        let count = if big_endian {
            u64::from_be_bytes(data[off + 4..off + 12].try_into().unwrap())
        } else {
            u64::from_le_bytes(data[off + 4..off + 12].try_into().unwrap())
        };

        let value_size = count.saturating_mul(data_type.size() as u64);
        let (entry_data, inline) = if value_size <= 8 {
            // T5: Inline value
            let end = (off + 12 + value_size as usize).min(off + 20);
            (&data[off + 12..end], true)
        } else {
            // Offset pointer
            let value_offset = if big_endian {
                u64::from_be_bytes(data[off + 12..off + 20].try_into().unwrap())
            } else {
                u64::from_le_bytes(data[off + 12..off + 20].try_into().unwrap())
            } as usize;

            let end = value_offset
                .saturating_add(value_size as usize)
                .min(data.len());
            if value_offset >= data.len() {
                return Err(Error::Truncated {
                    needed: value_size as usize,
                    available: 0,
                });
            }
            (&data[value_offset..end], false)
        };

        Ok(IfdEntry {
            tag,
            data_type,
            raw_type,
            count,
            data: entry_data,
            inline,
        })
    } else {
        // Standard TIFF entry: tag(2) + type(2) + count(4) + value/offset(4) = 12 bytes
        let count = reader.peek_u32(off + 4, big_endian)? as u64;

        // T7: Validate count against reasonable limits
        let value_size = count.saturating_mul(data_type.size() as u64);

        let (entry_data, inline) = if value_size <= 4 {
            // T5: Value fits inline in the 4-byte offset field
            let end = (off + 8 + value_size as usize).min(off + 12);
            (&data[off + 8..end], true)
        } else {
            // Value stored at offset
            let value_offset = reader.peek_u32(off + 8, big_endian)? as usize;

            if value_offset >= data.len() {
                return Err(Error::Truncated {
                    needed: value_size as usize,
                    available: 0,
                });
            }
            let end = value_offset
                .saturating_add(value_size as usize)
                .min(data.len());
            (&data[value_offset..end], false)
        };

        Ok(IfdEntry {
            tag,
            data_type,
            raw_type,
            count,
            data: entry_data,
            inline,
        })
    }
}

/// Follow the IFD chain starting from the given offset (T6, T8).
///
/// Returns all IFDs in the chain. Uses a recursion guard to detect circular chains (T8).
pub fn parse_ifd_chain<'a>(
    data: &'a [u8],
    first_offset: u64,
    big_endian: bool,
    bigtiff: bool,
) -> Result<Vec<Ifd<'a>>> {
    let mut ifds = Vec::new();
    let mut guard = RecursionGuard::new(MAX_IFD_CHAIN);
    let mut offset = first_offset;

    while offset != 0 {
        // T8: Detect circular IFD chains - stop gracefully
        if guard.enter(offset).is_err() {
            break;
        }

        match parse_ifd(data, offset, big_endian, bigtiff) {
            Ok(ifd) => {
                let next = ifd.next_ifd_offset;
                ifds.push(ifd);
                offset = next;
            }
            Err(_) => break, // Gracefully stop on parse errors in chain
        }
    }

    Ok(ifds)
}

/// Parse a complete TIFF structure: header + IFD chain.
pub fn parse_tiff<'a>(data: &'a [u8]) -> Result<(TiffHeader, Vec<Ifd<'a>>)> {
    let header = parse_header(data)?;
    let ifds = parse_ifd_chain(data, header.ifd0_offset, header.big_endian, header.bigtiff)?;
    Ok((header, ifds))
}

/// T10: Extract strip/tile data offsets and byte counts for raw image data.
pub fn strip_offsets_and_counts<'a>(
    ifd: &Ifd<'a>,
    big_endian: bool,
) -> Option<(Vec<u64>, Vec<u64>)> {
    // StripOffsets = tag 273, TileOffsets = tag 324
    // StripByteCounts = tag 279, TileByteCounts = 325
    let offsets_entry = ifd.entry(273).or_else(|| ifd.entry(324))?;
    let counts_entry = ifd.entry(279).or_else(|| ifd.entry(325))?;

    let offsets = read_u64_array(offsets_entry, big_endian);
    let counts = read_u64_array(counts_entry, big_endian);

    if offsets.is_empty() || counts.is_empty() {
        return None;
    }

    Some((offsets, counts))
}

/// Read an IFD entry as a `Vec<u64>`, handling SHORT, LONG, and LONG8 types.
pub fn read_u64_array(entry: &IfdEntry<'_>, big_endian: bool) -> Vec<u64> {
    let mut values = Vec::with_capacity(entry.count as usize);
    let data = entry.data;

    match entry.data_type {
        DataType::Short => {
            for i in 0..entry.count as usize {
                let off = i * 2;
                if off + 2 > data.len() {
                    break;
                }
                let v = if big_endian {
                    u16::from_be_bytes([data[off], data[off + 1]])
                } else {
                    u16::from_le_bytes([data[off], data[off + 1]])
                };
                values.push(v as u64);
            }
        }
        DataType::Long => {
            for i in 0..entry.count as usize {
                let off = i * 4;
                if off + 4 > data.len() {
                    break;
                }
                let v = if big_endian {
                    u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                } else {
                    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
                };
                values.push(v as u64);
            }
        }
        DataType::Long8 => {
            for i in 0..entry.count as usize {
                let off = i * 8;
                if off + 8 > data.len() {
                    break;
                }
                let v = if big_endian {
                    u64::from_be_bytes(data[off..off + 8].try_into().unwrap())
                } else {
                    u64::from_le_bytes(data[off..off + 8].try_into().unwrap())
                };
                values.push(v);
            }
        }
        _ => {}
    }

    values
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A little-endian TIFF whose chain holds `n` minimal IFDs (one SHORT entry each).
    fn build_chain_le(n: usize) -> Vec<u8> {
        const IFD_SIZE: u32 = 2 + 12 + 4;
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        for k in 0..n {
            data.extend_from_slice(&1u16.to_le_bytes()); // one entry
            data.extend_from_slice(&0x0100u16.to_le_bytes()); // ImageWidth
            data.extend_from_slice(&3u16.to_le_bytes()); // SHORT
            data.extend_from_slice(&1u32.to_le_bytes());
            data.extend_from_slice(&[1, 0, 0, 0]);
            let next = if k + 1 == n { 0 } else { 8 + IFD_SIZE * (k as u32 + 1) };
            data.extend_from_slice(&next.to_le_bytes());
        }
        data
    }

    #[test]
    fn a_long_ifd_chain_is_followed_to_its_end() {
        let data = build_chain_le(150);
        let (_, ifds) = parse_tiff(&data).unwrap();
        assert_eq!(ifds.len(), 150);
    }

    #[test]
    fn a_circular_ifd_chain_stops() {
        let mut data = build_chain_le(3);
        let last_next = data.len() - 4;
        data[last_next..].copy_from_slice(&8u32.to_le_bytes()); // last IFD points back at the first
        let (_, ifds) = parse_tiff(&data).unwrap();
        assert_eq!(ifds.len(), 3);
    }

    /// Build a minimal TIFF file (LE) with the given IFD entries.
    /// Each entry is (tag, type, count, value_bytes).
    fn build_tiff_le(entries: &[(u16, u16, u32, Vec<u8>)]) -> Vec<u8> {
        let mut data = Vec::new();

        // Header: II, magic 42, IFD0 offset = 8
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());

        // IFD at offset 8
        let entry_count = entries.len() as u16;
        data.extend_from_slice(&entry_count.to_le_bytes());

        // Calculate where external values will go
        let ifd_entries_end = 8 + 2 + entries.len() * 12 + 4; // header + count + entries + next_ifd
        let mut external_offset = ifd_entries_end;
        let mut external_data = Vec::new();

        for &(tag, dtype, count, ref value) in entries {
            data.extend_from_slice(&tag.to_le_bytes());
            data.extend_from_slice(&dtype.to_le_bytes());
            data.extend_from_slice(&count.to_le_bytes());

            let type_size = DataType::from_u16(dtype).map_or(1, |t| t.size());
            let total = count as usize * type_size;

            if total <= 4 {
                // Inline value (padded to 4 bytes)
                let mut padded = [0u8; 4];
                let copy_len = value.len().min(4);
                padded[..copy_len].copy_from_slice(&value[..copy_len]);
                data.extend_from_slice(&padded);
            } else {
                // External value: store offset
                data.extend_from_slice(&(external_offset as u32).to_le_bytes());
                external_data.extend_from_slice(value);
                external_offset += value.len();
            }
        }

        // Next IFD offset = 0 (no next)
        data.extend_from_slice(&0u32.to_le_bytes());

        // Append external data
        data.extend_from_slice(&external_data);

        data
    }

    #[test]
    fn t1_parse_header_le() {
        let data = build_tiff_le(&[]);
        let h = parse_header(&data).unwrap();
        assert!(!h.big_endian);
        assert!(!h.bigtiff);
        assert_eq!(h.ifd0_offset, 8);
    }

    #[test]
    fn t1_parse_header_be() {
        let mut data = Vec::new();
        data.extend_from_slice(b"MM");
        data.extend_from_slice(&42u16.to_be_bytes());
        data.extend_from_slice(&8u32.to_be_bytes());

        let h = parse_header(&data).unwrap();
        assert!(h.big_endian);
        assert!(!h.bigtiff);
        assert_eq!(h.ifd0_offset, 8);
    }

    #[test]
    fn t1_invalid_byte_order() {
        let data = [b'X', b'X', 0, 42, 0, 0, 0, 8];
        assert!(parse_header(&data).is_err());
    }

    #[test]
    fn t1_invalid_magic() {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&99u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        assert!(parse_header(&data).is_err());
    }

    #[test]
    fn t2_parse_ifd_entry() {
        // SHORT tag 256 (ImageWidth) = 1920
        let data = build_tiff_le(&[(256, 3, 1, 1920u16.to_le_bytes().to_vec())]);
        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        assert_eq!(ifd.entries.len(), 1);
        assert_eq!(ifd.entries[0].tag, 256);
        assert_eq!(ifd.entries[0].data_type, DataType::Short);
        assert_eq!(ifd.entries[0].count, 1);
    }

    #[test]
    fn t3_all_data_types() {
        for (type_id, expected) in [
            (1u16, DataType::Byte),
            (2, DataType::Ascii),
            (3, DataType::Short),
            (4, DataType::Long),
            (5, DataType::Rational),
            (6, DataType::SByte),
            (7, DataType::Undefined),
            (8, DataType::SShort),
            (9, DataType::SLong),
            (10, DataType::SRational),
            (11, DataType::Float),
            (12, DataType::Double),
            (16, DataType::Long8),
            (17, DataType::SLong8),
            (18, DataType::Ifd8),
        ] {
            assert_eq!(DataType::from_u16(type_id), Some(expected));
        }
        assert_eq!(DataType::from_u16(99), None);
    }

    #[test]
    fn t5_inline_vs_offset() {
        // Inline: SHORT count=1 (2 bytes <= 4)
        // Offset: ASCII count=10 (10 bytes > 4)
        let data = build_tiff_le(&[
            (256, 3, 1, 1920u16.to_le_bytes().to_vec()),
            (270, 2, 10, b"test desc\0".to_vec()),
        ]);
        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();

        assert!(ifd.entries[0].inline); // SHORT fits in 4 bytes
        assert!(!ifd.entries[1].inline); // ASCII 10 bytes doesn't fit

        // Verify values
        assert_eq!(ifd.entries[0].as_u16(false), Some(1920));
        assert_eq!(ifd.entries[1].as_ascii(), Some("test desc"));
    }

    #[test]
    fn t6_ifd_chain() {
        // Build TIFF with IFD0 pointing to IFD1
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes()); // IFD0 at offset 8

        // IFD0: 1 entry + next IFD offset
        data.extend_from_slice(&1u16.to_le_bytes()); // count = 1
        // Entry: tag=256, SHORT, count=1, value=100
        data.extend_from_slice(&256u16.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&100u32.to_le_bytes());
        // Next IFD offset = 26 (8 + 2 + 12 + 4 = 26)
        let ifd1_offset = (8 + 2 + 12 + 4) as u32;
        data.extend_from_slice(&ifd1_offset.to_le_bytes());

        // IFD1: 1 entry + no next IFD
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&257u16.to_le_bytes()); // ImageLength
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&200u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

        let h = parse_header(&data).unwrap();
        let ifds = parse_ifd_chain(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        assert_eq!(ifds.len(), 2);
        assert_eq!(ifds[0].entries[0].tag, 256);
        assert_eq!(ifds[1].entries[0].tag, 257);
    }

    #[test]
    fn t7_validate_entry_count() {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        // Claim 60000 entries (way more than data available)
        data.extend_from_slice(&60000u16.to_le_bytes());

        let h = parse_header(&data).unwrap();
        assert!(parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).is_err());
    }

    #[test]
    fn t8_circular_ifd_chain() {
        // Build a TIFF where IFD0's next pointer points back to IFD0
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&42u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());

        // IFD0: 1 entry, next = 8 (back to self!)
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&256u16.to_le_bytes());
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&1u32.to_le_bytes());
        data.extend_from_slice(&100u32.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes()); // circular!

        let h = parse_header(&data).unwrap();
        let ifds = parse_ifd_chain(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        // Should get exactly 1 IFD (cycle detected on second visit)
        assert_eq!(ifds.len(), 1);
    }

    #[test]
    fn t9_byte_order_consistency() {
        // Build a BE TIFF
        let mut data = Vec::new();
        data.extend_from_slice(b"MM");
        data.extend_from_slice(&42u16.to_be_bytes());
        data.extend_from_slice(&8u32.to_be_bytes());

        // IFD0: 1 entry
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&256u16.to_be_bytes()); // ImageWidth
        data.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        data.extend_from_slice(&1u32.to_be_bytes()); // count=1
        let mut value = [0u8; 4];
        value[0..2].copy_from_slice(&1920u16.to_be_bytes());
        data.extend_from_slice(&value);
        data.extend_from_slice(&0u32.to_be_bytes()); // no next IFD

        let h = parse_header(&data).unwrap();
        assert!(h.big_endian);
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        assert_eq!(ifd.entries[0].as_u16(true), Some(1920));
    }

    #[test]
    fn t10_strip_offsets() {
        // StripOffsets (273) = [100, 200, 300], StripByteCounts (279) = [50, 50, 50]
        let mut offsets = Vec::new();
        for v in [100u32, 200, 300] {
            offsets.extend_from_slice(&v.to_le_bytes());
        }
        let mut counts = Vec::new();
        for v in [50u32, 50, 50] {
            counts.extend_from_slice(&v.to_le_bytes());
        }

        let data = build_tiff_le(&[
            (273, 4, 3, offsets), // StripOffsets, LONG, count=3
            (279, 4, 3, counts),  // StripByteCounts, LONG, count=3
        ]);

        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        let (offs, cnts) = strip_offsets_and_counts(&ifd, false).unwrap();
        assert_eq!(offs, vec![100, 200, 300]);
        assert_eq!(cnts, vec![50, 50, 50]);
    }

    #[test]
    fn t11_bigtiff_header() {
        let mut data = Vec::new();
        data.extend_from_slice(b"II");
        data.extend_from_slice(&43u16.to_le_bytes()); // BigTIFF magic
        data.extend_from_slice(&8u16.to_le_bytes()); // offset size = 8
        data.extend_from_slice(&0u16.to_le_bytes()); // reserved
        data.extend_from_slice(&16u64.to_le_bytes()); // IFD0 offset

        let h = parse_header(&data).unwrap();
        assert!(h.bigtiff);
        assert!(!h.big_endian);
        assert_eq!(h.ifd0_offset, 16);
    }

    #[test]
    fn rational_value() {
        // RATIONAL tag: numerator=1, denominator=100
        let mut val = Vec::new();
        val.extend_from_slice(&1u32.to_le_bytes());
        val.extend_from_slice(&100u32.to_le_bytes());

        let data = build_tiff_le(&[
            (0x829A, 5, 1, val), // ExposureTime, RATIONAL
        ]);

        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        let (num, den) = ifd.entries[0].as_rational(false).unwrap();
        assert_eq!(num, 1);
        assert_eq!(den, 100);
    }

    #[test]
    fn entry_lookup() {
        let data = build_tiff_le(&[
            (256, 3, 1, 1920u16.to_le_bytes().to_vec()),
            (257, 3, 1, 1080u16.to_le_bytes().to_vec()),
        ]);
        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();

        assert!(ifd.entry(256).is_some());
        assert!(ifd.entry(257).is_some());
        assert!(ifd.entry(999).is_none());
    }

    #[test]
    fn sub_ifd_offset() {
        // Tag 0x8769 (ExifIFD pointer) = LONG with value 100
        let data = build_tiff_le(&[(0x8769, 4, 1, 100u32.to_le_bytes().to_vec())]);
        let h = parse_header(&data).unwrap();
        let ifd = parse_ifd(&data, h.ifd0_offset, h.big_endian, h.bigtiff).unwrap();
        assert_eq!(ifd.sub_ifd_offset(0x8769, false), Some(100));
    }

    #[test]
    fn parse_tiff_complete() {
        let data = build_tiff_le(&[
            (256, 3, 1, 640u16.to_le_bytes().to_vec()),
            (257, 3, 1, 480u16.to_le_bytes().to_vec()),
        ]);
        let (header, ifds) = parse_tiff(&data).unwrap();
        assert!(!header.big_endian);
        assert_eq!(ifds.len(), 1);
        assert_eq!(ifds[0].entries.len(), 2);
    }
}
