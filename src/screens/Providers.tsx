// DESIGN 6.5
import { useEffect, useRef, useState } from "react";
import { AlertTriangle, Check, ChevronDown, Eye, EyeOff, Search, X } from "lucide-react";
import { api, on, type KeyName, type ModelInfo, type OrModel, type ProviderTest, type Settings, type SttProvider } from "../lib/api";
import { ago, keyStore, modelName, save, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Dot, Label, radioKeys, Spinner, Title, Toggle, toast } from "../ui";

export const LANGUAGES: [string, string][] = [
  ["auto", "Auto-detect"],
  ["en", "English"],
  ["es", "Español"],
  ["pt", "Português"],
  ["fr", "Français"],
  ["de", "Deutsch"],
  ["it", "Italiano"],
  ["nl", "Nederlands"],
  ["ja", "日本語"],
  ["zh", "中文"],
];


/** Host of a server URL, or null if it does not parse (a hand-edited settings.json can hold anything). */
const hostOf = (u: string) => {
  try {
    return new URL(u, "http://x").host;
  } catch {
    return null;
  }
};
const checkUrl = (v: string) => (/^https?:\/\/./i.test(v) && hostOf(v) ? null : "Enter a full URL, like http://127.0.0.1:8000/v1");

// Key masks show only the last 4 characters. Older builds also stored the first 6: drop them.
for (const n of ["openrouter", "elevenlabs", "openai", "local_stt", "local_llm"] satisfies KeyName[]) {
  const k = `susurro.keymask.${n}`;
  const m = localStorage.getItem(k);
  if (m?.includes("…")) localStorage.setItem(k, `••••${m.slice(-4)}`);
}

