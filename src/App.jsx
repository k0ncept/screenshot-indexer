import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";

function App() {
  const [status, setStatus] = useState("processing");
  const [lastPath, setLastPath] = useState("");
  const [lastError, setLastError] = useState("");
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState([]);
  const [selectedPath, setSelectedPath] = useState("");
  const [selectedPaths, setSelectedPaths] = useState([]);
  const [viewerPath, setViewerPath] = useState("");
  const [batchProgress, setBatchProgress] = useState({
    total: 0,
    completed: 0,
    percent: 0,
    etaSeconds: 0,
    inProgress: false,
  });

  const normalizePath = useCallback((path) => {
    if (!path) return "";
    return String(path)
      .trim()
      .replace(/\/+/g, "/")
      .replace(/\/$/, "")
      .toLowerCase();
  }, []);

  const displayPath = useCallback((path) => {
    if (!path) {
      return "";
    }
    return path.replace("/Users/nicholas/", "/Users/k0ncept/");
  }, []);

  const formatDate = useCallback((dateString) => {
    const date = new Date(dateString);
    const now = new Date();
    const diffMs = now - date;
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) return "Just now";
    if (diffMins < 60) return `${diffMins} minute${diffMins === 1 ? "" : "s"} ago`;
    if (diffHours < 24) return `${diffHours} hour${diffHours === 1 ? "" : "s"} ago`;
    if (diffDays === 1) return "Yesterday";
    if (diffDays < 7) return `${diffDays} days ago`;
    return date.toLocaleDateString("en-US", { month: "short", day: "numeric", year: date.getFullYear() !== now.getFullYear() ? "numeric" : undefined });
  }, []);

  const formatDateTime = useCallback((dateString) => {
    const date = new Date(dateString);
    return date.toLocaleDateString("en-US", { 
      month: "short", 
      day: "numeric",
      hour: "numeric",
      minute: "2-digit",
      hour12: true
    });
  }, []);

  const getDateGroup = useCallback((dateString) => {
    const date = new Date(dateString);
    const now = new Date();
    const diffDays = Math.floor((now - date) / 86400000);
    
    if (diffDays === 0) return "Today";
    if (diffDays === 1) return "Yesterday";
    if (diffDays < 7) return "This Week";
    if (diffDays < 30) return "This Month";
    return date.toLocaleDateString("en-US", { month: "long", year: "numeric" });
  }, []);

  const isOcrTextValid = useCallback((text) => {
    if (!text || text.trim().length < 2) return false;
    const trimmed = text.trim();
    const words = trimmed.split(/\s+/).filter((w) => w.length > 0);
    if (words.length === 0) return false;
    const validChars = trimmed.match(/[a-zA-Z0-9]/g) || [];
    const totalChars = trimmed.length;
    if (totalChars === 0) return false;
    const validRatio = validChars.length / totalChars;
    if (validRatio < 0.2) return false;
    const avgWordLength = words.reduce((sum, w) => sum + w.length, 0) / words.length;
    if (avgWordLength > 25) return false;
    if (words.length === 1 && words[0].length < 2) return false;
    const hasReasonableContent = words.some((w) => w.length >= 2 && /[a-zA-Z]/.test(w));
    return hasReasonableContent;
  }, []);

  const getOcrExcerpt = useCallback((text, maxChars = 150) => {
    if (!text) return "";
    const trimmed = text.trim();
    if (trimmed.length <= maxChars) return trimmed;
    // Try to break at word boundary
    const truncated = trimmed.slice(0, maxChars);
    const lastSpace = truncated.lastIndexOf(" ");
    if (lastSpace > maxChars * 0.8) {
      return truncated.slice(0, lastSpace) + "...";
    }
    return truncated + "...";
  }, []);

  const filteredEntries = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    const filtered = normalized
      ? entries.filter((entry) => {
          const pathMatch = entry.path.toLowerCase().includes(normalized);
          const textMatch = entry.text.toLowerCase().includes(normalized);
          return pathMatch || textMatch;
        })
      : entries;
    
    if (entries.length > 0) {
      console.log("[FILTER] Total entries:", entries.length, "Query:", query, "Filtered:", filtered.length);
    }
    
    return [...filtered].sort((a, b) => {
      const dateA = new Date(a.at).getTime();
      const dateB = new Date(b.at).getTime();
      return dateB - dateA;
    });
  }, [entries, query]);

  const groupedEntries = useMemo(() => {
    const groups = new Map();
    filteredEntries.forEach((entry) => {
      const group = getDateGroup(entry.at);
      if (!groups.has(group)) {
        groups.set(group, []);
      }
      groups.get(group).push(entry);
    });
    
    return Array.from(groups.entries())
      .map(([groupName, groupEntries]) => [
        groupName,
        [...groupEntries].sort((a, b) => {
          const dateA = new Date(a.at).getTime();
          const dateB = new Date(b.at).getTime();
          return dateB - dateA;
        }),
      ])
      .sort((a, b) => {
        const order = ["Today", "Yesterday", "This Week", "This Month"];
        const aIdx = order.indexOf(a[0]);
        const bIdx = order.indexOf(b[0]);
        if (aIdx !== -1 && bIdx !== -1) return aIdx - bIdx;
        if (aIdx !== -1) return -1;
        if (bIdx !== -1) return 1;
        const aDate = new Date(a[1][0]?.at || 0).getTime();
        const bDate = new Date(b[1][0]?.at || 0).getTime();
        return bDate - aDate;
      });
  }, [filteredEntries, getDateGroup]);

  const previewEntry = useMemo(() => {
    if (!filteredEntries.length) {
      return null;
    }
    if (!selectedPath) {
      return filteredEntries[0];
    }
    return filteredEntries.find((entry) => entry.path === selectedPath) ?? filteredEntries[0];
  }, [filteredEntries, selectedPath]);
  const currentIndex = useMemo(() => {
    if (!filteredEntries.length || !previewEntry) {
      return -1;
    }
    return filteredEntries.findIndex((entry) => entry.path === previewEntry.path);
  }, [filteredEntries, previewEntry]);
  const selectedPathsSet = useMemo(() => new Set(selectedPaths), [selectedPaths]);
  const selectedCount = selectedPaths.length;
  const viewerSrc = viewerPath ? convertFileSrc(viewerPath) : "";

  const selectNext = useCallback(() => {
    if (!filteredEntries.length) {
      return;
    }
    const index = currentIndex >= 0 ? currentIndex : 0;
    const nextIndex = (index + 1) % filteredEntries.length;
    setSelectedPath(filteredEntries[nextIndex].path);
  }, [filteredEntries, currentIndex]);

  const selectPrevious = useCallback(() => {
    if (!filteredEntries.length) {
      return;
    }
    const index = currentIndex >= 0 ? currentIndex : 0;
    const prevIndex = (index - 1 + filteredEntries.length) % filteredEntries.length;
    setSelectedPath(filteredEntries[prevIndex].path);
  }, [filteredEntries, currentIndex]);

  const toggleSelection = useCallback((path) => {
    setSelectedPaths((current) =>
      current.includes(path) ? current.filter((item) => item !== path) : [...current, path],
    );
  }, []);

  const selectAllFiltered = useCallback(() => {
    if (!filteredEntries.length) {
      return;
    }
    setSelectedPaths(filteredEntries.map((entry) => entry.path));
  }, [filteredEntries]);

  const clearSelection = useCallback(() => {
    setSelectedPaths([]);
  }, []);

  const deleteSelected = useCallback(async () => {
    if (!selectedPaths.length) {
      return;
    }
    console.log("[DELETE] Attempting to delete", selectedPaths.length, "file(s):", selectedPaths);
    try {
      const result = await invoke("delete_files", { paths: selectedPaths });
      const deleted = Array.isArray(result?.deleted) ? result.deleted : [];
      const failed = Array.isArray(result?.failed) ? result.failed : [];
      console.log("[DELETE] Result - Deleted:", deleted.length, "Failed:", failed.length);
      
      // Remove successfully deleted entries (includes missing files, which backend treats as success)
      const deletedSet = new Set(deleted);
      const deletedNormalizedSet = new Set(deleted.map(p => normalizePath(p)));
      
      if (deleted.length) {
        setEntries((current) => {
          const before = current.length;
          const filtered = current.filter((entry) => {
            const entryPath = String(entry.path).trim().replace(/\/+/g, "/").replace(/\/$/, "");
            const entryPathLower = normalizePath(entry.path);
            // Check both exact match and normalized match
            return !deletedSet.has(entry.path) && 
                   !deletedSet.has(entryPath) &&
                   !deletedNormalizedSet.has(entryPathLower);
          });
          if (filtered.length !== before) {
            console.log("[DELETE] Removed", before - filtered.length, "entries from index");
          }
          return filtered;
        });
        
        setSelectedPaths((current) => current.filter((path) => {
          const pathNormalized = normalizePath(path);
          return !deletedSet.has(path) && !deletedNormalizedSet.has(pathNormalized);
        }));
      }
      
      if (failed.length) {
        console.error("[DELETE] Failed to delete files:", failed);
        setLastError(
          `Failed to delete ${failed.length} file${failed.length === 1 ? "" : "s"}.`,
        );
        // Clear error after 5 seconds
        setTimeout(() => setLastError(""), 5000);
      } else if (deleted.length) {
        // Success message for deleted entries
        setLastError("");
      }
    } catch (error) {
      console.error("[DELETE] Error:", error);
      setLastError(`Failed to delete files: ${String(error)}`);
      setTimeout(() => setLastError(""), 5000);
    }
  }, [selectedPaths, normalizePath]);

  const copyViewerImage = useCallback(async () => {
    if (!viewerPath) {
      return;
    }
    try {
      const response = await fetch(convertFileSrc(viewerPath));
      const blob = await response.blob();
      if (navigator.clipboard && window.ClipboardItem) {
        await navigator.clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
        setLastError("");
        return;
      }
      await navigator.clipboard.writeText(viewerPath);
      setLastError("Image copy not supported. Copied file path instead.");
    } catch (error) {
      setLastError(`Failed to copy image: ${String(error)}`);
    }
  }, [viewerPath]);



  useEffect(() => {
    let unlisten;
    let unlistenBatch;

    listen("ocr-status", (event) => {
      const payload = event.payload ?? {};
      const nextStatus = payload.status ?? "processing";
      setStatus(nextStatus);
      setLastPath(payload.path ?? "");
      setLastError(payload.error ?? "");

      if (payload.path) {
        const text = payload.text || "";
        const pathStr = String(payload.path).trim();
        if (!pathStr) {
          console.log("[OCR] Empty path, skipping");
          return;
        }
        
        // Get file creation date from metadata, fall back to current time if not available
        let createdAt = new Date().toISOString();
        if (payload.created_at) {
          try {
            // Backend sends timestamp in milliseconds as string
            const timestamp = parseInt(payload.created_at, 10);
            if (!isNaN(timestamp)) {
              createdAt = new Date(timestamp).toISOString();
            }
          } catch (e) {
            console.warn("[OCR] Failed to parse created_at:", payload.created_at, e);
          }
        }
        
        console.log("[OCR] Processing entry:", pathStr);
        console.log("[OCR] Text received:", text.substring(0, 100), "Length:", text.length);
        console.log("[OCR] Text valid:", isOcrTextValid(text), "Status:", nextStatus);
        console.log("[OCR] File created at:", createdAt);
        setEntries((current) => {
          const normalizedPath = pathStr.replace(/\/+/g, "/").replace(/\/$/, "");
          const normalizedPathLower = normalizePath(pathStr);
          
          // Extract basename and directory for comparison
          const getBasename = (p) => {
            const parts = p.split("/");
            return parts[parts.length - 1] || "";
          };
          const getDir = (p) => {
            const parts = p.split("/");
            parts.pop();
            return parts.join("/");
          };
          
          const newBasename = getBasename(normalizedPath);
          const newDir = getDir(normalizedPath);
          
          // Find existing entry by exact path match or normalized path match only
          // Don't match by timestamp - that causes false positives with different files
          const existingIndex = current.findIndex((entry) => {
            const entryPath = String(entry.path).trim().replace(/\/+/g, "/").replace(/\/$/, "");
            const entryPathLower = normalizePath(entry.path);
            
            // Only match exact or normalized path - be strict to avoid false duplicates
            return entryPath === normalizedPath || entryPathLower === normalizedPathLower;
          });
          
          const nextEntry = {
            path: normalizedPath,
            text: text, // Store full text, not truncated
            at: existingIndex >= 0 ? current[existingIndex].at : createdAt, // Preserve original creation date for updates
          };
          
          let updated;
          if (existingIndex >= 0) {
            updated = [...current];
            // Update existing entry with new path and text, but preserve original creation date
            updated[existingIndex] = { 
              ...current[existingIndex], 
              path: nextEntry.path,
              text: nextEntry.text,
              // Keep original 'at' (creation date) - don't overwrite it
            };
            console.log("[OCR] Entry updated at index", existingIndex, "Old path:", current[existingIndex].path, "New path:", normalizedPath);
          } else {
            updated = [nextEntry, ...current];
            console.log("[OCR] New entry added. Total entries:", updated.length, "Path:", normalizedPath, "Created at:", createdAt);
          }
          
          // More robust deduplication: remove ALL duplicates, not just first match
          const seen = new Set();
          const unique = updated.filter((entry) => {
            const entryPath = String(entry.path).trim().replace(/\/+/g, "/").replace(/\/$/, "");
            const entryPathLower = normalizePath(entry.path);
            if (seen.has(entryPathLower) || seen.has(entryPath)) {
              return false;
            }
            seen.add(entryPathLower);
            seen.add(entryPath);
            return true;
          });
          
          if (unique.length !== updated.length) {
            console.log("[OCR] Removed duplicates:", updated.length - unique.length, "Original:", updated.length, "Unique:", unique.length);
          }
          
          const sorted = [...unique].sort((a, b) => {
            const dateA = new Date(a.at || 0).getTime();
            const dateB = new Date(b.at || 0).getTime();
            if (dateB !== dateA) {
              return dateB - dateA;
            }
            return String(b.path).localeCompare(String(a.path));
          });
          
          if (sorted.length > 0) {
            const firstDate = new Date(sorted[0].at);
            const lastDate = new Date(sorted[sorted.length - 1].at);
            console.log("[OCR] Sorted entries - First:", firstDate.toLocaleString(), "Last:", lastDate.toLocaleString(), "Total:", sorted.length);
            if (sorted.length > 1 && firstDate < lastDate) {
              console.warn("[OCR] WARNING: Entries not sorted correctly! First entry is older than last entry.");
            }
          } else {
            console.warn("[OCR] WARNING: No entries after processing! This should not happen.");
          }
          
          console.log("[OCR] Final entry count:", sorted.length, "Path:", normalizedPath);
          return sorted;
        });
        if (nextStatus === "idle" && !payload.error) {
          setSelectedPath((current) => current || pathStr);
        }
      } else {
        console.log("[OCR] Missing path in payload:", payload);
      }
    }).then((stop) => {
      unlisten = stop;
    });

    listen("batch-progress", (event) => {
      const payload = event.payload ?? {};
      setBatchProgress({
        total: payload.total ?? 0,
        completed: payload.completed ?? 0,
        percent: payload.percent ?? 0,
        etaSeconds: payload.eta_seconds ?? 0,
        inProgress: payload.in_progress ?? false,
      });
    }).then((stop) => {
      unlistenBatch = stop;
    });

    return () => {
      if (unlisten) {
        unlisten();
      }
      if (unlistenBatch) {
        unlistenBatch();
      }
    };
  }, []);

  useEffect(() => {
    if (!filteredEntries.length) {
      setSelectedPath("");
      return;
    }
    setSelectedPath((current) =>
      current && filteredEntries.some((entry) => entry.path === current)
        ? current
        : filteredEntries[0].path,
    );
  }, [filteredEntries]);

  useEffect(() => {
    setSelectedPaths((current) =>
      current.filter((path) => entries.some((entry) => entry.path === path)),
    );
  }, [entries]);

  // Safety net: remove duplicates periodically (not on every change to avoid performance issues)
  useEffect(() => {
    // Only run cleanup if we have a significant number of entries to avoid constant re-runs
    if (entries.length < 10) return;
    
    // Use a debounced cleanup - only run every 2 seconds max
    const timeoutId = setTimeout(() => {
      setEntries((current) => {
        const beforeCount = current.length;
        const seen = new Set();
        const unique = current.filter((entry) => {
          const entryPath = String(entry.path).trim().replace(/\/+/g, "/").replace(/\/$/, "");
          const entryPathLower = entryPath.toLowerCase();
          if (seen.has(entryPathLower) || seen.has(entryPath)) {
            console.log("[CLEANUP] Removing duplicate:", entryPath);
            return false;
          }
          seen.add(entryPathLower);
          seen.add(entryPath);
          return true;
        });
        
        if (unique.length !== current.length) {
          console.log("[CLEANUP] Removed", current.length - unique.length, "duplicate entries. Before:", beforeCount, "After:", unique.length);
          // Re-sort after removing duplicates
          return [...unique].sort((a, b) => {
            const dateA = new Date(a.at || 0).getTime();
            const dateB = new Date(b.at || 0).getTime();
            if (dateB !== dateA) {
              return dateB - dateA;
            }
            return String(b.path).localeCompare(String(a.path));
          });
        }
        return current;
      });
    }, 2000);
    
    return () => clearTimeout(timeoutId);
  }, [entries.length]);

  useEffect(() => {
    setSelectedPaths([]);
  }, [query]);

  useEffect(() => {
    const handleKey = (event) => {
      if (!viewerPath) {
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setViewerPath("");
      }
    };
    window.addEventListener("keydown", handleKey);
    return () => {
      window.removeEventListener("keydown", handleKey);
    };
  }, [viewerPath]);

  const etaLabel = useMemo(() => {
    if (!batchProgress.etaSeconds) {
      return "Calculating time remaining...";
    }
    const minutes = Math.floor(batchProgress.etaSeconds / 60);
    const seconds = batchProgress.etaSeconds % 60;
    if (minutes > 0) {
      return `${minutes}m ${seconds}s remaining`;
    }
    return `${seconds}s remaining`;
  }, [batchProgress.etaSeconds]);

  useEffect(() => {
    const handleKey = (event) => {
      const target = event.target;
      const isEditable =
        target instanceof HTMLElement &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable);
      if (isEditable) {
        return;
      }

      if (event.key === "ArrowRight" || event.key === "ArrowDown") {
        event.preventDefault();
        selectNext();
      }
      if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
        event.preventDefault();
        selectPrevious();
      }
      if (event.key === "Enter" && previewEntry) {
        event.preventDefault();
        setViewerPath(previewEntry.path);
      }
      if (event.key === " " && !viewerPath) {
        event.preventDefault();
        if (previewEntry) {
          setViewerPath(previewEntry.path);
        }
      }
      if ((event.metaKey || event.ctrlKey) && event.key === "a") {
        event.preventDefault();
        selectAllFiltered();
      }
    };

    window.addEventListener("keydown", handleKey);
    return () => {
      window.removeEventListener("keydown", handleKey);
    };
  }, [selectNext, selectPrevious, previewEntry, selectAllFiltered, viewerPath]);

  return (
    <main className="min-h-full bg-[#111316] text-slate-100">
      <div className="relative min-h-full w-full overflow-hidden border border-slate-800/60 bg-[#111316]">
        <div className="pointer-events-none absolute inset-0 bg-linear-to-br from-white/5 via-transparent to-black/40" />
        {batchProgress.inProgress && (
          <div className="absolute top-0 left-0 right-0 z-50 h-1 bg-slate-800/50">
            <div
              className="h-full bg-emerald-400 transition-[width] duration-300"
              style={{ width: `${Math.min(100, Math.max(0, batchProgress.percent))}%` }}
            />
          </div>
        )}
        <div className="relative mx-auto w-full max-w-5xl px-10 py-8">
          {batchProgress.inProgress && (
            <div className="absolute right-4 top-4 z-40 flex items-center gap-2 rounded-lg border border-slate-700/50 bg-slate-900/90 px-2.5 py-1.5 text-[10px] shadow-lg backdrop-blur-sm">
              <div className="h-1.5 w-16 overflow-hidden rounded-full bg-slate-700/50">
                <div
                  className="h-full bg-emerald-400 transition-[width] duration-300"
                  style={{ width: `${Math.min(100, Math.max(0, batchProgress.percent))}%` }}
                />
              </div>
              <span className="text-slate-300">
                {batchProgress.completed}/{batchProgress.total}
              </span>
              <span className="text-slate-500">•</span>
              <span className="text-slate-400">{Math.round(batchProgress.percent)}%</span>
            </div>
          )}
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <div className="h-2.5 w-2.5 animate-pulse rounded-full bg-emerald-400" />
              <h1 className="text-lg font-semibold text-slate-100">Chronicle</h1>
            </div>
            <div className="text-right text-xs text-slate-400/80">
              {entries.length > 0 ? (
                <>
                  <div className="font-medium text-slate-200">{entries.length} indexed</div>
                  {lastPath && (
                    <div className="text-[10px]">Last: {formatDate(entries.find((e) => e.path === lastPath)?.at || new Date().toISOString())}</div>
                  )}
                </>
              ) : (
                <div className="text-[10px]">No screenshots yet</div>
              )}
            </div>
          </div>

        {lastError ? (
          <div className="mt-4 rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-sm text-rose-200">
            {lastError}
          </div>
        ) : null}

        <div className="mt-6">
          <div className="flex items-center gap-3 rounded-xl border border-slate-700/70 bg-[#0f1114] px-3 py-2 text-sm text-slate-100 shadow-[inset_0_1px_0_rgba(255,255,255,0.06),0_14px_40px_rgba(0,0,0,0.55)] focus-within:border-emerald-500/60 focus-within:ring-2 focus-within:ring-emerald-500/20">
            <svg
              aria-hidden="true"
              viewBox="0 0 24 24"
              className="h-4 w-4 text-slate-400"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
            >
              <circle cx="11" cy="11" r="7" />
              <path d="M16.5 16.5 21 21" />
            </svg>
            <input
              id="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder="Search screenshots..."
              className="w-full bg-transparent text-sm text-slate-100 placeholder:text-slate-500/80 focus:outline-none"
            />
          </div>
        </div>

        <div className="mt-6">
          <div className="flex flex-wrap items-center justify-between gap-3 text-xs uppercase tracking-[0.2em] text-slate-400/80">
            <p>
              {filteredEntries.length} result{filteredEntries.length === 1 ? "" : "s"}
            </p>
            <div className="flex flex-wrap items-center gap-2">
              {filteredEntries.length > 0 ? (
                <span className="normal-case tracking-normal text-slate-300/80">
                  {currentIndex + 1} of {filteredEntries.length}
                </span>
              ) : null}
              <span className="normal-case tracking-normal text-slate-400/80">
                {selectedCount} selected
              </span>
              <button
                type="button"
                onClick={selectAllFiltered}
                disabled={!filteredEntries.length}
                className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-500/70 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Select all
              </button>
              <button
                type="button"
                onClick={clearSelection}
                disabled={!selectedCount}
                className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-500/70 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Clear
              </button>
              <button
                type="button"
                onClick={deleteSelected}
                disabled={!selectedCount}
                className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-2 py-1 text-[11px] text-rose-200 transition enabled:hover:border-rose-400/70 enabled:hover:text-rose-100 disabled:opacity-40"
              >
                Delete
              </button>
              <button
                type="button"
                onClick={selectPrevious}
                disabled={!filteredEntries.length}
                className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-500/70 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Prev
              </button>
              <button
                type="button"
                onClick={selectNext}
                disabled={!filteredEntries.length}
                className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-500/70 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Next
              </button>
            </div>
          </div>
          <div className="mt-4">
            {filteredEntries.length === 0 ? (
              <div className="rounded-2xl border border-slate-700/70 bg-[#0f1114] px-6 py-12 text-center">
                <p className="text-base font-medium text-slate-200">No screenshots found</p>
                <p className="mt-2 text-sm text-slate-400/80">
                  {query ? "Try a different search term" : "Take a screenshot to get started"}
                </p>
              </div>
            ) : (
              <div className="max-h-[calc(100vh-280px)] space-y-6 overflow-y-auto pr-1">
                {groupedEntries.map(([groupName, groupEntries]) => (
                  <div key={groupName}>
                    <h3 className="mb-4 text-xs font-semibold uppercase tracking-[0.15em] text-white/50">
                      {groupName}
                    </h3>
                    <div className="grid grid-cols-2 gap-[20px] sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 xl:grid-cols-6">
                      {groupEntries.map((entry) => {
                        const isSelected = selectedPathsSet.has(entry.path);
                        const isActive = entry.path === previewEntry?.path;
                        return (
                          <div
                            key={entry.path}
                            className={`group relative rounded-lg border text-left transition-all duration-200 ease-in-out ${
                              isActive
                                ? "border-emerald-500/60 bg-emerald-500/10 shadow-lg shadow-emerald-500/10"
                                : "border-white/10 bg-white/3 hover:scale-[1.03] hover:border-emerald-500/50 hover:shadow-[0_4px_12px_rgba(0,0,0,0.3)]"
                            }`}
                            style={{
                              border: "1px solid rgba(255, 255, 255, 0.15)",
                              borderRadius: "8px",
                            }}
                            onClick={() => setSelectedPath(entry.path)}
                            role="button"
                            tabIndex={0}
                            onKeyDown={(event) => {
                              if (event.key === "Enter" || event.key === " ") {
                                setSelectedPath(entry.path);
                              }
                            }}
                          >
                            <button
                              type="button"
                              aria-pressed={isSelected}
                              aria-label={isSelected ? "Deselect screenshot" : "Select screenshot"}
                              onClick={(event) => {
                                event.stopPropagation();
                                toggleSelection(entry.path);
                              }}
                              className={`absolute right-2 top-2 z-10 flex h-6 w-6 items-center justify-center rounded-full transition-all ${
                                isSelected
                                  ? "border-2 border-emerald-500 bg-emerald-500 shadow-md shadow-emerald-500/20"
                                  : "border-2 border-white/30 bg-[#0f1114]/90 hover:border-emerald-400/60 hover:bg-emerald-400/10"
                              }`}
                              style={{
                                border: isSelected ? "2px solid rgb(16, 185, 129)" : "2px solid rgba(255, 255, 255, 0.3)",
                              }}
                            >
                              {isSelected && (
                                <svg
                                  className="h-3 w-3 text-white"
                                  fill="none"
                                  viewBox="0 0 24 24"
                                  stroke="currentColor"
                                  strokeWidth={3}
                                >
                                  <path strokeLinecap="round" strokeLinejoin="round" d="M5 13l4 4L19 7" />
                                </svg>
                              )}
                            </button>
                            <div className="p-1.5">
                              <div className="overflow-hidden rounded-lg bg-[#16191e]">
                                <img
                                  src={convertFileSrc(entry.path)}
                                  alt="Screenshot preview"
                                  className="h-36 w-full object-cover brightness-105 contrast-105 transition-transform duration-200 group-hover:scale-105"
                                  onClick={(event) => {
                                    event.stopPropagation();
                                    setViewerPath(entry.path);
                                  }}
                                />
                              </div>
                            </div>
                            <div className="px-2.5 pb-2.5 pt-1">
                              <p className="text-[10px] text-slate-400/80">
                                {formatDateTime(entry.at)}
                              </p>
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>
        </div>
        {viewerPath ? (
          <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-6"
            onClick={() => setViewerPath("")}
            role="button"
            tabIndex={0}
            onKeyDown={(event) => {
              if (event.key === "Escape") {
                setViewerPath("");
              }
            }}
          >
            <div
              className="w-full max-w-5xl rounded-3xl border border-slate-700/70 bg-[#0f1114] p-4 shadow-[0_30px_120px_rgba(0,0,0,0.7)]"
              onClick={(event) => event.stopPropagation()}
            >
              <div className="flex items-center justify-between gap-3 pb-3">
                <p className="truncate text-xs text-slate-300/80">
                  {displayPath(viewerPath)}
                </p>
                <div className="flex items-center gap-2">
                  <button
                    type="button"
                    onClick={copyViewerImage}
                    className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-3 py-1 text-[11px] text-slate-200 transition hover:border-slate-500/70 hover:text-slate-100"
                  >
                    Copy
                  </button>
                  <button
                    type="button"
                    onClick={() => setViewerPath("")}
                    className="rounded-lg border border-slate-700/70 bg-[#0f1114] px-3 py-1 text-[11px] text-slate-200 transition hover:border-slate-500/70 hover:text-slate-100"
                  >
                    Close
                  </button>
                </div>
              </div>
              <div className="flex max-h-[80vh] items-center justify-center overflow-hidden rounded-2xl bg-[#16191e]">
                <img
                  src={viewerSrc}
                  alt="Screenshot full view"
                  className="max-h-[80vh] w-full object-contain"
                />
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </main>
  );
}

export default App;
