//! `/share/...` — token-gated, read-only file server.
//! 404 when idle · ?t= -> cookie -> clean URL · no traversal/symlink escape ·
//! hidden files off · ranges via ServeFile · folder-as-ZIP (streamed) · JSON listing.

use std::{
    io::{self, Seek, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Serialize;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

use super::{GuestAssets, inbox::RateLimiter};
use crate::state::AppState;

const COOKIE: &str = "lan_t";

pub async fn handler(State((st, _rl)): State<(Arc<AppState>, Arc<RateLimiter>)>, req: Request) -> Response {
    // 1. No active share -> 404 (never reveal anything)
    let (root, token, open_mode, show_hidden) = match st.share.read().await.as_ref() {
        None => return StatusCode::NOT_FOUND.into_response(),
        Some(s) => (s.root.clone(), s.token.clone(), s.open_mode, s.show_hidden),
    };

    let raw_path = req.uri().path().to_string();
    let raw_query = req.uri().query().unwrap_or("").to_string();
    let query = parse_query(&raw_query);

    // 2. Token exchange: ?t=... -> cookie, redirect to clean URL
    if let Some((_, t)) = query.iter().find(|(k, _)| k == "t") {
        if !ct_eq(t, &token) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let rest: Vec<&str> = raw_query.split('&').filter(|p| !p.is_empty() && !p.starts_with("t=")).collect();
        let loc = if rest.is_empty() { raw_path } else { format!("{raw_path}?{}", rest.join("&")) };
        return Response::builder()
            .status(StatusCode::SEE_OTHER)
            .header(header::LOCATION, loc)
            .header(header::SET_COOKIE, format!("{COOKIE}={token}; Path=/share; HttpOnly; SameSite=Lax"))
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .unwrap();
    }

    // 3. Auth: cookie must match (skipped in explicit open mode)
    if !open_mode && !has_valid_cookie(&req, &token) {
        return (StatusCode::UNAUTHORIZED, "Open the share link you were given.").into_response();
    }

    // 4. Resolve path safely
    let Some(rel) = safe_rel(&raw_path, show_hidden) else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(root_c) = tokio::fs::canonicalize(&root).await else { return StatusCode::NOT_FOUND.into_response() };
    let Ok(target) = tokio::fs::canonicalize(root_c.join(&rel)).await else { return StatusCode::NOT_FOUND.into_response() };
    if !target.starts_with(&root_c) {
        return StatusCode::NOT_FOUND.into_response(); // symlink escape
    }
    let Ok(meta) = tokio::fs::metadata(&target).await else { return StatusCode::NOT_FOUND.into_response() };
    let has = |k: &str| query.iter().any(|(q, _)| q == k);

    // 5a. Directory: ?zip | ?json | guest page
    if meta.is_dir() {
        if has("zip") {
            let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "share".into());
            return zip_response(target, name, show_hidden);
        }
        if has("json") {
            return listing(&target, &root_c, &rel, show_hidden).await;
        }
        return match GuestAssets::get("index.html") {
            Some(f) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], f.data.into_owned()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        };
    }

    // 5b. File: ServeFile gives ranges, ETag, Last-Modified, mime
    let mut res = ServeFile::new(&target).oneshot(req).await.unwrap().into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, "sandbox".parse().unwrap()); // shared HTML can't run as the guest origin
    h.insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    if has("dl") {
        let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let enc = utf8_percent_encode(&name, NON_ALPHANUMERIC);
        h.insert(header::CONTENT_DISPOSITION, format!("attachment; filename*=UTF-8''{enc}").parse().unwrap());
    }
    res
}

// ---------- listing ----------

#[derive(Serialize)]
struct Entry { name: String, dir: bool, size: u64, modified: u64 }
#[derive(Serialize)]
struct Listing { path: String, entries: Vec<Entry> }

async fn listing(dir: &Path, root_c: &Path, rel: &Path, show_hidden: bool) -> Response {
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else { return StatusCode::NOT_FOUND.into_response() };
    let mut entries = Vec::new();
    while let Ok(Some(e)) = rd.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') { continue; }
        // resolve symlinks; drop anything pointing outside the share
        let Ok(real) = tokio::fs::canonicalize(e.path()).await else { continue };
        if !real.starts_with(root_c) { continue; }
        let Ok(m) = tokio::fs::metadata(&real).await else { continue };
        let modified = m.modified().ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs()).unwrap_or(0);
        entries.push(Entry { name, dir: m.is_dir(), size: if m.is_dir() { 0 } else { m.len() }, modified });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    let path = rel.to_string_lossy().replace('\\', "/");
    Json(Listing { path, entries }).into_response()
}

// ---------- streamed ZIP (no seek, no temp file; not resumable) ----------