export default function Providers() {
  const s = settingsStore.use();
  const keys = keyStore.use();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [lastTest, setLastTest] = useState<Partial<Record<SttProvider, ProviderTest>>>({});
  const [sttModels, setSttModels] = useState<OrModel[]>([]);
  const loadModels = () => api.whisperModels().then(setModels);
  useEffect(() => void loadModels(), []);
  if (!s || !keys) return <Title parts={S.title.providers} />;

  const p = s.stt.provider;
  const activeModel = models.find((m) => m.file === s.stt.whisper_model && m.downloaded);
  const state: Record<SttProvider, string> = {
    whisper_local: activeModel ? `ready · ${modelName(activeModel.file)}` : models.some((m) => m.downloaded) ? "pick a model" : "no model yet",
    openai_compat: lastTest.openai_compat && !lastTest.openai_compat.ok ? "not reachable" : (hostOf(s.stt.openai_compat.base_url) ?? "invalid URL"),
    elevenlabs: keys.elevenlabs ? "ready" : S.help.noKeys,
    elevenlabs_realtime: keys.elevenlabs ? "ready" : S.help.noKeys,
    openai_realtime: keys.openai ? "ready" : S.help.noKeys,
    openrouter: !keys.openrouter ? S.help.noKeys : s.stt.openrouter_model ? "ready" : "pick a model",
  };
  const what: Record<SttProvider, [string, string]> = {
    whisper_local: ["Local Whisper", "on this Mac · private · offline"],
    openai_compat: ["Local server", "OpenAI-compatible"],
    elevenlabs: ["ElevenLabs Scribe", "cloud"],
    elevenlabs_realtime: ["Scribe Realtime", "cloud · words while you speak"],
    openai_realtime: ["OpenAI Realtime", "cloud · gpt-live-transcribe · words while you speak"],
    openrouter: ["OpenRouter", "cloud STT model"],
  };
  // An English-only (.en) Whisper model transcribes other languages as English: warn where the language is picked.
  const usesWhisper = p === "whisper_local" || s.stt.local_fallback;
  const enOnly = usesWhisper && s.language !== "en" && /\.en[-.]/.test(s.stt.whisper_model);
  const langName = s.language === "auto" ? "other languages" : (LANGUAGES.find(([v]) => v === s.language)?.[1] ?? s.language);
  const onTest = (r: ProviderTest) => setLastTest((t) => ({ ...t, [p]: r }));
  const sttPick = sttModels.find((m) => m.id === s.stt.openrouter_model);

  return (
    <>
      <Title parts={S.title.providers} />
      <Label>Speech to text</Label>
      <div className="card" role="radiogroup" aria-label="Speech to text provider" onKeyDown={radioKeys}>
        {(Object.keys(what) as SttProvider[]).map((id) => {
          const sel = id === p;
          const ready = state[id].startsWith("ready") || (id === "openai_compat" && lastTest.openai_compat?.ok);
          return (
            <div className="prov" key={id}>
              <button className="prov-head" role="radio" aria-checked={sel} tabIndex={sel ? 0 : -1} onClick={() => !sel && save((x) => void (x.stt.provider = id))}>
                <span className="radio" />
                <span className="name">{what[id][0]}</span>
                {(id === "elevenlabs_realtime" || id === "openai_realtime") && <span className="badge-chip rec">LIVE</span>}
                <span className="small">{what[id][1]}</span>
                <span className="what">{sel && ready ? "" : state[id]}</span>
                <Dot tone={sel && ready ? "ok" : "idle"} />
              </button>
              <div className="expand" style={{ gridTemplateRows: sel ? "1fr" : "0fr" }}>
                <div>{sel && <div className="prov-body">{body(id)}</div>}</div>
              </div>
            </div>
          );
        })}
      </div>
      <div className="lang-row">
        <span className="label">Language</span>
        <select className="input" value={s.language} onChange={(e) => save((x) => void (x.language = e.target.value))} aria-label="Language">
          {LANGUAGES.map(([v, n]) => (
            <option key={v} value={v}>
              {n}
            </option>
          ))}
        </select>
        <span className="small">{S.help.language}</span>
      </div>
      {enOnly && (
        <p className="note" role="status">
          <AlertTriangle size={14} strokeWidth={1.5} className="warn-text" /> {modelName(s.stt.whisper_model)} only understands English.
          {" "}Pick a multilingual model to dictate in {langName}.
        </p>
      )}
      {p !== "whisper_local" && (
        <div className="lang-row">
          <Toggle label="Fall back to Local Whisper" on={s.stt.local_fallback} onChange={(v) => save((x) => void (x.stt.local_fallback = v))} />
          <span className="small">If the cloud fails, transcribe with Local Whisper (needs a downloaded model).</span>
        </div>
      )}

      <Label>Cleanup (LLM)</Label>
      <Cleanup s={s} hasKey={keys.openrouter} />

      <Label right={<span className="small">stored in keys.json, only readable by you</span>}>Keys</Label>
      <div className="card rows keys">
        <KeyRow name="openrouter" title="OpenRouter" s={s} saved={keys.openrouter} />
        <KeyRow name="elevenlabs" title="ElevenLabs" s={s} saved={keys.elevenlabs} />
        <KeyRow name="openai" title="OpenAI" sub="OpenAI Realtime" s={s} saved={keys.openai} />
        <KeyRow name="local_stt" title="Local STT server" sub="optional bearer token" s={s} saved={keys.local_stt} />
        <KeyRow name="local_llm" title="Local LLM server" sub="optional bearer token" s={s} saved={keys.local_llm} />
      </div>
    </>
  );

  function body(id: SttProvider) {
    switch (id) {
      case "whisper_local":
        return (
          <>
            <div className="label" style={{ marginBottom: 8 }}>
              Model
            </div>
            <Models models={models} active={s!.stt.whisper_model} reload={loadModels} />
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">
                Models live in <span className="mono">~/Library/Application Support/app.susurro.desktop/models</span>
              </span>
              <TestButton which="stt" onResult={onTest} />
            </div>
          </>
        );
      case "openai_compat":
        return (
          <>
            <div className="form-grid">
              <span className="label">URL</span>
              <TextField mono value={s!.stt.openai_compat.base_url} check={checkUrl} onSave={(v) => save((x) => void (x.stt.openai_compat.base_url = v))} />
              <span className="label">Model</span>
              <TextField mono value={s!.stt.openai_compat.model} onSave={(v) => save((x) => void (x.stt.openai_compat.model = v))} />
              <span className="label">Hotwords</span>
              <span className="inline">
                <Toggle label="Send hotwords" on={s!.stt.openai_compat.send_hotwords} onChange={(v) => save((x) => void (x.stt.openai_compat.send_hotwords = v))} />
                <span className="small">Send your words as hotwords (speaches only)</span>
              </span>
            </div>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{S.help.localServer}</span>
              <TestButton which="stt" onResult={onTest} />
            </div>
          </>
        );
      case "elevenlabs":
        return (
          <>
            <div className="form-grid">
              <span className="label">Model</span>
              <select className="input" style={{ width: 200 }} value={s!.stt.elevenlabs.model_id} onChange={(e) => save((x) => void (x.stt.elevenlabs.model_id = e.target.value))}>
                <option value="scribe_v2">scribe_v2</option>
                <option value="scribe_v1">scribe_v1</option>
              </select>
              <span className="label">Keyterms</span>
              <span className="inline">
                <Toggle label="Send keyterms" on={s!.stt.elevenlabs.send_keyterms} onChange={(v) => save((x) => void (x.stt.elevenlabs.send_keyterms = v))} />
                <span className="small">Send up to {s!.stt.elevenlabs.max_keyterms} of your words. ElevenLabs bills keyterms +20%.</span>
              </span>
            </div>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{keys!.elevenlabs ? "Key saved." : "Add your ElevenLabs key below."}</span>
              <TestButton which="stt" onResult={onTest} disabled={!keys!.elevenlabs} />
            </div>
          </>
        );
      case "elevenlabs_realtime":
        return (
          <>
            <p className="small" style={{ marginTop: 0 }}>
              Streams your voice over a WebSocket and pastes about 0.2 s after you let go (MAI: ~0.7 s, Local Whisper: ~0.4 s).
              If the stream fails, the clip goes to {keys!.openrouter ? "OpenRouter" : "ElevenLabs Scribe"}. Uses the ElevenLabs key and the Keyterms toggle of ElevenLabs Scribe.
            </p>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{keys!.elevenlabs ? "Key saved." : "Add your ElevenLabs key below."}</span>
              <TestButton which="stt" onResult={onTest} disabled={!keys!.elevenlabs} />
            </div>
          </>
        );
      case "openai_realtime":
        return (
          <>
            <p className="small" style={{ marginTop: 0 }}>
              Streams your voice to OpenAI&apos;s live transcription (gpt-live-transcribe) over a WebSocket. Not measured yet against Scribe Realtime.
              If the stream fails, the clip goes to {keys!.openrouter ? "OpenRouter" : "ElevenLabs Scribe"}. Uses the OpenAI key.
            </p>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{keys!.openai ? "Key saved." : "Add your OpenAI key below."}</span>
              <TestButton which="stt" onResult={onTest} disabled={!keys!.openai} />
            </div>
          </>
        );
      case "openrouter":
        return (
          <>
            <div className="form-grid">
              <span className="label">Model</span>
              <ModelPicker kind="stt" hasKey={keys!.openrouter} value={s!.stt.openrouter_model} onChange={(v) => save((x) => void (x.stt.openrouter_model = v))} onList={setSttModels} />
            </div>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{sttPick?.keywords ? S.help.keywordsStt(shortName(sttPick.name)) : S.help.openrouterStt}</span>
              <TestButton which="stt" onResult={onTest} disabled={!keys!.openrouter} />
            </div>
          </>
        );
    }
  }
}

