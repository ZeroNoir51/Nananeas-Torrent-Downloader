use eframe::egui;
use serde::{Deserialize, Serialize};
use base64::{engine::general_purpose::STANDARD, Engine};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::{mpsc::{self, Receiver}, Arc, atomic::{AtomicBool, Ordering}};
use librqbit::{AddTorrent, AddTorrentOptions, AddTorrentResponse, Session};


#[derive(Serialize, Deserialize)]
struct SourceArchive { format: String, version: u32, sources: Vec<ArchivedSource> }
#[derive(Serialize, Deserialize)]
struct ArchivedSource { kind: String, value: String, torrent_base64: Option<String> }

fn export_sources(db: &Connection, path: &std::path::Path) -> Result<usize, String> {
    let mut q = db.prepare("SELECT kind,value,torrent FROM sources ORDER BY id").map_err(|e| e.to_string())?;
    let rows = q.query_map([], |r| {
        let bytes: Option<Vec<u8>> = r.get(2)?;
        Ok(ArchivedSource { kind: r.get(0)?, value: r.get(1)?, torrent_base64: bytes.map(|b| STANDARD.encode(b)) })
    }).map_err(|e| e.to_string())?;
    let sources = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    let count = sources.len();
    let archive = SourceArchive { format: "torrent-finder-sources".into(), version: 1, sources };
    let bytes = serde_json::to_vec_pretty(&archive).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(count)
}
fn import_sources(db: &mut Connection, path: &std::path::Path) -> Result<usize, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let archive: SourceArchive = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if archive.format != "torrent-finder-sources" || archive.version != 1 { return Err("Unsupported source archive / Archivo de fuentes incompatible".into()); }
    let mut validated = Vec::new();
    for item in archive.sources {
        let blob = match (item.kind.as_str(), item.torrent_base64) {
            ("magnet", None) if item.value.starts_with("magnet:?") => None,
            ("torrent", Some(encoded)) => {
                let bytes = STANDARD.decode(encoded).map_err(|e| e.to_string())?;
                if bytes.is_empty() { return Err("Empty torrent / Torrent vacío".into()); }
                Some(bytes)
            }
            _ => return Err("Invalid source / Fuente inválida".into()),
        };
        if item.value.trim().is_empty() { return Err("Empty source / Fuente vacía".into()); }
        validated.push((item.kind, item.value, blob));
    }
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let mut added = 0;
    for (kind, value, blob) in validated {
        added += tx.execute("INSERT OR IGNORE INTO sources(kind,value,torrent) VALUES(?1,?2,?3)", params![kind,value,blob]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(added)
}

fn database() -> Result<Connection, String> {
    let dir = dirs::data_local_dir().ok_or("Cannot determine data directory")?.join("torrent-finder");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let db = Connection::open(dir.join("catalog.sqlite3")).map_err(|e| e.to_string())?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS sources (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, value TEXT NOT NULL, torrent BLOB, UNIQUE(kind, value));").map_err(|e| e.to_string())?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS indexed_sources (source_id INTEGER PRIMARY KEY, info_hash TEXT NOT NULL, name TEXT, torrent_bytes BLOB NOT NULL); CREATE TABLE IF NOT EXISTS indexed_files (source_id INTEGER NOT NULL, file_index INTEGER NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL, PRIMARY KEY(source_id, file_index));").map_err(|e| e.to_string())?;
    Ok(db)
}
fn setting(db: &Connection, key: &str, default: &str) -> String {
    db.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).unwrap_or_else(|_| default.to_owned())
}
fn save_setting(db: &Connection, key: &str, value: &str) -> Result<(), String> {
    db.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value]).map_err(|e| e.to_string())?;
    Ok(())
}
fn source_names(db: &Connection) -> Result<Vec<(i64, String)>, String> {
    let mut q = db.prepare("SELECT id, kind, value FROM sources ORDER BY id DESC").map_err(|e| e.to_string())?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, format!("{}: {}", r.get::<_, String>(1)?, r.get::<_, String>(2)?)))).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}
fn index_states(db: &Connection) -> Result<std::collections::BTreeMap<i64, String>, String> {
    let mut q = db.prepare("SELECT s.id, EXISTS(SELECT 1 FROM indexed_sources i WHERE i.source_id=s.id) FROM sources s ORDER BY s.id").map_err(|e| e.to_string())?;
    let rows = q.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, bool>(1)?))).map_err(|e| e.to_string())?;
    let mut states = std::collections::BTreeMap::new();
    for row in rows {
        let (id, indexed) = row.map_err(|e| e.to_string())?;
        states.insert(id, if indexed { "Indexed / Indexada: 100%" } else { "Pending / Pendiente" }.to_string());
    }
    Ok(states)
}

