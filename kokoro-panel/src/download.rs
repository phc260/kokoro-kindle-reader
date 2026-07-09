// Model download + integrity verify on a blocking reqwest client (run on a
// background thread). Files stream from HuggingFace per the embedded
// model-manifest.json into <app_data>/<model_id>/<path>; each is SHA-256-verified
// before being committed (renamed into place), so the download is resumable and a
// corrupt file is never left behind. The host reads these files; no webview.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use crate::MANIFEST_JSON;

struct ManifestFile {
    path: String,
    size: u64,
    sha256: String,
}

struct Manifest {
    base_url: String,
    model_id: String,
    files: Vec<ManifestFile>,
}

fn load_manifest() -> Manifest {
    let v: serde_json::Value = serde_json::from_str(MANIFEST_JSON).expect("embedded manifest");
    let files = v["files"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|f| ManifestFile {
                    path: f["path"].as_str().unwrap_or("").to_string(),
                    size: f["size"].as_u64().unwrap_or(0),
                    sha256: f["sha256"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Manifest {
        base_url: v["base_url"].as_str().unwrap_or("").to_string(),
        model_id: v["model_id"].as_str().unwrap_or("").to_string(),
        files,
    }
}

/// Resolve a manifest file's on-disk path under `<app_data>/<model_id>/`. Returns
/// `None` if `rel` isn't a plain relative path — every component must be `Normal`, so
/// an absolute path, a drive prefix, or a `..` is rejected and can never escape the
/// model dir. The manifest is embedded (trusted) today; this keeps a future
/// externally-sourced manifest from becoming a path-traversal write primitive.
fn file_path(app_data: &Path, model_id: &str, rel: &str) -> Option<PathBuf> {
    use std::path::Component;
    if rel.is_empty() || !Path::new(rel).components().all(|c| matches!(c, Component::Normal(_))) {
        return None;
    }
    Some(app_data.join(model_id).join(rel))
}

fn present(path: &Path, size: u64) -> bool {
    std::fs::metadata(path).map(|m| m.len() == size).unwrap_or(false)
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Hash `path` and compare to `sha256`, reporting cumulative bytes read via
/// `on_progress(done, total)` where `base` is the bytes already accounted for by
/// earlier files. Progress is reported even on a short read so the bar keeps moving.
fn valid_with_progress(
    path: &Path,
    size: u64,
    sha256: &str,
    base: u64,
    total: u64,
    on_progress: &mut impl FnMut(u64, u64),
) -> bool {
    if !present(path, size) {
        return false;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut read: u64 = 0;
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                hasher.update(&buf[..n]);
                read += n as u64;
                on_progress(base + read, total);
            }
            Err(_) => return false,
        }
    }
    hex(hasher.finalize()) == sha256
}

/// Whether every manifest file is present with the expected size (the model is
/// usable). Cheap — metadata only.
pub fn model_complete(app_data: &Path) -> bool {
    let m = load_manifest();
    // An unrejectable/unsafe path -> treat the model as incomplete (fail closed).
    m.files.iter().all(|f| {
        file_path(app_data, &m.model_id, &f.path)
            .map(|p| present(&p, f.size))
            .unwrap_or(false)
    })
}

/// Shared progress the UI polls while a download/verify runs.
#[derive(Default, Clone)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    pub file: String,
    pub done: bool,
    pub error: Option<String>,
}

/// Kick off a download on a background thread. `running` guards against re-entry;
/// `progress` is updated under its lock. Idempotent + resumable: files already
/// present with the right size are skipped, and each fetched file is SHA-256-checked
/// before commit.
pub fn start(
    app_data: PathBuf,
    running: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    repaint: impl Fn() + Send + 'static,
) {
    if running.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let result = download(&app_data, &progress, &repaint);
        {
            let mut p = progress.lock().unwrap();
            p.done = true;
            p.error = result.err();
        }
        running.store(false, Ordering::SeqCst);
        repaint();
    });
}

fn download(
    app_data: &Path,
    progress: &Arc<Mutex<Progress>>,
    repaint: &(impl Fn() + Send + 'static),
) -> Result<(), String> {
    let manifest = load_manifest();
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    {
        let mut p = progress.lock().unwrap();
        *p = Progress { total, ..Default::default() };
    }

    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;

    for f in &manifest.files {
        let dest = file_path(app_data, &manifest.model_id, &f.path)
            .ok_or_else(|| format!("unsafe path in manifest: {}", f.path))?;
        {
            let mut p = progress.lock().unwrap();
            p.file = f.path.clone();
        }

        // Resume: a file already present with the right size is kept (skip the
        // hash for speed; a corrupt one can be caught by "Verify & repair").
        if present(&dest, f.size) {
            downloaded += f.size;
            let mut p = progress.lock().unwrap();
            p.downloaded = downloaded;
            drop(p);
            repaint();
            continue;
        }

        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut hasher = Sha256::new();

        let url = format!("{}/{}", manifest.base_url, f.path);
        let mut resp = client
            .get(&url)
            .send()
            .map_err(|e| format!("GET {url} failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("GET {url} failed: {e}"))?;

        let mut buf = [0u8; 128 * 1024];
        loop {
            let n = resp.read(&mut buf).map_err(|e| format!("stream {url}: {e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            downloaded += n as u64;
            let mut p = progress.lock().unwrap();
            p.downloaded = downloaded;
            drop(p);
            repaint();
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);

        // Verify before committing; discard a corrupt/truncated download.
        let got = hex(hasher.finalize());
        if got != f.sha256 {
            let _ = std::fs::remove_file(&part);
            return Err(format!("checksum mismatch for {} (retry the download)", f.path));
        }
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Verify every model file against its manifest SHA-256, deleting any that are
/// missing/corrupt so a follow-up download re-fetches just those. Blocking; run on a
/// thread. `on_progress(done_bytes, total_bytes)` is called continuously as files are
/// hashed (weighted by size, so the bar tracks real work). Returns (checked,
/// repaired-count).
pub fn verify(app_data: &Path, mut on_progress: impl FnMut(u64, u64)) -> (usize, usize) {
    let m = load_manifest();
    let total: u64 = m.files.iter().map(|f| f.size).sum();
    let mut base: u64 = 0;
    let mut repaired = 0;
    for f in &m.files {
        let Some(path) = file_path(app_data, &m.model_id, &f.path) else {
            // Unsafe manifest path: count it as needing repair, don't touch disk.
            repaired += 1;
            base += f.size;
            on_progress(base, total);
            continue;
        };
        if !valid_with_progress(&path, f.size, &f.sha256, base, total, &mut on_progress) {
            let _ = std::fs::remove_file(&path);
            repaired += 1;
        }
        // Snap to this file's end even if it was missing/short (kept the bar honest).
        base += f.size;
        on_progress(base, total);
    }
    (m.files.len(), repaired)
}
