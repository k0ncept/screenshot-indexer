use notify::{Event, EventKind, RecursiveMode, Watcher};
use rusqlite::{Connection, Result as SqlResult};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, webview::WebviewWindowBuilder, utils::config::WebviewUrl};
use tauri_plugin_global_shortcut::{Code, Modifiers, ShortcutState, GlobalShortcutExt};
use tesseract::Tesseract;
use image::{ImageBuffer, GenericImageView};
use regex::Regex;
use image_hasher::{HashAlg, HasherConfig};

#[derive(Clone, Serialize)]
struct OcrStatus {
    status: String,
    path: Option<String>,
    error: Option<String>,
    text: Option<String>,
    created_at: Option<String>,
    tags: Option<String>,
    urls: Option<String>,
    emails: Option<String>,
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
    
    // Extract tags, URLs, emails if text is available
    let (tags, urls, emails) = if let Some(text_str) = &text {
        let detected_tags = detect_collections(text_str);
        let (extracted_urls, extracted_emails) = extract_urls_and_emails(text_str);
        (
            Some(serde_json::to_string(&detected_tags).unwrap_or_else(|_| "[]".to_string())),
            Some(serde_json::to_string(&extracted_urls).unwrap_or_else(|_| "[]".to_string())),
            Some(serde_json::to_string(&extracted_emails).unwrap_or_else(|_| "[]".to_string()))
        )
    } else {
        (None, None, None)
    };
    
    let payload = OcrStatus {
        status: status.to_string(),
        path: path.and_then(|value| value.to_str()).map(|value| value.to_string()),
        error,
        text,
        created_at,
        tags,
        urls,
        emails,
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

fn preprocess_image(path: &Path) -> Result<PathBuf, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Image path is not valid UTF-8".to_string())?;
    
    println!("[OCR] Preprocessing image: {}", path_str);
    
    // Load the image
    let img = image::open(path)
        .map_err(|e| format!("Failed to open image: {e}"))?;
    
    let (width, height) = img.dimensions();
    println!("[OCR] Original image size: {}x{}", width, height);
    
    // Convert to grayscale
    let gray_img = img.to_luma8();
    
    // Create a new grayscale image buffer for processed image
    let mut processed = ImageBuffer::new(width, height);
    
    // Apply lighter preprocessing for messaging apps
    // Less aggressive than before - just slight contrast enhancement
    for (x, y, pixel) in gray_img.enumerate_pixels() {
        let gray = pixel[0];
        
        // Light contrast enhancement (less aggressive for chat bubbles)
        let enhanced = if gray < 100 {
            // Darken dark areas slightly
            ((gray as f64 * 0.9).max(0.0)) as u8
        } else if gray > 155 {
            // Lighten light areas slightly
            (((gray as f64 - 155.0) * 1.1 + 155.0).min(255.0)) as u8
        } else {
            gray // Keep middle tones as-is
        };
        
        processed.put_pixel(x, y, image::Luma([enhanced]));
    }
    
    // Save processed image to temp file
    let temp_path = path.parent()
        .ok_or_else(|| "No parent directory".to_string())?
        .join(format!(".ocr_temp_{}.png", 
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()));
    
    processed.save(&temp_path)
        .map_err(|e| format!("Failed to save processed image: {e}"))?;
    
    println!("[OCR] Preprocessed image saved to: {:?}", temp_path);
    Ok(temp_path)
}

fn run_ocr_with_psm(path: &Path, psm_mode: &str, description: &str) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Image path is not valid UTF-8".to_string())?;
    
    println!("[OCR] Attempting OCR with PSM {} ({})", psm_mode, description);
    
    // Build Tesseract with enhanced configuration for messaging apps
    let tesseract = Tesseract::new(None, Some("eng"))
        .map_err(|error| format!("Tesseract initialization failed: {error}"))?;
    
    // Enhanced configuration for better UI/messaging text recognition
    let tesseract = match tesseract.set_variable("tessedit_pageseg_mode", psm_mode) {
        Ok(t) => {
            // Try to set multiple optimization variables
            let mut configured = t;
            
            // Set OEM to LSTM for better accuracy
            configured = match configured.set_variable("oem", "1") {
                Ok(c) => c,
                Err(_) => {
                    // If OEM fails, recreate with just PSM
                    Tesseract::new(None, Some("eng"))
                        .map_err(|error| format!("Tesseract initialization failed: {error}"))?
                        .set_variable("tessedit_pageseg_mode", psm_mode)
                        .unwrap_or_else(|_| {
                            Tesseract::new(None, Some("eng"))
                                .expect("Tesseract should work")
                        })
                }
            };
            
            // Try additional optimizations for UI text
            // Since set_variable consumes self, we chain them and recreate from scratch if any fails
            // Helper to recreate base Tesseract with essential settings
            let recreate_base = || -> Tesseract {
                Tesseract::new(None, Some("eng"))
                    .map_err(|_| "Failed".to_string())
                    .ok()
                    .and_then(|t| t.set_variable("tessedit_pageseg_mode", psm_mode).ok())
                    .and_then(|t| t.set_variable("oem", "1").ok())
                    .unwrap_or_else(|| {
                        // Last resort: just PSM
                        Tesseract::new(None, Some("eng"))
                            .expect("Tesseract should work")
                            .set_variable("tessedit_pageseg_mode", psm_mode)
                            .unwrap_or_else(|_| {
                                Tesseract::new(None, Some("eng"))
                                    .expect("Tesseract should work")
                            })
                    })
            };
            
            // Chain variable settings - if any fails, recreate base and continue
            let final_config = configured
                .set_variable("preserve_interword_spaces", "1")
                .unwrap_or_else(|_| recreate_base())
                .set_variable("load_system_dawg", "0")
                .unwrap_or_else(|_| recreate_base())
                .set_variable("load_freq_dawg", "0")
                .unwrap_or_else(|_| recreate_base())
                .set_variable("load_unambig_dawg", "0")
                .unwrap_or_else(|_| recreate_base())
                .set_variable("load_punc_dawg", "0")
                .unwrap_or_else(|_| recreate_base())
                .set_variable("load_number_dawg", "0")
                .unwrap_or_else(|_| recreate_base());
            
            // Note: We don't set a character whitelist because:
            // 1. Messages can contain emojis, special characters, etc.
            // 2. Whitelist can hurt OCR accuracy by restricting what Tesseract can recognize
            // 3. We'll clean the text post-OCR instead
            
            final_config
        }
        Err(e) => {
            println!("[OCR] Warning: Failed to set PSM {}: {:?}, using defaults", psm_mode, e);
            // Recreate if setting failed
            let base = Tesseract::new(None, Some("eng"))
                .map_err(|error| format!("Tesseract initialization failed: {error}"))?;
            // Try OEM on the base
            match base.set_variable("oem", "1") {
                Ok(configured) => configured,
                Err(_) => {
                    // Recreate since base was moved
                    Tesseract::new(None, Some("eng"))
                        .map_err(|error| format!("Tesseract initialization failed: {error}"))?
                }
            }
        }
    };
    
    // Run OCR
    let result = tesseract
        .set_image(path_str)
        .map_err(|error| format!("Failed to set image: {error}"))?
        .get_text()
        .map_err(|error| format!("OCR extraction failed: {error}"))?;
    
    let cleaned = result
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    
    let char_count = cleaned.len();
    println!("[OCR] PSM {} extracted {} characters", psm_mode, char_count);
    
    Ok(cleaned)
}

