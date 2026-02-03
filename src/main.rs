use anyhow::{Context, Result};
use base64::Engine;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

fn main() -> Result<()> {
    let test_mode = false;
    if test_mode {
        println!("*** TEST MODE: Files will NOT be deleted on CRC mismatch ***");
    }

    fs::create_dir_all("downloads")?;

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
        .build()?;

    loop {
        let urls: Vec<String> = fs::read_to_string("links.txt")
            .unwrap_or_default()
            .lines()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && !s.starts_with('#') && s.contains("vimm.net/vault/"))
            .map(String::from)
            .collect();

        if urls.is_empty() {
            println!("No URLs in links.txt. Waiting...");
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }

        let mut any_downloaded = false;
        for url in &urls {
            match process_url(&client, url, test_mode) {
                Ok(true) => {
                    any_downloaded = true;
                    println!("Completed: {}\n", url);
                }
                Ok(false) => println!("Already exists: {}\n", url),
                Err(e) => eprintln!("Error: {} - {}\n", url, e),
            }
        }

        if !any_downloaded {
            println!("All done. Waiting for new links...");
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

fn process_url(client: &Client, url: &str, test_mode: bool) -> Result<bool> {
    println!("Processing: {}", url);

    let html = client.get(url).send()?.text()?;
    let doc = Html::parse_document(&html);

    let (dl_url, media_id) = extract_download_info(&doc)?;
    let expected_crc = extract_text(&doc, "span#data-crc").context("No CRC on page")?;
    let iso_name = extract_filename(&doc)?;
    let archive_name = iso_name.rsplit_once('.').map(|(n, _)| format!("{}.7z", n)).unwrap_or_else(|| format!("{}.7z", iso_name));
    
    let iso_path = Path::new("downloads").join(&iso_name);
    let archive_path = Path::new("downloads").join(&archive_name);
    
    println!("Expected: {} (CRC: {})", iso_name, expected_crc);

    let mut downloaded = false;
    loop {
        // 1. Archive exists -> extract
        if archive_path.exists() {
            println!("Extracting {}...", archive_name);
            if let Err(e) = extract(&archive_path) {
                eprintln!("Extraction failed: {} - redownloading", e);
                let _ = fs::remove_file(&archive_path);
                downloaded = false;
                continue;
            }
            let _ = fs::remove_file(&archive_path);
        }

        // 2. ISO exists -> verify CRC
        if iso_path.exists() {
            let crc = get_file_crc(&iso_path)?;
            if crc == expected_crc {
                println!("Verified: {} (CRC: {})", iso_name, crc);
                return Ok(downloaded);
            }
            println!("CRC mismatch: expected {}, got {} - redownloading", expected_crc, crc);
            if test_mode {
                panic!("TEST MODE: CRC mismatch");
            }
            fs::remove_file(&iso_path)?;
        }

        // 3. Download
        println!("Downloading...");
        let response = client
            .get(format!("{}?mediaId={}", dl_url, media_id))
            .header("Referer", "https://vimm.net/")
            .send();

        let mut response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                eprintln!("Download failed: {} - retrying", r.status());
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
            Err(e) => {
                eprintln!("Download error: {} - retrying", e);
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            }
        };

        let pending_name = format!("{}.pending", archive_name);
        let pending = Path::new("downloads").join(&pending_name);
        let total = response.content_length().unwrap_or(0);
        let pb = ProgressBar::new(total);
        pb.set_style(ProgressStyle::default_bar()
            .template("{bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})")?
            .progress_chars("#>-"));

        let mut file = File::create(&pending)?;
        let mut buf = [0u8; 8192];
        let mut download_ok = true;
        loop {
            match response.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if file.write_all(&buf[..n]).is_err() {
                        download_ok = false;
                        break;
                    }
                    pb.inc(n as u64);
                }
                Err(_) => {
                    download_ok = false;
                    break;
                }
            }
        }
        pb.finish();

        if !download_ok {
            eprintln!("Download incomplete - retrying");
            let _ = fs::remove_file(&pending);
            std::thread::sleep(std::time::Duration::from_secs(5));
            continue;
        }

        fs::rename(&pending, &archive_path)?;
        println!("Download complete");
        downloaded = true;

        // Loop back to step 1 (extract and verify)
    }
}

fn extract_download_info(doc: &Html) -> Result<(String, String)> {
    let form_sel = Selector::parse("form").unwrap();
    let input_sel = Selector::parse("input[name='mediaId']").unwrap();

    for form in doc.select(&form_sel) {
        if let Some(action) = form.value().attr("action") {
            if action.contains("dl") {
                if let Some(input) = form.select(&input_sel).next() {
                    if let Some(id) = input.value().attr("value") {
                        let url = if action.starts_with("//") {
                            format!("https:{}", action)
                        } else {
                            action.to_string()
                        };
                        return Ok((url, id.to_string()));
                    }
                }
            }
        }
    }
    anyhow::bail!("No download form found")
}

fn extract_text(doc: &Html, selector: &str) -> Option<String> {
    let sel = Selector::parse(selector).ok()?;
    doc.select(&sel).next()?.text().next().map(|s| s.trim().to_lowercase())
}

fn extract_filename(doc: &Html) -> Result<String> {
    let sel = Selector::parse("canvas#canvas2").unwrap();
    let canvas = doc.select(&sel).next().context("No canvas#canvas2 found")?;
    let data_v = canvas.value().attr("data-v").context("No data-v attribute")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(data_v)?;
    String::from_utf8(bytes).context("Invalid UTF-8 in filename")
}

fn get_file_crc(path: &Path) -> Result<String> {
    let out = Command::new("7z").args(["h", "-scrcCRC32"]).arg(path).output()?;
    if !out.status.success() {
        anyhow::bail!("7z hash failed");
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.contains("CRC32") && l.contains("for data"))
        .and_then(|l| l.split_whitespace().last())
        .map(|s| s.to_lowercase())
        .context("No CRC from 7z hash")
}

fn extract(archive: &Path) -> Result<()> {
    let out = Command::new("7z")
        .args(["x", "-y", "-odownloads"])
        .arg(archive)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("Extraction failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}