enum IndexEvent { Progress(i64, String), Done(String) }
fn start_index(only_source: Option<i64>) -> Receiver<IndexEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> Result<String, String> {
            let mut db = database()?;
            let sources: Vec<(i64, String, String, Option<Vec<u8>>)> = {
                let mut q = db.prepare("SELECT s.id, s.kind, s.value, s.torrent FROM sources s WHERE (?1 IS NULL OR s.id=?1) AND NOT EXISTS (SELECT 1 FROM indexed_sources i WHERE i.source_id=s.id) ORDER BY s.id").map_err(|e| e.to_string())?;
                let mapped = q.query_map([only_source], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map_err(|e| e.to_string())?;
                mapped.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?
            };
            if sources.is_empty() { return Ok("No pending sources / No hay fuentes pendientes".into()); }
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            let engine_dir = dirs::data_local_dir().ok_or("No data directory")?.join("torrent-finder").join("engine");
            std::fs::create_dir_all(&engine_dir).map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let session = Session::new(engine_dir).await.map_err(|e| e.to_string())?;
                let total = sources.len();
                let (mut done, mut failed) = (0, 0);
                for (id, kind, value, blob) in sources {
                    let _ = tx.send(IndexEvent::Progress(id, format!("Resolving metadata / Buscando metadatos ({} / {}; % unavailable / no disponible)", done + failed + 1, total)));
                    let add = if kind == "magnet" { AddTorrent::from_url(value) } else if let Some(bytes) = blob { AddTorrent::from_bytes(bytes) } else { failed += 1; let _ = tx.send(IndexEvent::Progress(id, "Failed / Falló: missing torrent data / faltan datos del torrent".into())); continue; };
                    let opts = AddTorrentOptions { list_only: true, ..Default::default() };
                    let resolved = tokio::time::timeout(std::time::Duration::from_secs(120), session.add_torrent(add, Some(opts))).await;
                    match resolved {
                        Ok(Ok(AddTorrentResponse::ListOnly(info))) => {
                            let files: Vec<_> = info.info.iter_file_details().enumerate().map(|(i, f)| (i as i64, f.filename.to_string(), f.len as i64)).collect();
                            let hash = info.info_hash.as_string();
                            let name = info.info.name().map(|n| n.into_owned());
                            let saved = (|| -> Result<(), String> {
                            let count = files.len();
                            let transaction = db.transaction().map_err(|e| e.to_string())?;
                            transaction.execute("DELETE FROM indexed_files WHERE source_id=?1", [id]).map_err(|e| e.to_string())?;
                            transaction.execute("INSERT OR REPLACE INTO indexed_sources(source_id,info_hash,name,torrent_bytes) VALUES(?1,?2,?3,?4)", params![id, hash, name, info.torrent_bytes.as_ref()]).map_err(|e| e.to_string())?;
                            for (position, (index, path, size)) in files.into_iter().enumerate() {
                                transaction.execute("INSERT INTO indexed_files(source_id,file_index,path,size) VALUES(?1,?2,?3,?4)", params![id,index,path,size]).map_err(|e| e.to_string())?;
                                if (position + 1) % 100 == 0 || position + 1 == count {
                                    let percent = (position + 1) * 100 / count;
                                    let _ = tx.send(IndexEvent::Progress(id, format!("Saving index / Guardando índice: {percent}%")));
                                }
                            }
                            transaction.commit().map_err(|e| e.to_string())?;
                            Ok(())
                            })();
                            match saved {
                                Ok(()) => { done += 1; let _ = tx.send(IndexEvent::Progress(id, "Indexed / Indexada: 100%".into())); }
                                Err(e) => { failed += 1; let _ = tx.send(IndexEvent::Progress(id, format!("Failed to save index / Falló al guardar el índice: {e}"))); }
                            }
                        }
                        Ok(Err(e)) => { failed += 1; let _ = tx.send(IndexEvent::Progress(id, format!("Failed / Falló: {e:#}"))); }
                        Err(_) => { failed += 1; let _ = tx.send(IndexEvent::Progress(id, "Failed / Falló: metadata timeout after 120 seconds / tiempo de espera agotado tras 120 segundos".into())); }
                        Ok(Ok(_)) => { failed += 1; let _ = tx.send(IndexEvent::Progress(id, "Failed / Falló: unexpected engine response / respuesta inesperada del motor".into())); }
                    }
                }
                session.stop().await;
                Ok(format!("Indexed / Indexadas: {done}; failed or timed out / fallidas o sin respuesta: {failed}"))
            })
        })();
        let _ = tx.send(IndexEvent::Done(result.unwrap_or_else(|e| format!("Error: {e}"))));
    });
    rx
}
#[derive(Clone)]
struct SearchResult { source_id: i64, index: usize, path: String, size: i64, torrent: String }
fn matches_terms(path: &str, query: &str) -> bool {
    let path = path.to_lowercase();
    query.split_whitespace().all(|term| path.contains(&term.to_lowercase()))
}
fn search_files(db: &Connection, query: &str) -> Result<Vec<SearchResult>, String> {
    if query.trim().is_empty() { return Ok(Vec::new()); }
    let mut stmt = db.prepare("SELECT f.source_id, f.file_index, f.path, f.size, COALESCE(s.name, s.info_hash) FROM indexed_files f JOIN indexed_sources s ON s.source_id=f.source_id ORDER BY f.path").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| Ok(SearchResult { source_id: r.get(0)?, index: r.get::<_,i64>(1)? as usize, path: r.get(2)?, size: r.get(3)?, torrent: r.get(4)? })).map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    for row in rows {
        let item = row.map_err(|e| e.to_string())?;
        if matches_terms(&item.path, query) {
            found.push(item);
            if found.len() == 200 { break; }
        }
    }
    Ok(found)
}
#[cfg(test)]
mod search_tests {
    use super::matches_terms;
    #[test]
    fn words_match_independent_of_order_and_case() {
        assert!(matches_terms("TOMATES deliciosos ROTOS.zip", "tomates rotos"));
        assert!(matches_terms("TOMATES deliciosos ROTOS.zip", "rotos tomates"));
        assert!(!matches_terms("tomates sanos.zip", "tomates rotos"));
        assert!(matches_terms("Árbol/Ñandú.txt", "ñandú árbol"));
    }
}
async fn wait_shutdown(flag: &AtomicBool) {
    while !flag.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(200)).await; }
}