function Cleanup({ s, hasKey }: { s: Settings; hasKey: boolean }) {
  const prov = s.cleanup.level === "none" ? "off" : s.cleanup.provider;
  return (
    <div className="card pad">
      <div className="form-grid">
        <span className="label">Provider</span>
        <select
          className="input"
          style={{ width: 240 }}
          value={prov}
          aria-label="Cleanup provider"
          onChange={(e) => {
            const v = e.target.value;
            save((x) => {
              if (v === "off") x.cleanup.level = "none";
              else {
                x.cleanup.provider = v as "openrouter" | "local";
                if (x.cleanup.level === "none") x.cleanup.level = "light";
              }
            });
          }}
        >
          <option value="off">Off</option>
          <option value="openrouter">OpenRouter</option>
          <option value="local">Local server (OpenAI-compatible)</option>
        </select>
        {prov === "openrouter" && (
          <>
            <span className="label">Model</span>
            <ModelPicker kind="chat" hasKey={hasKey} value={s.cleanup.openrouter_model} onChange={(v) => save((x) => void (x.cleanup.openrouter_model = v))} />
          </>
        )}
        {prov === "local" && (
          <>
            <span className="label">URL</span>
            <TextField mono value={s.cleanup.local_base_url} check={checkUrl} onSave={(v) => save((x) => void (x.cleanup.local_base_url = v))} />
            <span className="label">Model</span>
            <TextField mono value={s.cleanup.local_model} placeholder="qwen3-8b" onSave={(v) => save((x) => void (x.cleanup.local_model = v))} />
          </>
        )}
      </div>
      <div className="inline" style={{ marginTop: 14, justifyContent: "space-between" }}>
        <span className="small">{prov === "off" ? "susurro pastes the transcript with your corrections, no rewrite." : prov === "local" ? "LM Studio http://127.0.0.1:1234/v1 works here. " + S.help.cleanupFails : S.help.cleanupFails}</span>
        {prov !== "off" && <TestButton which="cleanup" disabled={prov === "openrouter" && !hasKey} />}
      </div>
    </div>
  );
}