fn fix_ocr_character_mistakes(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    
    let mut fixed = text.to_string();
    
    // Common OCR mistakes: fix character misreadings
    // OCR often confuses "I" (capital i) with "l" (lowercase L)
    // This is especially common in lowercase words like "lmao" -> "Imao"
    
    // Fix specific common words FIRST (before general pattern)
    // "Imfa" -> "lmfa" (laughing my fucking ass) - do this first
    fixed = fixed.replace("Imfaooo0o", "lmfao");
    fixed = fixed.replace("Imfaoooo", "lmfao");
    fixed = fixed.replace("Imfaooo", "lmfao");
    fixed = fixed.replace("Imfaoo", "lmfao");
    fixed = fixed.replace("Imfao", "lmfao");
    fixed = fixed.replace("Imfa", "lmfa");
    
    // "Imao" -> "lmao" (laughing my ass off)
    fixed = fixed.replace("Imaoooo", "lmao");
    fixed = fixed.replace("Imaooo", "lmao");
    fixed = fixed.replace("Imaoo", "lmao");
    fixed = fixed.replace("Imao", "lmao");
    fixed = fixed.replace("imao", "lmao");
    fixed = fixed.replace("iMAO", "lmao");
    fixed = fixed.replace("ImAO", "lmao");
    
    // "Iol" -> "Lol" (laughing out loud)
    fixed = fixed.replace("IOl", "Lol");
    fixed = fixed.replace("IOI", "Lol");
    fixed = fixed.replace("ioI", "Lol");
    fixed = fixed.replace("Iol", "Lol");
    
    // Then fix general patterns where "I" at start of word should be "l"
    // Pattern: "I" followed by lowercase letters (likely "l" in lowercase word)
    let i_to_l_pattern = Regex::new(r"\bI([a-z]{2,})\b").unwrap();
    fixed = i_to_l_pattern.replace_all(&fixed, |caps: &regex::Captures| {
        format!("l{}", &caps[1])
    }).to_string();
    
    // "Iol" -> "Lol" (laughing out loud)
    fixed = fixed.replace("Iol", "Lol");
    fixed = fixed.replace("ioI", "Lol");
    fixed = fixed.replace("IOI", "Lol");
    fixed = fixed.replace("IOl", "Lol");
    
    // Fix "0" -> "o" in word contexts (but keep "0" in numbers)
    // Only replace "0" when it's clearly in a word (surrounded by letters)
    let zero_in_word = Regex::new(r"([a-zA-Z])0+([a-zA-Z])").unwrap();
    fixed = zero_in_word.replace_all(&fixed, |caps: &regex::Captures| {
        // Replace multiple zeros with single "o"
        format!("{}o{}", &caps[1], &caps[2])
    }).to_string();
    
    // Fix "5" -> "s" in word contexts
    let five_in_word = Regex::new(r"([a-zA-Z])5([a-zA-Z])").unwrap();
    fixed = five_in_word.replace_all(&fixed, |caps: &regex::Captures| {
        format!("{}s{}", &caps[1], &caps[2])
    }).to_string();
    
    // Fix "1" -> "l" or "i" in word contexts (but keep "1" in numbers)
    let one_in_word = Regex::new(r"([a-zA-Z])1([a-zA-Z])").unwrap();
    fixed = one_in_word.replace_all(&fixed, |caps: &regex::Captures| {
        format!("{}l{}", &caps[1], &caps[2])
    }).to_string();
    
    fixed
}

