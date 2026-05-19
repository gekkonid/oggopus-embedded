/*
 * Copyright (c) 2026 Gekkonid Scientfic
 * SPDX-License-Identifier: BSD-3-Clause
 */
//! Ogg Opus serialisation.

use crate::opus::OpusHeader;

/// Error from writing ogg opus data.
#[derive(Debug, PartialEq)]
pub enum OggWriteError {
    /// Output buffer is too small. Contains (available, needed) sizes.
    BufferTooSmall(usize, usize),
    /// Invalid input data.
    InvalidInput(&'static str),
}

impl core::fmt::Display for OggWriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            OggWriteError::BufferTooSmall(available, needed) => {
                write!(f, "buffer too small: {} available, {} needed", available, needed)
            }
            OggWriteError::InvalidInput(msg) => f.write_str(msg),
        }
    }
}

impl core::error::Error for OggWriteError {}

// Ogg CRC-32 table (polynomial 0x04C11DB7, no reflection, init 0, no final xor)
const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut j = 0;
        while j < 8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn ogg_crc_update(crc: u32, data: &[u8]) -> u32 {
    let mut crc = crc;
    for &byte in data {
        let idx = ((crc >> 24) as u8 ^ byte) as usize;
        crc = (crc << 8) ^ CRC_TABLE[idx];
    }
    crc
}

/// Serialises an OpusHead header packet into `buf`. Returns the number of bytes written.
///
/// `OpusHeader` must be a valid Family 0 (mono or stereo) header.
/// Panics if `buf` is too small (needs at least 19 bytes).
fn write_opus_head(header: &OpusHeader, buf: &mut [u8]) -> Result<usize, OggWriteError> {
    let channels = header.channels.get_channel_count();
    let table_len: usize = match &header.channels {
        crate::opus::ChannelMapping::Family0 { .. } => 0,
        crate::opus::ChannelMapping::Family1 { channels, .. } => {
            let table_size = *channels as usize + 2;
            table_size
        }
        #[cfg(feature = "family255")]
        crate::opus::ChannelMapping::Family255 { .. } | crate::opus::ChannelMapping::Reserved { .. } => {
            return Err(OggWriteError::InvalidInput("family255 not supported for writing"))
        }
    };

    // OpusHead (8) + version (1) + channels (1) + pre_skip (2) + sample_rate (4)
    // + output_gain (2) + channel_mapping_family (1) + optional table
    let needed = 19 + table_len;
    if buf.len() < needed {
        return Err(OggWriteError::BufferTooSmall(buf.len(), needed));
    }

    buf[0..8].copy_from_slice(b"OpusHead");
    buf[8] = header.version;
    buf[9] = channels;
    buf[10..12].copy_from_slice(&header.pre_skip.to_le_bytes());
    buf[12..16].copy_from_slice(&header.sample_rate.to_le_bytes());
    buf[16..18].copy_from_slice(&header.output_gain.to_le_bytes());

    match &header.channels {
        crate::opus::ChannelMapping::Family0 { .. } => {
            buf[18] = 0;
        }
        crate::opus::ChannelMapping::Family1 { channels, table } => {
            buf[18] = 1;
            buf[19] = table.stream_count;
            buf[20] = table.coupled_count;
            buf[21..21 + *channels as usize].copy_from_slice(&table.mapping[..*channels as usize]);
        }
        #[cfg(feature = "family255")]
        _ => unreachable!(),
    }

    Ok(needed)
}

// Magic OggS page sync code
const OGG_MAGIC: &[u8; 4] = b"OggS";