/** "Microsoft AI: MAI-Transcribe 2" -> "MAI-Transcribe 2" (the vendor shows in the id on hover). */
const shortName = (n: string) => n.replace(/^[^:]+:\s*/, "");
const price = (m: OrModel) => (m.price_hour == null ? "per token" : `$${m.price_hour < 0.1 ? m.price_hour.toFixed(3) : m.price_hour.toFixed(2)}/h`);

/** OpenRouter model picker for STT (VOICE_CONTROL.md E.4: ranked models first, MAI-Transcribe 2 recommended, price
 *  per hour, KEYWORDS on models that take the dictionary words) and for cleanup (470+ chat models: a native select is
 *  no way to find one). Combobox: type to filter, arrows move, Enter picks, Esc closes. */
function ModelPicker({ kind, hasKey, value, onChange, onList }: { kind: "stt" | "chat"; hasKey: boolean; value: string; onChange: (v: string) => void; onList?: (l: OrModel[]) => void }) {
  const [list, setList] = useState<OrModel[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState("");
  const [at, setAt] = useState(0);
  const root = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (hasKey) api.openrouterModels(kind).then((l) => (setList(l), onList?.(l)), (e) => setErr(String(e)));
  }, [hasKey, kind]);
  useEffect(() => {
    if (!open) return;
    const down = (e: PointerEvent) => !root.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener("pointerdown", down);
    return () => document.removeEventListener("pointerdown", down);
  }, [open]);
  const shown = (list ?? []).filter((m) => !q || `${m.name} ${m.id}`.toLowerCase().includes(q.toLowerCase()));
  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${at}"]`)?.scrollIntoView({ block: "nearest" });
  }, [at, open]);
  if (!hasKey) return <span className="small">Add an OpenRouter key below to pick a model.</span>;
  if (err) return <TextField mono value={value} placeholder={kind === "stt" ? "microsoft/mai-transcribe-2" : "google/gemini-2.5-flash-lite"} onSave={onChange} />;
  if (!list)
    return (
      <span className="inline small">
        <Spinner /> Loading models…
      </span>
    );
  const cur = list.find((m) => m.id === value);
  const pick = (m: OrModel | undefined) => {
    if (!m) return;
    onChange(m.id);
    setOpen(false);
  };
  const toggle = () => {
    setQ("");
    setAt(Math.max(0, list.findIndex((m) => m.id === value)));
    setOpen(!open);
  };
  const keys = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setAt((i) => Math.min(shown.length - 1, Math.max(0, i + (e.key === "ArrowDown" ? 1 : -1))));
    } else if (e.key === "Enter") {
      e.preventDefault();
      pick(shown[at]);
    } else if (e.key === "Escape") {
      e.stopPropagation();
      setOpen(false);
    }
  };
  const firstAll = shown.findIndex((m) => m.rank == null);
  return (
    <div className="stt-pick" ref={root}>
      <button className="input stt-trigger" onClick={toggle} aria-haspopup="listbox" aria-expanded={open} aria-label={`Model: ${cur ? shortName(cur.name) : value || "none"}`}>
        <span className="ellipsis">{cur ? shortName(cur.name) : value || "Pick a model"}</span>
        {cur?.recommended && <span className="badge-chip rec">RECOMMENDED</span>}
        <ChevronDown size={14} strokeWidth={1.5} className="chev" />
      </button>
      {open && (
        <div className="popover left stt-pop">
          <label className="stt-search">
            <Search size={14} strokeWidth={1.5} />
            <input
              autoFocus
              value={q}
              placeholder="Search models…"
              role="combobox"
              aria-expanded
              aria-controls={`${kind}-list`}
              aria-activedescendant={shown[at] ? `${kind}-${at}` : undefined}
              aria-label="Search models"
              onChange={(e) => (setQ(e.target.value), setAt(0))}
              onKeyDown={keys}
            />
          </label>
          <div className="stt-list" id={`${kind}-list`} role="listbox" aria-label={kind === "stt" ? "Speech to text models" : "Cleanup models"} ref={listRef}>
            {shown.map((m, i) => (
              <div key={m.id} className="stt-group-wrap">
                {i === 0 && m.rank != null && (
                  <div className="stt-group label" role="presentation">
                    <span>Ranked</span>
                    <span>OpenRouter top 10</span>
                  </div>
                )}
                {i === firstAll && (
                  <div className="stt-group label" role="presentation">
                    <span>All models</span>
                  </div>
                )}
                <div
                  id={`${kind}-${i}`}
                  data-i={i}
                  role="option"
                  aria-selected={m.id === value}
                  className={`stt-opt${i === at ? " at" : ""}`}
                  onPointerMove={() => setAt(i)}
                  onClick={() => pick(m)}
                  title={m.id}
                >
                  <span className="rank">{m.rank != null ? String(m.rank).padStart(2, "0") : ""}</span>
                  <span className="nm ellipsis">
                    {shortName(m.name)}
                  </span>
                  <span className="chips-r">
                    {m.recommended && <span className="badge-chip rec">RECOMMENDED</span>}
                    {m.keywords && <span className="badge-chip">KEYWORDS</span>}
                  </span>
                  <span className="price">{kind === "stt" && price(m)}</span>
                  <span className="sel">{m.id === value && <Check size={13} strokeWidth={2} />}</span>
                </div>
              </div>
            ))}
            {!shown.length && <p className="small stt-none">No model matches “{q}”.</p>}
          </div>
        </div>
      )}
    </div>
  );
}

