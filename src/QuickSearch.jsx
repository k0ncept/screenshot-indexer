import { useEffect, useState, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emit } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";

function QuickSearch() {
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef(null);
  const resultsRef = useRef(null);

  // Load entries on mount
  useEffect(() => {
    const loadEntries = async () => {
      try {
        const dbEntries = await invoke("load_all_entries");
        if (Array.isArray(dbEntries)) {
          const mappedEntries = dbEntries.map(entry => {
            let tags = [];
            try {
              if (entry.tags) {
                tags = typeof entry.tags === 'string' ? JSON.parse(entry.tags) : (Array.isArray(entry.tags) ? entry.tags : []);
              }
            } catch (e) {
              console.warn("Failed to parse tags:", e);
            }
            
            let atValue = entry.at || new Date().toISOString();
            if (typeof entry.at === 'string' && /^\d+$/.test(entry.at)) {
              const timestamp = parseInt(entry.at, 10);
              const date = timestamp < 946684800000 
                ? new Date(timestamp * 1000)
                : new Date(timestamp);
              atValue = date.toISOString();
            } else if (entry.at) {
              const parsed = new Date(entry.at);
              atValue = isNaN(parsed.getTime()) ? new Date().toISOString() : parsed.toISOString();
            }
            
            return {
              path: entry.path,
              text: entry.text || "",
              at: atValue,
              tags: tags
            };
          });
          
          // Sort by date (newest first)
          mappedEntries.sort((a, b) => {
            const dateA = new Date(a.at).getTime();
            const dateB = new Date(b.at).getTime();
            return dateB - dateA;
          });
          
          setEntries(mappedEntries);
        }
      } catch (error) {
        console.error("Failed to load entries:", error);
      }
    };
    
    loadEntries();
    
    // Focus input on mount
    setTimeout(() => {
      inputRef.current?.focus();
    }, 100);
  }, []);

  // Filter entries based on query
  const filteredEntries = useCallback(() => {
    if (!query.trim()) {
      // Show recent screenshots (last 10) when no query
      return entries.slice(0, 10);
    }
    
    const normalized = query.trim().toLowerCase();
    return entries.filter(entry => {
      const pathMatch = entry.path?.toLowerCase().includes(normalized) || false;
      const textMatch = (entry.text || "").toLowerCase().includes(normalized);
      return pathMatch || textMatch;
    }).slice(0, 20); // Limit to 20 results
  }, [query, entries]);

  const results = filteredEntries();

  // Handle keyboard navigation
  useEffect(() => {
    const handleKey = async (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        const window = getCurrentWindow();
        await window.hide();
        return;
      }
      
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setSelectedIndex(prev => Math.min(prev + 1, results.length - 1));
        return;
      }
      
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setSelectedIndex(prev => Math.max(prev - 1, 0));
        return;
      }
      
      if (event.key === "Enter" && results[selectedIndex]) {
        event.preventDefault();
        const entry = results[selectedIndex];
        const window = getCurrentWindow();
        await window.hide();
        // Emit event to main window to open this entry
        try {
          await emit("quick-search-select", { path: entry.path });
        } catch (e) {
          console.error("Failed to emit quick-search-select:", e);
        }
        return;
      }
    };
    
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [results, selectedIndex]);

  // Reset selected index when query changes
  useEffect(() => {
    setSelectedIndex(0);
  }, [query]);

  // Scroll selected item into view
  useEffect(() => {
    if (resultsRef.current && results[selectedIndex]) {
      const selectedElement = resultsRef.current.children[selectedIndex];
      if (selectedElement) {
        selectedElement.scrollIntoView({ block: "nearest", behavior: "smooth" });
      }
    }
  }, [selectedIndex, results]);

  const formatDate = (dateString) => {
    const date = new Date(dateString);
    const now = new Date();
    const diffMs = now - date;
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);

    if (diffMins < 1) return "Just now";
    if (diffMins < 60) return `${diffMins}m ago`;
    if (diffHours < 24) return `${diffHours}h ago`;
    if (diffDays === 1) return "Yesterday";
    if (diffDays < 7) return `${diffDays}d ago`;
    return date.toLocaleDateString("en-US", { month: "short", day: "numeric" });
  };

  const handleEntryClick = async (entry) => {
    const window = getCurrentWindow();
    await window.hide();
    // Emit event to main window to open this entry
    try {
      await emit("quick-search-select", { path: entry.path });
    } catch (e) {
      console.error("Failed to emit quick-search-select:", e);
    }
  };

  return (
    <div className="fixed inset-0 flex items-center justify-center pointer-events-none">
      <div 
        className="w-[600px] max-h-[500px] bg-[#0f1114] rounded-2xl border border-slate-700/70 shadow-2xl overflow-hidden pointer-events-auto"
        style={{
          backdropFilter: "blur(20px)",
          boxShadow: "0 20px 60px rgba(0, 0, 0, 0.5)"
        }}
      >
        {/* Search Input */}
        <div className="p-4 border-b border-slate-700/50">
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search screenshots..."
            className="w-full bg-slate-800/30 border border-slate-700/50 rounded-lg px-4 py-2.5 text-sm text-slate-200 placeholder:text-slate-500 focus:outline-none focus:border-slate-600/50"
            autoFocus
          />
        </div>

        {/* Results List */}
        <div 
          ref={resultsRef}
          className="overflow-y-auto max-h-[400px]"
          style={{ scrollbarWidth: 'none', msOverflowStyle: 'none' }}
        >
          {results.length === 0 ? (
            <div className="p-8 text-center text-slate-400 text-sm">
              {query ? "No results found" : "Start typing to search..."}
            </div>
          ) : (
            results.map((entry, index) => {
              const isSelected = index === selectedIndex;
              const fileName = entry.path.split('/').pop() || entry.path;
              const previewText = entry.text ? entry.text.substring(0, 100) : "";
              
              return (
                <div
                  key={entry.path}
                  onClick={() => handleEntryClick(entry)}
                  className={`px-4 py-3 cursor-pointer border-b border-slate-700/30 transition-colors ${
                    isSelected 
                      ? "bg-emerald-500/10 border-l-2 border-l-emerald-500" 
                      : "hover:bg-slate-800/30"
                  }`}
                >
                  <div className="flex items-start gap-3">
                    {/* Thumbnail */}
                    <div className="flex-shrink-0 w-16 h-16 rounded overflow-hidden bg-slate-800/50">
                      <img
                        src={convertFileSrc(entry.path)}
                        alt={fileName}
                        className="w-full h-full object-cover"
                      />
                    </div>
                    
                    {/* Content */}
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-2 mb-1">
                        <p className="text-sm font-medium text-slate-200 truncate">
                          {fileName}
                        </p>
                        {entry.tags && entry.tags.length > 0 && (
                          <span className="text-[10px] px-1.5 py-0.5 rounded bg-emerald-500/20 text-emerald-300 border border-emerald-500/40">
                            {entry.tags[0]}
                          </span>
                        )}
                      </div>
                      {previewText && (
                        <p className="text-xs text-slate-400 line-clamp-2 mb-1">
                          {previewText}
                        </p>
                      )}
                      <p className="text-[10px] text-slate-500">
                        {formatDate(entry.at)}
                      </p>
                    </div>
                  </div>
                </div>
              );
            })
          )}
        </div>

        {/* Footer */}
        <div className="px-4 py-2 border-t border-slate-700/50 flex items-center justify-between text-[10px] text-slate-500">
          <div className="flex items-center gap-4">
            <span>↑↓ Navigate</span>
            <span>Enter Open</span>
            <span>Esc Close</span>
          </div>
          <span>{results.length} result{results.length !== 1 ? 's' : ''}</span>
        </div>
      </div>
    </div>
  );
}

// Mount the component
import { createRoot } from "react-dom/client";
const container = document.getElementById("quick-search-root");
if (container) {
  const root = createRoot(container);
  root.render(<QuickSearch />);
}

export default QuickSearch;
