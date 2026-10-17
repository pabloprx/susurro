// DESIGN 6.7, validation per PLAN 4.1 (the backend validates again on save).
import { useEffect, useRef, useState } from "react";
import { AlertTriangle, X } from "lucide-react";
import type { Hotkeys } from "../lib/api";
import { keysOf, permStore, saveSettings, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Kbd, Label, Title, Toggle } from "../ui";
import type { Nav } from "../App";

type Slot = "ptt" | "hands_free" | "cancel" | "paste_last";
const NAMES: Record<Slot, string> = { ptt: "Push to talk", hands_free: "Hands-free", cancel: "Cancel", paste_last: "Paste last" };
const DEFAULTS: Hotkeys = { ptt: "Fn", hands_free: null, double_tap: true, cancel: "Escape", paste_last: "Ctrl+Cmd+V" };
const MODS: Record<string, string> = { Control: "Ctrl", Alt: "Alt", Meta: "Cmd", Shift: "Shift" };

function keyName(e: KeyboardEvent): string | null {
  const c = e.code;
  if (c.startsWith("Key")) return c.slice(3);
  if (c.startsWith("Digit")) return c.slice(5);
  if (/^F\d+$/.test(c)) return c;
  if (c.startsWith("Arrow")) return c.slice(5);
  const m: Record<string, string> = { Space: "Space", Enter: "Enter", Backspace: "Backspace", Tab: "Tab", Escape: "Escape", Minus: "-", Equal: "=", Comma: ",", Period: ".", Slash: "/", Semicolon: ";", Quote: "'", BracketLeft: "[", BracketRight: "]", Backslash: "\\", Backquote: "`" };
  return m[c] ?? null;
}

/** PLAN 4.1 rules. Returns the message to show, or null if the binding is fine. */
export function validate(binding: string, slot: Slot, hk: Hotkeys, codes: Set<string>): string | null {
  const parts = binding.split("+");
  const key = parts[parts.length - 1];
  const mods = parts.slice(0, -1);
  if (parts.length > 3) return "Use up to 3 keys";
  if (["Ctrl", "Alt", "Cmd", "Shift"].includes(key)) return "Add a key: only fn works alone";
  for (const m of ["Shift", "Control", "Alt", "Meta"]) if (codes.has(`${m}Left`) && codes.has(`${m}Right`)) return "Left and right of one modifier cannot be told apart";
  if (mods.length === 1 && mods[0] === "Cmd" && ["C", "V", "X", "Z", "Q"].includes(key)) return `⌘${key} is reserved by the system`;
  if (!mods.length && /^[A-Z0-9]$/.test(key)) return "Add a modifier: a single key types text";
  for (const s of Object.keys(NAMES) as Slot[]) if (s !== slot && hk[s] === binding) return `Already used by ${NAMES[s]}`;
  return null;
}

