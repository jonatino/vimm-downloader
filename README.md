# vimm-downloader

A Rust CLI tool to batch download games from [vimm.net](https://vimm.net/vault/).

## Features

- Batch download from `links.txt`
- Real-time progress bar
- CRC32 verification (reads from archive metadata)
- Supports `.7z` and `.zip` archives
- Auto-resumes on restart (skips verified files)
- `.pending` extension during download

## Usage

1. Create a `links.txt` file with vault URLs (one per line):
   ```
   https://vimm.net/vault/7894
   https://vimm.net/vault/9600
   ```

2. Run:
   ```bash
   cargo run --release
   ```

3. Files are saved to `./downloads/`

## Building

```bash
cargo build --release
```