fn write_ogg_page(
    out: &mut [u8],
    header_type: u8,
    granule: u64,
    serial: u32,
    page_seq: u32,
    packet: &[u8],
) -> Result<usize, OggWriteError> {
    // Build segment table
    let packet_len = packet.len();
    // Ogg requires segments of 0-254 bytes; 255 means "continued on next segment".
    // When the remaining data is exactly 255 bytes we need two segments:
    // [255, 0] (the zero terminates the packet)
    let seg_count = if packet_len == 0 {
        1
    } else {
        (packet_len + 255) / 255
    };
    if seg_count > 255 {
        return Err(OggWriteError::InvalidInput("packet too large for one page"));
    }

    // Page header: 27 bytes (4+1+1+8+4+4+4+1) + seg_count bytes segment table + payload
    let needed = 27 + seg_count + packet_len;
    if out.len() < needed {
        return Err(OggWriteError::BufferTooSmall(out.len(), needed));
    }

    // Write header with CRC field set to 0 for computation
    let page_seq_bytes = page_seq.to_le_bytes();
    let serial_bytes = serial.to_le_bytes();
    let granule_bytes = granule.to_le_bytes();

    out[0..4].copy_from_slice(OGG_MAGIC);
    out[4] = 0; // version
    out[5] = header_type;
    out[6..14].copy_from_slice(&granule_bytes);
    out[14..18].copy_from_slice(&serial_bytes);
    out[18..22].copy_from_slice(&page_seq_bytes);
    out[22..26].copy_from_slice(&[0; 4]); // CRC = 0 for now
    out[26] = seg_count as u8;

    // Write segment table
    let mut remaining = packet_len;
    let mut pos = 27;
    while remaining > 255 {
        out[pos] = 255;
        pos += 1;
        remaining -= 255;
    }
    if packet_len == 0 {
        // Zero-length packet: single 0 segment
        out[pos] = 0;
        pos += 1;
    } else {
        out[pos] = remaining as u8;
        pos += 1;
        // If the last data chunk fills a segment exactly (255 bytes),
        // add a zero segment to terminate the packet.  Without this
        // the demuxer believes the packet continues on the next page
        // and opusinfo reports "no completed packets" and undercounts
        // the sample count by one frame per affected page.
        if remaining == 255 {
            out[pos] = 0;
            pos += 1;
        }
    }

    // Write packet data
    let data_start = pos;
    if !packet.is_empty() {
        out[data_start..data_start + packet_len].copy_from_slice(packet);
    }

    // Compute CRC over the entire page with CRC field set to 0
    let crc = ogg_crc_update(0, &out[..needed]);
    out[22..26].copy_from_slice(&crc.to_le_bytes());

    Ok(needed)
}

/// Ogg Opus writer. Builds Ogg pages one at a time into caller-provided buffers.
#[derive(Debug)]
pub struct OggWriter {
    serial: u32,
    page_seq: u32,
    granule: u64,
    pre_skip: u16,
    bos_written: bool,
}

impl OggWriter {
    /// Create a new writer for a logical bitstream.
    ///
    /// `serial` is the Ogg bitstream serial number.
    /// `pre_skip` is the Opus pre-skip value (typically 3840 at 48 kHz).
    pub fn new(serial: u32, pre_skip: u16) -> Self {
        OggWriter {
            serial,
            page_seq: 0,
            granule: 0,
            pre_skip,
            bos_written: false,
        }
    }

    /// Write the OpusHead identification header page.
    ///
    /// This must be the first page written, with BeginOfStream flag set.
    pub fn write_header(&mut self, header: &OpusHeader, out: &mut [u8]) -> Result<usize, OggWriteError> {
        let mut head_buf = [0u8; 19 + 2 + 8]; // max: Family1 with 8 channels
        let head_len = write_opus_head(header, &mut head_buf)?;

        let flags = 0x02; // BeginOfStream
        let result = write_ogg_page(
            out,
            flags,
            0, // granule = 0 for header
            self.serial,
            self.page_seq,
            &head_buf[..head_len],
        )?;
        self.page_seq += 1;
        self.bos_written = true;
        Ok(result)
    }

