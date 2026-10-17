// DESIGN 6.7, validation per PLAN 4.1 (the backend validates again on save).
import { useEffect, useRef, useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import { api, type Hotkeys } from "../lib/api";
import { isModOnly, keyLabel, keysOf, permStore, save, saveSettings, settingsStore, VOICE_CONTROL } from "../lib/store";
import { S } from "../lib/strings";
import { Kbd, Label, Title, Toggle } from "../ui";
import type { Nav } from "../App";

export type Slot = "ptt" | "hands_free" | "cancel" | "paste_last" | "copy_last" | "command" | "recents";
const NAMES: Record<Slot, string> = { ptt: "Push to talk", hands_free: "Hands-free", cancel: "Cancel", paste_last: "Paste last", copy_last: "Copy last", command: "Voice control", recents: "Recents" };
export const DEFAULTS: Hotkeys = { ptt: "F1", hands_free: null, double_tap: true, hands_free_grace_ms: 1000, cancel: "Escape", paste_last: "Ctrl+Cmd+V", copy_last: "Ctrl+Cmd+C", command: "F2", recents: "Ctrl+F1" };
/** One-click bindings per action, shown while that action records. */
export const PICKS: Record<Slot, string[]> = {
  ptt: ["F1", "Delete", "RightOption", "RightCommand"],
  hands_free: ["Alt+Space", "Ctrl+Alt+Space"],
  cancel: ["Escape", "Ctrl+."],
  paste_last: ["Ctrl+Cmd+V", "Alt+Shift+V"],
  copy_last: ["Ctrl+Cmd+C", "Alt+Shift+C"],
  command: ["F2", "F3", "Ctrl+Alt+Space"],
  recents: ["Ctrl+F1", "Ctrl+Cmd+R"],
};
const MODS: Record<string, string> = { Control: "Ctrl", Alt: "Alt", Meta: "Cmd", Shift: "Shift" };
/** KeyboardEvent.code of a modifier -> its modifier-only binding (the contract names in format.ts MOD_ONLY). */
const SIDE: Record<string, string> = {
  AltRight: "RightOption",
  AltLeft: "LeftOption",
  MetaRight: "RightCommand",
  MetaLeft: "LeftCommand",
  ControlRight: "RightControl",
  ControlLeft: "LeftControl",
  ShiftRight: "RightShift",
  ShiftLeft: "LeftShift",
};

function keyName(e: KeyboardEvent): string | null {
  const c = e.code;
  if (c.startsWith("Key")) return c.slice(3);
  if (c.startsWith("Digit")) return c.slice(5);
  if (/^F\d+$/.test(c)) return c;
  if (c.startsWith("Arrow")) return c.slice(5);
  const m: Record<string, string> = { Space: "Space", Enter: "Enter", Backspace: "Backspace", Delete: "Delete", Tab: "Tab", Escape: "Escape", Minus: "-", Equal: "=", Comma: ",", Period: ".", Slash: "/", Semicolon: ";", Quote: "'", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Backquote: "`" };
  return m[c] ?? null;
}

/** PLAN 4.1 rules. Returns the message to show, or null if the binding is fine. */
export function validate(binding: string, slot: Slot, hk: Hotkeys, codes: Set<string>): string | null {
  for (const s of Object.keys(NAMES) as Slot[]) if (s !== slot && hk[s] === binding && (VOICE_CONTROL || s !== "command")) return `Already used by ${NAMES[s]}`;
  if (isModOnly(binding)) return slot === "ptt" ? null : "Add a key: only push to talk works with one key alone";
  const parts = binding.split("+");
  const key = parts[parts.length - 1];
  const mods = parts.slice(0, -1);
  if (parts.length > 3) return "Use up to 3 keys";
  for (const m of ["Shift", "Control", "Alt", "Meta"]) if (codes.has(`${m}Left`) && codes.has(`${m}Right`)) return "Left and right of one modifier cannot be told apart";
  if (mods.length === 1 && mods[0] === "Cmd" && ["C", "V", "X", "Z", "Q"].includes(key)) return `⌘${key} is reserved by the system`;
  if (!mods.length && /^[A-Z0-9]$/.test(key)) return "Add a modifier: a single key types text";
  return null;
}

/** Records one binding while `slot` is set: a modifier pressed and released alone = modifier-only binding, with
 *  other keys = combo. Hotkeys are paused meanwhile (hotkey_capture), so Right ⌥ does not start a dictation.
 *  Escape alone, or 30 s (when Rust resumes the hotkeys by itself), cancels. */
export function useRecorder(slot: Slot | null, hk: Hotkeys | undefined, onDone: (b: string) => void, onCancel: () => void) {
  const [held, setHeld] = useState<string[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const cb = useRef({ onDone, onCancel });
  cb.current = { onDone, onCancel };
  useEffect(() => {
    setHeld([]);
    setErr(null);
    if (!slot || !hk) return;
    const codes = new Set<string>();
    let alone: string | null = null; // the only key down so far, if it is a modifier
    let keyed = false; // a non-modifier key went down during this chord
    const commit = (b: string) => {
      const e = validate(b, slot, hk, codes);
      if (e) return setErr(e);
      cb.current.onDone(b);
    };
    const down = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;
      if (!codes.size) keyed = false;
      codes.add(e.code);
      const mods = (["Control", "Alt", "Meta", "Shift"] as const).filter((m) => e.getModifierState(m)).map((m) => MODS[m]);
      if (e.key === "Escape" && !mods.length) return cb.current.onCancel();
      const side = e.key === "Fn" ? "Fn" : SIDE[e.code];
      if (side) {
        alone = codes.size === 1 ? side : null;
        return setHeld(alone ? keysOf(alone) : keysOf(mods.join("+")));
      }
      keyed = true;
      alone = null;
      const k = keyName(e);
      if (!k) return;
      const b = [...mods, k].join("+");
      setHeld(keysOf(b));
      commit(b);
    };
    const up = (e: KeyboardEvent) => {
      const was = alone;
      alone = null;
      codes.delete(e.code);
      // macOS drops the keyup of a key pressed with ⌘: forget everything once no modifier is down
      if (!(["Control", "Alt", "Meta", "Shift"] as const).some((m) => e.getModifierState(m))) codes.clear();
      if (was && !codes.size) return commit(was);
      if (!codes.size && !keyed) (setHeld([]), setErr("Add a key, or press one modifier alone"));
    };
    void api.hotkeyCapture(true).catch(() => {});
    const t = setTimeout(() => cb.current.onCancel(), 30_000);
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up, true);
    return () => {
      clearTimeout(t);
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up, true);
      void api.hotkeyCapture(false).catch(() => {});
    };
  }, [slot, hk]);
  return { held, err };
}

