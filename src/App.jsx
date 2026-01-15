import { useEffect, useMemo, useState } from "react";
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

  const previewSrc = previewEntry ? convertFileSrc(previewEntry.path) : "";

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

  return (
    <main className="flex min-h-full items-center justify-center bg-slate-950 text-slate-100">
      <div className="w-full max-w-xl rounded-2xl border border-slate-800 bg-slate-900/60 p-8 shadow-xl">
        <div className="flex items-center gap-3">
          <div className="h-3 w-3 animate-pulse rounded-full bg-emerald-400" />
          <h1 className="text-xl font-semibold">Screenshot OCR</h1>
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
          <label className="text-sm font-medium text-slate-200" htmlFor="search">
            Search indexed text
          </label>
          <input
            id="search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Type to filter results..."
            className="mt-2 w-full rounded-lg border border-slate-800 bg-slate-950 px-3 py-2 text-sm text-slate-100 placeholder:text-slate-500 focus:outline-none focus:ring-2 focus:ring-emerald-500/60"
          />
        </div>

        <div className="mt-6">
          <p className="text-sm text-slate-400">
            {filteredEntries.length} result{filteredEntries.length === 1 ? "" : "s"}
          </p>
          <div className="mt-3 grid gap-4 md:grid-cols-[1.2fr_1fr]">
            <div className="rounded-xl border border-slate-800 bg-slate-950/60 p-3">
              {previewEntry ? (
                <>
                  <p className="truncate text-xs text-slate-400">{previewEntry.path}</p>
                  <div className="mt-3 flex items-center justify-center rounded-lg border border-slate-800 bg-slate-900/40 p-3">
                    <img
                      src={previewSrc}
                      alt="Screenshot preview"
                      className="max-h-64 w-full object-contain"
                    />
                  </div>
                </>
              ) : (
                <div className="flex min-h-[12rem] items-center justify-center text-sm text-slate-500">
                  No screenshot selected.
                </div>
              )}
            </div>
            <div className="max-h-64 space-y-3 overflow-y-auto pr-1">
            {filteredEntries.length === 0 ? (
              <div className="rounded-lg border border-slate-800 bg-slate-950/60 px-3 py-4 text-sm text-slate-500">
                No matches yet.
              </div>
            ) : (
              filteredEntries.map((entry) => (
                <div
                  key={entry.path}
                  className={`cursor-pointer rounded-lg border px-3 py-3 text-sm transition ${
                    entry.path === previewEntry?.path
                      ? "border-emerald-500/60 bg-emerald-500/10"
                      : "border-slate-800 bg-slate-950/60 hover:border-slate-700"
                  }`}
                  onClick={() => setSelectedPath(entry.path)}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") {
                      setSelectedPath(entry.path);
                    }
                  }}
                >
                  <p className="truncate font-medium text-slate-200">{entry.path}</p>
                  <p className="mt-2 whitespace-pre-wrap text-slate-300">
                    {entry.text}
                  </p>
                  <p className="mt-2 text-xs text-slate-500">
                    Indexed {new Date(entry.at).toLocaleString()}
                  </p>
                </div>
              ))
            )}
            </div>
          </div>
        </div>
      </div>
    </main>
  );
}

export default App;