    /// Write the OpusTags comment header page.
    ///
    /// Must follow the identification header. `vendor` is an arbitrary string.
    /// `tags` is a list of "KEY=VALUE" comment strings.
    pub fn write_tags(&mut self, vendor: &str, tags: &[&str], out: &mut [u8]) -> Result<usize, OggWriteError> {
        if !self.bos_written {
            return Err(OggWriteError::InvalidInput("header must be written before tags"));
        }

        // Build OpusTags packet
        let vendor_bytes = vendor.as_bytes();
        let vendor_len = vendor_bytes.len();
        let mut tags_total = 0;
        for tag in tags {
            tags_total += 4 + tag.len();
        }

        let packet_len = 8 // "OpusTags"
            + 4 // vendor_length
            + vendor_len
            + 4 // user_comment_list_length
            + tags_total;

        // Create a temporary buffer for the packet (limit to reasonable size)
        let packet_needed = packet_len;
        if packet_needed > 65536 {
            return Err(OggWriteError::InvalidInput("tags packet too large"));
        }
        let mut packet_buf = [0u8; 65536];
        if packet_needed > packet_buf.len() {
            return Err(OggWriteError::BufferTooSmall(packet_buf.len(), packet_needed));
        }

        let mut pos = 0;
        packet_buf[0..8].copy_from_slice(b"OpusTags");
        pos += 8;
        packet_buf[pos..pos + 4].copy_from_slice(&(vendor_len as u32).to_le_bytes());
        pos += 4;
        packet_buf[pos..pos + vendor_len].copy_from_slice(vendor_bytes);
        pos += vendor_len;
        packet_buf[pos..pos + 4].copy_from_slice(&(tags.len() as u32).to_le_bytes());
        pos += 4;
        for tag in tags {
            let tag_bytes = tag.as_bytes();
            packet_buf[pos..pos + 4].copy_from_slice(&(tag_bytes.len() as u32).to_le_bytes());
            pos += 4;
            packet_buf[pos..pos + tag_bytes.len()].copy_from_slice(tag_bytes);
            pos += tag_bytes.len();
        }

        let flags = 0x00; // no BOS, no EOS
        let result = write_ogg_page(
            out,
            flags,
            0, // granule = 0 for comments
            self.serial,
            self.page_seq,
            &packet_buf[..packet_len],
        )?;
        self.page_seq += 1;
        Ok(result)
    }

