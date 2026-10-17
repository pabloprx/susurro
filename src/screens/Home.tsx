// DESIGN 6.1
import { useEffect, useState } from "react";
import { ArrowRight, Copy, RotateCcw } from "lucide-react";
import { api, on, type HistoryItem, type Stats } from "../lib/api";
import { clock, keyLabel, keysOf, settingsStore } from "../lib/store";
import { S } from "../lib/strings";
import { copy, Empty, Kbd, toast } from "../ui";
import type { Nav } from "../App";

function greeting() {
  const h = new Date().getHours();
  return h < 5 ? "Late night" : h < 12 ? "Good morning" : h < 19 ? "Good afternoon" : "Good evening";
}

export default function Home({ nav }: { nav: Nav }) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [recent, setRecent] = useState<HistoryItem[] | null>(null);
  const settings = settingsStore.use();
  const load = () => {
    api.getStats().then(setStats);
    api.historyList(null, null, 12).then((l) => setRecent(l.filter((h) => h.kind !== "command").slice(0, 5))); // Recent = dictations
  };
  useEffect(() => {
    load();
    const off = on("history_added", load);
    return () => void off.then((f) => f());
  }, []);

  const ptt = settings?.hotkeys.ptt ?? "F1";
  const daily = stats?.daily ?? [];
  const maxDay = Math.max(1, ...daily.map((d) => d.words));
  // streak: consecutive days with words, ending today (or yesterday if today is still empty)
  let streak = 0;
  for (let i = daily.length - 1; i >= 0; i--) {
    if (daily[i].words > 0) streak++;
    else if (i < daily.length - 1) break;
  }
  const saved = Math.max(0, Math.round(stats?.minutes_saved ?? 0));
  const [g1, g2] = greeting().split(" ");

  return (
    <>
      <header className="screen-head home-head" data-tauri-drag-region>
        <div className="screen-title">
          <h1 className="hero">
            {g2 ? (
              <>
                {g1} <em>{g2}.</em>
              </>
            ) : (
              <em>{g1}.</em>
            )}
          </h1>
          <div className="hint-kbd">
            <Kbd keys={keysOf(ptt)} /> hold to talk
          </div>
        </div>
        <div className="ruler" aria-hidden />
      </header>

      <section className="card stats" aria-label="This week">
        <div className="stat">
          <span className="label">Words · 7d</span>
          <span className="num">{(stats?.words_7d ?? 0).toLocaleString("en-US")}</span>
          <span className="foot">
            <span className="spark" aria-label="Words per day, last 7 days">
              {daily.map((d, i) => (
                <i key={d.day} className={i === daily.length - 1 ? "today" : ""} style={{ height: 2 + (d.words / maxDay) * 16 }} title={`${d.day}: ${d.words} words`} />
              ))}
            </span>
          </span>
        </div>
        <div className="stat">
          <span className="label">Avg WPM</span>
          <span className="num">{Math.round(stats?.wpm ?? 0)}</span>
          <span className="foot">
            <span data-tip={S.help.typing}>vs 45 typed</span>
          </span>
        </div>
        <div className="stat">
          <span className="label">Streak</span>
          <span className="num">
            {streak}
            <small>{streak === 1 ? "day" : "days"}</small>
          </span>
          <span className="foot">
            <span className="streak" aria-hidden>
              {daily.map((d, i) => (
                <i key={d.day} className={d.words > 0 ? (i === daily.length - 1 ? "today" : "on") : ""} />
              ))}
            </span>
          </span>
        </div>
        <div className="stat">
          <span className="label">Time saved</span>
          <span className="num">
            {saved >= 60 ? (
              <>
                {Math.floor(saved / 60)}
                <small>h</small> {saved % 60}
                <small>m</small>
              </>
            ) : (
              <>
                {saved}
                <small>min</small>
              </>
            )}
          </span>
          <span className="foot">this week</span>
        </div>
      </section>

      <div className="label-row">
        <span className="label">Recent</span>
        {!!recent?.length && (
          <button className="btn ghost sm" onClick={() => nav.go("history")}>
            View all <ArrowRight size={14} strokeWidth={1.5} />
          </button>
        )}
      </div>
      {recent && !recent.length ? (
        <Empty line={S.empty.recent.line} help={S.empty.recent.help(keyLabel(ptt))} action={<button className="btn secondary" onClick={() => nav.go("shortcuts")}>Try it</button>} />
      ) : (
        <div className="rows recent">
          {recent?.map((h) => (
            <div className="row hoverable" key={h.id}>
              <span className="time">{clock(h.created_at)}</span>
              <span className="app ellipsis">{h.app_name ?? "Unknown"}</span>
              <span className="text ellipsis selectable">{h.final_text.replace(/\n+/g, " ")}</span>
              <span className="actions">
                <button className="icon-btn" aria-label="Copy" data-tip="Copy" onClick={() => copy(h.final_text)}>
                  <Copy size={15} strokeWidth={1.5} />
                </button>
                <button
                  className="icon-btn"
                  aria-label="Re-transcribe"
                  data-tip={h.has_audio ? "Re-transcribe" : "Audio expired"}
                  disabled={!h.has_audio}
                  onClick={async () => {
                    try {
                      await api.historyRetry(h.id);
                      load();
                      toast("Re-transcribed. Saved in History.", { kind: "success" });
                    } catch (e) {
                      toast(String(e), { kind: "error" });
                    }
                  }}
                >
                  <RotateCcw size={15} strokeWidth={1.5} />
                </button>
              </span>
            </div>
          ))}
        </div>
      )}
    </>
  );
}
