// DESIGN 6.5
import { useEffect, useRef, useState } from "react";
import { Check, Eye, EyeOff, X } from "lucide-react";
import { api, on, type KeyName, type ModelInfo, type OrModel, type ProviderTest, type Settings, type SttProvider } from "../lib/api";
import { ago, keyStore, modelName, saveSettings, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Dot, Label, Spinner, Title, Toggle, toast } from "../ui";

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

const save = (f: (s: Settings) => void) => saveSettings(f).catch((e) => toast(String(e), { kind: "error" }));

export default function Providers() {
  const s = settingsStore.use();
  const keys = keyStore.use();
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [lastTest, setLastTest] = useState<Partial<Record<SttProvider, ProviderTest>>>({});
  const loadModels = () => api.whisperModels().then(setModels);
  useEffect(() => void loadModels(), []);
  if (!s || !keys) return <Title parts={S.title.providers} />;

  const p = s.stt.provider;
  const activeModel = models.find((m) => m.file === s.stt.whisper_model && m.downloaded);
  const state: Record<SttProvider, string> = {
    whisper_local: activeModel ? `ready · ${modelName(activeModel.file)}` : models.some((m) => m.downloaded) ? "pick a model" : "no model yet",
    openai_compat: lastTest.openai_compat && !lastTest.openai_compat.ok ? "not reachable" : new URL(s.stt.openai_compat.base_url, "http://x").host,
    elevenlabs: keys.elevenlabs ? "ready" : S.help.noKeys,
    openrouter: !keys.openrouter ? S.help.noKeys : s.stt.openrouter_model ? "ready" : "pick a model",
  };
  const what: Record<SttProvider, [string, string]> = {
    whisper_local: ["Local Whisper", "on this Mac · private · offline"],
    openai_compat: ["Local server", "OpenAI-compatible"],
    elevenlabs: ["ElevenLabs Scribe", "cloud"],
    openrouter: ["OpenRouter", "cloud STT model"],
  };
  const onTest = (r: ProviderTest) => setLastTest((t) => ({ ...t, [p]: r }));

  return (
    <>
      <Title parts={S.title.providers} />
      <Label>Speech to text</Label>
      <div className="card" role="radiogroup" aria-label="Speech to text provider">
        {(Object.keys(what) as SttProvider[]).map((id) => {
          const sel = id === p;
          const ready = state[id].startsWith("ready") || (id === "openai_compat" && lastTest.openai_compat?.ok);
          return (
            <div className="prov" key={id}>
              <button className="prov-head" role="radio" aria-checked={sel} onClick={() => !sel && save((x) => void (x.stt.provider = id))}>
                <span className="radio" />
                <span className="name">{what[id][0]}</span>
                <span className="small">{what[id][1]}</span>
                <span className="what">{sel ? "" : state[id]}</span>
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
      {p !== "whisper_local" && (
        <div className="lang-row">
          <Toggle label="Fall back to Local Whisper" on={s.stt.local_fallback} onChange={(v) => save((x) => void (x.stt.local_fallback = v))} />
          <span className="small">If the cloud fails, transcribe with Local Whisper (needs a downloaded model).</span>
        </div>
      )}

      <Label>Cleanup (LLM)</Label>
      <Cleanup s={s} hasKey={keys.openrouter} />

      <Label right={<span className="small">stored in macOS Keychain</span>}>Keys</Label>
      <div className="card rows keys">
        <KeyRow name="openrouter" title="OpenRouter" s={s} saved={keys.openrouter} />
        <KeyRow name="elevenlabs" title="ElevenLabs" s={s} saved={keys.elevenlabs} />
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
              <TextField mono value={s!.stt.openai_compat.base_url} onSave={(v) => save((x) => void (x.stt.openai_compat.base_url = v))} />
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
              <span className="small">{keys!.elevenlabs ? "Key saved in Keychain." : "Add your ElevenLabs key below."}</span>
              <TestButton which="stt" onResult={onTest} disabled={!keys!.elevenlabs} />
            </div>
          </>
        );
      case "openrouter":
        return (
          <>
            <div className="form-grid">
              <span className="label">Model</span>
              <OrPicker kind="stt" hasKey={keys!.openrouter} value={s!.stt.openrouter_model} onChange={(v) => save((x) => void (x.stt.openrouter_model = v))} />
            </div>
            <div className="inline" style={{ marginTop: 12, justifyContent: "space-between" }}>
              <span className="small">{S.help.openrouterStt}</span>
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
            <OrPicker kind="chat" hasKey={hasKey} value={s.cleanup.openrouter_model} onChange={(v) => save((x) => void (x.cleanup.openrouter_model = v))} />
          </>
        )}
        {prov === "local" && (
          <>
            <span className="label">URL</span>
            <TextField mono value={s.cleanup.local_base_url} onSave={(v) => save((x) => void (x.cleanup.local_base_url = v))} />
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

function OrPicker({ kind, hasKey, value, onChange }: { kind: "stt" | "chat"; hasKey: boolean; value: string; onChange: (v: string) => void }) {
  const [list, setList] = useState<OrModel[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    if (hasKey) api.openrouterModels(kind).then(setList, (e) => setErr(String(e)));
  }, [hasKey, kind]);
  if (!hasKey) return <span className="small">Add an OpenRouter key below to pick a model.</span>;
  if (err) return <TextField mono value={value} placeholder="google/gemini-2.5-flash" onSave={onChange} />;
  if (!list)
    return (
      <span className="inline small">
        <Spinner /> Loading models…
      </span>
    );
  return (
    <select className="input mono" style={{ maxWidth: 360 }} value={value} onChange={(e) => onChange(e.target.value)} aria-label="Model">
      {!value && <option value="">Pick a model</option>}
      {value && !list.some((m) => m.id === value) && <option value={value}>{value}</option>}
      {list.map((m) => (
        <option key={m.id} value={m.id}>
          {m.id}
        </option>
      ))}
    </select>
  );
}

function TextField({ value, onSave, mono, placeholder }: { value: string; onSave: (v: string) => void; mono?: boolean; placeholder?: string }) {
  const [v, setV] = useState(value);
  useEffect(() => setV(value), [value]);
  const commit = () => v.trim() !== value && onSave(v.trim());
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
    <span className="inline" style={{ gap: 10, flexWrap: "nowrap" }}>
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

function Models({ models, active, reload }: { models: ModelInfo[]; active: string; reload: () => void }) {
  const [prog, setProg] = useState<Record<string, { done: number; total: number; rate: number; t: number }>>({});
  const [errs, setErrs] = useState<Record<string, string>>({});
  useEffect(() => {
    const off = on("model_progress", (m) =>
      setProg((p) => {
        const prev = p[m.file];
        const now = performance.now();
        const inst = prev ? ((m.done - prev.done) / (now - prev.t)) * 1000 : 0;
        return { ...p, [m.file]: { done: m.done, total: m.total, rate: prev ? prev.rate * 0.7 + inst * 0.3 : inst, t: now } };
      }),
    );
    return () => void off.then((f) => f());
  }, []);
  const download = async (file: string) => {
    setErrs(({ [file]: _, ...e }) => e);
    setProg((p) => ({ ...p, [file]: { done: 0, total: 1, rate: 0, t: performance.now() } }));
    try {
      await api.whisperDownload(file);
    } catch (e) {
      setErrs((x) => ({ ...x, [file]: String(e) }));
    }
    setProg(({ [file]: _, ...p }) => p);
    reload();
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
      ? s.stt.provider === "elevenlabs"
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
    if (value) localStorage.setItem(`susurro.keymask.${name}`, value.length > 12 ? `${value.slice(0, 6)}…${value.slice(-4)}` : "••••");
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
          placeholder={saved ? (mask ?? "saved in Keychain") : "Paste key…"}
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