// Auto-tagging functions
// Follows strict priority order: Messages → Code → Design → Receipts → Browser → Terminal → Errors → Documents → Images
fn detect_collections(text: &str) -> Vec<String> {
    let mut tags = Vec::new();
    let text_lower = text.to_lowercase();
    let text_trimmed = text.trim();
    let word_count = text_trimmed.split_whitespace().count();
    let char_count = text_trimmed.len();
    
    // Debug: Log what we're detecting
    let debug = word_count > 0 && word_count < 100; // Only debug shorter texts to avoid spam
    
    // STEP 1: MESSAGES DETECTION (Highest Priority - Check FIRST)
    // Time patterns (various formats) - be more lenient
    let time_pattern_12h = Regex::new(r"\d{1,2}:\d{2}\s*(?:AM|PM|am|pm)").unwrap();
    let time_pattern_24h = Regex::new(r"\b\d{1,2}:\d{2}\b").unwrap();
    let has_timestamps_12h = time_pattern_12h.find_iter(text).count() >= 1;
    let has_timestamps_24h = time_pattern_24h.find_iter(text).count() >= 1; // Even one timestamp suggests messages
    let has_any_timestamp = has_timestamps_12h || has_timestamps_24h;
    
    // Message app names and UI elements
    let has_message_apps = ["imessage", "slack", "discord", "whatsapp", "telegram", "signal", 
                            "messenger", "facebook messenger", "group chat", "direct message",
                            "dm", "thread", "channel", "conversation", "chat"].iter()
                            .any(|app| text_lower.contains(app));
    
    // Read receipts and message status
    let has_read_receipts = text_lower.contains("read") || text_lower.contains("delivered") ||
                           text_lower.contains("sent") || text_lower.contains("seen") ||
                           text_lower.contains("typing") || text_lower.contains("online") ||
                           text_lower.contains("offline") || text_lower.contains("last seen");
    
    // Chat/messaging words and patterns (expanded list)
    let has_chat_words = ["lmao", "lol", "omg", "btw", "imo", "tbh", "haha", "hahaha", 
                          "lmaoo", "lmfao", "fr", "ngl", "wyd", "wbu", "ttyl", "brb",
                          "thanks", "thank you", "np", "yw", "gg", "gl", "hf", "ikr",
                          "smh", "fyi", "asap", "tbh", "imo", "idk", "ik", "yeah", "yep",
                          "nah", "nope", "sure", "ok", "okay", "k", "kk", "got it",
                          "sounds good", "cool", "nice", "awesome", "perfect"].iter()
                          .any(|word| text_lower.contains(word));
    
    // Conversational patterns (questions, casual language)
    let has_questions = text.matches('?').count() >= 1; // Even one question suggests conversation
    let has_casual_greetings = ["hey", "hi", "hello", "sup", "what's up", "how are you", 
                                "how's it going", "what's going on", "how's everything",
                                "how have you been", "long time", "miss you"].iter()
                                .any(|greeting| text_lower.contains(greeting));
    
    // MESSAGE BUBBLES DETECTION (primary indicator)
    // Message bubbles have distinct patterns:
    // - Multiple short conversational lines
    // - Often have names/contacts before messages
    // - Often have timestamps on each line or message
    // - Lines are typically short (under 100 chars) and conversational
    // - Multiple messages from different "senders" (even if same person)
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    
    // Count short conversational lines (typical message bubble length)
    let short_lines = lines.iter().filter(|line| {
        let len = line.len();
        len > 0 && len < 120 // Message bubbles are typically short
    }).count();
    
    // Check for name patterns before messages (common in chat apps)
    // Patterns like "John:", "Sarah:", "You:", "Me:", or contact names
    let name_pattern = Regex::new(r"^[A-Z][a-z]+:|\b(You|Me|I):").unwrap();
    let has_name_prefixes = lines.iter().filter(|line| name_pattern.is_match(line)).count();
    
    // Check for timestamp patterns on lines (common in message bubbles)
    let line_with_time = Regex::new(r"\d{1,2}:\d{2}").unwrap();
    let lines_with_timestamps = lines.iter().filter(|line| line_with_time.is_match(line)).count();
    
    // Check for conversational structure (multiple short messages)
    // Message bubbles typically have 3+ short lines OR 2+ with strong indicators
    let has_multiple_short_messages = short_lines >= 3;
    
    // Check for alternating patterns (like back-and-forth conversation)
    // Even if it's the same person, messages appear as separate bubbles
    let _has_conversation_structure = short_lines >= 2 && 
                                    (has_name_prefixes >= 1 || lines_with_timestamps >= 1);
    
    // Strong message bubble indicators - this is the PRIMARY indicator
    // If we detect message bubbles, it's almost certainly a message screenshot
    let has_message_bubbles = has_multiple_short_messages || // 3+ short lines
                             (short_lines >= 2 && has_name_prefixes >= 1) || // 2+ short lines with name prefixes
                             (short_lines >= 2 && lines_with_timestamps >= 1) || // 2+ short lines with timestamps
                             (has_name_prefixes >= 2) || // Multiple name prefixes (strong indicator)
                             (lines_with_timestamps >= 2 && short_lines >= 1); // Multiple timestamps (strong indicator)
    
    // Date headers in messages (Today, Yesterday, etc.)
    let has_date_headers = ["today", "yesterday", "just now", "this week", "this month",
                            "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"].iter()
                          .any(|date| text_lower.contains(date));
    
    // Contact names or phone numbers (common in messages)
    let phone_pattern = Regex::new(r"\+?\d{1,3}[-.\s]?\(?\d{3}\)?[-.\s]?\d{3}[-.\s]?\d{4}").unwrap();
    let _has_phone = phone_pattern.is_match(text);
    
    // Conversational indicators (back-and-forth patterns)
    let has_conversation_indicators = text.matches(":").count() > 2 && // Multiple colons suggest timestamps or names
                                    (text.matches("\n").count() > 1 || short_lines > 1);
    
    // Emoji patterns (common in messages)
    let has_emoji_like = text.contains(":)") || text.contains(":(") || text.contains(":D") ||
                        text.contains("<3") || text.contains(":P") || text.contains(";)");
    
    // STEP 1: MESSAGES DETECTION (Highest Priority)
    // MESSAGE BUBBLES ARE THE PRIMARY INDICATOR - Check this FIRST
    if has_message_bubbles {
        tags.push("Messages".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Messages (bubbles detected): {} short lines, {} name prefixes, {} timestamps", 
                     short_lines, has_name_prefixes, lines_with_timestamps);
        }
        return tags; // Stop here - Messages takes priority
    } else if has_any_timestamp || 
       has_message_apps || 
       has_read_receipts ||
       (has_chat_words && (has_questions || has_conversation_indicators)) ||
       (has_questions && has_casual_greetings) ||
       (has_date_headers && has_any_timestamp) ||
       (has_emoji_like && has_questions) {
        tags.push("Messages".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Messages (secondary indicators)");
        }
        return tags; // Stop here - Messages takes priority
    }
    
    // STEP 2: CODE DETECTION (Only if not Messages)
    let code_keywords = ["function", "const", "let", "var", "class", "import", "export", "def", "return", "async", "await", "fn", "impl", "struct"];
    let code_symbols = ["{", "}", "=>", "->", "::", "()"];
    let has_code_keywords = code_keywords.iter().any(|kw| text_lower.contains(kw));
    let has_code_symbols = code_symbols.iter().any(|sym| text.contains(sym));
    let has_indentation = Regex::new(r"(?m)^    ").unwrap().is_match(text);
    let has_comments = text.contains("//") || text.contains("/*") || text.contains("#");
    
    if has_code_keywords && (has_code_symbols || has_indentation || has_comments) {
        tags.push("Code".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Code");
        }
        return tags; // Stop here
    }
    
    // STEP 3: DESIGN DETECTION
    let hex_pattern = Regex::new(r"#[0-9A-Fa-f]{6}").unwrap();
    let has_colors = hex_pattern.find_iter(text).count() > 0;
    let has_design_tools = ["figma", "sketch", "adobe", "photoshop", "illustrator"].iter().any(|tool| text_lower.contains(tool));
    let has_design_terms = ["px", "rem", "font", "color", "background", "border", "padding", "margin"].iter().any(|term| text_lower.contains(term));
    
    if has_colors || has_design_tools || (has_design_terms && text_lower.contains("design")) {
        tags.push("Design".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Design");
        }
        return tags; // Stop here
    }
    
    // STEP 4: RECEIPTS DETECTION
    let price_pattern = Regex::new(r"\$\d+\.\d{2}").unwrap();
    let has_prices = price_pattern.find_iter(text).count() > 0;
    let has_receipt_words = ["total", "subtotal", "tax", "receipt", "invoice", "paid", "order"].iter().any(|word| text_lower.contains(word));
    let date_pattern = Regex::new(r"\d{1,2}/\d{1,2}/\d{2,4}").unwrap();
    let has_dates = date_pattern.is_match(text);
    
    if has_prices && (has_receipt_words || has_dates) {
        tags.push("Receipts".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Receipts");
        }
        return tags; // Stop here
    }
    
    // STEP 5: BROWSER DETECTION
    let url_pattern = Regex::new(r"https?://[^\s]+").unwrap();
    let has_urls = url_pattern.find_iter(text).count() > 0;
    let has_www = text.contains("www.") || text.contains("http");
    
    // Browser UI elements
    let browser_ui = ["address bar", "bookmarks", "back", "forward", "refresh", "home", 
                      "chrome", "safari", "firefox", "edge", "brave", "opera",
                      "new tab", "close tab", "search", "omnibox", "url bar"];
    let has_browser_ui = browser_ui.iter().any(|ui| text_lower.contains(ui));
    
    // Navigation elements
    let has_nav_elements = text.contains("←") || text.contains("→") || 
                          text.contains("↻") || text.contains("⌂") ||
                          text_lower.contains("navigation") || text_lower.contains("menu");
    
    // Domain patterns (e.g., "google.com", "github.com")
    let domain_pattern = Regex::new(r"\b[a-z0-9-]+\.[a-z]{2,}\b").unwrap();
    let has_domains = domain_pattern.find_iter(&text_lower).count() > 2;
    
    // Check for browser-specific patterns
    let has_browser_patterns = text_lower.contains("://") || 
                              (has_urls && text.split_whitespace().count() > 20) ||
                              (has_domains && has_urls);
    
    if has_urls || has_www || has_browser_ui || has_nav_elements || has_browser_patterns {
        tags.push("Browser".to_string());
    }
    
    // TERMINAL DETECTION
    let has_prompts = text.contains("$ ") || text.contains("~ ") || text.contains("> ");
    let has_commands = ["cd ", "ls ", "git ", "npm ", "cargo ", "python ", "node "].iter().any(|cmd| text.contains(cmd));
    
    if has_prompts || has_commands {
        tags.push("Terminal".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Terminal");
        }
        return tags; // Stop here
    }
    
    // STEP 7: ERROR DETECTION
    let error_words = ["error", "exception", "failed", "panic", "segfault", "undefined", "traceback", "stack trace"];
    let has_errors = error_words.iter().any(|word| text_lower.contains(word));
    let has_stack_trace = (text.contains("at ") && text.contains(".js:")) || text.contains("Traceback");
    
    if has_errors || has_stack_trace {
        tags.push("Errors".to_string());
    }
    
    // STEP 8: DOCUMENTS DETECTION (Only if NOT Messages and no other tags)
    // IMPORTANT: Double-check that this is NOT a message
    let is_likely_message = has_any_timestamp || has_message_apps || has_read_receipts || 
                           has_chat_words || has_message_bubbles || has_date_headers ||
                           has_questions || has_casual_greetings;
    
    // Only proceed with Documents if we're confident it's NOT a message and no other tags
    if !is_likely_message && tags.is_empty() {
        let word_count = text.split_whitespace().count();
        let has_paragraphs = text.split("\n\n").count() > 2 || text.matches("\n").count() > 5;
        let has_sentences = text.matches('.').count() > 3 || text.matches('!').count() > 1 || text.matches('?').count() > 1;
        
        // Document-like patterns (formal writing)
        let document_patterns = ["chapter", "section", "paragraph", "article", "document", 
                                 "page", "heading", "title", "author", "date", "published",
                                 "abstract", "introduction", "conclusion", "references",
                                 "table of contents", "bibliography"];
        let has_doc_patterns = document_patterns.iter().any(|pattern| text_lower.contains(pattern));
        
        // Check for structured formatting (lists, numbered items)
        let numbered_list_pattern = Regex::new(r"(?m)^\d+\.\s").unwrap();
        let has_lists = text.contains("•") || text.contains("- ") || 
                       numbered_list_pattern.is_match(text) ||
                       text.matches("\n- ").count() > 2 || text.matches("\n• ").count() > 2;
        
        // Formal writing indicators (not casual/conversational)
        let has_formal_language = text_lower.contains("therefore") || text_lower.contains("however") ||
                                 text_lower.contains("furthermore") || text_lower.contains("moreover") ||
                                 text_lower.contains("in conclusion") || text_lower.contains("in summary");
        
        // Plain text document indicators - must be substantial AND structured
        let is_plain_text = word_count > 50 && // More words than typical messages
                           (has_paragraphs || has_sentences) && 
                           !has_urls && // Not a browser screenshot
                           !has_code_keywords && // Not code
                           !has_prompts && // Not terminal
                           (has_doc_patterns || has_lists || has_formal_language); // Must have document structure
        
        // If it looks like a document, add Documents tag
        if is_plain_text || (word_count > 100 && (has_doc_patterns || has_lists) && !has_questions) {
            tags.push("Documents".to_string());
            if debug {
                println!("[TAG] ✅ Tagged as Documents");
            }
            return tags; // Stop here
        }
    }
    
    // STEP 9: IMAGES/PHOTOS DETECTION (Fallback - very little or no text)
    // Check if this is primarily an image with minimal text
    let has_minimal_text = char_count < 50 || word_count < 10;
    
    // Image metadata or UI overlay text (short, non-descriptive)
    let is_ui_overlay = word_count < 20 && 
                       (text_lower.contains("screenshot") || 
                        text_lower.contains("image") ||
                        text_lower.contains("photo") ||
                        text_lower.contains("picture") ||
                        text_lower.contains("camera") ||
                        text_lower.contains("gallery") ||
                        text_lower.contains("album") ||
                        text_lower.contains("instagram") ||
                        text_lower.contains("snapchat") ||
                        text_lower.contains("filters"));
    
    // Random characters or OCR noise (not meaningful text)
    let is_ocr_noise = char_count > 0 && char_count < 30 && 
                      (text_trimmed.chars().filter(|c| c.is_alphanumeric()).count() < 15 ||
                       text_trimmed.matches(char::is_uppercase).count() > char_count / 2);
    
    // If there's very little text and no other meaningful tags, it's likely an image
    // Only tag as "Images" if we have some text (OCR picked up something) but it's minimal
    // OR if it's just UI overlay text
    if (has_minimal_text && tags.is_empty() && text_trimmed.len() > 0) || 
       (is_ui_overlay && tags.is_empty()) ||
       (is_ocr_noise && tags.is_empty()) {
        tags.push("Images".to_string());
        if debug {
            println!("[TAG] ✅ Tagged as Images (minimal text)");
        }
    }
    
    if debug && tags.is_empty() {
        println!("[TAG] ⚠️ No tags detected for text ({} words, {} chars)", word_count, char_count);
    }
    
    tags
}

