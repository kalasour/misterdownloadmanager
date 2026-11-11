//! Repair corrupted HLS/DASH segments: strip fake image headers some sites prepend to MPEG-TS
//! or fMP4 content to disguise streams as images (observed on TikTok's image CDN).

/// Find the offset of the first valid MPEG-TS sync byte in `data`, confirmed by checking that
/// the sync pattern (0x47) repeats at the expected packet boundaries (188 bytes apart).
/// Returns Some(offset) if a valid TS stream is found, None otherwise.
fn find_ts_sync(data: &[u8]) -> Option<usize> {
    const TS_SYNC: u8 = 0x47;
    const TS_PACKET_SIZE: usize = 188;
    
    // Need at least 3 packets to confirm sync pattern
    if data.len() < TS_PACKET_SIZE * 3 {
        return None;
    }
    
    // Search for sync byte candidates
    for offset in 0..data.len().saturating_sub(TS_PACKET_SIZE * 2) {
        if data[offset] == TS_SYNC {
            // Check if sync bytes repeat at packet boundaries
            let mut valid = true;
            for i in 1..3 {
                let next_sync_offset = offset + (i * TS_PACKET_SIZE);
                if next_sync_offset >= data.len() || data[next_sync_offset] != TS_SYNC {
                    valid = false;
                    break;
                }
            }
            if valid {
                return Some(offset);
            }
        }
    }
    
    None
}

/// Find the offset of the first valid fMP4 box in `data` by looking for the 'ftyp' box
/// (file type box) which should be at or near the beginning of an fMP4 segment.
fn find_fmp4_start(data: &[u8]) -> Option<usize> {
    // An fMP4 box starts with 4-byte size, then 4-byte type ('ftyp', 'moof', 'mdat', etc.)
    // Common init segments start with 'ftyp'
    const MIN_BOX_SIZE: usize = 8;
    
    if data.len() < MIN_BOX_SIZE {
        return None;
    }
    
    // Look for 'ftyp' box (common at the start of fMP4 segments)
    for offset in 0..data.len().saturating_sub(MIN_BOX_SIZE) {
        if offset + 8 <= data.len() {
            let box_type = &data[offset + 4..offset + 8];
            // Check for common fMP4 box types
            if box_type == b"ftyp" || box_type == b"moof" || box_type == b"mdat" {
                // Verify the size field is reasonable
                let size_bytes = &data[offset..offset + 4];
                let size = u32::from_be_bytes([
                    size_bytes[0],
                    size_bytes[1],
                    size_bytes[2],
                    size_bytes[3],
                ]) as usize;
                
                // Size should be at least MIN_BOX_SIZE and not exceed remaining data by too much
                if size >= MIN_BOX_SIZE && size <= data.len() + 1024 * 1024 {
                    return Some(offset);
                }
            }
        }
    }
    
    None
}

/// Strip any leading junk (fake image headers) from a media segment, returning the clean content.
/// - For MPEG-TS: strips everything before the first valid sync byte (0x47 at 188-byte intervals)
/// - For fMP4: strips everything before the first valid box structure
/// - If no valid stream is found or the data is already clean, returns the original data unchanged
pub fn strip_fake_header(data: &[u8]) -> &[u8] {
    // First try to find MPEG-TS sync
    if let Some(offset) = find_ts_sync(data) {
        if offset > 0 {
            // Found junk before the TS stream
            return &data[offset..];
        } else {
            // Already starts with valid TS
            return data;
        }
    }
    
    // Try fMP4 detection
    if let Some(offset) = find_fmp4_start(data)
        && offset > 0 {
            return &data[offset..];
        }
    
    // No recognizable stream format or already clean
    data
}

/// Strip fake headers from data in place, modifying the Vec to contain only the clean segment.
/// More efficient than `strip_fake_header` when ownership of the data is available.
pub fn strip_fake_header_owned(mut data: Vec<u8>) -> Vec<u8> {
    // Try MPEG-TS first
    if let Some(offset) = find_ts_sync(&data)
        && offset > 0 {
            data.drain(..offset);
            return data;
        }
    
    // Try fMP4
    if let Some(offset) = find_fmp4_start(&data)
        && offset > 0 {
            data.drain(..offset);
            return data;
        }
    
    data
}

