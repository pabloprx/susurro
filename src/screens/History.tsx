// DESIGN 6.2
import { Fragment, useEffect, useRef, useState, type ReactNode } from "react";
import { AlertTriangle, ChevronDown, Copy, Download, MoreHorizontal, Pause, Play, Search, Trash2 } from "lucide-react";
import { api, on, type HistoryItem } from "../lib/api";
import { clock, dayLabel, mmss, PROVIDER_LABEL, secs } from "../lib/store";
import { S } from "../lib/strings";
import { copy, Empty, Popover, Segmented, Sheet, Title, toast } from "../ui";

const PAGE = 50;
type Mode = "final" | "raw" | "diff";

export default function History() {
  const [query, setQuery] = useState("");
  const [items, setItems] = useState<HistoryItem[] | null>(null);
  const [more, setMore] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  const [confirm, setConfirm] = useState(false);
  const search = useRef<HTMLInputElement>(null);

  const load = async (q: string, before: number | null = null) => {
    const page = await api.historyList(q || null, before, PAGE);
    setItems((cur) => (before ? [...(cur ?? []), ...page] : page));
    setMore(page.length === PAGE);
  };
  useEffect(() => {
    const t = setTimeout(() => load(query), 120);
    return () => clearTimeout(t);
  }, [query]);
  useEffect(() => {
    // A retry re-emits an existing id: replace it in place.
    const off = on("history_added", (h) => setItems((cur) => (cur?.some((x) => x.id === h.id) ? cur.map((x) => (x.id === h.id ? h : x)) : query ? cur : [h, ...(cur ?? [])])));
    const key = (e: KeyboardEvent) => {
      if (e.metaKey && e.key === "f") (e.preventDefault(), search.current?.focus());
    };
    window.addEventListener("keydown", key);
    return () => {
      off.then((f) => f());
      window.removeEventListener("keydown", key);
    };
  }, [query]);

  const exportJson = async () => {
    const all = await api.historyList(null, null, 100000);
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([JSON.stringify(all, null, 2)], { type: "application/json" }));
    a.download = `susurro-history-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
  };

  // arrow keys move between rows
  const onListKey = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    const rows = [...document.querySelectorAll<HTMLElement>(".h-row")];
    const i = rows.indexOf(document.activeElement as HTMLElement);
    if (i < 0) return;
    e.preventDefault();
    rows[Math.max(0, Math.min(rows.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))]?.focus();
  };

  let lastDay = "";
  return (
    <>
      <Title parts={S.title.history} />
      <div className="toolbar">
        <label className="search">
          <Search size={15} strokeWidth={1.5} />
          <input ref={search} className="input" placeholder="Search transcripts…" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search transcripts" />
        </label>
        <Popover
          trigger={(t) => (
            <button className="icon-btn" aria-label="More" onClick={t}>
              <MoreHorizontal size={16} strokeWidth={1.5} />
            </button>
          )}
        >
          {(close) => (
            <>
              <button className="menu-item" onClick={() => (close(), exportJson())}>
                <Download size={14} strokeWidth={1.5} /> Export JSON
              </button>
              <div className="menu-sep" />
              <button className="menu-item danger-text" onClick={() => (close(), setConfirm(true))}>
                <Trash2 size={14} strokeWidth={1.5} /> Delete all…
              </button>
            </>
          )}
        </Popover>
      </div>

      {items && !items.length ? (
        query ? (
          <Empty line={[`No match for “${query}”.`]} help={S.empty.search.help} action={<button className="btn secondary" onClick={() => setQuery("")}>Clear search</button>} />
        ) : (
          <Empty line={S.empty.history.line} help={S.empty.history.help} />
        )
      ) : (
        <div className="h-list" onKeyDown={onListKey}>
          {items?.map((h) => {
            const day = dayLabel(h.created_at);
            const head = day !== lastDay;
            lastDay = day;
            return (
              <Fragment key={h.id}>
                {head && (
                  <div className="day label">
                    <span>{day}</span>
                  </div>
                )}
                <Row
                  h={h}
                  query={query}
                  open={open === h.id}
                  onToggle={() => setOpen(open === h.id ? null : h.id)}
                  onChange={(n) => setItems((cur) => cur!.map((x) => (x.id === n.id ? n : x)))}
                  onDelete={() => {
                    api.historyDelete([h.id]);
                    setItems((cur) => cur!.filter((x) => x.id !== h.id));
                  }}
                />
              </Fragment>
            );
          })}
          {more && (
            <div className="more">
              <button className="btn ghost" onClick={() => load(query, items![items!.length - 1].id)}>
                Show older
              </button>
            </div>
          )}
        </div>
      )}

      <Sheet open={confirm} onClose={() => setConfirm(false)} title="Delete all history?">
        <p className="dim">Every transcript and its audio leaves this Mac. Your dictionary stays.</p>
        <div className="sheet-foot">
          <button className="btn ghost" onClick={() => setConfirm(false)}>
            Keep history
          </button>
          <button
            className="btn danger fill"
            onClick={async () => {
              await api.historyClear();
              setItems([]);
              setConfirm(false);
            }}
          >
            Delete all history
          </button>
        </div>
      </Sheet>
    </>
  );
}

function Row({ h, query, open, onToggle, onChange, onDelete }: { h: HistoryItem; query: string; open: boolean; onToggle: () => void; onChange: (h: HistoryItem) => void; onDelete: () => void }) {
  const [mode, setMode] = useState<Mode>("final");
  const [busy, setBusy] = useState(false);
  const failed = h.status === "failed";
  const [prov, model] = h.provider.split(":");
  const meta = [clock(h.created_at), h.app_name ?? "Unknown", `${h.words} words`, secs(h.duration_ms), [PROVIDER_LABEL[prov] ?? prov, model].filter(Boolean).join(" "), h.cleanup ? h.cleanup[0].toUpperCase() + h.cleanup.slice(1) : "Raw"];
  const retry = async () => {
    setBusy(true);
    try {
      onChange(await api.historyRetry(h.id));
      toast("Re-transcribed. Saved in History.", { kind: "success" });
    } catch (e) {
      toast(String(e), { kind: "error" });
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className={`h-item${open ? " open" : ""}${failed ? " failed" : ""}`}>
      <button className="h-row" aria-expanded={open} onClick={onToggle}>
        <span className="meta">{open ? meta.join(" · ") : `${meta[0]} · ${meta[1]}`}</span>
        <span className="text ellipsis">{highlight(h.final_text.replace(/\n+/g, " "), query)}</span>
        {!open && failed && <span className="status danger-text">not pasted</span>}
        {!open && h.status === "copied" && <span className="status dim">copied</span>}
      </button>
      <div
        className="expand"
        onTransitionEnd={(e) => {
          // keep the opened row's controls on screen (a row near the bottom opens below the fold)
          if (open && e.target === e.currentTarget && e.propertyName === "grid-template-rows")
            e.currentTarget.parentElement!.scrollIntoView({ block: "nearest", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
        }}
      >
        <div>
          {open && (
            <div className="h-body">
              {h.error && (
                <p className="h-error">
                  <AlertTriangle size={14} strokeWidth={1.5} /> {h.error}
                </p>
              )}
              <div className="transcript selectable">{mode === "diff" ? <Diff h={h} /> : highlight(mode === "raw" ? h.raw : h.final_text, query)}</div>
              <div className="h-actions">
                <Segmented
                  label="Transcript view"
                  value={mode}
                  onChange={setMode}
                  options={[
                    { value: "final", label: "Formatted" },
                    { value: "raw", label: "Raw" },
                    { value: "diff", label: "Diff" },
                  ]}
                />
                {failed && (
                  <button className="btn secondary sm" onClick={() => copy(h.final_text)}>
                    Not pasted - Copy
                  </button>
                )}
                {h.has_audio && <Player id={h.id} ms={h.duration_ms} />}
                {!failed && (
                  <button className="btn ghost sm" onClick={() => copy(h.final_text)}>
                    <Copy size={14} strokeWidth={1.5} /> Copy
                  </button>
                )}
                <Popover
                  up
                  trigger={(t) => (
                    <button className="btn ghost sm" onClick={t} disabled={busy}>
                      {busy ? "Retrying…" : "Retry"} <ChevronDown size={13} strokeWidth={1.5} />
                    </button>
                  )}
                >
                  {(close) => (
                    <>
                      <button className="menu-item" disabled={!h.has_audio} onClick={() => (close(), retry())} title={h.has_audio ? undefined : "Audio expired"}>
                        Re-transcribe {!h.has_audio && <span className="hint">audio expired</span>}
                      </button>
                      <button className="menu-item" onClick={() => (close(), api.historyPaste(h.id, "final"))}>
                        Paste again
                      </button>
                      <button className="menu-item" onClick={() => (close(), api.historyPaste(h.id, "raw"))}>
                        Paste raw
                      </button>
                    </>
                  )}
                </Popover>
                <button className="icon-btn del" aria-label="Delete" data-tip="Delete" onClick={onDelete}>
                  <Trash2 size={15} strokeWidth={1.5} />
                </button>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function highlight(text: string, q: string): ReactNode {
  if (!q.trim()) return text;
  const parts = text.split(new RegExp(`(${q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, "gi"));
  return parts.map((p, i) => (i % 2 ? <mark key={i}>{p}</mark> : p));
}