fn extract_urls_and_emails(text: &str) -> (Vec<String>, Vec<String>) {
    let url_pattern = Regex::new(r"https?://[^\s]+").unwrap();
    let email_pattern = Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b").unwrap();
    
    let urls: Vec<String> = url_pattern.find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect();
    
    let emails: Vec<String> = email_pattern.find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect();
    
    (urls, emails)
}

fn compute_perceptual_hash(path: &Path) -> Result<Vec<u8>, String> {
    let img = image::open(path)
        .map_err(|e| format!("Failed to open image: {}", e))?;
    
    let hasher = HasherConfig::new()
        .hash_alg(HashAlg::Gradient)
        .hash_size(16, 16)
        .to_hasher();
    
    let hash = hasher.hash_image(&img);
    Ok(hash.as_bytes().to_vec())
}

fn hamming_distance(hash1: &[u8], hash2: &[u8]) -> u32 {
    hash1.iter()
        .zip(hash2.iter())
        .map(|(a, b)| (a ^ b).count_ones())
        .sum()
}

fn clean_ocr_text(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    
    // First fix common OCR character mistakes
    let text = fix_ocr_character_mistakes(text);
    
    // Store original (after character fixes) for safety checks
    let original_text = text.to_string();
    let original_len = text.len();
    let mut cleaned = text.to_string();
    
    // Remove timestamp patterns (e.g., "6:19 PM", "9:47 PM", "11:45 AM", "6:19", "09:47")
    // Use word boundaries to ensure we only match timestamps, not words containing numbers
    let timestamp_pattern = Regex::new(r"\b\d{1,2}:\d{2}(?:\s*(?:AM|PM|am|pm))?\b").unwrap();
    cleaned = timestamp_pattern.replace_all(&cleaned, " ").to_string(); // Replace with space, not empty string
    
    // Remove date + time patterns (e.g., "Jan 20 at 7:28 PM", "Jan 13 11:08:08 AM")
    let date_time_pattern = Regex::new(r"\b(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\s+\d{1,2}(?:\s+at\s+)?\d{1,2}:\d{2}(:\d{2})?(?:\s*(?:AM|PM|am|pm))?\b").unwrap();
    cleaned = date_time_pattern.replace_all(&cleaned, "").to_string();
    
    // Remove "Just now", "Today", "Yesterday", "moments ago", etc.
    let relative_time_pattern = Regex::new(r"\b(Just now|Today|Yesterday|This Week|This Month|moments? ago|\d+[smhd]\s+ago)\b").unwrap();
    cleaned = relative_time_pattern.replace_all(&cleaned, "").to_string();
    
    // Remove common UI elements (case insensitive)
    let ui_pattern = Regex::new(r"(?i)\b(Select all|Clear|Delete|Next|Prev|indexed|selected|RESULTS|result|of|Last:|Chronicle)\b").unwrap();
    cleaned = ui_pattern.replace_all(&cleaned, "").to_string();
    
    // Remove read receipts and status indicators
    let status_pattern = Regex::new(r"(?i)\b(Read|Delivered|Sending|Sent|✓|✔|✗|×|checkmark|read receipt)\b").unwrap();
    cleaned = status_pattern.replace_all(&cleaned, "").to_string();
    
    // Remove common messaging app UI elements
    let messaging_ui = Regex::new(r"(?i)\b(Search|Type a message|Send|Reply|Forward|Copy|Share|More|Options|Menu|Settings|Profile|Channel|DM|Direct Message|Group|Thread)\b").unwrap();
    cleaned = messaging_ui.replace_all(&cleaned, "").to_string();
    
    // Remove standalone time patterns like "1m", "5m", "2h" (message timestamps)
    let short_time = Regex::new(r"\b\d{1,2}[smhd]\b").unwrap();
    cleaned = short_time.replace_all(&cleaned, "").to_string();
    
    // Remove excessive whitespace and newlines
    // First, replace multiple newlines with single space
    let newline_pattern = Regex::new(r"\n\s*\n+").unwrap();
    cleaned = newline_pattern.replace_all(&cleaned, " ").to_string();
    
    // Replace multiple spaces with single space
    let space_pattern = Regex::new(r"\s+").unwrap();
    cleaned = space_pattern.replace_all(&cleaned, " ").to_string();
    
    // Remove leading/trailing whitespace
    cleaned = cleaned.trim().to_string();
    
    // Don't remove single character words - they might be important in messages
    // (e.g., "I", "a", "u" for "you", etc.)
    // Only remove if the entire text is mostly single characters (likely noise)
    
    // Count meaningful vs noise words
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let meaningful_count = words.iter().filter(|w| w.len() >= 2).count();
    let total_count = words.len();
    
    // Only filter single chars if they're the majority (likely OCR noise)
    if total_count > 0 && meaningful_count < total_count / 2 {
        // Too much noise, filter single chars
        let meaningful_words: Vec<&str> = words
            .iter()
            .filter(|word| {
                let w = word.trim();
                w.len() >= 2 || w.parse::<u32>().is_ok()
            })
            .copied()
            .collect();
        cleaned = meaningful_words.join(" ");
    }
    
    // Remove excessive isolated punctuation (but keep emojis and common punctuation)
    let isolated_punct = Regex::new(r"\s+[^\w\s]{3,}\s+").unwrap(); // Only remove 3+ char sequences
    cleaned = isolated_punct.replace_all(&cleaned, " ").to_string();
    
    // Final trim
    let final_cleaned = cleaned.trim().to_string();
    
    // Log cleaning results if significant cleaning occurred
    if original_len > 0 && final_cleaned.len() < original_len {
        let removed = original_len - final_cleaned.len();
        let percent_removed = (removed as f64 / original_len as f64) * 100.0;
        if percent_removed > 10.0 {
            println!("[CLEAN] Removed {} chars ({:.1}%) of noise/timestamps/UI elements", removed, percent_removed);
            if original_len < 200 {
                println!("[CLEAN] Before: {}", text);
                println!("[CLEAN] After:  {}", final_cleaned);
            } else {
                println!("[CLEAN] Before: {}...", text.chars().take(100).collect::<String>());
                println!("[CLEAN] After:  {}...", final_cleaned.chars().take(100).collect::<String>());
            }
        }
    }
    
    // SAFETY CHECK 1: Don't remove more than 70% of content
    if !final_cleaned.is_empty() && final_cleaned.len() < (original_len as f64 * 0.3) as usize {
        let percent_removed = (1.0 - (final_cleaned.len() as f64 / original_len as f64)) * 100.0;
        println!("[CLEAN] ⚠️ Cleaning too aggressive ({:.1}% removed), using original text", percent_removed);
        return original_text;
    }
    
    // SAFETY CHECK 2: Make sure we still have real words
    let real_words = final_cleaned.split_whitespace()
        .filter(|w| w.len() >= 3 && w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .count();
    
    let original_words = original_text.split_whitespace()
        .filter(|w| w.len() >= 3 && w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .count();
    
    if real_words == 0 && original_words > 0 {
        println!("[CLEAN] ⚠️ All real words removed, using original text");
        return original_text;
    }
    
    // SAFETY CHECK 3: Check specific important words
    let important_words = ["fights", "building", "lmao", "lmfao", "back", "done", "fuck", "shit"];
    check_word_preservation(&original_text, &final_cleaned, &important_words);
    
    final_cleaned
}

fn verify_tesseract() {
    println!("[OCR] ===== Verifying Tesseract Installation =====");
    
    match Tesseract::new(None, Some("eng")) {
        Ok(_) => {
            println!("[OCR] ✅ Tesseract initialized successfully");
            println!("[OCR] Language: English (eng)");
        }
        Err(e) => {
            eprintln!("[OCR] ❌ Tesseract initialization FAILED: {}", e);
            eprintln!("[OCR] Please ensure Tesseract is installed:");
            eprintln!("[OCR]   macOS: brew install tesseract");
            eprintln!("[OCR]   Linux: sudo apt-get install tesseract-ocr");
            eprintln!("[OCR]   Windows: Download from https://github.com/UB-Mannheim/tesseract/wiki");
        }
    }
    
    // Try to get Tesseract version info
    match Tesseract::new(None, Some("eng")) {
        Ok(tesseract) => {
            // Try to set a variable to verify it works
            match tesseract.set_variable("tessedit_pageseg_mode", "6") {
                Ok(_) => println!("[OCR] ✅ Tesseract configuration test passed"),
                Err(e) => println!("[OCR] ⚠️ Tesseract configuration warning: {:?}", e),
            }
        }
        Err(_) => {}
    }
    
    println!("[OCR] ============================================");
}

#[cfg(target_os = "macos")]
fn run_ocr_vision(path: &Path) -> Result<String, String> {
    use std::process::Command;
    
    let path_str = path
        .to_str()
        .ok_or_else(|| "Image path is not valid UTF-8".to_string())?;
    
    // Find vision_ocr.swift in multiple locations
    let mut script_locations = vec![
        PathBuf::from("vision_ocr.swift"),
        PathBuf::from("src-tauri/vision_ocr.swift"),
        PathBuf::from("../vision_ocr.swift"),
    ];
    
    // Add executable-relative paths
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            script_locations.push(parent.join("vision_ocr.swift"));
            script_locations.push(parent.join("../Resources/vision_ocr.swift"));
        }
    }
    
    // Add current directory relative path
    if let Ok(cwd) = std::env::current_dir() {
        script_locations.push(cwd.join("src-tauri/vision_ocr.swift"));
        script_locations.push(cwd.join("vision_ocr.swift"));
    }
    
    let script_path = script_locations
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| {
            println!("[OCR] ❌ Could not find vision_ocr.swift");
            println!("[OCR] Searched locations:");
            for loc in &script_locations {
                println!("[OCR]   - {} (exists: {})", loc.display(), loc.exists());
            }
            "vision_ocr.swift not found in any expected location".to_string()
        })?;
    
    println!("[OCR] 🍎 Using Vision Framework: {}", script_path.display());
    
    let output = Command::new("swift")
        .arg(script_path)
        .arg(path_str)
        .output()
        .map_err(|e| format!("Failed to execute swift: {}. Is Swift installed?", e))?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Vision OCR failed: {}", stderr));
    }
    
    let text = String::from_utf8(output.stdout)
        .map_err(|e| format!("Invalid UTF-8 from Vision: {}", e))?
        .trim()
        .to_string();
    
    if text.starts_with("ERROR:") {
        return Err(text);
    }
    
    println!("[OCR] ✅ Vision extracted {} chars", text.len());
    if text.len() < 200 {
        println!("[OCR] Vision text: {}", text);
    } else {
        println!("[OCR] Vision preview: {}...", text.chars().take(150).collect::<String>());
    }
    
    Ok(text)
}

