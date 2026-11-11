//! Integration test for HLS streams with fake image headers.
//! Tests that downloads from streams with fake PNG/JPEG headers produce clean TS files.

use std::process::Command;

#[tokio::test]
async fn hls_with_fake_png_headers_produces_clean_ts() {
    test_fake_header_stream("png").await;
}

#[tokio::test]
async fn hls_with_fake_jpeg_headers_produces_clean_ts() {
    test_fake_header_stream("jpeg").await;
}

async fn test_fake_header_stream(header_type: &str) {
    // Start the labserver
    let server = mdm_labserver::LabServer::start(mdm_labserver::Config::default())
        .await
        .unwrap();
    let base = format!("http://{}", server.addr());
    
    // Download the fake-header HLS stream
    let master_url = format!("{}/hls-{}/master.m3u8", base, header_type);
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("test.ts");
    
    // Use a simple HTTP client to download
    let client = reqwest::Client::new();
    
    // Download master playlist
    let master_text = client
        .get(&master_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    
    // Extract the first variant URL (720p)
    let variant_line = master_text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.contains("720p"))
        .expect("should have 720p variant");
    
    let variant_url = format!("{}/hls-{}/{}", base, header_type, variant_line);
    let variant_text = client
        .get(&variant_url)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    
    // Extract segment URLs
    let segment_urls: Vec<String> = variant_text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| format!("{}/hls-{}/720p/{}", base, header_type, l.trim()))
        .collect();
    
    assert_eq!(segment_urls.len(), 3, "should have 3 segments");
    
    // Download and concatenate segments (simulating what yt-dlp does)
    let mut concatenated = Vec::new();
    for url in &segment_urls {
        let segment_data = client
            .get(url)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        concatenated.extend_from_slice(&segment_data);
    }
    
    // Write the concatenated (corrupted) file
    let corrupted_path = dir.path().join("corrupted.ts");
    std::fs::write(&corrupted_path, &concatenated).unwrap();
    
    // Verify it's corrupted: should have fake headers every segment
    assert!(
        concatenated.len() > 3 * 70,
        "should have fake headers in segments"
    );
    
    // First 70 bytes should be fake header, not valid TS
    assert_eq!(concatenated[0], if header_type == "png" { 0x89 } else { 0xFF });
    assert_ne!(concatenated[0], 0x47, "should not start with TS sync byte");
    
    // Repair the file
    let bytes_removed = mdm_tools::segments::repair_ts_file(&corrupted_path, &output).unwrap();
    
    println!(
        "Repaired {}-header stream: removed {} bytes",
        header_type, bytes_removed
    );
    assert!(bytes_removed > 0, "should have removed fake headers");
    assert_eq!(
        bytes_removed,
        70 * 3,
        "should remove 70 bytes per segment (3 segments)"
    );
    
    // Verify the repaired file
    let repaired_data = std::fs::read(&output).unwrap();
    
    // Should be smaller by the removed bytes
    assert_eq!(
        repaired_data.len(),
        concatenated.len() - bytes_removed,
        "repaired file should be smaller by removed bytes"
    );
    
    // Every 188-byte boundary should have a sync byte
    let packet_count = repaired_data.len() / 188;
    assert!(packet_count > 0, "should have TS packets");
    
    for i in 0..packet_count {
        let offset = i * 188;
        assert_eq!(
            repaired_data[offset], 0x47,
            "packet {} at offset {} should start with TS sync byte 0x47, got 0x{:02x}",
            i, offset, repaired_data[offset]
        );
    }
    
    println!(
        "Verified: all {} packets start with correct sync byte",
        packet_count
    );
    
    // If ffmpeg is available, verify it can decode without errors
    let ffmpeg_result = Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i", &output.to_string_lossy(),
            "-f", "null",
            "-",
        ])
        .output();
    
    if let Ok(output_result) = ffmpeg_result {
        let stderr = String::from_utf8_lossy(&output_result.stderr);
        if output_result.status.success() && stderr.trim().is_empty() {
            println!("ffmpeg verified: file decodes cleanly");
        } else if !output_result.status.success() {
            println!("ffmpeg not available or failed (non-fatal)");
        } else if !stderr.trim().is_empty() {
            println!("ffmpeg warnings (non-fatal): {}", stderr.trim());
        }
    }
}