// Only inspect regular files in this application's download-* directories.
// Do not traverse symlinks or remove non-empty files.
fn clean_empty_cache() -> Result<usize, String> {
    #[cfg(target_os = "linux")]
    {
        for entry in std::fs::read_dir("/proc").map_err(|e| e.to_string())?.flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|x| x.parse::<u32>().ok()) else { continue; };
            if pid == std::process::id() { continue; }
            if let Ok(exe) = std::fs::read_link(entry.path().join("exe")) {
                if exe.file_name().and_then(|v| v.to_str()) == Some("torrent-finder") {
                    return Err("Another Torrent Finder instance is open; cache was not cleaned / Hay otra instancia abierta; no se limpió la caché".into());
                }
            }
        }
    }
    fn clean_dir(path: &std::path::Path, count: &mut usize) -> Result<(), String> {
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_dir() { clean_dir(&entry.path(), count)?; }
            else if kind.is_file() && entry.metadata().map_err(|e| e.to_string())?.len() == 0 {
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
                *count += 1;
            }
        }
        Ok(())
    }
    let root = dirs::cache_dir().ok_or("Cannot determine cache directory")?.join("torrent-finder");
    if !root.exists() { return Ok(0); }
    let mut count = 0;
    for entry in std::fs::read_dir(root).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() && entry.file_name().to_string_lossy().starts_with("download-") {
            clean_dir(&entry.path(), &mut count)?;
        }
    }
    Ok(count)
}

enum TransferEvent { Status(String), Progress { text: String, rate: f64, fraction: f32 } }
struct Transfer { name: String, rx: Receiver<TransferEvent>, status: String, fraction: f32, rate: f64, active: bool }
fn start_download(item: SearchResult, output: String, seed_enabled: Arc<AtomicBool>, shutting_down: Arc<AtomicBool>, move_root: bool) -> Receiver<TransferEvent> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let report = tx.clone();
        let result = (|| -> Result<(), String> {
            let db = database()?;
            let (kind, value, torrent_bytes, hash): (String, String, Vec<u8>, String) = db.query_row(
                "SELECT s.kind,s.value,i.torrent_bytes,i.info_hash FROM sources s JOIN indexed_sources i ON i.source_id=s.id WHERE s.id=?1", [item.source_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(|e| e.to_string())?;
            let root = PathBuf::from(&output);
            if !root.is_dir() { return Err("Download folder does not exist / La carpeta de descarga no existe".into()); }
            let relative = std::path::Path::new(&item.path);
            if relative.components().any(|c| !matches!(c, std::path::Component::Normal(_))) { return Err("Unsafe file path / Ruta de archivo insegura".into()); }
            let final_path = if move_root { root.join(relative.file_name().ok_or("Invalid filename")?) } else { root.join(&hash).join(relative) };
            if final_path.exists() { return Err("The selected output file already exists; it will not be overwritten / El archivo de salida ya existe; no se sobrescribirá".into()); }
            let cache = dirs::cache_dir().ok_or("Cannot determine cache directory")?.join("torrent-finder");
            std::fs::create_dir_all(&cache).map_err(|e| e.to_string())?;
            let staging = tempfile::Builder::new().prefix("download-").tempdir_in(cache).map_err(|e| e.to_string())?;
            let folder = staging.path().to_path_buf();
            let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
            runtime.block_on(async {
                let session = Session::new(folder.clone()).await.map_err(|e| e.to_string())?;
                let add = if kind == "magnet" { AddTorrent::from_url(value) } else { AddTorrent::from_bytes(torrent_bytes) };
                let options = AddTorrentOptions { only_files: Some(vec![item.index]), output_folder: Some(folder.to_string_lossy().into_owned()), overwrite: false, ..Default::default() };
                let handle = tokio::select! {
                    result = session.add_torrent(add, Some(options)) => result.map_err(|e| format!("{e:#}"))?.into_handle().ok_or("Torrent could not start")?,
                    _ = wait_shutdown(&shutting_down) => { session.stop().await; return Ok(()); }
                };
                let name = item.path.clone();
                let wait = handle.wait_until_completed();
                tokio::pin!(wait);
                let mut last_time = std::time::Instant::now();
                let mut previous_downloaded = 0u64;
                let mut previous_uploaded = 0u64;
                loop {
                    tokio::select! {
                        result = &mut wait => { result.map_err(|e| e.to_string())?; break; }
                        _ = wait_shutdown(&shutting_down) => { session.stop().await; return Ok(()); }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                            let stats = handle.stats();
                            let pct = if stats.total_bytes > 0 { 100.0 * stats.progress_bytes as f64 / stats.total_bytes as f64 } else { 0.0 };
                            let elapsed = last_time.elapsed().as_secs_f64().max(0.001);
                            let fetched = stats.live.as_ref().map(|live| live.snapshot.fetched_bytes).unwrap_or(previous_downloaded);
                            let down = fetched.saturating_sub(previous_downloaded) as f64 / elapsed / 1024.0;
                            let up = stats.uploaded_bytes.saturating_sub(previous_uploaded) as f64 / elapsed / 1024.0;
                            previous_downloaded = fetched;
                            previous_uploaded = stats.uploaded_bytes;
                            last_time = std::time::Instant::now();
                            let peers = stats.live.as_ref().map(|live| live.snapshot.peer_stats.live.to_string()).unwrap_or_else(|| "?".to_owned());
                            let _ = report.send(TransferEvent::Progress { text: format!("{:.1}%  v {:.1} KiB/s  ^ {:.1} KiB/s  Peers: {}  Seeders/Leechers: ?", pct.min(100.0), down, up, peers), rate: down, fraction: (pct / 100.0).clamp(0.0, 1.0) as f32 });
                        }
                    }
                }
                if shutting_down.load(Ordering::Relaxed) { session.stop().await; return Ok(()); }
                let paused = move_root || !seed_enabled.load(Ordering::Relaxed);
                if paused { session.pause(&handle).await.map_err(|e| e.to_string())?; }
                let source = handle.output_folder().join(relative);
                let canonical_folder = std::fs::canonicalize(handle.output_folder()).map_err(|e| e.to_string())?;
                let canonical_source = std::fs::canonicalize(&source).map_err(|e| e.to_string())?;
                if !canonical_source.starts_with(&canonical_folder) { return Err("Unsafe file path / Ruta de archivo insegura".into()); }
                let parent = final_path.parent().ok_or("Invalid destination")?;
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                let mut src = std::fs::File::open(&canonical_source).map_err(|e| e.to_string())?;
                let mut dst = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
                std::io::copy(&mut src, &mut dst).map_err(|e| e.to_string())?;
                dst.persist_noclobber(&final_path).map_err(|e| format!("Cannot create output file / No se pudo crear el archivo: {}", e.error))?;
                if !paused && seed_enabled.load(Ordering::Relaxed) {
                    let _ = report.send(TransferEvent::Status(format!("{}: complete; seeding / completado; compartiendo", name)));
                    while seed_enabled.load(Ordering::Relaxed) && !shutting_down.load(Ordering::Relaxed) {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    }
                    session.pause(&handle).await.map_err(|e| e.to_string())?;
                }
                session.stop().await;
                let _ = report.send(TransferEvent::Status(format!("{}: complete; stopped / completado; detenido", name)));
                Ok(())
            })
        })();
        if let Err(e) = result { let _ = tx.send(TransferEvent::Status(format!("Error: {e}"))); }
    });
    rx
}