#[cfg(not(target_os = "macos"))]
fn run_ocr_vision(_path: &Path) -> Result<String, String> {
    Err("Apple Vision Framework is only available on macOS".to_string())
}

fn combine_ocr_results(vision_text: Option<String>, tesseract_text: Option<String>) -> String {
    match (vision_text, tesseract_text) {
        (Some(v), Some(t)) => {
            println!("[OCR] Combining Vision ({} chars) + Tesseract ({} chars)", v.len(), t.len());
            
            // If one is significantly longer, prefer it
            if v.len() > t.len() * 2 {
                println!("[OCR] Using Vision (much longer)");
                return v;
            }
            if t.len() > v.len() * 2 {
                println!("[OCR] Using Tesseract (much longer)");
                return t;
            }
            
            // Otherwise, merge unique words from both
            let mut all_words = Vec::new();
            let mut seen = HashSet::new();
            
            // Prefer Vision words first (usually more accurate for UI)
            for word in v.split_whitespace() {
                let normalized = word.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string();
                if !normalized.is_empty() && seen.insert(normalized.clone()) {
                    all_words.push(word.to_string());
                }
            }
            
            // Add Tesseract words that Vision missed
            for word in t.split_whitespace() {
                let normalized = word.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string();
                if !normalized.is_empty() && seen.insert(normalized.clone()) {
                    all_words.push(word.to_string());
                }
            }
            
            let combined = all_words.join(" ");
            println!("[OCR] Combined result: {} chars (unique words from both engines)", combined.len());
            combined
        }
        (Some(v), None) => {
            println!("[OCR] Using Vision only ({} chars)", v.len());
            v
        }
        (None, Some(t)) => {
            println!("[OCR] Using Tesseract only ({} chars)", t.len());
            t
        }
        (None, None) => {
            println!("[OCR] ❌ Both Vision and Tesseract failed");
            String::new()
        }
    }
}

fn find_word_context(text: &str, word: &str, context_chars: usize) -> String {
    let lower = text.to_lowercase();
    if let Some(pos) = lower.find(&word.to_lowercase()) {
        let start = pos.saturating_sub(context_chars);
        let end = (pos + word.len() + context_chars).min(text.len());
        format!("...{}...", &text[start..end])
    } else {
        String::new()
    }
}

fn check_word_preservation(original: &str, cleaned: &str, important_words: &[&str]) {
    println!("[CLEAN] Checking word preservation:");
    for word in important_words {
        let in_original = original.to_lowercase().contains(word);
        let in_cleaned = cleaned.to_lowercase().contains(word);
        
        if in_original && !in_cleaned {
            println!("[CLEAN] ❌ WARNING: '{}' was REMOVED during cleaning!", word);
            println!("[CLEAN]    Original context: {}", find_word_context(original, word, 40));
        } else if in_original && in_cleaned {
            println!("[CLEAN] ✅ '{}' preserved", word);
        }
    }
}

fn run_ocr(path: &Path) -> Result<String, String> {
    let path_str = path
        .to_str()
        .ok_or_else(|| "Image path is not valid UTF-8".to_string())?;
    
    println!("[OCR] ===== Starting multi-engine OCR for: {} =====", path_str);
    
    // Verify file
    let metadata = fs::metadata(path)
        .map_err(|e| format!("File not accessible: {}", e))?;
    let file_size = metadata.len();
    println!("[OCR] File size: {} bytes", file_size);
    
    if file_size == 0 {
        return Err("File is empty".to_string());
    }
    
    // Get image info
    if let Ok(img) = image::open(path) {
        let (width, height) = img.dimensions();
        println!("[OCR] Image dimensions: {}x{} pixels", width, height);
    }
    
    let mut vision_result: Option<String> = None;
    let mut tesseract_result: Option<String> = None;
    
    // TRY VISION FRAMEWORK (macOS only)
    #[cfg(target_os = "macos")]
    {
        println!("[OCR] 🍎 Attempting Apple Vision Framework...");
        match run_ocr_vision(path) {
            Ok(text) if !text.trim().is_empty() => {
                println!("[OCR] ✅ Vision success: {} chars", text.len());
                vision_result = Some(text);
            }
            Ok(_) => {
                println!("[OCR] ⚠️ Vision returned empty text");
            }
            Err(e) => {
                println!("[OCR] ⚠️ Vision failed: {}", e);
            }
        }
    }
    
    // TRY TESSERACT (always run, even if Vision succeeded - we'll combine results)
    println!("[OCR] 🔄 Running Tesseract OCR...");
    
    let mut tesseract_paths = vec![path.to_path_buf()];
    
    // Add preprocessed version as fallback
    if let Ok(preprocessed) = preprocess_image(path) {
        println!("[OCR] ✅ Preprocessed image created");
        tesseract_paths.push(preprocessed);
    }
    
    match run_ocr_with_modes(path, &tesseract_paths) {
        Ok(text) if !text.trim().is_empty() => {
            println!("[OCR] ✅ Tesseract success: {} chars", text.len());
            tesseract_result = Some(text);
        }
        Ok(_) => {
            println!("[OCR] ⚠️ Tesseract returned empty text");
        }
        Err(e) => {
            println!("[OCR] ⚠️ Tesseract failed: {}", e);
        }
    }
    
    // COMBINE RESULTS FROM BOTH ENGINES
    let raw_combined = combine_ocr_results(vision_result, tesseract_result);
    
    // If both engines failed or returned empty, return empty string instead of error
    // This allows screenshots without text to still be saved and displayed
    if raw_combined.is_empty() {
        println!("[OCR] ⚠️ Both OCR engines failed or returned empty results - saving with empty text");
        return Ok(String::new());
    }
    
    println!("[OCR] Raw combined text: {} chars", raw_combined.len());
    if raw_combined.len() < 200 {
        println!("[OCR] Raw text: {}", raw_combined);
    } else {
        println!("[OCR] Raw preview: {}...", raw_combined.chars().take(150).collect::<String>());
    }
    
    // APPLY CLEANING
    let cleaned = clean_ocr_text(&raw_combined);
    
    println!("[OCR] After cleaning: {} chars (removed {} chars)", 
        cleaned.len(), raw_combined.len().saturating_sub(cleaned.len()));
    
    // Safety check: if cleaning removed everything but we had content, use raw
    if cleaned.is_empty() && raw_combined.len() > 10 {
        println!("[OCR] ⚠️ Cleaning removed everything! Using raw text instead");
        return Ok(raw_combined);
    }
    
    if cleaned.len() < 100 {
        println!("[OCR] Final text: {}", cleaned);
    } else {
        println!("[OCR] Final preview: {}...", cleaned.chars().take(100).collect::<String>());
    }
    
    println!("[OCR] ===== OCR Complete =====");
    Ok(cleaned)
}

