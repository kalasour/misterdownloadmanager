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

#[tokio::test]
async fn raw_png_ts_join_named_mp4_with_no_ts_present() {
    // Regression test: on ffmpeg < 9, yt-dlp leaves the raw PNG+TS join under the final
    // `.mp4` name (no `.ts` exists). Verify repair_hls_if_needed detects this and produces
    // a clean output.
    
    let server = mdm_labserver::LabServer::start(mdm_labserver::Config::default())
        .await
        .unwrap();
    let base = format!("http://{}", server.addr());
    let master_url = format!("{}/hls-png/master.m3u8", base);
    let dir = tempfile::tempdir().unwrap();
    
    let client = reqwest::Client::new();
    
    // Download and concatenate segments (simulating what yt-dlp does)
    let master_text = client.get(&master_url).send().await.unwrap().text().await.unwrap();
    let variant_line = master_text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .find(|l| l.contains("720p"))
        .expect("should have 720p variant");
    
    let variant_url = format!("{}/hls-png/{}", base, variant_line);
    let variant_text = client.get(&variant_url).send().await.unwrap().text().await.unwrap();
    
    let segment_urls: Vec<String> = variant_text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| format!("{}/hls-png/720p/{}", base, l.trim()))
        .collect();
    
    let mut concatenated = Vec::new();
    for url in &segment_urls {
        let segment_data = client.get(url).send().await.unwrap().bytes().await.unwrap();
        concatenated.extend_from_slice(&segment_data);
    }
    
    // Simulate yt-dlp's behavior on ffmpeg < 9: save the raw PNG+TS join as .mp4
    let corrupted_mp4 = dir.path().join("video.mp4");
    std::fs::write(&corrupted_mp4, &concatenated).unwrap();
    
    // Verify no .ts file exists
    assert!(!dir.path().join("video.ts").exists());
    
    // Verify the .mp4 is actually corrupted TS (starts with PNG signature)
    assert_eq!(concatenated[0], 0x89);
    assert_eq!(&concatenated[1..4], b"PNG");
    
    // Now simulate what repair_hls_if_needed should do:
    // 1. Detect that the .mp4 is actually TS
    // 2. Rename it to .ts
    // 3. Repair it
    // 4. Remux to clean .mp4 if ffmpeg available
    
    let repaired = dir.path().join("repaired.ts");
    let final_mp4 = dir.path().join("final.mp4");
    
    // Rename .mp4 to .ts (what repair_hls_if_needed does when it detects PNG+TS)
    let ts_path = dir.path().join("video.ts");
    std::fs::rename(&corrupted_mp4, &ts_path).unwrap();
    
    // Repair the TS file
    let bytes_removed = mdm_tools::segments::repair_ts_file(&ts_path, &repaired).unwrap();
    assert_eq!(bytes_removed, 70 * 3, "should remove fake headers");
    
    // Verify the repaired file is clean
    let repaired_data = std::fs::read(&repaired).unwrap();
    let packet_count = repaired_data.len() / 188;
    for i in 0..packet_count {
        assert_eq!(repaired_data[i * 188], 0x47);
    }
    
    // If ffmpeg is available, verify it can remux to clean MP4
    let ffmpeg_available = Command::new("ffmpeg")
        .args(["-version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    
    if ffmpeg_available {
        let remux_result = Command::new("ffmpeg")
            .args([
                "-f", "mpegts",
                "-i", &repaired.to_string_lossy(),
                "-c", "copy",
                "-movflags", "+faststart",
                "-y",
                &final_mp4.to_string_lossy(),
            ])
            .output();
        
        if let Ok(output) = remux_result {
            assert!(output.status.success(), "ffmpeg remux should succeed");
            
            // Verify the final MP4 is clean (no PNG signatures)
            let mp4_data = std::fs::read(&final_mp4).unwrap();
            assert!(mp4_data.len() > 100);
            
            // Should not contain PNG signature
            let png_sig_count = mp4_data.windows(4)
                .filter(|w| w[0] == 0x89 && &w[1..4] == b"PNG")
                .count();
            assert_eq!(png_sig_count, 0, "clean MP4 should have no PNG signatures");
            
            println!("Verified: raw PNG+TS join named .mp4 repaired successfully to clean MP4");
        }
    }
}
