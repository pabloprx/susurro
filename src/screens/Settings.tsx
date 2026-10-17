// DESIGN 6.8
import { useState, type ReactNode } from "react";
import { Copy } from "lucide-react";
import { api, type Settings as T } from "../lib/api";
import { applyTheme, getTheme, saveSettings, settingsStore, type Theme } from "../lib/store";
import { S } from "../lib/strings";
import { copy, Label, Segmented, Sheet, Title, Toggle, toast } from "../ui";

const DATA_DIR = "~/Library/Application Support/app.susurro.desktop";
const save = (f: (s: T) => void) => saveSettings(f).catch((e) => toast(String(e), { kind: "error" }));

function Row({ title, help, children }: { title: string; help?: string; children: ReactNode }) {
  return (
    <div className="set-row">
      <div className="txt">
        <div>{title}</div>
        {help && <div className="help">{help}</div>}
      </div>
      {children}
    </div>
  );
}

export default function Settings() {
  const s = settingsStore.use();
  const [theme, setTheme] = useState<Theme>(getTheme());
  const [confirm, setConfirm] = useState(false);
  if (!s) return <Title parts={S.title.settings} />;
  const keep = !s.privacy.store_audio ? "0" : String(s.privacy.audio_retention_days);

  return (
    <>
      <Title parts={S.title.settings} />
      <Label>General</Label>
      <div className="card">
        <Row title="Launch at login" help="susurro waits in the menu bar.">
          <Toggle label="Launch at login" on={s.autostart} onChange={(v) => save((x) => void (x.autostart = v))} />
        </Row>
        <Row title="Sounds" help="Tink at start, Pop at stop, Basso on error.">
          <Toggle label="Sounds" on={s.recording.sounds} onChange={(v) => save((x) => void (x.recording.sounds = v))} />
        </Row>
        <Row title="Theme">
          <Segmented<Theme>
            label="Theme"
            value={theme}
            onChange={(t) => (setTheme(t), applyTheme(t))}
            options={[
              { value: "system", label: "System" },
              { value: "dark", label: "Dark" },
              { value: "light", label: "Light" },
            ]}
          />
        </Row>
        <Row title="Show idle pill" help="A thin bar above the Dock. Click it to start hands-free.">
          <Toggle label="Show idle pill" on={s.overlay.show_idle} onChange={(v) => save((x) => void (x.overlay.show_idle = v))} />
        </Row>
        <Row title="Max recording length" help="susurro warns you one minute before the end.">
          <select className="input" value={s.recording.max_seconds} onChange={(e) => save((x) => void (x.recording.max_seconds = +e.target.value))} aria-label="Max recording length">
            {[120, 300, 600].map((v) => (
              <option key={v} value={v}>
                {v / 60} minutes
              </option>
            ))}
          </select>
        </Row>
      </div>

      <Label>Dictation</Label>
      <div className="card">
        <Row title="Spoken punctuation" help={"Say “comma”, “period”, “new line”."}>
          <Toggle label="Spoken punctuation" on={s.spoken_punctuation} onChange={(v) => save((x) => void (x.spoken_punctuation = v))} />
        </Row>
        <Row title="Fuzzy word matching" help="Catch near-misses of your dictionary words, not only exact aliases.">
          <Toggle label="Fuzzy word matching" on={s.dictionary.fuzzy} onChange={(v) => save((x) => void (x.dictionary.fuzzy = v))} />
        </Row>
        <Row title="Restore clipboard" help="Put back what you had copied after the paste.">
          <Toggle label="Restore clipboard" on={s.paste.restore_clipboard} onChange={(v) => save((x) => void (x.paste.restore_clipboard = v))} />
        </Row>
      </div>

      <Label>Data</Label>
      <div className="card">
        <Row title="Save history" help="Transcripts stay on this Mac, in a local database.">
          <Toggle label="Save history" on={s.privacy.store_history} onChange={(v) => save((x) => void (x.privacy.store_history = v))} />
        </Row>
        <Row title="Keep audio" help="Needed for Re-transcribe.">
          <Segmented
            label="Keep audio"
            value={keep}
            onChange={(v) =>
              save((x) => {
                x.privacy.store_audio = v !== "0";
                if (v !== "0") x.privacy.audio_retention_days = +v;
              })
            }
            options={[
              { value: "0", label: "Never" },
              { value: "1", label: "1 day" },
              { value: "14", label: "14 days" },
              { value: "30", label: "30 days" },
            ]}
          />
        </Row>
        <Row title="Data folder" help={DATA_DIR}>
          <button className="btn secondary sm" onClick={() => copy(DATA_DIR)}>
            <Copy size={13} strokeWidth={1.5} /> Copy path
          </button>
        </Row>
        <Row title="Clear history" help="Deletes every transcript and its audio. Your dictionary stays.">
          <button className="btn danger sm" onClick={() => setConfirm(true)}>
            Delete all history…
          </button>
        </Row>
      </div>

      <Label>Experimental</Label>
      <div className="card">
        <Row title="Press-enter command" help={"End with “press enter” to send the message after the paste."}>
          <Toggle label="Press-enter command" on={s.paste.press_enter_command} onChange={(v) => save((x) => void (x.paste.press_enter_command = v))} />
        </Row>
      </div>

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
              setConfirm(false);
              toast("History deleted", { kind: "success" });
            }}
          >
            Delete all history
          </button>
        </div>
      </Sheet>
    </>
  );
}
