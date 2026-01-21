use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
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
    created_at: Option<String>,
}

#[derive(Serialize)]
struct DeleteResult {
    deleted: Vec<String>,
    failed: Vec<String>,
}

#[derive(Clone, Serialize)]
struct BatchProgress {
    total: usize,
    completed: usize,
    percent: f64,
    eta_seconds: u64,
    in_progress: bool,
}

fn emit_batch_progress(app: &AppHandle, progress: BatchProgress) {
    if let Err(error) = app.emit("batch-progress", progress) {
        eprintln!("Failed to emit batch progress: {error}");
    }
}

fn get_file_created_at(path: &Path) -> Option<String> {
    fs::metadata(path)
        .ok()
        .and_then(|metadata| {
            // Try created() first (file creation time), fall back to modified() if not available
            // On some systems, created() may not be available, so we use modified() as fallback
            metadata.created()
                .or_else(|_| metadata.modified())
                .ok()
        })
        .and_then(|system_time| {
            system_time
                .duration_since(UNIX_EPOCH)
                .ok()
                .map(|duration| {
                    // Convert to milliseconds for JavaScript Date constructor
                    let secs = duration.as_secs();
                    let nanos = duration.subsec_nanos();
                    // Convert nanoseconds to milliseconds and add to seconds
                    let millis = secs * 1000 + (nanos / 1_000_000) as u64;
                    format!("{}", millis)
                })
        })
}

fn emit_status(
    app: &AppHandle,
    status: &str,
    path: Option<&Path>,
    error: Option<String>,
    text: Option<String>,
) {
    let created_at = path.and_then(|p| get_file_created_at(p));
    
    let payload = OcrStatus {
        status: status.to_string(),
        path: path.and_then(|value| value.to_str()).map(|value| value.to_string()),
        error,
        text,
        created_at,
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
    
    // Build Tesseract - configuration is optional, so we'll use defaults if it fails
    let tesseract = Tesseract::new(None, Some("eng"))
        .map_err(|error| format!("{error}"))?;
    
    // Try to configure for better text extraction, but continue with defaults if it fails
    // Note: These settings are optional optimizations
    let tesseract = tesseract
        .set_variable("tessedit_pageseg_mode", "3")
        .and_then(|t| t.set_variable("oem", "1"))
        .unwrap_or_else(|_| {
            // If configuration fails, recreate with defaults
            // This is safe because Tesseract::new should always succeed if it succeeded before
            Tesseract::new(None, Some("eng"))
                .expect("Tesseract initialization should not fail after successful creation")
        });
    
    tesseract
        .set_image(path_str)
        .map_err(|error| format!("{error}"))?
        .get_text()
        .map_err(|error| format!("{error}"))
}

fn summarize_text(text: &str) -> String {
    let mut best_line = "";
    let mut best_score = 0usize;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let score = trimmed.chars().filter(|ch| ch.is_ascii_alphanumeric()).count();
        if score > best_score {
            best_score = score;
            best_line = trimmed;
        }
    }

    let source = if best_line.is_empty() { text } else { best_line };
    let mut words = Vec::new();
    for word in source.split_whitespace() {
        let cleaned = word
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric())
            .to_ascii_lowercase();
        if cleaned.len() < 3 {
            continue;
        }
        words.push(cleaned);
        if words.len() >= 5 {
            break;
        }
    }

    words.join(" ")
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
    let summary = summarize_text(text);
    let slug = slugify_text(&summary);
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
    known_map: Arc<Mutex<HashSet<PathBuf>>>,
) {
    // Mark original path as known immediately to prevent duplicate processing
    {
        let mut guard = known_map.lock().unwrap();
        guard.insert(path.clone());
    }
    
    emit_status(&app, "processing", Some(&path), None, None);

    if !path.exists() {
        emit_status(&app, "idle", Some(&path), None, None);
        return;
    }

    if let Err(error) = wait_for_file(&path) {
        eprintln!("File not ready: {} ({error})", path.display());
        emit_status(&app, "idle", Some(&path), Some(error), None);
        return;
    }

    match run_ocr(&path) {
        Ok(text) => {
            let trimmed = text.trim().to_string();
            let final_path = match rename_with_text(&path, &trimmed) {
                Ok(new_path) => {
                    // Mark renamed path as known to prevent processing it again
                    {
                        let mut guard = known_map.lock().unwrap();
                        guard.insert(new_path.clone());
                    }
                    remember_ignore(&ignore_map, &new_path);
                    new_path
                }
                Err(error) => {
                    eprintln!("Rename failed for {}: {error}", path.display());
                    path.clone()
                }
            };
            remember_ignore(&ignore_map, &path);
            println!("OCR result for {}:\n{}", final_path.display(), trimmed);
            // Emit with the final path (renamed) and full text
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
    known_map: &Arc<Mutex<HashSet<PathBuf>>>,
) {
    if !matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) {
        return;
    }

    for path in event.paths {
        if !is_png(&path) || is_hidden(&path) || is_ignored(ignore_map, &path) {
            continue;
        }

        let already_known = {
            let guard = known_map.lock().unwrap();
            guard.contains(&path)
        };
        if already_known {
            continue;
        }

        if !is_ignored(ignore_map, &path) {
            let now = Instant::now();
            {
                let mut guard = debounce_map.lock().unwrap();
                guard.insert(path.clone(), now);
            }

            let app_handle = app.clone();
            let debounce_map = Arc::clone(debounce_map);
            let ignore_map = Arc::clone(ignore_map);
            let known_map = Arc::clone(known_map);
            tauri::async_runtime::spawn_blocking(move || {
                thread::sleep(Duration::from_millis(750));
                let should_process = {
                    let guard = debounce_map.lock().unwrap();
                    guard.get(&path).map(|seen| *seen == now).unwrap_or(false)
                };

                if should_process {
                    process_screenshot(app_handle, path, ignore_map, known_map);
                }
            });
        }
    }
}

fn load_existing_screenshots(watch_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut existing = Vec::new();

    for dir in watch_dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if is_png(&path) && !is_hidden(&path) {
                existing.push(path);
            }
        }
    }

    existing
}

