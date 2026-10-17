// DESIGN 6.6
import { useEffect, useRef, useState } from "react";
import { AlertTriangle, ChevronRight, GripVertical } from "lucide-react";
import { api, type DeviceInfo, type KnownDevice } from "../lib/api";
import { KIND_LABEL, micStore, saveSettings, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { Dot, Empty, Label, Meter, radioKeys, Title, useLevel } from "../ui";
import type { Nav } from "../App";

const ROW = 44;
type Item = { id: string; name: string; kind: DeviceInfo["kind"]; connected: boolean };

export default function Audio({ nav }: { nav: Nav }) {
  const mic = micStore.use();
  const settings = settingsStore.use();
  const [testing, setTesting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [quiet, setQuiet] = useState(false);
  const [drag, setDrag] = useState<{ id: string; from: number; y0: number; dy: number } | null>(null);
  const [said, setSaid] = useState("");
  const lvl = useLevel(testing);
  const lastLoud = useRef(0);

  const stop = () => {
    api.micTestStop();
    setTesting(false);
    nav.setTesting(false);
  };
  const start = async () => {
    setErr(null);
    setQuiet(false);
    try {
      await api.micTestStart(mic?.active ?? null);
      lastLoud.current = performance.now();
      setTesting(true);
      nav.setTesting(true);
    } catch (e) {
      setErr(String(e));
    }
  };
  // the test stops on window blur, after 30 s, and when leaving the screen
  useEffect(() => {
    if (!testing) return;
    const t = setTimeout(stop, 30_000);
    window.addEventListener("blur", stop);
    return () => {
      clearTimeout(t);
      window.removeEventListener("blur", stop);
    };
  }, [testing]);
  useEffect(() => () => void (testing && stop()), [testing]);
  // "We hear nothing": below -70 dBFS for 3 s
  useEffect(() => {
    if (!testing) return;
    if (lvl.db > -70) lastLoud.current = performance.now();
    else if (performance.now() - lastLoud.current > 3000 && !quiet) setQuiet(true);
  }, [lvl, testing]);

  if (!mic || !settings) return <Title parts={S.title.audio} />;

  const active = mic.devices.find((d) => d.id === mic.active);
  const connected = new Set(mic.devices.map((d) => d.id));
  const showVirtual = settings.mic.show_virtual;
  const known: KnownDevice[] = mic.priority;
  const all: Item[] = [
    ...known.map((k) => ({ id: k.id, name: k.name, kind: k.kind, connected: connected.has(k.id) })),
    ...mic.devices.filter((d) => !known.some((k) => k.id === d.id)).map((d) => ({ id: d.id, name: d.name, kind: d.kind, connected: true })),
  ];
  const items = all.filter((d) => showVirtual || d.kind !== "virtual");
  const virtuals = all.filter((d) => d.kind === "virtual");
  const auto = mic.selection === "auto";
  const fixed = mic.selection !== "auto" ? mic.selection.device.id : null;

  const reorder = (from: number, to: number) => {
    if (from === to) return;
    const next = [...items];
    const [m] = next.splice(from, 1);
    next.splice(to, 0, m);
    const ids = [...next.map((d) => d.id), ...all.filter((d) => !next.includes(d)).map((d) => d.id)];
    api.setMicPriority(ids).then(micStore.set);
    setSaid(`${m.name} moved to position ${to + 1}`);
  };
  const target = drag ? Math.max(0, Math.min(items.length - 1, drag.from + Math.round(drag.dy / ROW))) : -1;
  const shift = (i: number) => {
    if (!drag || i === drag.from) return 0;
    if (drag.from < target && i > drag.from && i <= target) return -ROW;
    if (target < drag.from && i >= target && i < drag.from) return ROW;
    return 0;
  };

  return (
    <>
      <Title parts={S.title.audio} />
      {!mic.devices.length ? (
        <div className="card">
          <Empty line={S.empty.devices.line} help={S.empty.devices.help} />
        </div>
      ) : (
        <div className="card">
          <div className="inuse">
            <span className="label">In use</span>
            <span>
              <div className="dname">{active?.name ?? "No microphone"}</div>
              <div className="meta">{[active && KIND_LABEL[active.kind], active?.manufacturer, auto ? "auto" : "fixed"].filter(Boolean).join(" · ")}</div>
            </span>
            <span className="status">
              <Dot tone={testing ? "live" : "idle"} /> {testing ? "live" : "closed at rest"}
            </span>
          </div>
          {mic.fallback_from && (
            <p className="note" style={{ padding: "0 20px 14px", margin: 0 }}>
              <AlertTriangle size={14} strokeWidth={1.5} className="warn-text" /> {mic.fallback_from} is disconnected. Using {active?.name ?? "nothing"} until it is back.
            </p>
          )}
          <div className="level">
            <Meter db={lvl.db} peak={lvl.peak} />
            <span className="inline" style={{ flexWrap: "nowrap" }}>
              <span className="db">{testing ? `${Math.max(-60, Math.round(lvl.db))} dB` : "- dB"}</span>
              <button className={`btn ${testing ? "ghost" : "secondary"} sm`} onClick={testing ? stop : start} style={{ width: 84 }}>
                {testing ? "Stop" : "Test mic"}
              </button>
            </span>
            <div className="scale" aria-hidden>
              {[-60, -40, -20, -10, -6, 0].map((d) => (
                <span key={d} style={{ left: `${((d + 60) / 60) * 100}%` }}>
                  {d}
                </span>
              ))}
            </div>
            <span />
            {(err || quiet) && (
              <p className="field-error" style={{ gridColumn: "1 / -1", margin: 0 }} role="alert">
                <AlertTriangle size={13} strokeWidth={1.5} />
                {err ? (/denied|permission/i.test(err) ? "Permission denied - Open System Settings" : /use|busy|unavailable/i.test(err) ? `${active?.name ?? "The microphone"} is in use by another app or unavailable` : err) : "We hear nothing - is it muted?"}
              </p>
            )}
          </div>
        </div>
      )}

      <Label>Priority - drag to reorder. Auto uses the first connected device.</Label>
      <div role="radiogroup" aria-label="Microphone selection" onKeyDown={radioKeys}>
      <div className="card prio">
        <div className="row hoverable auto-row" role="radio" aria-checked={auto} tabIndex={auto ? 0 : -1} onClick={() => api.setMicSelection("auto").then(micStore.set)} onKeyDown={(e) => e.key === " " && api.setMicSelection("auto").then(micStore.set)}>
          <span className="radio" />
          <span>Auto</span>
          <span className="small">highest connected device</span>
        </div>
        <div className="rows" style={{ borderTop: "1px solid var(--line)" }}>
          {items.map((d, i) => {
            const isDrag = drag?.id === d.id;
            return (
              <div key={d.id}>
                <div
                  className={`row${isDrag ? " lifted" : ""}${d.connected ? "" : " gone"}`}
                  style={{ transform: `translateY(${isDrag ? drag!.dy : shift(i)}px)` }}
                >
                  <button
                    className="handle"
                    aria-label={`Reorder ${d.name}. Option arrow keys move it.`}
                    onPointerDown={(e) => {
                      e.currentTarget.setPointerCapture(e.pointerId);
                      setDrag({ id: d.id, from: i, y0: e.clientY, dy: 0 });
                    }}
                    onPointerMove={(e) => drag && setDrag({ ...drag, dy: Math.max(-i * ROW, Math.min((items.length - 1 - i) * ROW, e.clientY - drag.y0)) })}
                    onPointerUp={() => {
                      if (drag) reorder(drag.from, target);
                      setDrag(null);
                    }}
                    onKeyDown={(e) => {
                      if (!e.altKey) return;
                      if (e.key === "ArrowUp" && i > 0) (e.preventDefault(), reorder(i, i - 1));
                      if (e.key === "ArrowDown" && i < items.length - 1) (e.preventDefault(), reorder(i, i + 1));
                    }}
                  >
                    <GripVertical size={14} strokeWidth={1.5} />
                  </button>
                  <span className="idx">{i + 1}</span>
                  <span className="dname ellipsis">{d.name}</span>
                  <span className="kind">{KIND_LABEL[d.kind]}</span>
                  <span className="state">
                    <Dot tone={d.connected ? "ok" : "idle"} />
                    {d.connected ? (d.id === mic.active ? "in use" : "connected") : "disconnected"}
                  </span>
                  <span style={{ width: 64, display: "flex", justifyContent: "flex-end" }}>
                    {!d.connected && (
                      <button className="btn ghost sm" onClick={() => api.forgetDevice(d.id).then(micStore.set)}>
                        Forget
                      </button>
                    )}
                  </span>
                </div>
                {d.kind === "bluetooth" && <div className="warnline">{S.help.bluetooth}</div>}
              </div>
            );
          })}
        </div>
        {virtuals.length > 0 && (
          <button className="disclosure" aria-expanded={showVirtual} onClick={() => saveSettings((x) => void (x.mic.show_virtual = !showVirtual))}>
            <ChevronRight size={14} strokeWidth={1.5} />
            {showVirtual ? "Hide" : "Show"} virtual devices ({virtuals.length})<span className="small ellipsis">{virtuals.map((v) => v.name).join(", ")}</span>
          </button>
        )}
      </div>
      <div className="lang-row">
        <button className="style-opt" role="radio" aria-checked={!auto} tabIndex={auto ? -1 : 0} onClick={() => mic.devices[0] && api.setMicSelection({ device: { id: fixed ?? mic.active ?? mic.devices[0].id } }).then(micStore.set)}>
          <span className="radio" />
          Always use
        </button>
        <select className="input" value={fixed ?? mic.active ?? ""} onChange={(e) => api.setMicSelection({ device: { id: e.target.value } }).then(micStore.set)} aria-label="Always use this microphone">
          {mic.devices.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
            </option>
          ))}
        </select>
        <span className="small">New mics prompt you once. A lost mic mid-dictation switches without stopping.</span>
      </div>
      </div>
      <div className="sr-only" aria-live="polite">
        {said}
      </div>
    </>
  );
}