fn run_ocr_with_modes(original_path: &Path, image_paths: &[PathBuf]) -> Result<String, String> {
    // Try multiple PSM modes optimized for messaging apps and screenshots
    // PSM 4 is particularly good for chat/messaging apps (single column, vertical text flow)
    // PSM 11 is good for sparse text (like chat bubbles with gaps)
    // PSM 6 works well for UI screenshots with uniform blocks
    let psm_modes = vec![
        ("4", "Single column (best for chat/messaging apps - vertical message flow)"),
        ("11", "Sparse text (good for chat bubbles with gaps)"),
        ("6", "Uniform block of text (good for UI screenshots)"),
        ("3", "Fully automatic page segmentation"),
        ("7", "Single text line"),
        ("13", "Raw line (treat image as single text line)"),
    ];
    
    let mut last_error = None;
    let mut best_result = String::new();
    let mut best_length = 0;
    let mut best_source = "";
    
    // Try each image path (original and preprocessed)
    for image_path in image_paths {
        let image_type = if image_path == original_path { "original" } else { "preprocessed" };
        println!("[OCR] Trying {} image", image_type);
        
        // Try each PSM mode
        for (psm, desc) in &psm_modes {
            match run_ocr_with_psm(image_path, psm, desc) {
                Ok(text) => {
                    let text_len = text.trim().len();
                    println!("[OCR] {} image + PSM {} success: {} characters", image_type, psm, text_len);
                    
                    // Log a preview of the extracted text for debugging
                    if text_len > 0 {
                        let preview = text.chars().take(150).collect::<String>();
                        println!("[OCR] Text preview ({} chars): {}", text_len, preview);
                        
                        // Check if text looks like messaging content (has timestamps, read receipts, etc.)
                        let has_messaging_patterns = text.contains("PM") || text.contains("AM") || 
                            text.contains("Read") || text.contains("Delivered") ||
                            text.contains("Today") || text.contains("Yesterday") ||
                            text.contains("Just now") || text.matches(":").count() > 3; // Multiple timestamps
                        if has_messaging_patterns {
                            println!("[OCR] ⚠️ Detected messaging app patterns - will apply aggressive cleaning");
                        }
                    }
                    
                    // Use the result with the most text
                    if text_len > best_length {
                        best_result = text;
                        best_length = text_len;
                        best_source = image_type;
                    }
                    
                    // If we got a good result (more than 20 chars), use it immediately
                    if text_len > 20 {
                        println!("[OCR] ✅ Using result from {} image + PSM {} ({} chars)", image_type, psm, text_len);
                        
                        // Clean up temp files
                        for temp_path in image_paths {
                            if temp_path != original_path && temp_path.exists() {
                                let _ = fs::remove_file(temp_path);
                            }
                        }
                        
                        // Apply text cleaning
                        let cleaned = clean_ocr_text(&best_result);
                        let cleaned_len = cleaned.len();
                        if cleaned_len < best_result.len() {
                            println!("[OCR] After cleaning: {} chars (removed {} chars of noise)", 
                                cleaned_len, best_result.len() - cleaned_len);
                            println!("[OCR] Cleaned preview: {}", cleaned.chars().take(100).collect::<String>());
                        }
                        
                        return Ok(cleaned);
                    }
                }
                Err(e) => {
                    println!("[OCR] {} image + PSM {} failed: {}", image_type, psm, e);
                    if last_error.is_none() {
                        last_error = Some(e);
                    }
                }
            }
        }
    }
    
    // Clean up temp files
    for temp_path in image_paths {
        if temp_path != original_path && temp_path.exists() {
            let _ = fs::remove_file(temp_path);
        }
    }
    
    // If we got any result, return it (after cleaning)
    if best_length > 0 {
        println!("[OCR] Using best result from {} image: {} characters", best_source, best_length);
        
        // Apply text cleaning
        let cleaned = clean_ocr_text(&best_result);
        let cleaned_len = cleaned.len();
        if cleaned_len < best_result.len() {
            println!("[OCR] After cleaning: {} chars (removed {} chars of noise)", 
                cleaned_len, best_result.len() - cleaned_len);
        }
        
        return Ok(cleaned);
    }
    
    // If all failed, return detailed error
    let error_msg = format!(
        "All OCR attempts failed. Last error: {}. File: {}",
        last_error.unwrap_or_else(|| "Unknown error".to_string()),
        original_path.to_str().unwrap_or("unknown")
    );
    eprintln!("[OCR] ❌ {}", error_msg);
    Err(error_msg)
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

// Database functions
fn get_db_path(app: &AppHandle) -> PathBuf {
    let app_data_dir = app.path().app_data_dir().expect("Failed to get app data directory");
    fs::create_dir_all(&app_data_dir).expect("Failed to create app data directory");
    app_data_dir.join("chronicle.db")
}

fn init_database(app: &AppHandle) -> SqlResult<Connection> {
    let db_path = get_db_path(app);
    let conn = Connection::open(&db_path)?;
    
    conn.execute(
        "CREATE TABLE IF NOT EXISTS entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            processed_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            tags TEXT,
            urls TEXT,
            emails TEXT,
            perceptual_hash BLOB
        )",
        [],
    )?;
    
    // Add new columns if they don't exist (for existing databases)
    let columns_to_add = vec![
        ("tags", "TEXT"),
        ("urls", "TEXT"),
        ("emails", "TEXT"),
        ("perceptual_hash", "BLOB"),
    ];
    
    for (col_name, col_type) in columns_to_add {
        let check_sql = format!("SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='{}'", col_name);
        let count: i64 = conn.query_row(&check_sql, [], |row| row.get(0)).unwrap_or(0);
        if count == 0 {
            let alter_sql = format!("ALTER TABLE entries ADD COLUMN {} {}", col_name, col_type);
            if let Err(e) = conn.execute(&alter_sql, []) {
                eprintln!("[DB] Warning: Failed to add column {}: {}", col_name, e);
            } else {
                println!("[DB] Added column: {}", col_name);
            }
        }
    }
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_path ON entries(path)",
        [],
    )?;
    
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_created_at ON entries(created_at)",
        [],
    )?;
    
    // Only create index on tags if the column exists
    let tags_exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('entries') WHERE name='tags'",
        [],
        |row| row.get(0)
    ).unwrap_or(0);
    
    if tags_exists > 0 {
        if let Err(e) = conn.execute("CREATE INDEX IF NOT EXISTS idx_tags ON entries(tags)", []) {
            eprintln!("[DB] Warning: Failed to create tags index: {}", e);
        }
    }
    
    println!("[DB] Database initialized at: {}", db_path.display());
    Ok(conn)
}

fn save_entry_to_db(app: &AppHandle, path: &str, text: &str, created_at: &str) -> SqlResult<()> {
    let conn = init_database(app)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let now_str = now.to_string();
    
    // Auto-detect tags
    // If text is empty or very minimal, it's likely an image/photo
    let tags = if text.trim().is_empty() || text.trim().len() < 10 {
        vec!["Images".to_string()]
    } else {
        detect_collections(text)
    };
    let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
    
    // Extract URLs and emails
    let (urls, emails) = extract_urls_and_emails(text);
    let urls_json = serde_json::to_string(&urls).unwrap_or_else(|_| "[]".to_string());
    let emails_json = serde_json::to_string(&emails).unwrap_or_else(|_| "[]".to_string());
    
    // Compute perceptual hash for similarity detection
    let perceptual_hash = compute_perceptual_hash(Path::new(path)).ok();
    
    conn.execute(
        "INSERT OR REPLACE INTO entries (path, text, created_at, processed_at, updated_at, tags, urls, emails, perceptual_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![path, text, created_at, now_str, now_str, tags_json, urls_json, emails_json, perceptual_hash],
    )?;
    
    if !tags.is_empty() {
        println!("[DB] ✅ Saved entry: {} ({} chars) - Tags: {:?}", path, text.len(), tags);
    } else {
        println!("[DB] ✅ Saved entry: {} ({} chars)", path, text.len());
    }
    
    if !urls.is_empty() {
        println!("[DB]   Found {} URLs: {:?}", urls.len(), urls);
    }
    if !emails.is_empty() {
        println!("[DB]   Found {} emails: {:?}", emails.len(), emails);
    }
    
    Ok(())
}

#[derive(Serialize)]
struct DbEntry {
    path: String,
    text: String,
    at: String,
    tags: Option<String>,
    urls: Option<String>,
    emails: Option<String>,
}

