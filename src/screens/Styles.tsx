// DESIGN 6.4, cut to the v1 scope: one global style + cleanup level + per-app Raw list (orchestrator decision).
import { useEffect, useState } from "react";
import { Info, Plus, Trash2 } from "lucide-react";
import { api, type CleanupLevel, type Style } from "../lib/api";
import { keyStore, save, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Label, Popover, radioKeys, Segmented, Title } from "../ui";
import type { Nav } from "../App";

const SAID = "hey are you free tonight i was thinking we could grab dinner";
const STYLES: { value: Style; name: string; sub: string; sample: string }[] = [
  { value: "formal", name: "Formal", sub: "Full sentences, no slang", sample: "Hello, are you free tonight? I was thinking we could have dinner." },
  { value: "casual", name: "Casual", sub: "Clean, relaxed", sample: "Hey, are you free tonight? I was thinking we could grab dinner." },
  { value: "very_casual", name: "Very casual", sub: "Lowercase, light punctuation", sample: "hey are you free tonight? was thinking we could grab dinner" },
  { value: "excited", name: "Excited", sub: "Warm, with energy", sample: "Hey, are you free tonight? I was thinking we could grab dinner!" },
];
/** Bundle ids that ship as sensible Raw candidates when History has none yet. */
const KNOWN: Record<string, string> = {
  "com.apple.Terminal": "Terminal",
  "com.googlecode.iterm2": "iTerm",
  "com.mitchellh.ghostty": "Ghostty",
  "com.todesktop.230313mzl4w4u92": "Cursor",
  "com.microsoft.VSCode": "VS Code",
  "com.anthropic.claudefordesktop": "Claude",
  "com.openai.chat": "ChatGPT",
};

export default function Styles({ nav }: { nav: Nav }) {
  const s = settingsStore.use();
  const keys = keyStore.use();
  const [recent, setRecent] = useState<[string, string][]>([]);
  const [manual, setManual] = useState("");
  useEffect(() => {
    api.historyList(null, null, 200).then((h) => {
      const m = new Map<string, string>();
      h.forEach((x) => x.app_bundle && !m.has(x.app_bundle) && m.set(x.app_bundle, x.app_name ?? x.app_bundle));
      setRecent([...m]);
    });
  }, []);
  if (!s) return <Title parts={S.title.styles} />;

  const llmReady = s.cleanup.provider === "openrouter" ? !!keys?.openrouter && !!s.cleanup.openrouter_model : !!s.cleanup.local_base_url && !!s.cleanup.local_model;
  const toneOff = s.cleanup.level === "none" || !llmReady;
  const names = new Map<string, string>([...Object.entries(KNOWN), ...recent]);
  const current = STYLES.find((x) => x.value === s.styles.style)!;
  const candidates = [...recent, ...Object.entries(KNOWN).filter(([b]) => !recent.some(([r]) => r === b))].filter(([b]) => !s.styles.raw_apps.includes(b));
  const addRaw = (bundle: string) => save((x) => void (x.styles.raw_apps = [...new Set([...x.styles.raw_apps, bundle.trim()])]));

  return (
    <>
      <Title parts={S.title.styles} />
      <Label right={!llmReady && <button className="link small" onClick={() => nav.go("providers")}>needs an LLM provider</button>}>Cleanup level</Label>
      <Segmented<CleanupLevel>
        label="Cleanup level"
        value={s.cleanup.level}
        onChange={(v) => save((x) => void (x.cleanup.level = v))}
        options={[
          { value: "none", label: "None" },
          { value: "light", label: "Light" },
          { value: "medium", label: "Medium" },
        ]}
      />
      <p className="small" style={{ marginTop: 8 }}>
        {{ none: "Pastes what Whisper heard, with your corrections and spoken punctuation.", light: "Removes fillers and stutters, fixes grammar and punctuation. Keeps your wording.", medium: "Also rewords for clarity. Keeps your meaning and your voice." }[s.cleanup.level]}
      </p>

      <Label>Style - one voice everywhere</Label>
      <div className="card style-grid">
        <div className="style-list" role="radiogroup" aria-label="Style" onKeyDown={radioKeys}>
          {STYLES.map((o) => (
            <button key={o.value} className="style-opt" role="radio" aria-checked={o.value === s.styles.style} tabIndex={o.value === s.styles.style ? 0 : -1} onClick={() => save((x) => void (x.styles.style = o.value))}>
              <span className="radio" />
              <span>
                <div>{o.name}</div>
                <div className="sub">{o.sub}</div>
              </span>
            </button>
          ))}
        </div>
        <div className="style-sample" aria-live="polite">
          <div className="label">You say</div>
          <div className="said">&ldquo;{SAID}&rdquo;</div>
          <div className="label" style={{ marginTop: 8 }}>
            susurro writes
          </div>
          <div className="becomes selectable">{current.sample}</div>
        </div>
      </div>
      {toneOff && (
        <p className="note">
          <Info size={14} strokeWidth={1.5} /> {S.help.toneNeedsLlm}
        </p>
      )}

      <Label
        right={
          <Popover
            trigger={(t) => (
              <button className="btn ghost sm" onClick={t}>
                <Plus size={14} strokeWidth={1.5} /> Add app
              </button>
            )}
          >
            {(close) => (
              <div style={{ width: 300 }}>
                {candidates.length > 0 && <div className="label" style={{ padding: "6px 10px" }}>{recent.length ? "Recent apps" : "Suggestions"}</div>}
                {candidates.slice(0, 8).map(([b, n]) => (
                  <button key={b} className="menu-item" onClick={() => (close(), addRaw(b))}>
                    {n}
                    <span className="hint mono ellipsis" style={{ maxWidth: 150 }}>
                      {b}
                    </span>
                  </button>
                ))}
                <div className="menu-sep" />
                <form
                  style={{ display: "flex", gap: 6, padding: 4 }}
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (/^[\w-]+(\.[\w-]+)+$/.test(manual.trim())) (addRaw(manual), setManual(""), close());
                  }}
                >
                  <input className="input mono" style={{ flex: 1 }} placeholder="com.example.app" value={manual} onChange={(e) => setManual(e.target.value)} aria-label="Bundle id" />
                  <button className="btn secondary sm" style={{ height: 30 }} disabled={!/^[\w-]+(\.[\w-]+)+$/.test(manual.trim())}>
                    Add
                  </button>
                </form>
              </div>
            )}
          </Popover>
        }
      >
        Raw apps - no cleanup, no style, corrections still apply
      </Label>
      <div className="card">
        {s.styles.raw_apps.length ? (
          <div className="rows">
            {s.styles.raw_apps.map((b) => (
              <div className="row hoverable app-row" key={b}>
                <span className="name">{names.get(b) ?? b.split(".").pop()}</span>
                <span className="bundle ellipsis">{b}</span>
                <span className="meta">raw text</span>
                <span className="actions" style={{ marginLeft: 0 }}>
                  <button className="icon-btn sm del" aria-label={`Remove ${b}`} onClick={() => save((x) => void (x.styles.raw_apps = x.styles.raw_apps.filter((a) => a !== b)))}>
                    <Trash2 size={14} strokeWidth={1.5} />
                  </button>
                </span>
              </div>
            ))}
          </div>
        ) : (
          <p className="small" style={{ padding: "14px 16px" }}>
            No raw apps. Add terminals and AI chats, where you want your exact words.
          </p>
        )}
      </div>
    </>
  );
}