export default function Shortcuts({ nav }: { nav: Nav }) {
  const s = settingsStore.use();
  const perms = permStore.use();
  const [rec, setRec] = useState<Slot | null>(null);
  const [held, setHeld] = useState<string[]>([]);
  const [msg, setMsg] = useState<{ slot: Slot; text: string } | null>(null);
  const codes = useRef(new Set<string>());

  const set = (slot: Slot, binding: string | null) =>
    saveSettings((x) => void (x.hotkeys = { ...x.hotkeys, [slot]: binding }))
      .then(() => setMsg(null))
      .catch((e) => setMsg({ slot, text: String(e) }));

  useEffect(() => {
    if (!rec || !s) return;
    codes.current = new Set();
    const down = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      codes.current.add(e.code);
      const mods = (["Control", "Alt", "Meta", "Shift"] as const).filter((m) => e.getModifierState(m)).map((m) => MODS[m]);
      if (e.key === "Escape" && !mods.length) return void (setRec(null), setHeld([]), setMsg(null));
      if (e.key in MODS) return setHeld(mods);
      const k = keyName(e);
      if (!k) return;
      const b = [...mods, k].join("+");
      setHeld(b.split("+"));
      const err = validate(b, rec, s.hotkeys, codes.current);
      if (err) return setMsg({ slot: rec, text: err });
      setRec(null);
      setHeld([]);
      set(rec, b);
    };
    const up = (e: KeyboardEvent) => {
      if (e.key in MODS && !e.getModifierState("Control") && !e.getModifierState("Alt") && !e.getModifierState("Meta") && !e.getModifierState("Shift") && held.length && held.every((k) => Object.values(MODS).includes(k))) {
        setMsg({ slot: rec, text: "Add a key: only fn works alone" });
        setHeld([]);
      }
      codes.current.delete(e.code);
    };
    window.addEventListener("keydown", down, true);
    window.addEventListener("keyup", up, true);
    return () => {
      window.removeEventListener("keydown", down, true);
      window.removeEventListener("keyup", up, true);
    };
  }, [rec, s, held]);

  if (!s) return <Title parts={S.title.shortcuts} />;
  const hk = s.hotkeys;

  const Binding = ({ slot, empty }: { slot: Slot; empty?: string }) => {
    const v = hk[slot];
    const on = rec === slot;
    return (
      <button className={`bind${on ? " recording" : ""}`} onClick={() => (setRec(on ? null : slot), setHeld([]), setMsg(null))} aria-label={`${NAMES[slot]}: ${v ?? "none"}. Press to record a new shortcut.`}>
        {on ? held.length ? <Kbd keys={keysOf(held.join("+"))} /> : <span className="rec-hint">Press up to 3 keys… Esc to cancel</span> : v ? <Kbd keys={keysOf(v)} /> : <span className="placeholder">{empty ?? "none"}</span>}
      </button>
    );
  };
  const Msg = ({ slot }: { slot: Slot }) =>
    msg?.slot === slot ? (
      <p className={`recorder-msg ${rec === slot ? "warn-text" : "danger-text"}`} role="alert">
        <AlertTriangle size={13} strokeWidth={1.5} /> {msg.text}
      </p>
    ) : null;

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
            {rec === "ptt" && (
              <button className="btn ghost sm" onClick={() => (setRec(null), set("ptt", "Fn"))}>
                Use fn
              </button>
            )}
          </span>
          <button className="btn ghost sm" onClick={() => set("ptt", DEFAULTS.ptt)} disabled={hk.ptt === DEFAULTS.ptt}>
            Reset
          </button>
          <Msg slot="ptt" />
          {hk.ptt === "Fn" && perms && !perms.input_monitoring && (
            <p className="recorder-msg warn-text">
              <AlertTriangle size={13} strokeWidth={1.5} /> fn needs Input Monitoring -{" "}
              <button className="link" onClick={() => nav.openOnboarding(3)}>
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
              <Toggle label="Double-tap push to talk" on={hk.double_tap} onChange={(v) => saveSettings((x) => void (x.hotkeys.double_tap = v))} />
              <span className="small">double-tap {keysOf(hk.ptt).join(" ")}</span>
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
          </span>
          <span />
          <Msg slot="hands_free" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Cancel</div>
            <div className="sub">Only while recording</div>
          </div>
          <Binding slot="cancel" />
          <button className="btn ghost sm" onClick={() => set("cancel", DEFAULTS.cancel)} disabled={hk.cancel === DEFAULTS.cancel}>
            Reset
          </button>
          <Msg slot="cancel" />
        </div>
        <div className="sc-row">
          <div>
            <div className="label">Paste last</div>
            <div className="sub">The last transcript, again</div>
          </div>
          <Binding slot="paste_last" />
          <button className="btn ghost sm" onClick={() => set("paste_last", DEFAULTS.paste_last)} disabled={hk.paste_last === DEFAULTS.paste_last}>
            Reset
          </button>
          <Msg slot="paste_last" />
        </div>
      </div>

      <Label>Try it here</Label>
      <textarea className="input practice" style={{ width: "100%", maxWidth: "none" }} rows={4} placeholder={`Click here, hold ${keysOf(hk.ptt).join(" ")} and talk. Your words land in this box.`} aria-label="Practice field" />
    </>
  );
}