function TextField({ value, onSave, check, mono, placeholder }: { value: string; onSave: (v: string) => void; check?: (v: string) => string | null; mono?: boolean; placeholder?: string }) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  const commit = () => {
    const t = v.trim();
    if (t === value) return;
    const err = check?.(t);
    if (err) return void (toast(err, { kind: "error" }), setV(value));
    onSave(t);
  };
  return <input className={`input${mono ? " mono" : ""}`} style={{ maxWidth: 360 }} value={v} placeholder={placeholder} onChange={(e) => setV(e.target.value)} onBlur={commit} onKeyDown={(e) => e.key === "Enter" && commit()} />;
}

/** Test -> spinner -> "✓ 340 ms" / "✕ 401 unauthorized". The number is measured by the backend. */
export function TestButton({ which, disabled, onResult }: { which: "stt" | "cleanup"; disabled?: boolean; onResult?: (r: ProviderTest) => void }) {
  const [r, setR] = useState<(ProviderTest & { at: number }) | "busy" | null>(null);
  const [, tick] = useState(0);
  useEffect(() => {
    const t = setInterval(() => tick((n) => n + 1), 30_000);
    return () => clearInterval(t);
  }, []);
  const run = async () => {
    setR("busy");
    let res: ProviderTest;
    try {
      res = await api.testProvider(which);
    } catch (e) {
      res = { ok: false, latency_ms: 0, text: "", error: String(e) };
    }
    setR({ ...res, at: Date.now() });
    onResult?.(res);
  };
  return (
    <span className="inline" style={{ gap: 10, flexWrap: "nowrap", marginLeft: "auto" }}>
      {r && r !== "busy" && (
        <span className={`test-result ${r.ok ? "ok-text" : "danger-text"}`} title={r.ok ? r.text : (r.error ?? "")}>
          {r.ok ? <Check size={13} strokeWidth={2} /> : <X size={13} strokeWidth={2} />}
          <span className="ellipsis" style={{ maxWidth: 260 }}>{r.ok ? `${r.latency_ms} ms` : r.error}</span>
          <span className="dim">· {ago(r.at)}</span>
        </span>
      )}
      <button className="btn secondary sm test-btn" onClick={run} disabled={disabled || r === "busy"}>
        {r === "busy" ? <Spinner /> : "Test"}
      </button>
    </span>
  );
}

