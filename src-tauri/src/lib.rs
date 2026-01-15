use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tesseract::Tesseract;

#[derive(Clone, Serialize)]
struct OcrStatus {
    status: String,
    path: Option<String>,
    error: Option<String>,
    text: Option<String>,
}

fn emit_status(
    app: &AppHandle,
    status: &str,
    path: Option<&Path>,
    error: Option<String>,
    text: Option<String>,
) {
    let payload = OcrStatus {
        status: status.to_string(),
        path: path.and_then(|value| value.to_str()).map(|value| value.to_string()),
        error,
        text,
    };

    if let Err(error) = app.emit("ocr-status", payload) {
        eprintln!("Failed to emit status: {error}");
    }
}

fn resolve_watch_dirs() -> Vec<PathBuf> {
    let Ok(home) = std::env::var("HOME") else {
        eprintln!("HOME environment variable not set. File watcher disabled.");
        return Vec::new();
    };

    let home_dir = PathBuf::from(home);
    vec![
        home_dir.join("Desktop"),
        home_dir.join("Pictures").join("Screenshots"),
    ]
}

fn is_png(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("png"))
        .unwrap_or(false)
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

fn wait_for_file(path: &Path) -> Result<(), String> {
    let mut last_size = None;

    for _ in 0..12 {
        match fs::metadata(path) {
            Ok(metadata) => {
                let size = metadata.len();
                if size > 0 && last_size == Some(size) {
                    return Ok(());
                }
                last_size = Some(size);
            }
            Err(_) => {}
        }

        thread::sleep(Duration::from_millis(200));
    }

    Err("File not ready after waiting".to_string())
}

fn run_ocr(path: &Path) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Image path is not valid UTF-8".to_string())?;
    Tesseract::new(None, Some("eng"))
        .map_err(|error| format!("{error}"))?
        .set_image(path_str)
        .map_err(|error| format!("{error}"))?
        .get_text()
        .map_err(|error| format!("{error}"))
}

fn slugify_text(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;

    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if ch.is_ascii_whitespace() || ch == '-' || ch == '_' {
            if !last_dash && !out.is_empty() {
                out.push('-');
                last_dash = true;
            }
        }
        if out.len() >= 60 {
            break;
        }
    }

    if out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        "screenshot".to_string()
    } else {
        out
    }
}

fn rename_with_text(path: &Path, text: &str) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Screenshot path missing parent directory".to_string())?;
    let slug = slugify_text(text);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("{error}"))?
        .as_secs();
    let filename = format!("{slug}-{stamp}.png");
    let new_path = parent.join(filename);

    fs::rename(path, &new_path)
        .map_err(|error| format!("Failed to rename file: {error}"))?;
    Ok(new_path)
}

fn remember_ignore(ignore_map: &Arc<Mutex<HashMap<PathBuf, Instant>>>, path: &Path) {
    let mut guard = ignore_map.lock().unwrap();
    guard.insert(path.to_path_buf(), Instant::now());
}

fn is_ignored(ignore_map: &Arc<Mutex<HashMap<PathBuf, Instant>>>, path: &Path) -> bool {
    let mut guard = ignore_map.lock().unwrap();
    let cutoff = Instant::now() - Duration::from_secs(5);
    guard.retain(|_, seen| *seen >= cutoff);
    guard.get(path).is_some()
}

fn process_screenshot(
    app: AppHandle,
    path: PathBuf,
    ignore_map: Arc<Mutex<HashMap<PathBuf, Instant>>>,
) {
    emit_status(&app, "processing", Some(&path), None, None);

    if let Err(error) = wait_for_file(&path) {
        eprintln!("File not ready: {} ({error})", path.display());
        emit_status(&app, "idle", Some(&path), Some(error), None);
        return;
    }

    match run_ocr(&path) {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            let final_path = match rename_with_text(&path, &trimmed) {
                Ok(new_path) => new_path,
                Err(error) => {
                    eprintln!("Rename failed for {}: {error}", path.display());
                    path.clone()
                }
            };
            remember_ignore(&ignore_map, &final_path);
            println!("OCR result for {}:\n{}", final_path.display(), trimmed);
            emit_status(&app, "idle", Some(&final_path), None, Some(trimmed));
        }
        Err(error) => {
            eprintln!("OCR failed for {}: {error}", path.display());
            emit_status(&app, "idle", Some(&path), Some(error), None);
        }
    }
}

fn handle_event(
    app: &AppHandle,
    event: Event,
    debounce_map: &Arc<Mutex<HashMap<PathBuf, Instant>>>,
    ignore_map: &Arc<Mutex<HashMap<PathBuf, Instant>>>,
) {
    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
        return;
    }

    for path in event.paths {
        if is_png(&path) && !is_hidden(&path) && !is_ignored(ignore_map, &path) {
            let now = Instant::now();
            {
                let mut guard = debounce_map.lock().unwrap();
                guard.insert(path.clone(), now);
            }

            let app_handle = app.clone();
            let debounce_map = Arc::clone(debounce_map);
            let ignore_map = Arc::clone(ignore_map);
            tauri::async_runtime::spawn_blocking(move || {
                thread::sleep(Duration::from_millis(750));
                let should_process = {
                    let guard = debounce_map.lock().unwrap();
                    guard.get(&path).map(|seen| *seen == now).unwrap_or(false)
                };

                if should_process {
                    process_screenshot(app_handle, path, ignore_map);
                }
            });
        }
    }
}

fn start_watcher(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = mpsc::channel();
        let debounce_map = Arc::new(Mutex::new(HashMap::new()));
        let ignore_map = Arc::new(Mutex::new(HashMap::new()));

        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("Failed to start file watcher: {error}");
                return;
            }
        };

        let watch_dirs = resolve_watch_dirs();
        if watch_dirs.is_empty() {
            return;
        }

        for dir in watch_dirs {
            if let Err(error) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                eprintln!("Failed to watch {}: {error}", dir.display());
            } else {
                println!("Watching {}", dir.display());
            }
        }

        for res in rx {
            match res {
                Ok(event) => handle_event(&app, event, &debounce_map, &ignore_map),
                Err(error) => eprintln!("Watch error: {error}"),
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            start_watcher(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