#[derive(Deserialize)]
struct Translations {
    search: String, seed: String, settings: String,
    sources: String, magnet: String, add: String, torrent: String,
    location: String, language: String,
    export: String, import: String, exported: String, imported: String,
}

fn translations_dir() -> Result<PathBuf, String> {
    let base = dirs::config_dir().ok_or("Cannot determine configuration directory")?;
    #[cfg(target_os = "windows")]
    let dir = base.join("torrent-finder").join("userdata").join("translations");
    #[cfg(not(target_os = "windows"))]
    let dir = base.join("torrent-finder").join("translations");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, contents) in [("en.xml", include_str!("../translations/en.xml")), ("es.xml", include_str!("../translations/es.xml"))] {
        use std::io::Write;
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(name)) {
            Ok(mut file) => file.write_all(contents.as_bytes()).map_err(|e| e.to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(dir)
}
fn load_language(language: &str) -> Result<Translations, String> {
    let path = translations_dir()?.join(format!("{language}.xml"));
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    quick_xml::de::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

struct App {
    text: Translations,
    mascot: egui::TextureHandle,
    language: usize,
    settings: bool,
    seed: bool,
    seed_enabled: Arc<AtomicBool>,
    search: String,
    magnet: String,
    torrent: String,
    location: String,
    error: Option<String>,
    db: Connection,
    source_names: Vec<(i64, String)>,
    confirm_delete_source: Option<(i64, String)>,
    message: Option<String>,
    index_rx: Option<Receiver<IndexEvent>>,
    index_status: String,
    index_details: std::collections::BTreeMap<i64, String>,
    settings_was_open: bool,
    index_pending: bool,
    retry_source: Option<i64>,
    skip_settings_index: bool,
    copy_root: bool,
    pending: Option<SearchResult>,
    downloads: Vec<Transfer>,
    speed_history: std::collections::VecDeque<f64>,
    last_graph_sample: std::time::Instant,
    confirm_delete: bool,
    shutdown: Arc<AtomicBool>,
    confirm_close: bool,
    closing: bool,
    allow_close: bool,
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.index_rx {
            let mut finished = false;
            while let Ok(event) = rx.try_recv() {
                match event { IndexEvent::Progress(id, s) => { self.index_details.insert(id, s); }, IndexEvent::Done(s) => { self.index_status = s; finished = true; } }
            }
            if finished { self.index_rx = None; }
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
        for download in &mut self.downloads {
            loop { match download.rx.try_recv() {
                Ok(TransferEvent::Status(text)) => { if text.contains("complete;") { download.fraction = 1.0; } download.status = text; download.rate = 0.0; },
                Ok(TransferEvent::Progress { text, rate, fraction }) => { download.status = text; download.rate = rate; download.fraction = fraction; },
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => { download.active = false; download.rate = 0.0; break; },
            }}
        }
        if self.last_graph_sample.elapsed() >= std::time::Duration::from_secs(1) {
            self.speed_history.push_back(self.downloads.iter().map(|d| d.rate).sum());
            if self.speed_history.len() > 60 { self.speed_history.pop_front(); }
            self.last_graph_sample = std::time::Instant::now();
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if !self.closing { self.confirm_close = true; }
        }
        if self.closing {
            if !self.downloads.iter().any(|d| d.active) {
                match clean_empty_cache() {
                    Ok(_) => { self.allow_close = true; ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                    Err(e) => { self.error = Some(e); self.closing = false; self.shutdown.store(false, Ordering::Relaxed); }
                }
            } else {
                egui::CentralPanel::default().show(ctx, |ui| { ui.label(if self.language == 0 { "Stopping transfers and cleaning cache..." } else { "Deteniendo transferencias y limpiando la caché..." }); });
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
                return;
            }
        }
        if self.downloads.iter().any(|d| d.active) { ctx.request_repaint_after(std::time::Duration::from_secs(1)); }
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.image((self.mascot.id(), egui::vec2(64.0, 64.0)));
                ui.vertical(|ui| { ui.heading("Nananeas Torrent Downloader"); ui.label("v0.1"); });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(if self.language == 0 { "Close" } else { "Cerrar" }).clicked() { self.confirm_close = true; }
                    if ui.button(&self.text.settings).clicked() { self.settings = !self.settings; }
                });
            });
        });
        if !self.downloads.is_empty() {
            egui::TopBottomPanel::bottom("transfers").resizable(true).default_height(190.0).show(ctx, |ui| {
                ui.label(if self.language == 0 { "Downloads · total speed (KiB/s)" } else { "Descargas · velocidad total (KiB/s)" });
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 55.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(25));
                let max = self.speed_history.iter().copied().fold(1.0_f64, f64::max);
                let points: Vec<_> = self.speed_history.iter().enumerate().map(|(i, v)| egui::pos2(rect.left() + rect.width() * i as f32 / 59.0, rect.bottom() - rect.height() * (*v / max) as f32)).collect();
                if points.len() > 1 { ui.painter().add(egui::Shape::line(points, egui::Stroke::new(2.0_f32, egui::Color32::LIGHT_BLUE))); }
                ui.label(format!("v {:.1} KiB/s", self.downloads.iter().map(|d| d.rate).sum::<f64>()));
                egui::ScrollArea::vertical().show(ui, |ui| for d in &self.downloads {
                    ui.label(format!("{} — {}", d.name, d.status));
                    ui.add(egui::ProgressBar::new(d.fraction).show_percentage());
                });
            });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            let area = ui.max_rect();
            let width = (area.width() - 48.0).clamp(80.0, 680.0);
            let center = egui::pos2(area.center().x, area.center().y - 12.0);
            let search_rect = egui::Rect::from_center_size(center, egui::vec2(width, 60.0));
            ui.put(search_rect, egui::TextEdit::singleline(&mut self.search)
                .hint_text(&self.text.search)
                .font(egui::FontId::proportional(25.0))
                .horizontal_align(egui::Align::Center)
                .vertical_align(egui::Align::Center));
            let checkbox_rect = egui::Rect::from_center_size(center + egui::vec2(0.0, 55.0), egui::vec2(width, 24.0));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(checkbox_rect), |ui| {
                ui.vertical_centered(|ui| {
                    if ui.checkbox(&mut self.seed, &self.text.seed).changed() {
                        if self.seed && self.copy_root { self.copy_root = false; let _ = save_setting(&self.db, "copy_root", "0"); }
                        self.seed_enabled.store(self.seed, Ordering::Relaxed);
                        if let Err(e) = save_setting(&self.db, "seed", if self.seed { "1" } else { "0" }) { self.error = Some(e); }
                    }
                });
            });
            let root_rect = egui::Rect::from_center_size(center + egui::vec2(0.0, 82.0), egui::vec2(width, 24.0));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(root_rect), |ui| { ui.vertical_centered(|ui| {
                if ui.checkbox(&mut self.copy_root, if self.language == 0 { "Move completed file to download folder" } else { "Mover archivo terminado a la carpeta de descarga" }).changed() {
                    if self.copy_root { self.seed = false; self.seed_enabled.store(false, Ordering::Relaxed); let _ = save_setting(&self.db, "seed", "0"); }
                    if let Err(e) = save_setting(&self.db, "copy_root", if self.copy_root { "1" } else { "0" }) { self.error = Some(e); }
                }
            }); });
            let notice_rect = egui::Rect::from_min_size(area.min + egui::vec2(10.0, 10.0), egui::vec2((area.width() - 20.0).max(0.0), 36.0));
            ui.put(notice_rect, egui::Label::new(egui::RichText::new(self.index_status.clone()).color(egui::Color32::LIGHT_BLUE)).wrap());
            if !self.search.trim().is_empty() {
                let result_rect = egui::Rect::from_min_max(egui::pos2(area.left() + 24.0, root_rect.bottom() + 15.0), egui::pos2(area.right() - 24.0, area.bottom()));
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(result_rect), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        match search_files(&self.db, &self.search) {
                            Ok(files) => for item in files { if ui.button(format!("{}  •  {:.2} MiB  •  {}", item.path, item.size as f64 / 1048576.0, item.torrent)).clicked() { self.pending = Some(item); } },
                            Err(e) => { ui.colored_label(egui::Color32::RED, e); }
                        }
                    });
                });
            }
            if let Some(error) = &self.error { ui.colored_label(egui::Color32::RED, error); }
        });
        if self.settings {
            let settings_title = self.text.settings.clone();
            egui::Window::new(settings_title).default_width(620.0).open(&mut self.settings).show(ctx, |ui| {
                ui.set_max_width(620.0);
                ui.heading(&self.text.sources);
                if !self.index_status.is_empty() { ui.label(&self.index_status); }
                if ui.add_enabled(self.index_rx.is_none(), egui::Button::new(if self.language == 0 { "Retry pending sources" } else { "Reintentar fuentes pendientes" })).clicked() { self.index_pending = true; self.skip_settings_index = true; }
                if ui.add_enabled(self.index_rx.is_none() && !self.downloads.iter().any(|d| d.active), egui::Button::new(if self.language == 0 { "Delete all sources and index" } else { "Borrar todas las fuentes y el índice" })).clicked() { self.confirm_delete = true; }
                ui.horizontal(|ui| {
                    if ui.button(&self.text.export).clicked() {
                        if let Some(path) = rfd::FileDialog::new().set_file_name("torrent-finder-sources.json").add_filter("JSON", &["json"]).save_file() {
                            match export_sources(&self.db, &path) {
                                Ok(count) => { self.message = Some(format!("{}: {}", self.text.exported, count)); self.error = None; }
                                Err(e) => { self.error = Some(e); self.message = None; }
                            }
                        }
                    }
                    if ui.button(&self.text.import).clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                            match import_sources(&mut self.db, &path) {
                                Ok(count) => { if count > 0 { self.skip_settings_index = false; } self.source_names = source_names(&self.db).unwrap_or_default(); self.message = Some(format!("{}: {}", self.text.imported, count)); self.error = None; }
                                Err(e) => { self.error = Some(e); self.message = None; }
                            }
                        }
                    }
                });
                if let Some(message) = &self.message { ui.label(message); }
                ui.label(&self.text.magnet);
                ui.text_edit_singleline(&mut self.magnet);
                if ui.button(&self.text.add).clicked() {
                    let magnet = self.magnet.trim();
                    if !magnet.starts_with("magnet:?") {
                        self.error = Some("Invalid magnet link / Enlace magnet inválido".into());
                    } else {
                        match self.db.execute("INSERT OR IGNORE INTO sources(kind,value) VALUES('magnet',?1)", [magnet]) {
                            Ok(_) => { self.skip_settings_index = false; self.magnet.clear(); self.source_names = source_names(&self.db).unwrap_or_default(); self.error = None; }
                            Err(e) => self.error = Some(e.to_string()),
                        }
                    }
                }
                ui.separator();
                ui.label(&self.text.torrent);
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.torrent);
                    if ui.button("Browse / Examinar").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("Torrent", &["torrent"]).pick_file() {
                            self.torrent = path.to_string_lossy().into_owned();
                        }
                    }
                });
                if ui.button(&self.text.add).clicked() {
                    let path = PathBuf::from(&self.torrent);
                    match std::fs::read(&path) {
                        Ok(bytes) if !bytes.is_empty() => {
                            match self.db.execute("INSERT INTO sources(kind,value,torrent) VALUES('torrent',?1,?2) ON CONFLICT(kind,value) DO UPDATE SET torrent=excluded.torrent", params![self.torrent, bytes]) {
                                Ok(_) => { self.skip_settings_index = false; self.torrent.clear(); self.source_names = source_names(&self.db).unwrap_or_default(); self.error = None; }
                                Err(e) => self.error = Some(e.to_string()),
                            }
                        }
                        Ok(_) => self.error = Some("Empty file / Archivo vacío".into()),
                        Err(e) => self.error = Some(e.to_string()),
                    }
                }
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for (id, name) in &self.source_names {
                        ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.index_rx.is_none() && !self.downloads.iter().any(|d| d.active), egui::Button::new(if self.language == 0 { "Delete" } else { "Borrar" })).clicked() {
                                self.confirm_delete_source = Some((*id, name.clone()));
                            }
                            let source: Option<(String, String, bool)> = self.db.query_row("SELECT s.kind,s.value,EXISTS(SELECT 1 FROM indexed_sources i WHERE i.source_id=s.id) FROM sources s WHERE s.id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).ok();
                            if let Some((kind, value, indexed)) = source {
                                if ui.add_enabled(self.index_rx.is_none() && !indexed, egui::Button::new(if self.language == 0 { "Retry" } else { "Reintentar" })).clicked() {
                                    self.retry_source = Some(*id);
                                    self.skip_settings_index = true;
                                }
                                if kind == "magnet" && ui.button(if self.language == 0 { "Copy magnet" } else { "Copiar magnet" }).clicked() {
                                    ctx.copy_text(value);
                                    self.message = Some(if self.language == 0 { "Magnet copied".into() } else { "Magnet copiado".into() });
                                }
                            }
                            let width = ui.available_width().max(1.0);
                            ui.add_sized([width, 20.0], egui::Label::new(name).truncate());
                        });
                        if let Some(state) = self.index_details.get(id) {
                            ui.add_sized([ui.available_width().max(1.0), 20.0], egui::Label::new(state).truncate()).on_hover_text(state);
                        }
                        ui.separator();
                        });
                    }
                });
                ui.separator();
                ui.label(&self.text.location);
                ui.horizontal(|ui| {
                    if ui.text_edit_singleline(&mut self.location).lost_focus() {
                        if let Err(e) = save_setting(&self.db, "location", &self.location) { self.error = Some(e); }
                    }
                    if ui.button("Browse / Examinar").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_folder() {
                            self.location = path.to_string_lossy().into_owned();
                            if let Err(e) = save_setting(&self.db, "location", &self.location) { self.error = Some(e); }
                        }
                    }
                });
                ui.separator();
                let before = self.language;
                egui::ComboBox::from_label(&self.text.language)
                    .selected_text(if self.language == 0 { "English" } else { "Español" })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.language, 0, "English");
                        ui.selectable_value(&mut self.language, 1, "Español");
                    });
                if before != self.language {
                    let path = if self.language == 0 { "en" } else { "es" };
                    match load_language(path) {
                        Ok(text) => { self.text = text; self.error = save_setting(&self.db, "language", if self.language == 0 { "en" } else { "es" }).err(); }
                        Err(e) => { self.language = before; self.error = Some(e); }
                    }
                }
                if let Some(error) = &self.error { ui.colored_label(egui::Color32::RED, error); }
            });
        }
        if self.settings_was_open && !self.settings {
            if !self.skip_settings_index { self.index_pending = true; }
            self.skip_settings_index = false;
        }
        if let Some((id, name)) = self.confirm_delete_source.clone() {
            egui::Window::new(if self.language == 0 { "Delete source?" } else { "¿Borrar fuente?" }).collapsible(false).show(ctx, |ui| {
                ui.label(if self.language == 0 { "Delete this source and its index? Downloaded files will be kept." } else { "¿Borrar esta fuente y su índice? Los archivos descargados se conservarán." });
                let mut short_name: String = name.chars().take(90).collect();
                if name.chars().count() > 90 { short_name.push('…'); }
                ui.label(short_name);
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.index_rx.is_none() && !self.downloads.iter().any(|d| d.active), egui::Button::new(if self.language == 0 { "Delete" } else { "Borrar" })).clicked() {
                        let result = (|| -> Result<(), String> {
                            let tx = self.db.transaction().map_err(|e| e.to_string())?;
                            tx.execute("DELETE FROM indexed_files WHERE source_id=?1", [id]).map_err(|e| e.to_string())?;
                            tx.execute("DELETE FROM indexed_sources WHERE source_id=?1", [id]).map_err(|e| e.to_string())?;
                            tx.execute("DELETE FROM sources WHERE id=?1", [id]).map_err(|e| e.to_string())?;
                            tx.commit().map_err(|e| e.to_string())?;
                            Ok(())
                        })();
                        match result {
                            Ok(()) => {
                                self.source_names = source_names(&self.db).unwrap_or_default();
                                self.index_details.remove(&id);
                                if self.pending.as_ref().is_some_and(|item| item.source_id == id) { self.pending = None; }
                                self.message = Some(if self.language == 0 { "Source deleted".into() } else { "Fuente borrada".into() });
                                self.error = None;
                            }
                            Err(e) => self.error = Some(e),
                        }
                        self.confirm_delete_source = None;
                    }
                    if ui.button(if self.language == 0 { "Cancel" } else { "Cancelar" }).clicked() { self.confirm_delete_source = None; }
                });
            });
        }
        if self.confirm_delete {
            egui::Window::new(if self.language == 0 { "Delete sources?" } else { "¿Borrar fuentes?" }).collapsible(false).show(ctx, |ui| {
                ui.label(if self.language == 0 { "Delete all saved sources and their index? Downloaded files stay on disk." } else { "¿Borrar todas las fuentes guardadas y su índice? Los archivos descargados se conservarán." });
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.index_rx.is_none() && !self.downloads.iter().any(|d| d.active), egui::Button::new(if self.language == 0 { "Delete" } else { "Borrar" })).clicked() {
                        match self.db.execute_batch("BEGIN IMMEDIATE; DELETE FROM indexed_files; DELETE FROM indexed_sources; DELETE FROM sources; COMMIT;") {
                            Ok(()) => { self.source_names.clear(); self.index_status.clear(); self.index_details.clear(); self.message = Some(if self.language == 0 { "Sources deleted".into() } else { "Fuentes eliminadas".into() }); }
                            Err(e) => { let _ = self.db.execute_batch("ROLLBACK;"); self.error = Some(e.to_string()); }
                        }
                        self.confirm_delete = false;
                    }
                    if ui.button(if self.language == 0 { "Cancel" } else { "Cancelar" }).clicked() { self.confirm_delete = false; }
                });
            });
        }
        if (self.index_pending || self.retry_source.is_some()) && self.index_rx.is_none() {
            match index_states(&self.db) {
                Ok(states) => { for (id, state) in states { self.index_details.entry(id).or_insert(state); } }
                Err(e) => { self.error = Some(e); }
            }
            self.index_status = "Starting indexing / Iniciando indexación...".into();
            let only_source = self.retry_source.take();
            self.index_rx = Some(start_index(only_source));
            self.index_pending = false;
        }
        self.settings_was_open = self.settings;
        if self.confirm_close {
            egui::Window::new(if self.language == 0 { "Close application?" } else { "¿Cerrar la aplicación?" }).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(if self.language == 0 { "Active transfers will stop and empty cache files will be removed." } else { "Se detendrán las transferencias y se borrarán los archivos vacíos de la caché." });
                ui.horizontal(|ui| {
                    if ui.button(if self.language == 0 { "Close" } else { "Cerrar" }).clicked() {
                        self.confirm_close = false;
                        self.closing = true;
                        self.shutdown.store(true, Ordering::Relaxed);
                        ctx.request_repaint();
                    }
                    if ui.button(if self.language == 0 { "Cancel" } else { "Cancelar" }).clicked() { self.confirm_close = false; }
                });
            });
        }
        if let Some(item) = self.pending.clone() {
            egui::Window::new(if self.language == 0 { "Confirm download" } else { "Confirmar descarga" }).collapsible(false).show(ctx, |ui| {
                ui.label(&item.path);
                ui.label(format!("{:.2} MiB", item.size as f64 / 1048576.0));
                ui.label(&self.location);
                ui.horizontal(|ui| {
                    if ui.button(if self.language == 0 { "Download" } else { "Descargar" }).clicked() {
                        let rx = start_download(item.clone(), self.location.clone(), self.seed_enabled.clone(), self.shutdown.clone(), self.copy_root);
                        self.downloads.push(Transfer { name: item.path.clone(), rx, status: "Starting / Iniciando".into(), fraction: 0.0, rate: 0.0, active: true });
                        self.pending = None;
                    }
                    if ui.button(if self.language == 0 { "Cancel" } else { "Cancelar" }).clicked() { self.pending = None; }
                });
            });
        }
    }
}

