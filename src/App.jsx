import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";

function App() {
  const [status, setStatus] = useState("processing");
  const [lastPath, setLastPath] = useState("");
  const [lastError, setLastError] = useState("");
  const [query, setQuery] = useState("");
  const [entries, setEntries] = useState([]);
  const [selectedPath, setSelectedPath] = useState("");

  const filteredEntries = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) {
      return entries;
    }
    return entries.filter(
      (entry) =>
        entry.path.toLowerCase().includes(normalized) ||
        entry.text.toLowerCase().includes(normalized),
    );
  }, [entries, query]);

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

  useEffect(() => {
    let unlisten;

    listen("ocr-status", (event) => {
      const payload = event.payload ?? {};
      const nextStatus = payload.status ?? "processing";
      setStatus(nextStatus);
      setLastPath(payload.path ?? "");
      setLastError(payload.error ?? "");

      if (payload.text && payload.path) {
        const nextEntry = {
          path: payload.path,
          text: payload.text,
          at: new Date().toISOString(),
        };
        setEntries((current) => {
          const without = current.filter((entry) => entry.path !== payload.path);
          return [nextEntry, ...without];
        });
        setSelectedPath((current) => current || payload.path);
      }
    }).then((stop) => {
      unlisten = stop;
    });

    return () => {
      if (unlisten) {
        unlisten();
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
    };

    window.addEventListener("keydown", handleKey);
    return () => {
      window.removeEventListener("keydown", handleKey);
    };
  }, [selectNext, selectPrevious]);

  return (
    <main className="flex min-h-full items-center justify-center bg-[#0a0b0f] text-slate-100">
      <div className="relative w-full max-w-2xl overflow-hidden rounded-3xl border border-slate-800/70 bg-[#0d1016] shadow-[0_20px_80px_rgba(0,0,0,0.6)]">
        <div className="pointer-events-none absolute inset-0 bg-linear-to-br from-slate-900/40 via-transparent to-slate-950/80" />
        <div className="relative p-8">
          <div className="flex items-center gap-3">
            <div className="h-2.5 w-2.5 animate-pulse rounded-full bg-emerald-400" />
            <h1 className="text-lg font-semibold text-slate-200">Screenshot OCR</h1>
          </div>

        <p className="mt-6 text-3xl font-semibold">
          {status === "processing" ? "Processing..." : "Idle"}
        </p>

        <div className="mt-6 space-y-2 text-sm text-slate-300">
          <p>
            Watching <span className="font-medium">Desktop</span> and{" "}
            <span className="font-medium">Pictures/Screenshots</span>
          </p>
          {lastPath ? (
            <p className="truncate">Last file: {lastPath}</p>
          ) : (
            <p>Waiting for your next screenshot...</p>
          )}
          {lastError ? (
            <p className="rounded-lg border border-rose-500/40 bg-rose-500/10 px-3 py-2 text-rose-200">
              {lastError}
            </p>
          ) : null}
        </div>

        <div className="mt-8">
          <label className="text-xs font-medium uppercase tracking-[0.2em] text-slate-500" htmlFor="search">
            Search
          </label>
          <div className="mt-3 flex items-center gap-3 rounded-2xl border border-slate-800/80 bg-[#0b0d12] px-4 py-3 text-sm text-slate-100 shadow-[inset_0_1px_0_rgba(255,255,255,0.05),0_10px_30px_rgba(0,0,0,0.5)] focus-within:border-emerald-500/60 focus-within:ring-2 focus-within:ring-emerald-500/20">
            <svg
              aria-hidden="true"
              viewBox="0 0 24 24"
              className="h-4 w-4 text-slate-500"
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
              className="w-full bg-transparent text-sm text-slate-100 placeholder:text-slate-500 focus:outline-none"
            />
          </div>
        </div>

        <div className="mt-6">
          <div className="flex flex-wrap items-center justify-between gap-3 text-xs uppercase tracking-[0.2em] text-slate-500">
            <p>
              {filteredEntries.length} result{filteredEntries.length === 1 ? "" : "s"}
            </p>
            <div className="flex items-center gap-2">
              {filteredEntries.length > 0 ? (
                <span className="normal-case tracking-normal text-slate-400">
                  {currentIndex + 1} of {filteredEntries.length}
                </span>
              ) : null}
              <button
                type="button"
                onClick={selectPrevious}
                disabled={!filteredEntries.length}
                className="rounded-lg border border-slate-800/80 bg-[#0b0d12] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-600/80 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Prev
              </button>
              <button
                type="button"
                onClick={selectNext}
                disabled={!filteredEntries.length}
                className="rounded-lg border border-slate-800/80 bg-[#0b0d12] px-2 py-1 text-[11px] text-slate-200 transition enabled:hover:border-slate-600/80 enabled:hover:text-slate-100 disabled:opacity-40"
              >
                Next
              </button>
            </div>
          </div>
          <div className="mt-4">
            {filteredEntries.length === 0 ? (
              <div className="rounded-2xl border border-slate-800/70 bg-[#0b0d12] px-3 py-6 text-center text-sm text-slate-500">
                No matches yet.
              </div>
            ) : (
              <div className="grid max-h-112 grid-cols-1 gap-3 overflow-y-auto pr-1 sm:grid-cols-2 lg:grid-cols-3">
                {filteredEntries.map((entry) => (
                  <button
                    key={entry.path}
                    type="button"
                    className={`group rounded-2xl border text-left transition ${
                      entry.path === previewEntry?.path
                        ? "border-emerald-500/60 bg-emerald-500/10"
                        : "border-slate-800/80 bg-[#0b0d12] hover:border-slate-600/80 hover:bg-[#0d1016]"
                    }`}
                    onClick={() => setSelectedPath(entry.path)}
                  >
                    <div className="rounded-2xl bg-[#11141b]">
                      <img
                        src={convertFileSrc(entry.path)}
                        alt="Screenshot preview"
                        className="h-40 w-full rounded-2xl object-cover"
                      />
                    </div>
                    <div className="px-3 pb-3 pt-2">
                      <p className="truncate text-xs text-slate-300">{entry.path}</p>
                      <p className="mt-1 text-[11px] text-slate-500">
                        Indexed {new Date(entry.at).toLocaleString()}
                      </p>
                    </div>
                  </button>
                ))}
              </div>
            )}
          </div>
        </div>
        </div>
      </div>
    </main>
  );
}

export default App;