export default function Shortcuts({ nav }: { nav: Nav }) {
  const s = settingsStore.use();
  const perms = permStore.use();
  const [rec, setRec] = useState<Slot | null>(null);
  const [msg, setMsg] = useState<{ slot: Slot; text: string } | null>(null);

  const set = (slot: Slot, binding: string | null) => {
    setRec(null);
    return saveSettings((x) => void (x.hotkeys = { ...x.hotkeys, [slot]: binding }))
      .then(() => setMsg(null))
      .catch((e) => setMsg({ slot, text: String(e) }));
  };
  const { held, err } = useRecorder(rec, s?.hotkeys, (b) => rec && set(rec, b), () => (setRec(null), setMsg(null)));

  if (!s) return <Title parts={S.title.shortcuts} />;
  const hk = s.hotkeys;
  const ptt = keyLabel(hk.ptt);

  const Binding = ({ slot, empty }: { slot: Slot; empty?: string }) => {
    const v = hk[slot];
    const on = rec === slot;
    return (
      <button className={`bind${on ? " recording" : ""}`} onClick={() => (setRec(on ? null : slot), setMsg(null))} aria-label={`${NAMES[slot]}: ${v ?? "none"}. Press to record a new shortcut.`}>
        {on ? held.length ? <Kbd keys={held} /> : <span className="rec-hint">Press a key alone or a combo… Esc to cancel</span> : v ? <Kbd keys={keysOf(v)} /> : <span className="placeholder">{empty ?? "none"}</span>}
      </button>
    );
  };
  const Picks = ({ slot }: { slot: Slot }) =>
    rec === slot ? (
      <span className="chips picks" aria-label={`Quick picks for ${NAMES[slot]}`}>
        {PICKS[slot].map((b) => (
          <button key={b} className="chip" onClick={() => set(slot, b)} aria-label={`Use ${keyLabel(b)}`} disabled={hk[slot] === b}>
            {keysOf(b).join(" ")}
          </button>
        ))}
      </span>
    ) : null;
  const Msg = ({ slot }: { slot: Slot }) => {
    const text = rec === slot && err ? err : msg?.slot === slot ? msg.text : null;
    return text ? (
      <p className={`recorder-msg ${rec === slot ? "warn-text" : "danger-text"}`} role="alert">
        <AlertTriangle size={13} strokeWidth={1.5} /> {text}
      </p>
    ) : null;
  };
  const Reset = ({ slot }: { slot: Slot }) => (
    <button className="btn ghost sm" onClick={() => set(slot, DEFAULTS[slot])} disabled={hk[slot] === DEFAULTS[slot]}>
      Reset
    </button>
  );

  return (
    <>
      <Title parts={S.title.shortcuts} />
      <div className="card">
        <div className="sc-row">
          <div>
            <div className="label">Push to talk</div>
            <div className="sub">Hold, talk, release</div>
          </div>
          <span className="inline">
            <Binding slot="ptt" />
            <Picks slot="ptt" />
          </span>
          <Reset slot="ptt" />
          <Msg slot="ptt" />
          {isModOnly(hk.ptt) && perms && !perms.input_monitoring && (
            <p className="recorder-msg warn-text">
              <AlertTriangle size={13} strokeWidth={1.5} /> {ptt} needs Input Monitoring -{" "}
              <button className="link" onClick={() => nav.openOnboarding(1) /* the permissions page */}>
                Grant
              </button>
            </p>
          )}
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Hands-free</div>
            <div className="sub">Talk without holding</div>
          </div>
          <span className="inline">
            <span className="inline" style={{ gap: 8 }}>
              <Toggle label="Double-tap push to talk" on={hk.double_tap} onChange={(v) => save((x) => void (x.hotkeys.double_tap = v))} />
              <span className="small">double-tap {ptt}</span>
            </span>
            <span className="small">or</span>
            <span className="combo">
              <Binding slot="hands_free" empty="+ combo" />
              {hk.hands_free && rec !== "hands_free" && (
                <button className="icon-btn sm" aria-label="Remove hands-free combo" onClick={() => set("hands_free", null)}>
                  <X size={13} strokeWidth={1.5} />
                </button>
              )}
            </span>
            <Picks slot="hands_free" />
          </span>
          <span />
          <Msg slot="hands_free" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Hands-free grace</div>
            <div className="sub">After you let go of {ptt}, tap it within this time to keep talking hands-free. Also the double-tap window.</div>
          </div>
          <span className="inline">
            <input
              type="range"
              min={0}
              max={1500}
              step={50}
              value={hk.hands_free_grace_ms}
              onChange={(e) => save((x) => void (x.hotkeys.hands_free_grace_ms = +e.target.value))}
              aria-label="Hands-free grace"
            />
            <span className="small">{hk.hands_free_grace_ms ? `${hk.hands_free_grace_ms} ms` : "off"}</span>
          </span>
          <span />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Cancel</div>
            <div className="sub">Only while recording</div>
          </div>
          <span className="inline">
            <Binding slot="cancel" />
            <Picks slot="cancel" />
          </span>
          <Reset slot="cancel" />
          <Msg slot="cancel" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Paste last</div>
            <div className="sub">The last transcript, again</div>
          </div>
          <span className="inline">
            <Binding slot="paste_last" />
            <Picks slot="paste_last" />
          </span>
          <Reset slot="paste_last" />
          <Msg slot="paste_last" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Copy last</div>
            <div className="sub">The last transcript, to the clipboard</div>
          </div>
          <span className="inline">
            <Binding slot="copy_last" />
            <Picks slot="copy_last" />
          </span>
          <Reset slot="copy_last" />
          <Msg slot="copy_last" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Recents</div>
            <div className="sub">The last 9 dictations above the pill: 1-9 pastes, ⌘1-9 copies</div>
          </div>
          <span className="inline">
            <Binding slot="recents" />
            <Picks slot="recents" />
          </span>
          <Reset slot="recents" />
          <Msg slot="recents" />
        </div>
      </div>

      <Label>Try it here</Label>
      <textarea className="input practice" style={{ width: "100%", maxWidth: "none" }} rows={4} placeholder={`Click here, hold ${ptt} and talk. Your words land in this box.`} aria-label="Practice field" />
    </>
  );
}