    /// Write an audio data page with one Opus packet.
    ///
    /// `packet` is the raw encoded Opus frame.
    /// `samples` is the number of 48 kHz samples per channel in this frame.
    /// `is_last` should be true for the final page (sets EndOfStream flag).
    /// `out` is the buffer to write the page into.
    pub fn write_packet(
        &mut self,
        packet: &[u8],
        samples: u16,
        is_last: bool,
        out: &mut [u8],
    ) -> Result<usize, OggWriteError> {
        if !self.bos_written {
            return Err(OggWriteError::InvalidInput("header must be written before audio packets"));
        }

        self.granule += samples as u64;
        let granule = self.granule + if is_last { self.pre_skip as u64 } else { 0 };

        let mut flags = 0x00;
        if is_last {
            flags |= 0x04; // EndOfStream
        }

        let result = write_ogg_page(out, flags, granule, self.serial, self.page_seq, packet)?;
        self.page_seq += 1;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus::{ChannelMapping, OpusHeader};

    #[test]
    fn write_header_has_ogg_magic() {
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: 3840,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut buf = [0u8; 256];
        let mut writer = OggWriter::new(42, 3840);
        let written = writer.write_header(&header, &mut buf).unwrap();

        assert!(written >= 27 + 1 + 19); // Ogg header + seg table + OpusHead
        assert_eq!(&buf[0..4], b"OggS");
        assert_eq!(buf[5], 0x02); // BOS flag
        assert_eq!(buf[26], 1);   // 1 segment
        // Verify the segment is our OpusHead packet
        let seg_len = buf[27] as usize;
        assert_eq!(&buf[28..28 + 8], b"OpusHead");
        assert_eq!(seg_len, 19);
    }

    #[test]
    fn write_and_read_back_header() {
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: 3840,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut buf = [0u8; 256];
        let mut writer = OggWriter::new(42, 3840);
        let written = writer.write_header(&header, &mut buf).unwrap();

        // Find the OpusHead packet payload after the segment table
        let seg_count = buf[26] as usize;
        let data_start = 27 + seg_count;
        let packet = &buf[data_start..written];

        // Parse it back
        let parsed = OpusHeader::parse(packet).unwrap();
        assert_eq!(parsed.version, header.version);
        assert_eq!(parsed.channels, header.channels);
        assert_eq!(parsed.pre_skip, header.pre_skip);
        assert_eq!(parsed.sample_rate, header.sample_rate);
        assert_eq!(parsed.output_gain, header.output_gain);
    }

    #[test]
    fn write_tags_has_opus_tags_magic() {
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: 3840,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut buf = [0u8; 1024];
        let mut writer = OggWriter::new(42, 3840);

        writer.write_header(&header, &mut buf).unwrap();
        let written = writer.write_tags("test-vendor", &["ENCODER=test"], &mut buf).unwrap();

        assert!(&buf[0..4] == b"OggS");
        // Tags page is at page_seq=1
        let seq = u32::from_le_bytes(buf[18..22].try_into().unwrap());
        assert_eq!(seq, 1);

        // Extract packet and check OpusTags magic
        let seg_count = buf[26] as usize;
        let data_start = 27 + seg_count;
        let packet = &buf[data_start..written];
        assert_eq!(&packet[0..8], b"OpusTags");
    }

    #[test]
    fn write_packet_single_page() {
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: 3840,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut writer = OggWriter::new(12345, 3840);
        let mut buf = [0u8; 4096];

        // Write header
        let off = writer.write_header(&header, &mut buf).unwrap();

        // Write tags
        let tags_off = off;
        let off2 = off + writer.write_tags("rust-opus", &[], &mut buf[off..]).unwrap();

        // Write first audio packet (not last)
        let packet_data = &[0xABu8; 100];
        let audio1_off = off2;
        let off3 = off2 + writer.write_packet(packet_data, 960, false, &mut buf[off2..]).unwrap();

        // Write second audio packet (last)
        let audio2_off = off3;
        let _off4 = off3 + writer.write_packet(packet_data, 960, true, &mut buf[off3..]).unwrap();

        // Verify header page at buf[0]
        assert_eq!(&buf[0..4], b"OggS");
        assert_eq!(buf[5], 0x02); // BOS
        assert_eq!(u32::from_le_bytes(buf[18..22].try_into().unwrap()), 0); // page 0

        // Verify tags page
        assert_eq!(&buf[tags_off..tags_off + 4], b"OggS");
        assert_eq!(buf[tags_off + 5], 0x00);
        assert_eq!(u32::from_le_bytes(buf[tags_off + 18..tags_off + 22].try_into().unwrap()), 1);

        // Verify audio page 1: granule = 960 (no pre_skip), page_seq = 2
        let g1 = u64::from_le_bytes(buf[audio1_off + 6..audio1_off + 14].try_into().unwrap());
        assert_eq!(g1, 960);
        assert_eq!(u32::from_le_bytes(buf[audio1_off + 18..audio1_off + 22].try_into().unwrap()), 2);
        assert_eq!(buf[audio1_off + 5], 0x00);

        // Verify audio page 2: granule = 960 + 960 + pre_skip = 5760, EOS
        let g2 = u64::from_le_bytes(buf[audio2_off + 6..audio2_off + 14].try_into().unwrap());
        assert_eq!(g2, 960 + 960 + 3840);
        assert_eq!(u32::from_le_bytes(buf[audio2_off + 18..audio2_off + 22].try_into().unwrap()), 3);
        assert_eq!(buf[audio2_off + 5], 0x04); // EOS

        // Serial consistent across all pages
        assert_eq!(u32::from_le_bytes(buf[14..18].try_into().unwrap()), 12345);
        assert_eq!(u32::from_le_bytes(buf[audio2_off + 14..audio2_off + 18].try_into().unwrap()), 12345);

        // Each page carries the correct packet data
        let seg1_count = buf[audio1_off + 26] as usize;
        let seg1_size = buf[audio1_off + 27] as usize;
        assert_eq!(seg1_size, 100);
        assert_eq!(&buf[audio1_off + 28..audio1_off + 28 + seg1_size], &[0xABu8; 100]);

        // Verify segment table is correct (100 bytes < 255, so single segment)
        assert_eq!(seg1_count, 1);
    }

    #[test]
    fn write_multiple_packets_tracks_granule() {
        let pre_skip = 3840u64;
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: pre_skip as u16,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut writer = OggWriter::new(1, pre_skip as u16);
        let mut buf = [0u8; 4096];
        let mut off = 0;

        off += writer.write_header(&header, &mut buf[off..]).unwrap();
        off += writer.write_tags("test", &[], &mut buf[off..]).unwrap();
        off += writer.write_packet(b"packet-a", 960, false, &mut buf[off..]).unwrap();
        off += writer.write_packet(b"packet-b", 480, false, &mut buf[off..]).unwrap();
        let _ = writer.write_packet(b"packet-c", 960, true, &mut buf[off..]).unwrap();

        let hdr_len = 28usize;

        // Check granule positions
        let mut pos = 0;
        pos += {
            let seg = buf[hdr_len - 1] as usize;
            hdr_len + seg
        };

        // Page 2: just first packet =  960
        pos += {
            // Page 1
            let seg = buf[pos + hdr_len - 1] as usize;
            hdr_len + seg
        };
        let g2 = u64::from_le_bytes(buf[pos + 6..pos + 14].try_into().unwrap());
        assert_eq!(g2, 960);

        // Page 3: first two packets = 960 + 480
        let seg3 = buf[pos + hdr_len - 1] as usize;
        let p3_start = pos;
        pos += hdr_len + seg3;
        let g3 = u64::from_le_bytes(buf[pos + 6..pos + 14].try_into().unwrap());
        assert_eq!(g3, 960 + 480);

        // Page 4: first three  packets = 960 + 480 + 960, but this is last packet so we add
        // pre-skip of 3480 too
        let seg4 = buf[pos + hdr_len - 1] as usize;
        let p4_start = pos;
        pos += hdr_len + seg4;
        let g4 = u64::from_le_bytes(buf[pos + 6..pos + 14].try_into().unwrap());
        assert_eq!(g4, 960 + 480 + 960 + pre_skip);
        assert_eq!(buf[pos + 5], 0x04); // EOS

        // Serial number should be consistent
        assert_eq!(u32::from_le_bytes(buf[14..18].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(buf[p3_start + 14..p3_start + 18].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(buf[p4_start + 14..p4_start + 18].try_into().unwrap()), 1);
    }

    #[test]
    fn tags_without_header_fails() {
        let mut writer = OggWriter::new(1, 3840);
        let mut buf = [0u8; 256];
        let result = writer.write_tags("test", &[], &mut buf);
        assert_eq!(result, Err(OggWriteError::InvalidInput("header must be written before tags")));
    }

    #[test]
    fn packet_without_header_fails() {
        let mut writer = OggWriter::new(1, 3840);
        let mut buf = [0u8; 256];
        let result = writer.write_packet(b"data", 960, false, &mut buf);
        assert_eq!(result, Err(OggWriteError::InvalidInput("header must be written before audio packets")));
    }

    #[test]
    fn packet_too_large_for_page() {
        // Max page segment table is 255 entries, each 255 bytes = 65025 bytes max
        // A single packet larger than that requires multi-page handling which we don't support
        let header = OpusHeader {
            version: 1,
            channels: ChannelMapping::Family0 { channels: 1 },
            pre_skip: 3840,
            sample_rate: 48000,
            output_gain: 0,
        };

        let mut writer = OggWriter::new(1, 3840);
        let mut buf = [0u8; 70000];
        let mut off = 0;
        off += writer.write_header(&header, &mut buf[off..]).unwrap();
        off += writer.write_tags("test", &[], &mut buf[off..]).unwrap();

        // Too-large packet (255*255 + 1 bytes)
        let huge = [0u8; 255 * 255 + 1];
        let result = writer.write_packet(&huge, 9999, false, &mut buf[off..]);
        assert_eq!(result, Err(OggWriteError::InvalidInput("packet too large for one page")));
    }
}
