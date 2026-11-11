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
}
