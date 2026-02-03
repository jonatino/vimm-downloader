use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use sevenz_rust::SevenZReader;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::Path;
use zip::ZipArchive;

fn main() -> Result<()> {
    // Check for --test flag
    let test_mode = false;
    if test_mode {
        println!("*** TEST MODE: Files will NOT be deleted on CRC mismatch ***");
    }

    // Create downloads folder if it doesn't exist
    let downloads_dir = "downloads";
    fs::create_dir_all(downloads_dir)?;

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/144.0.0.0 Safari/537.36")
        .build()?;

    loop {
        // Read links.txt file
        let links_content = match fs::read_to_string("links.txt") {
            Ok(content) => content,
            Err(e) => {
                eprintln!("Error reading links.txt: {}. Waiting...", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        let urls: Vec<String> = links_content
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
            .map(|s| s.to_string())
            .collect();

        if urls.is_empty() {
            println!("No URLs found in links.txt. Waiting...");
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }

        let mut downloaded_any = false;
        for url in urls {
            if !url.contains("vimm.net/vault/") {
                println!("Skipping invalid URL: {}", url);
                continue;
            }

            match process_url(&client, &url, downloads_dir, test_mode) {
                Ok(true) => {
                    downloaded_any = true;
                    println!("Successfully downloaded from: {}", url);
                    println!()
                }
                Ok(false) => {
                    println!("File already exists for: {}", url);
                    println!()
                }
                Err(e) => {
                    eprintln!("Error processing {}: {}", url, e);
                }
            }
        }

        if !downloaded_any {
            println!("All files already downloaded. Waiting for new links...");
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

fn process_url(client: &Client, url: &str, downloads_dir: &str, test_mode: bool) -> Result<bool> {
    println!("Processing: {}", url);

    // Fetch the vault page
    let response = client
        .get(url)
        .send()
        .context("Failed to fetch vault page")?;

    let html = response.text()?;
    let document = Html::parse_document(&html);

    // Debug: Write HTML to file to inspect
    // fs::write("debug.html", &html)?;

    // Find the download link - look for links containing "download" or media IDs
    // Try multiple selectors to find the actual download link
    let (download_url, media_id) = find_download_link(&document, url)?;

    println!("Download URL: {}", download_url);
    println!("Media ID: {}", media_id);

    // Extract CRC hash from the page for verification (required)
    let expected_crc =
        extract_hash(&document, "data-crc").context("Could not find CRC hash on page")?;
    println!("Expected CRC: {}", expected_crc);

    // Extract filename from the page title or use vault ID
    let title_selector = Selector::parse("title").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|el| el.text().collect::<String>())
        .unwrap_or_else(|| "download".to_string());

    // Clean up title for filename
    let vault_id = url.split('/').last().unwrap_or("unknown");
    let filename = sanitize_filename(&title, vault_id);

    // Download the file
    println!("Initiating download...");

    // Submit GET request with mediaId as query parameter
    let download_url_with_params = format!("{}?mediaId={}", download_url, media_id);
    println!("Final download URL: {}", download_url_with_params);

    let mut response = client
        .get(&download_url_with_params)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8")
        .header("Accept-Encoding", "gzip, deflate, br, zstd")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Cache-Control", "no-cache")
        .header("Cookie", "counted=1")
        .header("Pragma", "no-cache")
        .header("Referer", "https://vimm.net/")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "same-site")
        .header("Sec-Fetch-User", "?1")
        .header("Sec-GPC", "1")
        .header("Upgrade-Insecure-Requests", "1")
        .header("sec-ch-ua", "\"Not(A:Brand\";v=\"8\", \"Chromium\";v=\"144\", \"Brave\";v=\"144\"")
        .header("sec-ch-ua-mobile", "?0")
        .header("sec-ch-ua-platform", "\"Windows\"")
        .send()
        .context("Failed to download file")?;

    let status = response.status();
    if !status.is_success() {
        let response_text = response
            .text()
            .unwrap_or_else(|_| "Failed to read response".to_string())
            .clone();
        println!("Response body: {}", response_text);
        anyhow::bail!("Download failed with status: {}", status);
    }

    // Extract filename from content-disposition header
    let actual_filename = if let Some(content_disp) = response.headers().get("content-disposition")
    {
        if let Ok(disp_str) = content_disp.to_str() {
            // Parse: attachment; filename="Army Men - Air Attack 2 (USA).7z"
            if let Some(filename_part) = disp_str.split("filename=").nth(1) {
                filename_part.trim_matches('"').to_string()
            } else {
                filename.clone()
            }
        } else {
            filename.clone()
        }
    } else {
        filename.clone()
    };

    println!("Actual filename: {}", actual_filename);

    // Update paths with actual filename
    let final_path = Path::new(downloads_dir).join(&actual_filename);
    let pending_path = Path::new(downloads_dir).join(format!("{}.pending", actual_filename));

    let total_size = response.content_length().unwrap_or(0);

    // Check if file already exists and verify CRC from archive metadata
    if final_path.exists() {
        println!("Verifying existing file...");
        match get_crc_from_archive(&final_path) {
            Ok(archive_crc) => {
                if archive_crc == expected_crc {
                    println!(
                        "File already exists and CRC32 verified (expected: {}, got: {}): {}",
                        expected_crc,
                        archive_crc,
                        final_path.display()
                    );
                    return Ok(false);
                } else {
                    println!(
                        "File exists but CRC32 mismatch. Expected: {}, Got: {}",
                        expected_crc, archive_crc
                    );
                    if test_mode {
                        panic!("TEST MODE: CRC mismatch on existing file - not deleting");
                    }
                    println!("Re-downloading...");
                    fs::remove_file(&final_path)?;
                }
            }
            Err(e) => {
                println!("Error reading archive CRC: {}", e);
                if test_mode {
                    panic!("TEST MODE: Failed to read CRC from existing file - not deleting");
                }
                println!("Re-downloading...");
                fs::remove_file(&final_path)?;
            }
        }
    }

    println!("Downloading to: {}", pending_path.display());

    // Write to pending file
    let mut file = File::create(&pending_path)?;

    // Setup progress bar
    let pb = ProgressBar::new(total_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
            .unwrap()
            .progress_chars("#>-"),
    );

    // Download with progress
    let mut downloaded: u64 = 0;
    let mut buffer = [0; 8192];
    loop {
        let n = response.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        file.write_all(&buffer[..n])?;
        downloaded += n as u64;
        pb.set_position(downloaded);
    }

    pb.finish_with_message("Download completed");
    println!();

    // Rename to final filename
    fs::rename(&pending_path, &final_path).context("Failed to rename file from .pending")?;

    // Verify download by reading CRC from archive metadata
    println!("Verifying download...");
    let archive_crc =
        get_crc_from_archive(&final_path).context("Failed to read CRC from archive")?;

    if archive_crc == expected_crc {
        println!(
            "✓ CRC32 verification passed (expected: {}, got: {})",
            expected_crc, archive_crc
        );
    } else {
        println!("✗ CRC32 verification FAILED!");
        println!("  Expected: {}", expected_crc);
        println!("  Got:      {}", archive_crc);
        if test_mode {
            panic!("TEST MODE: CRC mismatch on downloaded file - not deleting");
        }
        fs::remove_file(&final_path)?;
        anyhow::bail!("Downloaded file CRC does not match expected CRC");
    }

    println!("Download completed: {}", final_path.display());
    Ok(true)
}

fn sanitize_filename(title: &str, vault_id: &str) -> String {
    // Remove "Vimm's Lair -" and other prefixes
    let title = title
        .replace("Vimm's Lair - ", "")
        .replace("Vimm's Lair", "")
        .trim()
        .to_string();

    // If title is empty or too generic, use vault ID
    let base_name = if title.is_empty() || title.len() < 3 {
        format!("game_{}", vault_id)
    } else {
        title
    };

    // Remove invalid filename characters
    let cleaned: String = base_name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => c,
        })
        .collect();

    // Truncate if too long
    let result = cleaned.chars().take(200).collect::<String>();

    result
}

fn find_download_link(document: &Html, _page_url: &str) -> Result<(String, String)> {
    // Find the form with the download button
    let form_selector = Selector::parse("form").unwrap();
    let input_selector = Selector::parse("input[name='mediaId']").unwrap();

    for form in document.select(&form_selector) {
        if let Some(action) = form.value().attr("action") {
            if action.contains("dl") || action.contains("vimm.net") {
                // Found the download form, extract mediaId
                if let Some(input) = form.select(&input_selector).next() {
                    if let Some(media_id) = input.value().attr("value") {
                        // Construct the full download URL
                        let base_url = if action.starts_with("//") {
                            format!("https:{}", action)
                        } else if action.starts_with("http") {
                            action.to_string()
                        } else {
                            format!("https://vimm.net{}", action)
                        };

                        // Return both the URL and mediaId
                        return Ok((base_url, media_id.to_string()));
                    }
                }
            }
        }
    }

    anyhow::bail!("Could not find download form with mediaId on page")
}

fn extract_hash(document: &Html, span_id: &str) -> Option<String> {
    let selector = Selector::parse(&format!("span#{}", span_id)).ok()?;
    document
        .select(&selector)
        .next()
        .and_then(|el| el.text().next())
        .map(|s| s.trim().to_lowercase())
}

fn get_crc_from_archive(file_path: &Path) -> Result<String> {
    let extension = file_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "7z" => get_crc_from_7z(file_path),
        "zip" => get_crc_from_zip(file_path),
        _ => anyhow::bail!("Unsupported archive format: {}", extension),
    }
}

fn get_crc_from_7z(file_path: &Path) -> Result<String> {
    let len = fs::metadata(file_path)?.len();
    let file = File::open(file_path)?;
    let reader = SevenZReader::new(file, len, "".into()).context("Failed to open 7z archive")?;

    // Get the first file's CRC from the archive
    for entry in reader.archive().files.iter() {
        if entry.has_stream() && !entry.is_directory() {
            let crc = entry.crc;
            if crc != 0 {
                return Ok(format!("{:08x}", crc));
            }
        }
    }

    anyhow::bail!("No CRC found in 7z archive")
}

fn get_crc_from_zip(file_path: &Path) -> Result<String> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut archive = ZipArchive::new(reader).context("Failed to open zip archive")?;

    // Get the first file's CRC from the archive
    for i in 0..archive.len() {
        let entry = archive.by_index(i)?;
        if !entry.is_dir() {
            let crc = entry.crc32();
            if crc != 0 {
                return Ok(format!("{:08x}", crc));
            }
        }
    }

    anyhow::bail!("No CRC found in zip archive")
}