fn main() -> eframe::Result {
    if std::env::var_os("NANANEAS_DIAGNOSTIC").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("warn,librqbit=debug,librqbit_tracker_comms=debug")
            .with_ansi(false)
            .try_init();
    }
    let db = database().expect("Cannot open the local catalog database");
    let language = if setting(&db, "language", "en") == "es" { 1 } else { 0 };
    let text = load_language(if language == 0 { "en" } else { "es" }).expect("Cannot load the user translation XML");
    let seed = setting(&db, "seed", "0") == "1";
    let copy_root = !seed && setting(&db, "copy_root", "0") == "1";
    let existing_location: Result<String, _> = db.query_row("SELECT value FROM settings WHERE key='location'", [], |row| row.get(0));
    let location = match existing_location {
        Ok(path) => path,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            let default = dirs::download_dir().or_else(|| dirs::home_dir().map(|p| p.join("Downloads"))).expect("Cannot find the user Downloads folder").join("Torrents");
            std::fs::create_dir_all(&default).expect("Cannot create default Torrents folder");
            let value = default.to_string_lossy().into_owned();
            save_setting(&db, "location", &value).expect("Cannot save default download folder");
            value
        }
        Err(e) => panic!("Cannot read download location: {e}"),
    };
    let source_names = source_names(&db).unwrap_or_default();
    let index_details = index_states(&db).unwrap_or_default();
    let icon = image::load_from_memory(include_bytes!("../assets/cat-icon.png")).expect("Invalid application icon").resize(256, 256, image::imageops::FilterType::Lanczos3).to_rgba8();
    let icon_data = egui::IconData { width: icon.width(), height: icon.height(), rgba: icon.into_raw() };
    let mascot = image::load_from_memory(include_bytes!("../assets/cat-mascot.png")).expect("Invalid mascot").to_rgba8();
    let mascot_image = egui::ColorImage::from_rgba_unmultiplied([mascot.width() as usize, mascot.height() as usize], mascot.as_raw());
    let app = move |cc: &eframe::CreationContext<'_>| {
    let mascot = cc.egui_ctx.load_texture("cat-mascot", mascot_image, egui::TextureOptions::LINEAR);
    App { text, mascot, language, settings: false, seed, seed_enabled: Arc::new(AtomicBool::new(seed)), search: String::new(), magnet: String::new(), torrent: String::new(), location, error: None, db, source_names, confirm_delete_source: None, message: None, index_rx: None, index_status: String::new(), index_details, settings_was_open: false, index_pending: false, retry_source: None, skip_settings_index: false, copy_root, pending: None, downloads: Vec::new(), speed_history: std::collections::VecDeque::new(), last_graph_sample: std::time::Instant::now(), confirm_delete: false, shutdown: Arc::new(AtomicBool::new(false)), confirm_close: false, closing: false, allow_close: false }
    };
    let options = eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([850.0, 560.0]).with_icon(icon_data), ..Default::default() };
    eframe::run_native("Nananeas Torrent Downloader", options, Box::new(|cc| Ok(Box::new(app(cc)))))
}