/** Downloads outlive the screen: a remounted Models picks them up here (Rust keeps downloading either way). */
const inflight = new Map<string, Promise<void>>();

function Models({ models, active, reload }: { models: ModelInfo[]; active: string; reload: () => void }) {
  const [prog, setProg] = useState<Record<string, { done: number; total: number; rate: number; t: number }>>({});
  const [errs, setErrs] = useState<Record<string, string>>({});
  const track = (file: string, p: Promise<void>) => {
    setProg((x) => (x[file] ? x : { ...x, [file]: { done: 0, total: 1, rate: 0, t: performance.now() } }));
    p.catch((e) => setErrs((x) => ({ ...x, [file]: String(e) }))).finally(() => {
      setProg(({ [file]: _, ...x }) => x);
      reload();
    });
  };
  useEffect(() => inflight.forEach((p, file) => track(file, p)), []);
  useEffect(() => {
    const off = on("model_progress", (m) =>
      setProg((p) => {
        if (!inflight.has(m.file)) return p; // a late event after the command returned must not bring the bar back
        const prev = p[m.file];
        const now = performance.now();
        const inst = prev ? ((m.done - prev.done) / (now - prev.t)) * 1000 : 0;
        return { ...p, [m.file]: { done: m.done, total: m.total, rate: prev ? prev.rate * 0.7 + inst * 0.3 : inst, t: now } };
      }),
    );
    return () => void off.then((f) => f());
  }, []);
  const download = (file: string) => {
    setErrs(({ [file]: _, ...e }) => e);
    const p = api.whisperDownload(file).finally(() => inflight.delete(file));
    inflight.set(file, p);
    track(file, p);
  };
  return (
    <div className="models">
      {models.map((m) => {
        const p = prog[m.file];
        const inUse = m.downloaded && m.file === active;
        const err = errs[m.file];
        return (
          <div className="row" key={m.file}>
            <span className="mark">{inUse ? <Dot tone="ok" /> : m.downloaded ? <Check size={13} strokeWidth={2} className="ok-text" /> : null}</span>
            <span className="mname">{modelName(m.file)}</span>
            <span className="size">{m.size_mb >= 1000 ? `${(m.size_mb / 1024).toFixed(1)} GB` : `${m.size_mb} MB`}</span>
            {p ? (
              <Progress {...p} />
            ) : err ? (
              <span className="small danger-text ellipsis" style={{ flex: 1 }} role="alert">
                {err}
              </span>
            ) : (
              <span className="small note ellipsis">{m.note}</span>
            )}
            <span className="end">
              {inUse ? (
                <span className="mono small">in use</span>
              ) : m.downloaded ? (
                <button className="btn secondary sm" onClick={() => save((x) => void (x.stt.whisper_model = m.file))}>
                  Use
                </button>
              ) : !p ? (
                <button className="btn secondary sm" onClick={() => download(m.file)}>
                  {err ? "Retry" : "Download"}
                </button>
              ) : null}
              {m.downloaded && (
                <button
                  className="icon-btn sm del"
                  aria-label={`Delete ${modelName(m.file)}`}
                  data-tip={inUse ? "In use: pick another model first" : "Delete"}
                  disabled={inUse}
                  onClick={() => api.whisperDelete(m.file).then(reload)}
                >
                  <X size={14} strokeWidth={1.5} />
                </button>
              )}
            </span>
          </div>
        );
      })}
    </div>
  );
}