fn load_all_entries_from_db(app: &AppHandle) -> SqlResult<Vec<DbEntry>> {
    let conn = init_database(app)?;
    let mut stmt = conn.prepare("SELECT path, text, created_at, tags, urls, emails FROM entries ORDER BY created_at DESC")?;
    let rows = stmt.query_map([], |row| {
        Ok(DbEntry {
            path: row.get(0)?,
            text: row.get(1)?,
            at: row.get(2)?,
            tags: row.get(3).ok(),
            urls: row.get(4).ok(),
            emails: row.get(5).ok(),
        })
    })?;
    
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    
    println!("[DB] ✅ Loaded {} entries from database", entries.len());
    Ok(entries)
}

fn delete_entry_from_db(app: &AppHandle, path: &str) -> SqlResult<()> {
    let conn = init_database(app)?;
    conn.execute("DELETE FROM entries WHERE path = ?1", rusqlite::params![path])?;
    println!("[DB] ✅ Deleted entry: {}", path);
    Ok(())
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
            
            // Log detailed results
            if trimmed.is_empty() {
                eprintln!("[OCR] ⚠️ WARNING: OCR returned EMPTY text for {}", path.display());
                eprintln!("[OCR] This could indicate:");
                eprintln!("[OCR]   1. Image has no readable text");
                eprintln!("[OCR]   2. OCR configuration needs adjustment");
                eprintln!("[OCR]   3. Image quality is too poor");
            } else {
                let char_count = trimmed.len();
                let word_count = trimmed.split_whitespace().count();
                println!("[OCR] ✅ Successfully extracted {} characters, {} words from {}", 
                    char_count, word_count, path.display());
                
                // Check for specific words that user is looking for
                let important_words = vec!["fights", "building", "lmao", "lmfao"];
                let text_lower = trimmed.to_lowercase();
                for word in &important_words {
                    if text_lower.contains(word) {
                        println!("[OCR] ✅ Found '{}' in extracted text", word);
                        // Show context around the word
                        if let Some(pos) = text_lower.find(word) {
                            let start = pos.saturating_sub(20);
                            let end = (pos + word.len() + 20).min(trimmed.len());
                            println!("[OCR] Context: ...{}...", &trimmed[start..end]);
                        }
                    } else {
                        println!("[OCR] ⚠️ '{}' NOT found in extracted text", word);
                    }
                }
                
                if char_count < 100 {
                    println!("[OCR] Full text: {}", trimmed);
                } else {
                    println!("[OCR] Text preview: {}...", trimmed.chars().take(100).collect::<String>());
                }
            }
            
            let final_path = match rename_with_text(&path, &trimmed) {
                Ok(new_path) => {
                    // Mark both original and renamed paths as known to prevent duplicate processing
                    {
                        let mut guard = known_map.lock().unwrap();
                        guard.insert(path.clone()); // Original path
                        guard.insert(new_path.clone()); // Renamed path
                        println!("[RENAME] Marked both paths as known: {} -> {}", path.display(), new_path.display());
                    }
                    remember_ignore(&ignore_map, &new_path);
                    remember_ignore(&ignore_map, &path); // Also ignore original path
                    new_path
                }
                Err(error) => {
                    eprintln!("Rename failed for {}: {error}", path.display());
                    // Still mark original as known even if rename failed
                    {
                        let mut guard = known_map.lock().unwrap();
                        guard.insert(path.clone());
                    }
                    path.clone()
                }
            };
            remember_ignore(&ignore_map, &path);
            
            // Get creation date from ORIGINAL path (before rename) - this is what we'll match against on startup
            let created_at = get_file_created_at(&path)
                .unwrap_or_else(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .to_string()
                });
            
            // Save to database with final_path (renamed path) but original creation date
            if let Err(e) = save_entry_to_db(&app, &final_path.to_string_lossy(), &trimmed, &created_at) {
                eprintln!("[DB] ⚠️ Failed to save entry to database: {}", e);
            }
            
            // Always emit the text, even if empty (so frontend knows OCR ran)
            // Emit with final_path (renamed path if successful, original if not)
            emit_status(&app, "idle", Some(&final_path), None, Some(trimmed));
        }
        Err(error) => {
            eprintln!("[OCR] ❌ OCR failed for {}: {error}", path.display());
            eprintln!("[OCR] Error details: {}", error);
            
            // Still save the entry to database even if OCR failed
            // This allows the screenshot to appear in the UI, even without text
            let created_at = get_file_created_at(&path)
                .unwrap_or_else(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        .to_string()
                });
            
            // Save with empty text - user can still see the image
            if let Err(e) = save_entry_to_db(&app, &path.to_string_lossy(), "", &created_at) {
                eprintln!("[DB] ⚠️ Failed to save entry to database after OCR failure: {}", e);
            } else {
                println!("[DB] ✅ Saved entry (no OCR text) to database: {}", path.display());
            }
            
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
    // Handle Remove events (happens when files are renamed)
    if matches!(event.kind, EventKind::Remove(_)) {
        // When a file is removed, it's likely a rename - mark it as known to prevent re-processing
        for path in event.paths.iter() {
            let mut guard = known_map.lock().unwrap();
            guard.insert(path.clone());
            println!("[WATCHER] File removed (likely renamed): {}, marking as known", path.display());
        }
        return;
    }
    
    // Only process Create and Modify events
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
                    
                    // Log detailed results
                    if trimmed.is_empty() {
                        eprintln!("[OCR] ⚠️ WARNING: OCR returned EMPTY text for {}", path.display());
                    } else {
                        let char_count = trimmed.len();
                        let word_count = trimmed.split_whitespace().count();
                        println!("[OCR] ✅ Extracted {} chars, {} words from {}", 
                            char_count, word_count, path.display());
                    }
                    
                    // Get creation date from original path (before any potential rename)
                    let created_at = get_file_created_at(&path)
                        .unwrap_or_else(|| {
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .unwrap()
                                .as_secs()
                                .to_string()
                        });
                    
                    // Save to database (using original path since process_existing_screenshots doesn't rename)
                    if let Err(e) = save_entry_to_db(&app, &path.to_string_lossy(), &trimmed, &created_at) {
                        eprintln!("[DB] ⚠️ Failed to save entry to database: {}", e);
                    }
                    
                    // Always emit the text, even if empty
                    emit_status(&app, "idle", Some(&path), None, Some(trimmed));
                }
                Err(error) => {
                    eprintln!("[OCR] ❌ OCR failed for {}: {error}", path.display());
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
        
        // Check database to see which screenshots are already indexed
        // Since files get renamed, we match by creation date (most reliable)
        let mut already_indexed_dates = HashSet::new();
        let db_entry_count = if let Ok(db_entries) = load_all_entries_from_db(&app) {
            let count = db_entries.len();
            for entry in db_entries {
                // Parse creation date from database (stored as milliseconds timestamp string)
                if let Ok(created_timestamp) = entry.at.parse::<u64>() {
                    // Round to nearest second for matching tolerance
                    // This handles small timestamp differences
                    let rounded = (created_timestamp / 1000) * 1000;
                    already_indexed_dates.insert(rounded);
                }
            }
            count
        } else {
            0
        };
        
        println!("[WATCHER] Found {} entries in database, checking {} existing screenshots", 
            db_entry_count, existing.len());
        
        // Only process screenshots that aren't already in the database
        let mut skipped = 0;
        let to_process: Vec<PathBuf> = existing.iter()
            .filter(|path| {
                // Get file creation date
                let Some(created_at_str) = get_file_created_at(path) else {
                    // Can't get creation date, process it to be safe
                    return true;
                };
                
                let Ok(created_timestamp) = created_at_str.parse::<u64>() else {
                    // Can't parse timestamp, process it
                    return true;
                };
                
                // Round to nearest second for matching (same as database)
                let rounded = (created_timestamp / 1000) * 1000;
                
                // Check if this creation date is already indexed
                if already_indexed_dates.contains(&rounded) {
                    skipped += 1;
                    println!("[WATCHER] ✅ Skipping {} - already indexed (creation date: {})", 
                        path.file_name().and_then(|n| n.to_str()).unwrap_or("unknown"),
                        created_timestamp);
                    return false;
                }
                
                true // Not indexed, process it
            })
            .cloned()
            .collect();
        
        println!("[WATCHER] Skipping {} already indexed, processing {} new screenshots", skipped, to_process.len());
        
        {
            let mut guard = known_map.lock().unwrap();
            // Mark all existing screenshots as known (whether we process them or not)
            for path in &existing {
                guard.insert(path.clone());
            }
        }
        
        if !to_process.is_empty() {
            println!("[WATCHER] Processing {} new screenshots (skipping {} already indexed)", 
                to_process.len(), existing.len() - to_process.len());
            process_existing_screenshots(app.clone(), to_process);
        } else {
            println!("[WATCHER] All {} existing screenshots already indexed, skipping processing", existing.len());
            // Emit batch progress to indicate we're done
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
                Ok(event) => handle_event(&app, event, &debounce_map, &ignore_map, &known_map),
                Err(error) => eprintln!("Watch error: {error}"),
            }
        }
    });
}