function Player({ id, ms }: { id: number; ms: number }) {
  const audio = useRef<HTMLAudioElement | null>(null);
  const [t, setT] = useState<number | null>(null);
  useEffect(() => () => audio.current?.pause(), []);
  const toggle = async () => {
    if (audio.current && !audio.current.paused) return audio.current.pause();
    if (!audio.current) {
      const buf = await api.historyAudio(id);
      const a = new Audio(URL.createObjectURL(new Blob([buf], { type: "audio/wav" })));
      a.ontimeupdate = () => setT(a.currentTime);
      a.onpause = a.onended = () => setT(null);
      audio.current = a;
    }
    audio.current.play();
  };
  const playing = t !== null;
  return (
    <button className="btn ghost sm play" onClick={toggle} aria-label={playing ? "Pause audio" : "Play audio"}>
      {playing ? <Pause size={13} strokeWidth={1.5} /> : <Play size={13} strokeWidth={1.5} />}
      {mmss(playing ? t : ms / 1000)}
    </button>
  );
}

// ---------- word diff (raw vs formatted), signature view ----------

const norm = (w: string) => w.toLowerCase().replace(/[^\p{L}\p{N}]/gu, "");
type Op = { t: "eq" | "del" | "ins"; w: string };

/** LCS on words, compared without case and punctuation, so "lunch" -> "lunch?" reads as kept. */
export function wordDiff(a: string, b: string): Op[] {
  const A = a.split(/\s+/).filter(Boolean);
  const B = b.split(/(\s+)/).filter((x) => x.trim());
  const n = A.length;
  const m = B.length;
  const L = Array.from({ length: n + 1 }, () => new Uint16Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) L[i][j] = norm(A[i]) === norm(B[j]) ? L[i + 1][j + 1] + 1 : Math.max(L[i + 1][j], L[i][j + 1]);
  const out: Op[] = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (norm(A[i]) === norm(B[j])) out.push({ t: "eq", w: B[j] }), i++, j++;
    else if (L[i + 1][j] >= L[i][j + 1]) out.push({ t: "del", w: A[i++] });
    else out.push({ t: "ins", w: B[j++] });
  }
  while (i < n) out.push({ t: "del", w: A[i++] });
  while (j < m) out.push({ t: "ins", w: B[j++] });
  return out;
}

function Diff({ h }: { h: HistoryItem }) {
  const [dictWords, setDictWords] = useState<Set<string>>(new Set());
  useEffect(() => {
    api.dictPreview(h.raw).then((ap) => {
      const chars = [...ap.text];
      const s = new Set<string>();
      for (const m of ap.matches) chars.slice(m.start, m.end).join("").split(/\s+/).forEach((w) => s.add(norm(w)));
      setDictWords(s);
    });
  }, [h.raw]);
  const ops = wordDiff(h.raw, h.final_text);
  return (
    <>
      {ops.map((o, i) => (
        <Fragment key={i}>
          {o.t === "eq" ? o.w : o.t === "del" ? <del>{o.w}</del> : <ins>{o.w}</ins>}
          {o.t === "ins" && dictWords.has(norm(o.w)) && !(ops[i + 1]?.t === "ins" && dictWords.has(norm(ops[i + 1].w))) && <span className="tag">DICTIONARY</span>}{" "}
        </Fragment>
      ))}
    </>
  );
}