function Progress({ done, total, rate }: { done: number; total: number; rate: number }) {
  const pct = Math.floor((done / total) * 100);
  const left = rate > 0 ? (total - done) / rate : 0;
  const ref = useRef<HTMLDivElement>(null);
  return (
    <div className="progress" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} aria-label="Download progress">
      <div className="track">
        <div className="fill" ref={ref} style={{ width: `${pct}%` }} />
      </div>
      <span className="mono">
        {pct}% · {(rate / 1_048_576).toFixed(0)} MB/s{left > 0 ? ` · ${Math.floor(left / 60)}:${String(Math.round(left % 60)).padStart(2, "0")} left` : ""}
      </span>
    </div>
  );
}

function KeyRow({ name, title, sub, s, saved }: { name: KeyName; title: string; sub?: string; s: Settings; saved: boolean }) {
  const [v, setV] = useState("");
  const [show, setShow] = useState(false);
  const mask = localStorage.getItem(`susurro.keymask.${name}`);
  const which: "stt" | "cleanup" | null =
    name === "elevenlabs"
      ? s.stt.provider === "elevenlabs" || s.stt.provider === "elevenlabs_realtime"
        ? "stt"
        : null
      : name === "openai"
        ? s.stt.provider === "openai_realtime"
          ? "stt"
          : null
      : name === "openrouter"
        ? s.cleanup.level !== "none" && s.cleanup.provider === "openrouter"
          ? "cleanup"
          : s.stt.provider === "openrouter"
            ? "stt"
            : null
        : name === "local_stt"
          ? s.stt.provider === "openai_compat"
            ? "stt"
            : null
          : s.cleanup.level !== "none" && s.cleanup.provider === "local"
            ? "cleanup"
            : null;
  const store = async (value: string | null) => {
    const status = await api.setApiKey(name, value);
    keyStore.set(status);
    if (value) localStorage.setItem(`susurro.keymask.${name}`, value.length > 12 ? `••••${value.slice(-4)}` : "••••");
    else localStorage.removeItem(`susurro.keymask.${name}`);
    setV("");
    setShow(false);
  };
  return (
    <div className="row">
      <span className="kname">
        {title}
        {sub && <small>{sub}</small>}
      </span>
      <form
        className="secret"
        onSubmit={(e) => {
          e.preventDefault();
          if (v.trim()) store(v.trim());
        }}
      >
        <input
          className="input mono"
          type={show ? "text" : "password"}
          value={v}
          placeholder={saved ? (mask ?? "saved") : "Paste key…"}
          onChange={(e) => setV(e.target.value.trim())}
          aria-label={`${title} key`}
          autoComplete="off"
          spellCheck={false}
        />
        <button type="button" className="icon-btn sm" aria-label={show ? "Hide key" : "Show key"} onClick={() => setShow(!show)} disabled={!v}>
          {show ? <EyeOff size={14} strokeWidth={1.5} /> : <Eye size={14} strokeWidth={1.5} />}
        </button>
      </form>
      <span className="inline" style={{ marginLeft: "auto", flexWrap: "nowrap" }}>
        {v ? (
          <button className="btn secondary sm" onClick={() => store(v.trim())}>
            Save
          </button>
        ) : saved ? (
          <>
            {which ? <TestButton which={which} /> : <span className="small">not in use</span>}
            <button className="icon-btn sm del" aria-label={`Remove ${title} key`} data-tip="Remove key" onClick={() => store(null)}>
              <X size={14} strokeWidth={1.5} />
            </button>
          </>
        ) : (
          <span className="small">{sub ? "none" : S.help.noKeys}</span>
        )}
      </span>
    </div>
  );
}