fn zip_response(dir: PathBuf, name: String, show_hidden: bool) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(8);
    tokio::task::spawn_blocking(move || {
        let w = ChanWriter { tx: tx.clone(), buf: Vec::with_capacity(CHUNK), pos: 0 };
        if let Err(e) = write_zip(w, &dir, show_hidden) {
            let _ = tx.blocking_send(Err(e));
        }
    });
    let enc = utf8_percent_encode(&format!("{name}.zip"), NON_ALPHANUMERIC).to_string();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(header::CONTENT_DISPOSITION, format!("attachment; filename*=UTF-8''{enc}"))
        .body(Body::from_stream(ReceiverStream::new(rx)))
        .unwrap()
}

fn write_zip(w: ChanWriter, dir: &Path, show_hidden: bool) -> io::Result<()> {
    let mut zip = ZipWriter::new(w);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored).large_file(true);
    walk(&mut zip, dir, "", show_hidden, opts)?;
    zip.finish().map_err(io::Error::other)?;
    Ok(())
}

fn walk<W: Write + Seek>(zip: &mut ZipWriter<W>, dir: &Path, prefix: &str, show_hidden: bool, opts: SimpleFileOptions) -> io::Result<()> {
    let mut items: Vec<_> = std::fs::read_dir(dir)?.filter_map(Result::ok).collect();
    items.sort_by_key(|e| e.file_name());
    for e in items {
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') { continue; }
        let ft = e.file_type()?; // lstat: does not follow symlinks
        if ft.is_symlink() { continue; } // never follow links into/out of the tree
        let entry_name = format!("{prefix}{name}");
        if ft.is_dir() {
            zip.add_directory(format!("{entry_name}/"), opts).map_err(io::Error::other)?;
            walk(zip, &e.path(), &format!("{entry_name}/"), show_hidden, opts)?;
        } else if ft.is_file() {
            zip.start_file(entry_name, opts).map_err(io::Error::other)?;
            io::copy(&mut std::fs::File::open(e.path())?, zip)?;
        }
    }
    Ok(())
}

const CHUNK: usize = 64 * 1024;

/// Bridges the blocking zip writer to the async response body.
/// A failed send (client disconnected) aborts the zip.
/// `pos` tracks the virtual stream position so that zip v2's internal Seek calls
/// (used for the central directory) return plausible offsets without actual seeking.
struct ChanWriter { tx: mpsc::Sender<Result<Bytes, io::Error>>, buf: Vec<u8>, pos: u64 }

impl ChanWriter {
    fn send_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() { return Ok(()); }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx.blocking_send(Ok(chunk)).map_err(|_| io::ErrorKind::BrokenPipe.into())
    }
}
impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        self.pos += data.len() as u64;
        if self.buf.len() >= CHUNK { self.send_buf()?; }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> { self.send_buf() }
}
impl io::Seek for ChanWriter {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        // Zip v2 with Stored compression only seeks backwards to patch the local header
        // after writing data. We can't actually seek in a streaming channel, but we can
        // return the correct position so the library can calculate offsets.
        let new_pos: i64 = match pos {
            io::SeekFrom::Start(n) => n as i64,
            io::SeekFrom::Current(n) => self.pos as i64 + n,
            io::SeekFrom::End(n) => self.pos as i64 + n, // can't go before end in a stream
        };
        self.pos = new_pos.max(0) as u64;
        Ok(self.pos)
    }
}
impl Drop for ChanWriter {
    fn drop(&mut self) { let _ = self.send_buf(); }
}

// ---------- helpers ----------

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|p| !p.is_empty()).map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (k.to_string(), percent_decode_str(v).decode_utf8_lossy().into_owned())
    }).collect()
}

fn has_valid_cookie(req: &Request, token: &str) -> bool {
    req.headers().get_all(header::COOKIE).iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_owned))
        .any(|v| ct_eq(&v, token))
}

/// Constant-time string compare.
fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// "/share/a/b%20c" -> "a/b c". None if it tries to escape or hits a hidden segment.
fn safe_rel(uri_path: &str, show_hidden: bool) -> Option<PathBuf> {
    let rest = uri_path.strip_prefix("/share")?;
    let mut out = PathBuf::new();
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        let seg = percent_decode_str(seg).decode_utf8().ok()?;
        if seg == "." || seg == ".." || seg.contains(['/', '\\', '\0']) { return None; }
        if cfg!(windows) && seg.contains(':') { return None; } // drive letters / ADS
        if !show_hidden && seg.starts_with('.') { return None; }
        out.push(seg.as_ref());
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_hidden() {
        assert!(safe_rel("/share/../etc/passwd", false).is_none());
        assert!(safe_rel("/share/a/%2e%2e/b", false).is_none());
        assert!(safe_rel("/share/a%2F..%2Fb", false).is_none());
        assert!(safe_rel("/share/a\\b", false).is_none());
        assert!(safe_rel("/share/.git/config", false).is_none());
        assert!(safe_rel("/share/.git/config", true).is_some());
    }

    #[test]
    fn decodes_normal_paths() {
        assert_eq!(safe_rel("/share/a/b%20c.txt", false).unwrap(), PathBuf::from("a/b c.txt"));
        assert_eq!(safe_rel("/share", false).unwrap(), PathBuf::new());
    }

    #[test]
    fn token_compare() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "ab"));
    }
}