/// Repair a concatenated MPEG-TS file that has fake headers at segment boundaries.
/// Walks through the file and removes any junk between valid TS packets, writing only
/// valid packets (those starting with 0x47 at proper 188-byte boundaries) to the output.
/// 
/// Returns Ok(bytes_removed) if any junk was found and removed, Ok(0) if the file was
/// already clean, or Err if the file couldn't be read/written or isn't a valid TS file.
pub fn repair_ts_file(input: &std::path::Path, output: &std::path::Path) -> std::io::Result<usize> {
    use std::io::Write;
    
    const TS_PACKET_SIZE: usize = 188;
    const TS_SYNC: u8 = 0x47;
    
    // Read entire file (video segments are typically small enough)
    let data = std::fs::read(input)?;
    let mut output_file = std::fs::File::create(output)?;
    
    let mut pos = 0;
    let mut packets_written = 0;
    // An unverified packet must directly follow the last written one, so a lone 0x47 in junk
    // like the 'G' of '\x89PNG' isn't taken as a packet
    let mut last_end = usize::MAX;
    
    while pos < data.len() {
        // Try to find a valid TS sync pattern at current position
        if pos + TS_PACKET_SIZE * 3 <= data.len()
            && data[pos] == TS_SYNC
            && data[pos + TS_PACKET_SIZE] == TS_SYNC
            && data[pos + TS_PACKET_SIZE * 2] == TS_SYNC
        {
            // Found valid TS sync pattern - write this packet
            output_file.write_all(&data[pos..pos + TS_PACKET_SIZE])?;
            packets_written += 1;
            pos += TS_PACKET_SIZE;
            last_end = pos;
        } else if pos + TS_PACKET_SIZE <= data.len()
            && packets_written > 0
            && data[pos] == TS_SYNC
            && pos == last_end
        {
            // We're continuing from valid packets and found a sync byte
            // (last packet of the file might not have 3 packets after it to verify)
            output_file.write_all(&data[pos..pos + TS_PACKET_SIZE])?;
            packets_written += 1;
            pos += TS_PACKET_SIZE;
            last_end = pos;
        } else {
            // Not a valid sync pattern or at end - skip this byte (it's junk)
            pos += 1;
        }
    }
    
    output_file.flush()?;
    
    let bytes_written = packets_written * TS_PACKET_SIZE;
    let bytes_removed = data.len() - bytes_written;
    Ok(bytes_removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_valid_ts_stream_with_no_prefix() {
        // Create a minimal valid TS stream (3 packets)
        let mut data = Vec::new();
        for _ in 0..3 {
            let mut packet = vec![0x47]; // sync byte
            packet.extend(vec![0; 187]); // rest of packet
            data.extend(packet);
        }
        
        assert_eq!(find_ts_sync(&data), Some(0));
        assert_eq!(strip_fake_header(&data), &data[..]);
    }

    #[test]
    fn strips_fake_png_header_from_ts() {
        // PNG signature
        let fake_png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let mut junk = fake_png.clone();
        junk.extend(vec![0; 62]); // total 70 bytes of junk (as observed)
        
        // Valid TS stream
        let mut ts_data = Vec::new();
        for _ in 0..3 {
            let mut packet = vec![0x47];
            packet.extend(vec![0; 187]);
            ts_data.extend(packet);
        }
        
        let mut prefixed = junk.clone();
        prefixed.extend(&ts_data);
        
        assert_eq!(find_ts_sync(&prefixed), Some(70));
        assert_eq!(strip_fake_header(&prefixed), &ts_data[..]);
    }

    #[test]
    fn strips_fake_jpeg_header_from_ts() {
        // JPEG signature
        let fake_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0];
        let mut junk = fake_jpeg.clone();
        junk.extend(vec![0; 66]); // total 70 bytes
        
        // Valid TS stream
        let mut ts_data = Vec::new();
        for _ in 0..3 {
            let mut packet = vec![0x47];
            packet.extend(vec![0; 187]);
            ts_data.extend(packet);
        }
        
        let mut prefixed = junk.clone();
        prefixed.extend(&ts_data);
        
        assert_eq!(find_ts_sync(&prefixed), Some(70));
        assert_eq!(strip_fake_header(&prefixed), &ts_data[..]);
    }

    #[test]
    fn finds_fmp4_ftyp_box() {
        let mut data = vec![0; 10]; // some junk
        // ftyp box: 4-byte size (20), then 'ftyp', then brand 'isom'
        data.extend(&20u32.to_be_bytes());
        data.extend(b"ftyp");
        data.extend(b"isom");
        data.extend(vec![0; 8]); // rest of box
        
        assert_eq!(find_fmp4_start(&data), Some(10));
    }

    #[test]
    fn returns_original_for_unrecognized_data() {
        let random_data = vec![0x12, 0x34, 0x56, 0x78, 0x9A];
        assert_eq!(strip_fake_header(&random_data), &random_data[..]);
    }

    #[test]
    fn owned_version_works_correctly() {
        let fake_png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let mut prefixed = fake_png.clone();
        prefixed.extend(vec![0; 62]);
        
        let mut ts_data = Vec::new();
        for _ in 0..3 {
            let mut packet = vec![0x47];
            packet.extend(vec![0; 187]);
            ts_data.extend(packet);
        }
        
        prefixed.extend(&ts_data);
        
        let cleaned = strip_fake_header_owned(prefixed);
        assert_eq!(cleaned, ts_data);
    }

    #[test]
    fn repairs_ts_file_with_fake_headers_at_segment_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("corrupted.ts");
        let output = dir.path().join("repaired.ts");
        
        // Create a "corrupted" TS file: 3 segments, each with a fake PNG header
        let mut corrupted = Vec::new();
        
        for segment_idx in 0..3 {
            // Fake header (70 bytes) - use a pattern that definitely won't match TS sync
            // (avoid 0x47 entirely to prevent false positives)
            corrupted.extend(vec![0x89, 0x50, 0x4E, 0x45, 0x0D, 0x0A, 0x1A, 0x0A]); // PNG-like but no 0x47
            corrupted.extend(vec![0xFFu8; 62]); // Use 0xFF instead of 0 to avoid accidental sync
            
            // Valid TS packets (10 packets per segment)
            for packet_idx in 0..10 {
                let mut packet = vec![0x47]; // sync byte
                // Make packets slightly different - use high bit set to avoid 0x47 accidentally
                packet.push(0x80 | (segment_idx * 10 + packet_idx));
                packet.extend(vec![0xAA; 186]); // Use 0xAA instead of 0
                corrupted.extend(packet);
            }
        }
        
        std::fs::write(&input, &corrupted).unwrap();
        
        let bytes_removed = repair_ts_file(&input, &output).unwrap();
        assert_eq!(bytes_removed, 70 * 3, "should remove 70 bytes per segment");
        
        // Verify the repaired file
        let repaired = std::fs::read(&output).unwrap();
        assert_eq!(repaired.len(), 188 * 30, "should have exactly 30 packets");
        
        // Verify every packet starts with sync byte at correct position
        for i in 0..30 {
            let offset = i * 188;
            assert_eq!(
                repaired[offset], 0x47,
                "packet {} should start with sync byte",
                i
            );
        }
        
        // Verify packet content is preserved  
        assert_eq!(repaired[1], 0x80); // First segment, first packet
        assert_eq!(repaired[188 + 1], 0x81); // First segment, second packet
        assert_eq!(repaired[188 * 10 + 1], 0x8A); // Second segment, first packet
        assert_eq!(repaired[188 * 20 + 1], 0x94); // Third segment, first packet
    }

    #[test]
    fn repair_does_nothing_to_clean_ts_file() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("clean.ts");
        let output = dir.path().join("output.ts");
        
        // Create a clean TS file
        let mut clean = Vec::new();
        for i in 0..20 {
            let mut packet = vec![0x47];
            packet.push(i);
            packet.extend(vec![0; 186]);
            clean.extend(packet);
        }
        
        std::fs::write(&input, &clean).unwrap();
        
        let bytes_removed = repair_ts_file(&input, &output).unwrap();
        assert_eq!(bytes_removed, 0, "clean file should have nothing removed");
        
        let repaired = std::fs::read(&output).unwrap();
        assert_eq!(repaired, clean, "clean file should be unchanged");
    }

    #[test]
    fn repair_handles_junk_in_middle_of_stream() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("junk.ts");
        let output = dir.path().join("clean.ts");
        
        let mut data = Vec::new();
        
        // First 5 clean packets
        for i in 0..5 {
            let mut packet = vec![0x47];
            packet.push(i);
            packet.extend(vec![0; 186]);
            data.extend(packet);
        }
        
        // 100 bytes of junk
        data.extend(vec![0xFF; 100]);
        
        // Next 5 clean packets
        for i in 5..10 {
            let mut packet = vec![0x47];
            packet.push(i);
            packet.extend(vec![0; 186]);
            data.extend(packet);
        }
        
        std::fs::write(&input, &data).unwrap();
        
        let bytes_removed = repair_ts_file(&input, &output).unwrap();
        assert_eq!(bytes_removed, 100);
        
        let repaired = std::fs::read(&output).unwrap();
        assert_eq!(repaired.len(), 188 * 10);
        
        // Verify all 10 packets are there with correct content
        for i in 0..10 {
            assert_eq!(repaired[i * 188], 0x47);
            assert_eq!(repaired[i * 188 + 1], i as u8);
        }
    }
}
