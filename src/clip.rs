//! Clipboard helpers built on wl-copy.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub fn is_image(name: &str) -> bool {
    mime_guess::from_path(name)
        .first()
        .map(|m| m.type_() == mime_guess::mime::IMAGE)
        .unwrap_or(false)
}

async fn copy_bytes(mime: &str, data: Vec<u8>) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .arg("--type")
        .arg(mime)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("wl-copy not found")?;
    {
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        stdin.write_all(&data).await?;
        stdin.shutdown().await?;
    }
    let status = child.wait().await?;
    if !status.success() {
        return Err(anyhow!("wl-copy exited with {status}"));
    }
    Ok(())
}

pub async fn copy_text(text: &str) -> Result<()> {
    copy_bytes("text/plain;charset=utf-8", text.as_bytes().to_vec()).await
}

pub fn encode_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Copies files so a file manager or chat app can paste them.
pub async fn copy_files(paths: &[PathBuf]) -> Result<()> {
    let list: String = paths.iter().map(|p| encode_uri(p) + "\r\n").collect();
    copy_bytes("text/uri-list", list.into_bytes()).await
}

/// Decodes any common image, applies EXIF rotation, and re-encodes as PNG,
/// the one image type every app accepts on paste.
fn to_png(path: &Path) -> Result<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{DynamicImage, ImageDecoder, ImageReader};

    let reader = ImageReader::open(path)?.with_guessed_format()?;
    let mut decoder = reader.into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    img.apply_orientation(orientation);
    let mut out = Vec::new();
    let encoder = PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive);
    img.write_with_encoder(encoder)?;
    Ok(out)
}

/// ImageMagick handles what the image crate cannot (HEIC, AVIF, ...).
async fn magick_png(path: &Path) -> Result<Vec<u8>> {
    let out = Command::new("magick")
        .arg(path)
        .arg("-auto-orient")
        .arg("png:-")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .context("magick not found")?;
    if !out.status.success() || out.stdout.is_empty() {
        return Err(anyhow!("magick failed"));
    }
    Ok(out.stdout)
}

pub async fn copy_image(path: &Path) -> Result<()> {
    let is_png = mime_guess::from_path(path)
        .first()
        .map(|m| m.essence_str() == "image/png")
        .unwrap_or(false);
    if is_png {
        let data = tokio::fs::read(path).await?;
        return copy_bytes("image/png", data).await;
    }

    let owned = path.to_path_buf();
    let png = match tokio::task::spawn_blocking(move || to_png(&owned)).await? {
        Ok(png) => png,
        Err(e) => {
            debug!("image crate failed on {path:?}: {e}; trying magick");
            match magick_png(path).await {
                Ok(png) => png,
                Err(_) => return copy_files(&[path.to_path_buf()]).await,
            }
        }
    };
    copy_bytes("image/png", png).await
}
