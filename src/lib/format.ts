// Pure formatters, no side effects: the overlay window imports them without the stores.
import type { Toast } from "./contract.gen";

/** Timestamps are unix ms everywhere (Rust history::now_ms). */
export const clock = (t: number) =>
  new Date(t).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });

export function dayLabel(t: number): string {
  const d = new Date(t);
  const today = new Date();
  const diff = Math.round((new Date(today.toDateString()).getTime() - new Date(d.toDateString()).getTime()) / 864e5);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
}

/** 4.8s / 1:04 */
export const secs = (ms: number) => {
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)}s`;
  return `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
};

export const mmss = (s: number) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

export function ago(t: number): string {
  const s = Math.max(0, (Date.now() - t) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return `${Math.round(s / 60)}m ago`;
  if (s < 86400) return `${Math.round(s / 3600)}h ago`;
  return `${Math.round(s / 86400)}d ago`;
}

/** "ggml-large-v3-turbo-q5_0.bin" -> "large-v3-turbo q5_0" */
export function modelName(file: string): string {
  const m = file.replace(/^ggml-/, "").replace(/\.bin$/, "");
  const q = m.match(/-(q\d_\d)$/);
  return q ? `${m.slice(0, -q[0].length)} ${q[1]}` : m;
}

const GLYPH: Record<string, string> = {
  Fn: "fn",
  Ctrl: "⌃",
  Alt: "⌥",
  Cmd: "⌘",
  Shift: "⇧",
  Escape: "esc",
  Space: "Space",
  Enter: "↩",
  Backspace: "⌫",
  Tab: "⇥",
  Up: "↑",
  Down: "↓",
  Left: "←",
  Right: "→",
};
/** "Ctrl+Cmd+V" -> ["⌃", "⌘", "V"] */
export const keysOf = (binding: string) => binding.split("+").map((k) => GLYPH[k] ?? k);

/** Undo is only valid for ttl_ms in Rust (session.rs UNDO): its toast must not outlive that window. */
export const hasDeadline = (t: Pick<Toast, "actions">) => t.actions.some((a) => a.id === "undo");