fn process_existing_screenshots(app: AppHandle, paths: Vec<PathBuf>) {
    tauri::async_runtime::spawn_blocking(move || {
        let total = paths.len();
        if total == 0 {
            emit_batch_progress(
                &app,
                BatchProgress {
                    total: 0,
                    completed: 0,
                    percent: 100.0,
                    eta_seconds: 0,
                    in_progress: false,
                },
            );
            return;
        }

        emit_batch_progress(
            &app,
            BatchProgress {
                total,
                completed: 0,
                percent: 0.0,
                eta_seconds: 0,
                in_progress: true,
            },
        );

        let mut completed = 0usize;
        let mut total_elapsed = Duration::from_secs(0);

        for path in paths {
            let start = Instant::now();
            emit_status(&app, "processing", Some(&path), None, None);
            if let Err(error) = wait_for_file(&path) {
                eprintln!("File not ready: {} ({error})", path.display());
                emit_status(&app, "idle", Some(&path), Some(error), None);
                completed += 1;
                total_elapsed += start.elapsed();
                let average = total_elapsed.as_secs_f64() / completed as f64;
                let remaining = total.saturating_sub(completed) as f64;
                emit_batch_progress(
                    &app,
                    BatchProgress {
                        total,
                        completed,
                        percent: (completed as f64 / total as f64) * 100.0,
                        eta_seconds: (average * remaining).round() as u64,
                        in_progress: completed < total,
                    },
                );
                continue;
            }

            match run_ocr(&path) {
                Ok(text) => {
                    let trimmed = text.trim().to_string();
                    println!("OCR result for {}:\n{}", path.display(), trimmed);
                    emit_status(&app, "idle", Some(&path), None, Some(trimmed));
                }
                Err(error) => {
                    eprintln!("OCR failed for {}: {error}", path.display());
                    emit_status(&app, "idle", Some(&path), Some(error), None);
                }
            }

            completed += 1;
            total_elapsed += start.elapsed();
            let average = total_elapsed.as_secs_f64() / completed as f64;
            let remaining = total.saturating_sub(completed) as f64;
            emit_batch_progress(
                &app,
                BatchProgress {
                    total,
                    completed,
                    percent: (completed as f64 / total as f64) * 100.0,
                    eta_seconds: (average * remaining).round() as u64,
                    in_progress: completed < total,
                },
            );
        }
    });
}

fn start_watcher(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let (tx, rx) = mpsc::channel();
        let debounce_map = Arc::new(Mutex::new(HashMap::new()));
        let ignore_map = Arc::new(Mutex::new(HashMap::new()));
        let known_map = Arc::new(Mutex::new(HashSet::new()));

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

        let existing = load_existing_screenshots(&watch_dirs);
        {
            let mut guard = known_map.lock().unwrap();
            for path in &existing {
                guard.insert(path.clone());
            }
        }
        process_existing_screenshots(app.clone(), existing);

        for dir in watch_dirs {
            if let Err(error) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                eprintln!("Failed to watch {}: {error}", dir.display());
            } else {
                println!("Watching {}", dir.display());
            }
        }

        for res in rx {
            match res {
                Ok(event) => handle_event(&app, event, &debounce_map, &ignore_map, &known_map),
                Err(error) => eprintln!("Watch error: {error}"),
            }
        }
    });
}

#[tauri::command]
fn delete_files(paths: Vec<String>) -> Result<DeleteResult, String> {
    if paths.is_empty() {
        return Err("No files selected for deletion".to_string());
    }

    let watch_dirs = resolve_watch_dirs();
    if watch_dirs.is_empty() {
        return Err("Watch directories not available".to_string());
    }

    let mut deleted = Vec::new();
    let mut failed = Vec::new();

    for path_str in paths {
        let path = PathBuf::from(&path_str);
        
        // Check if file exists first
        if !path.exists() {
            // File doesn't exist - treat as successful deletion (remove from index)
            deleted.push(path_str);
            continue;
        }
        
        let Ok(canonical_path) = fs::canonicalize(&path) else {
            // Can't canonicalize but file exists - try direct deletion
            match fs::remove_file(&path) {
                Ok(_) => {
                    deleted.push(path_str);
                }
                Err(e) => {
                    eprintln!("[DELETE] Failed to delete {} (canonicalize failed): {:?}", path_str, e);
                    failed.push(path_str);
                }
            }
            continue;
        };

        let allowed = watch_dirs.iter().any(|dir| {
            fs::canonicalize(dir)
                .map(|canonical_dir| canonical_path.starts_with(&canonical_dir))
                .unwrap_or(false)
        });

        if !allowed {
            eprintln!("[DELETE] Path not in allowed directories: {}", path_str);
            failed.push(path_str);
            continue;
        }

        match fs::remove_file(&canonical_path) {
            Ok(_) => {
                // Return the original path_str, not canonicalized, so it matches frontend entries
                deleted.push(path_str);
            }
            Err(e) => {
                eprintln!("[DELETE] Failed to delete {}: {:?}", path_str, e);
                failed.push(path_str);
            }
        }
    }

    Ok(DeleteResult { deleted, failed })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![delete_files])
        .setup(|app| {
            start_watcher(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
