# Chronicle

A powerful screenshot indexer and search tool built with Tauri and React. Chronicle automatically indexes your screenshots, extracts text using OCR, and lets you search through them instantly.

## Features

- 🔍 **Full-Text Search**: Search through screenshots by their extracted text content
- 📸 **Automatic Indexing**: Automatically watches and indexes screenshots from your Desktop and Screenshots folders
- 🎯 **OCR Text Extraction**: Uses Apple Vision Framework (macOS) or Tesseract OCR with optimized settings for screenshot text extraction, especially messaging apps
- 📅 **Date Grouping**: View screenshots organized by date (Today, Yesterday, This Week, etc.)
- 🗑️ **Bulk Operations**: Select and delete multiple screenshots at once
- 🎨 **Modern UI**: Dark-themed interface inspired by Raycast
- ⚡ **Fast & Lightweight**: Built with Tauri for native performance

## How It Works

1. Chronicle watches your `~/Desktop` and `~/Pictures/Screenshots` folders
2. When new screenshots are detected, they're automatically processed:
   - OCR extracts all text from the image
   - Screenshots are optionally renamed with a slugified version of the extracted text
   - File metadata (creation date) is captured
3. All screenshots are indexed and searchable by their text content
4. Search results show thumbnails with creation dates

## Requirements

- macOS (tested on macOS)
- Tesseract OCR installed on your system

### Installing Tesseract

**macOS (using Homebrew):**
```bash
brew install tesseract
```

**Linux:**
```bash
sudo apt-get install tesseract-ocr  # Debian/Ubuntu
sudo yum install tesseract          # RHEL/CentOS
```

**Windows:**
Download from [GitHub](https://github.com/UB-Mannheim/tesseract/wiki)

## Development

### Prerequisites

- Node.js (v18 or later)
- Rust (latest stable)
- Tesseract OCR

### Setup

1. Clone the repository:
```bash
git clone <repository-url>
cd screenshot-indexer
```

2. Install dependencies:
```bash
npm install
```

3. Run in development mode:
```bash
npm run tauri dev
```

### Building

Build for production:
```bash
npm run tauri build
```

## Project Structure

```
screenshot-indexer/
├── src/                    # React frontend
│   ├── App.jsx            # Main application component
│   └── index.css          # Styles
├── src-tauri/             # Rust backend
│   ├── src/
│   │   ├── lib.rs         # Core logic (file watching, OCR, indexing)
│   │   └── main.rs        # Entry point
│   └── Cargo.toml         # Rust dependencies
└── package.json            # Node.js dependencies
```

## Configuration

The app watches these directories by default:
- `~/Desktop`
- `~/Pictures/Screenshots`

You can modify the watch directories in `src-tauri/src/lib.rs` in the `resolve_watch_dirs()` function.

## OCR Settings

**macOS**: Chronicle uses Apple Vision Framework as the primary OCR engine, which provides superior accuracy for messaging app screenshots. Falls back to Tesseract if Vision is unavailable.

**Tesseract Settings** (used as fallback on macOS or primary on other platforms):
- **Page Segmentation Mode 4**: Single column mode (best for chat/messaging apps)
- **Page Segmentation Mode 11**: Sparse text mode (good for chat bubbles)
- **OCR Engine Mode 1**: Uses LSTM neural nets for better accuracy
- **Dictionary disabled**: Better recognition of slang, typos, and UI text
- **Post-processing**: Aggressive text cleaning to remove timestamps, UI elements, and metadata

## Keyboard Shortcuts

- `Arrow Keys`: Navigate between screenshots
- `Enter`: Open full-screen image viewer
- `Spacebar`: Preview screenshot
- `Cmd/Ctrl + A`: Select all filtered results
- `Escape`: Close image viewer

## License

[Add your license here]

## Contributing

[Add contribution guidelines if applicable]