#[tauri::command]
fn delete_files(app: AppHandle, paths: Vec<String>) -> Result<DeleteResult, String> {
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
            // Also remove from database
            let _ = delete_entry_from_db(&app, &path_str);
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
                // Also remove from database
                let _ = delete_entry_from_db(&app, &path_str);
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

#[tauri::command]
#[cfg(target_os = "macos")]
fn copy_image_to_clipboard(path: String) -> Result<(), String> {
    use std::process::Command;
    
    let path = PathBuf::from(&path);
    if !path.exists() {
        return Err(format!("File does not exist: {}", path.display()));
    }
    
    let path_str = path.to_str().ok_or("Invalid path")?;
    
    // Use macOS osascript to copy image to clipboard
    // This is more reliable than web clipboard APIs in Tauri
    let script = format!(
        r#"set the clipboard to (read file POSIX file "{}" as «class PNGf»)"#,
        path_str.replace('"', "\\\"")
    );
    
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("Failed to execute osascript: {}", e))?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("Failed to copy image: {}\nStdout: {}", stderr, stdout));
    }
    
    println!("[COPY] ✅ Image copied to clipboard via macOS osascript");
    Ok(())
}

#[tauri::command]
#[cfg(not(target_os = "macos"))]
fn copy_image_to_clipboard(_path: String) -> Result<(), String> {
    Err("Image clipboard copy is only supported on macOS".to_string())
}

#[tauri::command]
fn load_all_entries(app: AppHandle) -> Result<Vec<DbEntry>, String> {
    load_all_entries_from_db(&app)
        .map_err(|e| format!("Failed to load entries: {}", e))
}

#[tauri::command]
fn find_similar_screenshots(app: AppHandle, threshold: Option<u32>) -> Result<Vec<Vec<String>>, String> {
    let conn = init_database(&app)
        .map_err(|e| format!("DB error: {}", e))?;
    
    let mut stmt = conn.prepare("SELECT path, perceptual_hash FROM entries WHERE perceptual_hash IS NOT NULL")
        .map_err(|e| format!("Query error: {}", e))?;
    
    let entries: Vec<(String, Vec<u8>)> = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?))
    })
    .map_err(|e| format!("Query map error: {}", e))?
    .filter_map(|r| r.ok())
    .collect();
    
    let threshold = threshold.unwrap_or(10); // Default threshold
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut assigned = vec![false; entries.len()];
    
    for i in 0..entries.len() {
        if assigned[i] {
            continue;
        }
        
        let mut group = vec![i];
        assigned[i] = true;
        
        for j in (i + 1)..entries.len() {
            if assigned[j] {
                continue;
            }
            
            let distance = hamming_distance(&entries[i].1, &entries[j].1);
            if distance <= threshold {
                group.push(j);
                assigned[j] = true;
            }
        }
        
        if group.len() > 1 {
            groups.push(group.into_iter().map(|idx| entries[idx].0.clone()).collect());
        }
    }
    
    println!("[SIMILARITY] Found {} groups of similar screenshots", groups.len());
    Ok(groups)
}

#[tauri::command]
fn open_quick_search(app: AppHandle) -> Result<(), String> {
    // Check if quick search window already exists
    if let Some(window) = app.get_webview_window("quick-search") {
        window.show().map_err(|e| format!("Failed to show window: {}", e))?;
        window.set_focus().map_err(|e| format!("Failed to focus window: {}", e))?;
        return Ok(());
    }
    
    // Create new quick search window
    // Use dev URL in development, app URL in production
    #[cfg(debug_assertions)]
    let url = WebviewUrl::External("http://localhost:1420/quick-search.html".parse().map_err(|e| format!("Invalid URL: {}", e))?);
    #[cfg(not(debug_assertions))]
    let url = WebviewUrl::App("quick-search.html".into());
    
    let window = WebviewWindowBuilder::new(
        &app,
        "quick-search",
        url
    )
    .title("Quick Search")
    .inner_size(600.0, 500.0)
    .resizable(false)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .build()
    .map_err(|e| format!("Failed to create window: {}", e))?;
    
    // Center the window on screen
    if let Err(e) = window.center() {
        eprintln!("Failed to center window: {}", e);
    }
    
    window.show().map_err(|e| format!("Failed to show window: {}", e))?;
    window.set_focus().map_err(|e| format!("Failed to focus window: {}", e))?;
    
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        // Check if it's Cmd+Shift+F (or Ctrl+Shift+F on Windows/Linux)
                        #[cfg(target_os = "macos")]
                        let is_cmd_shift_f = shortcut.matches(Modifiers::META | Modifiers::SHIFT, Code::KeyF);
                        #[cfg(not(target_os = "macos"))]
                        let is_cmd_shift_f = shortcut.matches(Modifiers::CONTROL | Modifiers::SHIFT, Code::KeyF);
                        
                        if is_cmd_shift_f {
                            let app_handle = app.app_handle().clone();
                            if let Err(e) = open_quick_search(app_handle) {
                                eprintln!("Failed to open quick search: {}", e);
                            }
                        }
                    }
                })
                .build()
        )
        .invoke_handler(tauri::generate_handler![
            delete_files,
            copy_image_to_clipboard,
            load_all_entries,
            find_similar_screenshots,
            open_quick_search,
            reprocess_all_tags,
            compute_missing_hashes
        ])
        .setup(|app| {
            // Verify Tesseract on startup
            verify_tesseract();
            start_watcher(app.app_handle().clone());
            
            // Register the global shortcut
            let app_handle = app.app_handle().clone();
            match app_handle.global_shortcut().register("CommandOrControl+Shift+F") {
                Ok(_) => println!("[SHORTCUT] ✅ Registered Cmd+Shift+F for quick search"),
                Err(e) => eprintln!("[SHORTCUT] ❌ Failed to register global shortcut: {}", e),
            }
            
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
fn reprocess_all_tags(app: AppHandle) -> Result<usize, String> {
    let conn = init_database(&app)
        .map_err(|e| format!("DB error: {}", e))?;
    
    // Get ALL entries to reprocess with improved detection logic
    let mut stmt = conn.prepare("SELECT path, text FROM entries")
        .map_err(|e| format!("Query error: {}", e))?;
    
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).map_err(|e| format!("Query map error: {}", e))?;
    
    let mut updated = 0;
    let mut with_tags = 0;
    let mut without_tags = 0;
    
    for row in rows {
        let (path, text) = row.map_err(|e| format!("Row error: {}", e))?;
        
        // Detect tags from text using improved detection logic
        let tags = detect_collections(&text);
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
        
        // Update the entry with new tags (always update, even if tags changed)
        if let Err(e) = conn.execute(
            "UPDATE entries SET tags = ?1 WHERE path = ?2",
            rusqlite::params![tags_json, path]
        ) {
            eprintln!("[REPROCESS] Failed to update {}: {}", path, e);
        } else {
            updated += 1;
            if !tags.is_empty() {
                with_tags += 1;
                println!("[REPROCESS] ✅ Updated {}: {:?}", path, tags);
            } else {
                without_tags += 1;
            }
        }
    }
    
    println!("[REPROCESS] ✅ Reprocessed {} entries total ({} with tags, {} without tags)", 
             updated, with_tags, without_tags);
    Ok(updated)
}

#[tauri::command]
fn compute_missing_hashes(app: AppHandle) -> Result<usize, String> {
    let conn = init_database(&app)
        .map_err(|e| format!("DB error: {}", e))?;
    
    // Get all entries without perceptual hashes
    let mut stmt = conn.prepare("SELECT path FROM entries WHERE perceptual_hash IS NULL")
        .map_err(|e| format!("Query error: {}", e))?;
    
    let rows = stmt.query_map([], |row| {
        Ok(row.get::<_, String>(0)?)
    }).map_err(|e| format!("Query map error: {}", e))?;
    
    let mut computed = 0;
    for row in rows {
        let path_str = row.map_err(|e| format!("Row error: {}", e))?;
        let path = Path::new(&path_str);
        
        // Compute perceptual hash
        match compute_perceptual_hash(path) {
            Ok(hash_bytes) => {
                // Update the entry with the hash
                if let Err(e) = conn.execute(
                    "UPDATE entries SET perceptual_hash = ?1 WHERE path = ?2",
                    rusqlite::params![hash_bytes, path_str]
                ) {
                    eprintln!("[HASH] Failed to update {}: {}", path_str, e);
                } else {
                    computed += 1;
                    if computed % 10 == 0 {
                        println!("[HASH] Computed {} hashes...", computed);
                    }
                }
            }
            Err(e) => {
                eprintln!("[HASH] Failed to compute hash for {}: {}", path_str, e);
            }
        }
    }
    
    println!("[HASH] ✅ Computed {} perceptual hashes", computed);
    Ok(computed)
}
