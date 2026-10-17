# susurro - Design Spec

The frontend follows this file exactly. If code and this file disagree, this file wins. If this file is silent, pick the quieter option.

---

## 1. Direction: "Tally Light"

**Idea.** susurro is a recording booth at night. The room is dark, warm and quiet. One light matters: the red-orange tally lamp that says "you are on air". The app is an instrument, not a dashboard.

**Rules that make it distinct**
1. Dark first. Warm graphite, never blue-black. Light theme is "paper", a full peer, not an afterthought.
2. One accent only: **tally** (vermilion). It means "live / your attention". It is never decoration. A screen at rest shows tally in at most one place.
3. Editorial type: a serif display face with italic accent words ("Say it *once*."). System sans for UI. Mono for numbers, labels, keys.
4. Instrument details: hairline rules, tick marks, mono micro-labels in caps (`INPUT`, `-18 dB`, `142 WPM`), meters that look like real VU meters.
5. No gradients on surfaces, no glassmorphism cards, no purple, no drop-shadow stacks. Depth comes from 1px lines and value steps.

**Why.** Wispr Flow is cream + serif + many pastel accents. susurro keeps the editorial serif idea but inverts it: dark, one light, instrument precision. It reads as a pro macOS tool (Logic, Final Cut, Things) and not as a SaaS web app.

---

## 2. Typography

No CDN. No bundled font files in v1. WKWebView maps the CSS generic families to the Apple system faces, which are excellent:

| Role | Stack | On macOS |
|---|---|---|
| Display | `ui-serif, "New York", "Iowan Old Style", Georgia, serif` | New York |
| UI | `-apple-system, BlinkMacSystemFont, "SF Pro Text", ui-sans-serif, "Segoe UI", system-ui, sans-serif` | SF Pro |
| Mono | `ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace` | SF Mono |

ponytail: Linux/Windows get Georgia + system sans. If the port needs the same look, bundle an OFL serif (for example Instrument Serif) as woff2 in `src/assets/fonts/` then.

**Scale** (px, line-height)
| Token | Size / LH | Family | Use |
|---|---|---|---|
| `--t-hero` | 44 / 48, weight 400, tracking -0.02em | display | Home greeting, onboarding titles |
| `--t-h1` | 30 / 36, 400, -0.015em | display | Screen titles |
| `--t-h2` | 20 / 26, 400 | display | Card titles |
| `--t-body` | 13 / 20, 400 | UI | Default text (macOS native size) |
| `--t-body-lg` | 15 / 22, 400 | UI | Transcript text |
| `--t-small` | 12 / 16, 400 | UI | Helper text |
| `--t-label` | 10.5 / 14, 500, tracking 0.08em, uppercase | mono | Micro-labels, column headers |
| `--t-num` | 28 / 32, 400, `font-variant-numeric: tabular-nums` | mono | Stat numbers |

- Display italic is the signature: one italic word per headline, maximum.
- All numbers that change (timers, dB, WPM, counts, progress) use `tabular-nums`.
- Never bold the serif. Emphasis in the serif = italic.

---

## 3. Tokens (paste into `src/styles/tokens.css`)

Contrast was measured (WCAG formula, node script). Results are in the comments.

```css
:root {
  /* ---------- type ---------- */
  --font-display: ui-serif, "New York", "Iowan Old Style", Georgia, serif;
  --font-ui: -apple-system, BlinkMacSystemFont, "SF Pro Text", ui-sans-serif, "Segoe UI", system-ui, sans-serif;
  --font-mono: ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace;

  /* ---------- color: dark (default) ---------- */
  --bg:         #0E0D0B;  /* window */
  --bg-sunk:    #0A0908;  /* sidebar, wells, inputs */
  --surface:    #1B1A17;  /* cards */
  --surface-2:  #23211D;  /* hover, raised rows, popovers */
  --line:       #2E2B26;  /* hairlines */
  --line-strong:#423E37;  /* input borders, dividers that must read */
  --text:       #EDE8DF;  /* 15.9:1 on bg */
  --text-2:     #A39C90;  /* 6.4:1 on surface */
  --text-3:     #918A7D;  /* 5.1:1 on surface; placeholder, meta */
  --tally:      #FF6A3D;  /* the one accent; 6.8:1 on bg */
  --tally-ink:  #1A0A04;  /* text on tally fill; 6.8:1 */
  --tally-soft: rgb(255 106 61 / 0.14); /* selected row, focus halo */
  --ok:         #7FD1A8;  /* 9.6:1 */
  --warn:       #F5B94A;  /* 9.9:1 */
  --danger:     #FF5C7A;  /* 5.9:1 */
  --meter-low:  #7FD1A8;
  --meter-mid:  #F5B94A;
  --meter-hot:  #FF5C7A;

  /* ---------- space (4px grid) ---------- */
  --s-1: 4px;  --s-2: 8px;  --s-3: 12px; --s-4: 16px;
  --s-5: 20px; --s-6: 24px; --s-8: 32px; --s-10: 40px; --s-12: 48px; --s-16: 64px;

  /* ---------- radius ---------- */
  --r-xs: 4px;    /* chips, kbd keys */
  --r-sm: 6px;    /* inputs, buttons */
  --r-md: 10px;   /* cards, popovers */
  --r-lg: 14px;   /* modal sheets */
  --r-pill: 999px;

  /* ---------- elevation (dark: lines do the work, shadows only off-surface) ---------- */
  --shadow-pop:  0 0 0 1px var(--line-strong), 0 8px 24px -6px rgb(0 0 0 / 0.6);
  --shadow-sheet:0 0 0 1px var(--line-strong), 0 24px 64px -12px rgb(0 0 0 / 0.7);
  --shadow-pill: 0 0 0 1px rgb(255 255 255 / 0.07), 0 10px 30px -8px rgb(0 0 0 / 0.75), inset 0 1px 0 rgb(255 255 255 / 0.06);
  --glow-tally:  0 0 0 1px rgb(255 106 61 / 0.55), 0 0 18px -2px rgb(255 106 61 / 0.45);

  /* ---------- motion ---------- */
  --ease-out:    cubic-bezier(0.23, 1, 0.32, 1);    /* enter, press release */
  --ease-in-out: cubic-bezier(0.77, 0, 0.175, 1);   /* on-screen morph */
  --ease-drawer: cubic-bezier(0.32, 0.72, 0, 1);    /* sheets, pill width */
  --d-press: 120ms;   /* :active scale */
  --d-fast:  150ms;   /* hover color, tooltips */
  --d-base:  200ms;   /* popovers, toasts, tab underline */
  --d-morph: 260ms;   /* pill state morph */
  --d-sheet: 320ms;   /* modal sheet, onboarding step */

  /* ---------- focus ---------- */
  --focus: 0 0 0 2px var(--bg), 0 0 0 4px var(--tally);

  /* ---------- layout ---------- */
  --sidebar-w: 216px;
  --content-max: 880px;
  --titlebar-h: 44px;

  color-scheme: dark;
}

/* ---------- color: light ("paper") ---------- */
:root[data-theme="light"] {
  --bg:         #F4F1EA;
  --bg-sunk:    #ECE7DD;
  --surface:    #FBF9F4;
  --surface-2:  #ECE7DD;
  --line:       #DDD6C9;
  --line-strong:#C6BEAF;
  --text:       #1A1813;  /* 15.7:1 */
  --text-2:     #5E574B;  /* 6.8:1 */
  --text-3:     #7A7264;  /* 4.5:1 */
  --tally:      #C2410C;  /* 4.9:1 on surface; darker so it reads on paper */
  --tally-ink:  #FFF6F0;  /* 4.9:1 */
  --tally-soft: rgb(194 65 12 / 0.10);
  --ok:         #1F7A52;  /* 5.0:1 */
  --warn:       #8A5A00;  /* 5.6:1 */
  --danger:     #B4233F;  /* 6.1:1 */
  --shadow-pop:  0 0 0 1px var(--line-strong), 0 8px 24px -8px rgb(60 45 20 / 0.18);
  --shadow-sheet:0 0 0 1px var(--line-strong), 0 24px 64px -16px rgb(60 45 20 / 0.25);
  color-scheme: light;
}

/* The overlay pill is ALWAYS dark, in both themes. It floats over any app. */
.pill-root { --pill-bg: #121110; --pill-text: #EDE8DF; --pill-dim: #918A7D; }

@media (prefers-reduced-motion: reduce) {
  :root { --d-morph: 0ms; --d-sheet: 120ms; --d-base: 120ms; }
}
```

Simpler path for "system": set `data-theme` from JS using `matchMedia('(prefers-color-scheme: light)')`. Do not duplicate the light block.

---

## 4. Base components

| Component | Spec |
|---|---|
| Button primary | Fill `--tally`, text `--tally-ink`, h 30, px 14, `--r-sm`, 13/500. One per screen, max. |
| Button secondary | Fill `--surface-2`, 1px `--line-strong`, text `--text`. |
| Button ghost | No fill. Text `--text-2`, hover fill `--surface-2`. |
| Button danger | Ghost with `--danger` text. Destructive confirm turns it into a fill. |
| Press feedback | All pressables: `transition: transform var(--d-press) var(--ease-out)`; `:active { transform: scale(0.97) }`. |
| Input | h 30, `--bg-sunk` fill, 1px `--line-strong`, `--r-sm`, px 10. Focus: border `--tally`, `box-shadow: 0 0 0 3px var(--tally-soft)`. |
| Secret input | Input + mono font + eye toggle + "Test" button inline at the right. Value shows `sk-or-...a91f` (first 6, last 4) when saved. |
| Toggle | 28x16 track. Off `--line-strong`, on `--tally`. Thumb 12px `--text`. Slide 150ms ease-out. |
| Segmented control | `--bg-sunk` well, 2px inset, active segment `--surface-2` + 1px `--line-strong`. Active segment slides (transform), 200ms `--ease-out`. |
| Card | `--surface`, 1px `--line`, `--r-md`, padding `--s-5`. No shadow. |
| Row list | Rows 40px, 1px `--line` between, hover `--surface-2`. Actions appear on hover/focus-within (opacity 0 -> 1, 150ms), always visible to keyboard focus. |
| Kbd | Mono 11px, `--surface-2`, 1px `--line-strong`, bottom border 2px, `--r-xs`, px 6. Render glyphs: `fn`, `⌃`, `⌥`, `⌘`, `⇧`, `Space`. |
| Micro-label | `--t-label`, `--text-3`. Above every field group and every stat. |
| Status dot | 6px circle. `ok` connected, `--text-3` idle, `--tally` live (pulses only while recording), `--danger` error. Always paired with text. |
| Toast | Bottom-right of main window, 320 wide, `--surface-2`, `--shadow-pop`, `--r-md`. Enter: `translateY(8px) opacity 0` -> 0, 200ms `--ease-out` (use transitions). Auto-dismiss 5s, pause on hover and when window hidden. Max 3 stacked. |
| Tooltip | 500ms delay first time, then instant for 800ms window. `--surface-2`, 12px, `--r-xs`. |
| Popover | `--shadow-pop`, `--r-md`, scale 0.97 + opacity from the trigger origin, 150ms `--ease-out`. |
| Modal sheet | Center, max 520 wide, `--shadow-sheet`, `--r-lg`, backdrop `rgb(0 0 0 / 0.5)`. Enter scale 0.97 + opacity, 200ms. Esc closes. |

Icons: Lucide (`lucide-react`), 16px, stroke 1.5. No other icon set. No emoji.

---

## 5. The overlay pill

A separate Tauri window: transparent, no decorations, always on top, skip taskbar, not focusable (`focus: false`) so it never steals focus from the target app. Ignore mouse events except over the pill itself.

### Position and size
- Bottom center of the screen that holds the mouse cursor (or the frontmost window).
- Bottom edge = `visibleFrame.minY + 12px` (macOS visible frame already excludes the Dock). If the Dock autohides, use `frame.minY + 24px`.
- Window is fixed at 360x96 and centered (the same size as PLAN 1.6). The pill animates inside it (no window resizing during animation: resizing windows is janky).

| State | Pill size (w x h) | Content |
|---|---|---|
| `idle` | 44 x 8 | Thin bar, `--pill-bg` at 70% opacity, 1px line. Hover: grows to 120 x 24 and shows `fn to talk` in mono 10px. |
| `listening` | 200 x 36 | `[x]` cancel 20px - live waveform 120px - `[■]` stop 20px. Left edge: 6px tally dot. |
| `handsfree` | 232 x 36 | Same as listening + timer `0:42` mono 11px. Tally dot is steady (no pulse) with a 1px tally ring around the pill. |
| `processing` | 120 x 36 | Waveform freezes and collapses into 5 dots that travel (see below). Label none. |
| `done` | 120 x 36 | Check icon `--ok` + `Pasted` 12px. Holds 700ms, then back to idle. |
| `copied` (no Accessibility) | 260 x 36 | Clipboard icon + `Copied - grant Accessibility`. Holds 2.5s. |
| `error` | auto, max 300 x 36 | `--danger` icon + short message + `Retry` link if retry exists. Holds 4s or until hover leaves. |
| `warning` | listening + | At `max_seconds - 60` (9:00 with the 600 s default) the timer turns `--warn` and shows `1:00 left`. |

State source: `session_state` (PLAN 1.5). `Recording` = listening, `Locked` = handsfree, `Processing` = processing, `Done` with message `pasted` = done, `Done` with message `copied` = copied, `Error` = error. The overlay shows one notice at a time inside the pill (hot-plug, device switch, errors). The toast stack lives in the main window only.

ASCII:
```
idle         ▔▔▔▔                         (44x8)

listening   ╭─────────────────────────────╮
            │ ● ✕  ▁▃▆█▆▃▂▁▂▅▇▅▂▁  ■      │   (200x36)
            ╰─────────────────────────────╯

handsfree   ╭───────────────────────────────────╮
            │ ● ✕  ▁▃▆█▆▃▂▁▂▅▇▅▂▁  0:42  ■      │   tally ring
            ╰───────────────────────────────────╯

processing        ╭──────────────╮
                  │   · • ● • ·   │
                  ╰──────────────╯

done              ╭──────────────╮
                  │  ✓  Pasted    │
                  ╰──────────────╯

error       ╭──────────────────────────────────────╮
            │ ⚠  Mic disconnected  ·  Retry        │
            ╰──────────────────────────────────────╯
```

### Styling
- `background: var(--pill-bg)`, `box-shadow: var(--shadow-pill)`, `border-radius: var(--r-pill)`.
- Listening adds `--glow-tally` at 40% strength. This is the "on air" moment: the only glow in the whole product.
- Right-click the pill: a native menu (`@tauri-apps/api/menu` `Menu.new(...).popup()`) with the mic list: radio items, current device checked, `Auto` at top, `Open audio settings…` at bottom. The same items as the tray Microphone submenu. A list does not fit in the 96px window, so it is not HTML.
- Click on idle pill = start hands-free. Click stop = finish. Click X = cancel. Esc = cancel (global while recording).

### Morph motion
- Animate `width` and `height` of the pill element with `transition: width var(--d-morph) var(--ease-drawer), height var(--d-morph) var(--ease-drawer)`. The pill is one small element in its own window, so layout cost is irrelevant here. This is the one allowed exception to "only transform/opacity".
- Content cross-fades: outgoing content `opacity 0, filter blur(2px)` in 120ms; incoming content fades in after 60ms, 160ms.
- Press-to-talk start has **no entry delay**. The pill must reach listening size within the first frame of the `session_state` event with phase `Recording`. The morph runs while audio already records. Never gate capture on animation.
- Cancel: the pill shrinks to idle 200ms `--ease-out` with a tiny horizontal shake (2 cycles, 3px, 160ms) only if cancel came from an error. Plain cancel: no shake.
- Reduced motion: no morph. States swap with a 120ms opacity fade. Waveform still draws (it is information), the processing dots become a static `…` with opacity pulse.

### Live waveform (canvas)
1. Backend emits `audio_level` events at 30 Hz (every ~33ms) with `{ db: f32, peak_db: f32 }` (dBFS) per frame, from the capture thread (computed in Rust, not raw samples over IPC).
2. Frontend keeps a ring buffer of the last 32 values.
3. Draw on a `<canvas>` sized 120x24 CSS px, backing store `* devicePixelRatio`.
4. Rendering: 32 vertical bars, 2px wide, 1.75px gap, rounded caps (`lineCap = "round"`), centered vertically (mirror up and down). Color `--pill-text`; the newest 4 bars blend toward `--tally` (lerp 0 -> 60%).
5. Level mapping: take `db` as is (no conversion), clamp to `[-60, 0]`, `v = (db + 60) / 60`, then `v = v ** 1.6` (expands the speech range, keeps the noise floor flat). Min bar height 2px so silence still reads as a line.
6. Smoothing: attack/release envelope per bar, not a moving average. `display += (target - display) * (target > display ? 0.55 : 0.18)` each rAF. Fast rise, slow fall, like a VU needle.
7. Scroll: new values enter from the right. Interpolate the x offset between events with the rAF clock so motion is continuous, not 30 Hz stepped.
8. Stop the rAF loop when the pill is idle. Do not draw at rest.
9. Processing transition: the bars collapse to 5 dots (heights tween to 2px, x tweens to 5 slots over 200ms), then the dots travel: each dot opacity follows `0.3 + 0.7 * max(0, sin(t*2π/900ms - i*0.6))`.

### Sounds
- macOS system sounds through `NSSound`: `Tink` at start, `Pop` at stop, `Basso` on error. No custom sound files. Toggle in Settings > General. Play from Rust so latency is not tied to the webview.

---

## 6. Main window

Native titlebar hidden, traffic lights inset (`titleBarStyle: "Overlay"`, `hiddenTitle: true`). Min size 880x600, default 1040x700. `data-tauri-drag-region` on the top 44px of both sidebar and content.

### Shell layout

```
┌──────────────┬────────────────────────────────────────────────────────┐
│ ● ● ●        │                                            (drag area) │
│              │                                                        │
│  susurro     │   Screen title in serif, *italic* word                 │
│              │   ─────────────────────────────────────────────────    │
│  ⌂ Home      │                                                        │
│  ☰ History   │   content column, max 880, padding 40 / 48            │
│  Aa Dictionary│                                                       │
│  ✎ Styles    │                                                        │
│              │                                                        │
│  SETUP       │                                                        │
│  ◎ Providers │                                                        │
│  ◉ Audio     │                                                        │
│  ⌘ Shortcuts │                                                        │
│  ⚙ Settings  │                                                        │
│              │                                                        │
│ ──────────── │                                                        │
│ INPUT        │                                                        │
│ MacBook Mic  │                                                        │
│ ▮▮▮▮▯▯▯▯▯▯   │                                                        │
│ WHISPER      │                                                        │
│ large-v3-t ● │                                                        │
└──────────────┴────────────────────────────────────────────────────────┘
```

- Sidebar `--sidebar-w`, fill `--bg-sunk`, right hairline `--line`.
- Wordmark: `susurro` in display italic 20px, `--text`. No logo mark in v1.
- Nav item: h 30, `--r-sm`, icon 16 + label 13. Active: fill `--surface-2`, text `--text`, and a 2px tally tick at the left edge (the only tally in the sidebar). Inactive: `--text-2`. Keyboard: `⌘1`..`⌘8` jump to items.
- Sidebar footer is the "instrument panel": current input device name + a mini 10-segment meter (live only during `Test mic`, else static: the mic stays closed at rest), and the active STT provider/model with a status dot. Click either to jump to its screen.
- Screen change: no animation (frequent action). Content scrolls; sidebar does not.

### 6.1 Home

```
  Good evening, *Pablo.*                                       fn  hold to talk
  ──────────────────────────────────────────────────────────────────────────

  ┌────────────┬────────────┬────────────┬────────────┐
  │ WORDS · 7D │ AVG WPM    │ STREAK     │ TIME SAVED │
  │ 4,812      │ 148        │ 6 days     │ 1h 22m     │
  │ ▁▂▅▃▇▆█    │ vs 45 typed│ ■■■■■■□    │ this week  │
  └────────────┴────────────┴────────────┴────────────┘

  RECENT                                                      View all →
  ──────────────────────────────────────────────────────────────────────
  14:02  Slack          Can you push the release after lunch? Thanks.   ⧉ ↺
  13:47  Cursor         Rename the handler to onPasteFailed and add a…  ⧉ ↺
  13:30  Mail           Hi Nadia, attached is the March report. Best,…  ⧉ ↺
  ──────────────────────────────────────────────────────────────────────
```
- Greeting: display `--t-hero`, time-of-day aware, italic name. Right side: kbd hint of the PTT hotkey.
- Stats strip: 4 cells in one card, divided by vertical hairlines (not 4 separate cards). Label `--t-label`, number `--t-num`. Sparkline (7 bars, 2px wide, `--text-3`, today in `--tally`) under words. Streak shows 7 squares, filled `--text-2`, today tally if dictated.
- WPM = words / speaking seconds (audio duration), not wall time. Time saved = words/45 - words/WPM, in minutes. Show "vs 45 typed" as the reference with the source in a tooltip ("assumed typing baseline: 45 wpm").
- Recent: last 5. Row = mono time, app name (`--text-3`, 12px), single-line text truncated, copy and retry on hover.
- Number counters do not animate (seen often).

### 6.2 History

```
  *Every* word you said.
  ──────────────────────────────────────────────────────────────────────
  [⌕ Search transcripts…            ]                             ⋯

  TODAY
  ┌────────────────────────────────────────────────────────────────────┐
  │ 14:02 · Slack · 12 words · 4.8s · Whisper large-v3-turbo · Light   │
  │ Can you push the release after lunch? Thanks.                      │
  │                                                                    │
  │ [Formatted | Raw | Diff]          ▶ 0:05   ⧉ Copy   ↺ Retry ▾   ⌫ │
  └────────────────────────────────────────────────────────────────────┘
  13:47 · Cursor · Rename the handler to onPasteFailed and add a test…
  13:30 · Mail   · Hi Nadia, attached is the March report. Best, Pablo
```
- Search: `history_list(query)` (SQL `LIKE` on raw and formatted text), debounce 120ms, highlights matches with `--tally-soft` background (no tally text).
- Grouped by day with sticky mono day labels.
- Collapsed row: meta + one line. Click or Enter expands inline (height animation allowed here, 200ms `--ease-out`, it is occasional). Only one row expanded at a time.
- Expanded row: segmented `Formatted | Raw | Diff`. Diff view = the signature motion: a client-side word diff of raw vs formatted. Removed words are struck through in `--text-3` and inserted words are underlined in `--tally`. Dictionary fixes get a `DICTIONARY` mono tag, from `dict_preview(raw)`. No other tags: the pipeline stores none.
- Retry menu: `Re-transcribe` (`history_retry`; needs audio; disabled with tooltip if audio expired), `Paste again`, `Paste raw`.
- Audio play: an `<audio>` element on the Blob from `history_audio`, with a mono time. No waveform: no peaks are stored.
- Failed items show a `--danger` left tick and `Not pasted - Copy` as first action.
- Bulk: `⋯` menu = Export JSON (client-side), Delete all (`history_clear`, confirm button `Delete all history`).

### 6.3 Dictionary

Three tabs (segmented): `Words` | `Corrections` | `Snippets`. The backend has two kinds (PLAN 2.1): Word (text + aliases) and Snippet. Words lists Word entries. Corrections shows each Word alias as an `alias → text` row. Adding a correction to an existing Word appends an alias. Adding a correction to an unknown target creates a new Word with that alias. One shared namespace: the add form blocks a term that already exists in any tab, and says where.

```
  Your *vocabulary.*                                  ↓ Import   ↑ Export
  ──────────────────────────────────────────────────────────────────────
  [ Words 42 | Corrections 17 | Snippets 9 ]          [⌕ Filter…      ]

  WORDS - susurro hints these to every provider
  ┌────────────────────────────────────────────────────────────────────┐
  │ [ Add a word or phrase…                             ]  [ Add ↵ ]   │
  ├────────────────────────────────────────────────────────────────────┤
  │ Contaflow                                    used 31×   ✎  ⌫     │
  │ Tauri                                        used 12×   ✎  ⌫     │
  │ Francisca Ugarte                             used 4×    ✎  ⌫     │
  └────────────────────────────────────────────────────────────────────┘
  BIASING: Whisper prompt 38/42 fit · ElevenLabs keyterms 42 · LLM hints on

  CORRECTIONS
  │ [ heard as…        ] → [ write as…          ]  [ Add ↵ ]           │
  │ contra flow          →  Contaflow                    used 9×  ✎ ⌫ │
  │ sea lion            →  CLI                            used 2×  ✎ ⌫ │

  SNIPPETS
  │ TRIGGER              EXPANSION                                      │
  │ my calendly          https://calendly.com/pablo/30min     ✎ ⌫     │
  │ sign off             Best,\nPablo - {date}                 ✎ ⌫     │
```
- Add field is always first and focused when the tab opens. Enter adds. Validation inline under the field in `--danger` 12px: "Already in Snippets", "Max 60 characters", "Needs at least one letter or number".
- New row enters at top with `--tally-soft` background that fades out over 1.2s (occasional, so delight is fine).
- Correction row: two mono-ish columns with a `→`. Preview line under the add form: "contra flow is great" -> "**Contaflow** is great" rendered live as the user types.
- Snippet editor: click a row opens a sheet with trigger input, multiline expansion (mono, max 4000 with counter `312 / 4000`), variable chips to insert (`{date}`, `{time}`, `{weekday}`), and a live preview.
- Biasing footer: mono 11px, `--text-3`. It tells the truth about limits per provider (for example whisper's prompt token budget). Click opens a popover that lists which words did not fit, ordered by usage.
- Import/Export: JSON only, no backend. Export = `JSON.stringify(dict_list())`. Import loops over `dict_upsert` and then shows the result: `24 new · 3 duplicates skipped · 1 invalid`.
- Delete: no confirm. Toast with `Undo` for 5s.

### 6.4 Styles

Open decision (taste call for Pablo, PLAN 12): cut this screen to the cleanup level, one global style and a per-app `raw text` toggle, and move the 4 categories to v1.1. Recommendation: cut. Until Pablo decides, build the full screen below.

```
  How you *sound*, per app.
  ──────────────────────────────────────────────────────────────────────
  CLEANUP LEVEL     [ None | Light | Medium ]        needs an LLM provider

  ┌──────────────────────┐ ┌──────────────────────┐
  │ PERSONAL             │ │ WORK                 │
  │ Messages, WhatsApp…  │ │ Slack, Linear, Teams │
  │ ( ) Formal           │ │ (•) Formal           │
  │ (•) Casual           │ │ ( ) Casual           │
  │ ( ) Very casual      │ │ ( ) Very casual      │
  │ ( ) Excited          │ │ ( ) Excited          │
  │ ┌──────────────────┐ │ │ ┌──────────────────┐ │
  │ │ hey are you free │ │ │ │ Hey, are you free│ │
  │ │ tonight          │ │ │ │ tonight?         │ │
  │ └──────────────────┘ │ │ └──────────────────┘ │
  └──────────────────────┘ └──────────────────────┘
  ┌──────────────────────┐ ┌──────────────────────┐
  │ EMAIL                │ │ OTHER                │
  │ Mail, Superhuman…    │ │ everything else      │
  │ + greeting & sign-off│ │                      │
  └──────────────────────┘ └──────────────────────┘

  APP RULES                                              + Add app
  │ [icon] Cursor        com.todesktop.230313…     Other · raw text ▾   │
  │ [icon] Slack         com.tinyspeck.slackmacgap Work ▾              │
  │ [icon] Terminal      com.apple.Terminal        Other · raw text ▾   │
```
- 2x2 grid of category cards. Each has a radio list and a fixed sample sentence that re-renders in the chosen style (deterministic preview from the rule part; no LLM call on click).
- App rules: list of bundle IDs with the app name (no icon: no backend command), category select, and a `raw text` toggle (skip formatting for terminals and AI apps). `+ Add app` lists the recent apps from History first.
- If cleanup is `None` or no LLM provider is configured, cards stay usable (rule-based parts still apply) and show a one-line note: "Tone rewrites need an LLM. Punctuation and casing rules still apply."

### 6.5 Providers

```
  Where your *voice* goes.
  ──────────────────────────────────────────────────────────────────────
  SPEECH TO TEXT
  ┌────────────────────────────────────────────────────────────────────┐
  │ (•) Local Whisper            on this Mac · private · offline   ●  │
  │     MODEL                                                          │
  │     ┌──────────────────────────────────────────────────────────┐   │
  │     │ ✓ base.en q5_1       60 MB   fast           [Use] ⌫     │   │
  │     │ ● large-v3-turbo q5_0 574 MB best/default   in use  ⌫   │   │
  │     │   small q5_1        ··· MB   multilingual   [Download]   │   │
  │     │   large-v3-turbo q8_0 ▓▓▓▓▓▓░░░ 61%  12 MB/s  ✕         │   │
  │     └──────────────────────────────────────────────────────────┘   │
  │     Models live in ~/Library/Application Support/susurro/models    │
  ├────────────────────────────────────────────────────────────────────┤
  │ ( ) Local server (OpenAI-compatible)                            ○  │
  │     URL [ http://127.0.0.1:8080/v1        ]  MODEL [ whisper-1 ]   │
  │                                                     [ Test ]       │
  ├────────────────────────────────────────────────────────────────────┤
  │ ( ) ElevenLabs Scribe                         needs key         ○  │
  ├────────────────────────────────────────────────────────────────────┤
  │ ( ) OpenRouter (STT model)                    needs key         ○  │
  └────────────────────────────────────────────────────────────────────┘
  LANGUAGE   [ Auto-detect ▾ ]   One language is more accurate than auto.

  CLEANUP (LLM)
  ┌────────────────────────────────────────────────────────────────────┐
  │ PROVIDER [ OpenRouter ▾ ]   MODEL [ google/gemini-2.5-flash ▾ ]   │
  │ Off · OpenRouter · Local server (OpenAI-compatible)               │
  │ If cleanup fails, susurro pastes the raw transcript.              │
  └────────────────────────────────────────────────────────────────────┘

  KEYS                                   stored in macOS Keychain
  │ ElevenLabs   [ ••••••••••••••••a91f   ] ◐  [ Test ]  ✓ valid 2m ago │
  │ OpenRouter   [ Paste key…             ] ◐  [ Test ]                 │
```
- Provider list is a radio card group. Only the selected one expands its config (height animate 200ms). Unselected rows show a one-line state: `needs key`, `not reachable`, `ready`.
- Model manager: progress bar is a 4px track `--line`, fill `--text` (not tally: downloading is not "live"). Mono `61% · 12 MB/s · 1:04 left`. Cancel `✕`. A finished download (size matches) shows `✓`. Errors inline: "Download failed: disk full. 1.1 GB needed." + Retry.
- Model sizes come from the catalog in `models.rs`, not from this file. The values above are layout examples.
- Local server: default URL is whisper-server `http://127.0.0.1:8080/v1` (started with `--inference-path /v1/audio/transcriptions`). Helper line: "whisper-server :8080 or speaches :8000. LM Studio has no transcription endpoint." Every URL ends in `/v1`. LM Studio `http://127.0.0.1:1234/v1` is a cleanup option only.
- OpenRouter STT: model picker from `openrouter_models("stt")`. Helper line: "OpenRouter ignores the vocabulary prompt. Your words still apply in cleanup and corrections."
- Test buttons: label morphs `Test` -> spinner (12px) -> `✓ 340 ms` in `--ok` or `✕ 401 unauthorized` in `--danger`. Result stays inline with a relative time. Latency number is measured, never estimated.
- Key fields: never echo the full key after save. Paste auto-trims whitespace.
- No keys = default state works: Local Whisper selected, Cleanup `Off`. The screen must not look broken in this state. No red anywhere when keys are missing; use `--text-3` "needs key".

### 6.6 Audio

```
  Your *microphone*, handled.
  ──────────────────────────────────────────────────────────────────────
  IN USE   MacBook Pro Microphone · 48 kHz · mono          ● live

  LEVEL    ▮▮▮▮▮▮▮▮▮▮▮▮▮▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯▯ -18 dB     [ Test mic ]
           ╵    ╵    ╵    ╵    ╵    ╵    ╵   -60 -40 -20 -10 -6 0
  PRIORITY - drag to reorder. Auto uses the first connected device.
  ┌────────────────────────────────────────────────────────────────────┐
  │ (•) Auto (highest connected)                                       │
  │ ⋮⋮ 1  Shure MV7                USB      ● connected               │
  │ ⋮⋮ 2  MacBook Pro Microphone   built-in ● connected               │
  │ ⋮⋮ 3  AirPods Pro              Bluetooth○ disconnected  Forget    │
  │       Bluetooth lowers mic quality while in use.                   │
  ├────────────────────────────────────────────────────────────────────┤
  │ ▸ Show virtual devices (2)   BlackHole 2ch, Krisp Microphone       │
  └────────────────────────────────────────────────────────────────────┘
  ( ) Always use: [ Shure MV7 ▾ ]
```
- Main meter: 32 segments, 6x14 each, 2px gap. Segments fill `--meter-low` up to -20 dB, `--meter-mid` to -6 dB, `--meter-hot` above. Unlit segments `--line`. Peak-hold tick (1 segment) decays after 1s. Tick scale below in mono 10px. Same attack/release smoothing as the waveform.
- One live meter only, on the selected device (`mic_test_start(id)`). No per-row meters.
- New-mic prompts and the mid-dictation switch are always on (PLAN 3.3, 3.4). No toggles for them.
- Drag to reorder: pointer capture, row lifts (`--surface-2`, `--shadow-pop`), siblings shift with transform 150ms. Keyboard alternative: focus the handle, `⌥↑` / `⌥↓`. Live region announces "Shure MV7 moved to position 1".
- Disconnected rows: text at `--text-3` (contrast still passes), no opacity change, `Forget` ghost button.
- `Test mic` runs the live meter on the selected device (no recording, no playback). It stops on blur or after 30s. Errors inline: "No microphone found", "Microphone is in use by another app", "Permission denied - Open System Settings".
- Hot-plug toast (main window visible) or pill error-style notice (window hidden):
```
  ┌──────────────────────────────────────────┐
  │ ◉ New mic: Shure MV7                      │
  │ Switch to it for dictation?               │
  │                     [ Don't switch ] [Switch] │
  └──────────────────────────────────────────┘
```
  Device lost mid-session: pill shows `Switched to MacBook Pro Microphone` for 2.5s (warn icon, no danger). Recording continues.

### 6.7 Shortcuts

```
  Keys, *your* way.
  ──────────────────────────────────────────────────────────────────────
  PUSH TO TALK          [ right ⌥ ]                           Reset
  HANDS-FREE            double-tap push-to-talk ☑   or  [ + combo ]
  CANCEL                [ esc ]
  PASTE LAST            [ ⌃ ][ ⌘ ][ V ]

  ┌──────── recording a shortcut ────────┐
  │   Press up to 3 keys…                 │
  │   [ ⌥ ][ ⌘ ][ … ]                     │
  │   ⚠ ⌘C is reserved by the system      │
  │                        Esc to cancel  │
  └───────────────────────────────────────┘
```
- Each action row: micro-label, exactly one binding as a kbd group (click to record a new one), `Reset` ghost. Hands-free has an optional combo, hover `✕` removes it.
- Modifier-only bindings (CGEventTap): `fn`, and the left or right side of ⌥ ⌘ ⌃ ⇧ (`RightOption` ...). Default push to talk: `right ⌥` (most Mac keyboards that are not Apple's have no fn the Mac can see). A modifier pressed and released alone records that side; with other keys it records a combo. Only push to talk takes a modifier alone. Two modifiers with no key: "Add a key, or press one modifier alone".
- While a row records, quick-pick chips show next to it (push to talk: `right ⌥`, `right ⌘`, `fn`). Recording pauses every hotkey (`hotkey_capture`), so pressing the key does not start a dictation.
- Recording state: the binding slot shows a tally ring (this is "live" input, so tally is right) and pulses opacity. Validation messages per PLAN 4.1 (reserved combos, single letters, duplicates, left+right same modifier, modifier-only other than fn). Conflicts name the other action: "Already used by Hands-free".
- A modifier-only key needs Input Monitoring: if not granted, the row shows `--warn` "Right ⌥ needs Input Monitoring - Grant" linking to the permission step.
- Copy never says "fn": it names the real binding ("Hold Right ⌥ to talk").
- Practice field at bottom: "Try it here" textarea that receives a dictation, so the user tests without leaving.

### 6.8 Settings (general, data)
Plain grouped list: General (launch at login, sounds, theme `System | Dark | Light` stored in `localStorage`, show idle pill, max recording length), Data (save history on/off, keep audio `Never | 1 day | 14 days | 30 days`, Open data folder, Clear history with a confirm button `Delete all history`), Experimental (press-enter command). Rows 44px, label left, control right, helper text under the label.

### 6.9 Permissions onboarding

Full-window flow (replaces the shell, no sidebar). Shows on first launch and whenever a required permission is missing (resumes at the first missing step). Progress = 7 mono ticks at the top: `● ● ○ ○ ○ ○ ○`.

```
  ● ● ○ ○ ○ ○                                                 Skip for now

                                 susurro

               Talk. It *types.*

       um so I think we should, uh, ship it friday no actually thursday
                                 ↓
                 I think we should ship it Thursday.

                           [ Get started ]
```
Steps:
1. **Welcome**: the hero above. The messy line animates once into the clean line: fillers fade and strike (`FILLER` tags float up), "friday no actually" collapses (`BACKTRACK`), casing and period settle. 2.4s total, plays once, `Replay` link. Reduced motion: show both lines static.
2. **Microphone**: explain in one sentence, `Allow microphone` primary. On grant: live meter appears with "Say something" and turns `--ok` when speech > -40 dB is seen.
3. **Accessibility**: needed to paste. `Open System Settings` (deep link `x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`). The screen polls the permission every 1s, re-checks the moment the window gets focus back, and advances by itself with a check. Show a small static diagram of the toggle to flip. If the user came back and the grant still does not show after 3 s, or `needs_relaunch` is true, the primary becomes `Restart susurro` (`relaunch_app`) with one line: "macOS applies <pane> after a restart. susurro reopens right here." The secondary link `I already enabled it` restarts too. The app reopens on the same step (localStorage `susurro.obStep`).
4. **Input Monitoring (+ fn key)**: needed for a modifier-only key; the title names it ("Hold Right ⌥ to talk."). The fn check below applies only when the key is `fn`. Same pattern (`?Privacy_ListenEvent`). After the grant, the same step checks the "Press fn key to" setting: if it is not `Do Nothing`, show "Set 'Press fn key to' = Do Nothing, or fn also opens the emoji picker" with a deep link to Keyboard settings, and advance when it is fixed. `Use ⌥ Space instead` secondary link skips the step and sets the fallback hotkey (global shortcut, no permission).
5. **Your talk key**: one big keycap with the binding (click it to record your own), four tiles: `right ⌥` Recommended, `right ⌘`, `fn` + globe (Apple keyboards), `Record your own`. A live line under it: "Right ⌥ is ready. Hold it and say something." turns `--ok` ("Right ⌥ works.") when a dictation starts; `hotkey_status` errors show in `--warn`.
6. **Your shortcut**: choose `Hold to talk` vs `Tap to toggle`, shows kbd. Language select (Auto + top picks `English`, `Español`).
7. **Try it**: a practice textarea. "Hold <key> and say: *this is my first dictation*." Success shows the transcript, words count and time, and `Finish`. While here, check the default whisper model exists; if not, the step shows the download progress (base q5_1 first so the first try is fast; turbo q5_0 downloads after in the background), or a link to pick a cloud provider.

Step transition: content slides 16px + fades, 320ms `--ease-drawer`. Back allowed. Every step states why the permission is needed in one line, and what does not happen ("Audio stays on this Mac with Local Whisper.").

---

## 7. Empty states

Pattern: serif line (h2, one italic word) + one helper line + at most one action. No illustrations. A faint 12-bar static waveform glyph in `--line-strong` above the line is the only ornament, same everywhere.

| Screen | Line | Helper | Action |
|---|---|---|---|
| Home recent | Nothing said *yet.* | Hold `<key>` anywhere and talk. | Try it (opens practice) |
| History | *Quiet* in here. | Your dictations show up here. | - |
| History search | No match for "deploy". | Search covers raw and formatted text. | Clear search |
| Words | Teach it your *words.* | Names, brands, jargon. susurro hints them to every provider. | focus add field |
| Corrections | Fix what it *mishears.* | "contra flow" → "Contaflow", every time. | focus add field |
| Snippets | Say less, *paste more.* | A short phrase expands into any text. | New snippet |
| Audio, no devices | No microphone *found.* | Plug one in. susurro picks it up right away. | - |
| Providers download error | see 6.5 inline | | Retry |

## 8. Microcopy

- Voice: calm studio engineer. Short, concrete, second person, present tense. No exclamation marks except the "Excited" style sample. No "Oops", no "Something went wrong".
- Errors = what happened + what to do: "OpenRouter rejected the key (401). Check it in Providers." / "Mic disconnected. Switched to MacBook Pro Microphone."
- Never blame the user. Never lose text: every failure copy that involves a transcript ends with where the text is ("Copied to clipboard", "Saved in History").
- Buttons are verbs: `Download`, `Test`, `Switch`, `Grant`, not `OK`. Confirm dialogs restate the action in the button: `Delete 17 corrections`.
- Units in mono, lowercase: `ms`, `dB`, `MB/s`, `wpm` in labels is caps `WPM` (micro-label style).
- Spanish copy is a later task. Keep strings in one `strings.ts` map from day one so it is cheap.

## 9. Accessibility

1. Focus: every interactive element uses `:focus-visible { outline: none; box-shadow: var(--focus); }`. Never remove focus without the replacement. Inputs use their own focus border + halo (section 4).
2. Contrast: all text tokens pass 4.5:1 on their intended surfaces (measured, see token comments). Tally on `--surface-2` in light mode is 4.2:1: use it only for non-text (focus ring, ticks), never small text there.
3. Color is never the only signal: status dots have text, meter has dB number, diff changes use strike/underline plus the `DICTIONARY` label, errors have an icon.
4. Keyboard: full app usable without a mouse. Sidebar `⌘1-8`, `⌘F` focuses search on History/Dictionary, `⌘N` adds in Dictionary, `Esc` closes sheets/popovers, arrow keys move in lists and segmented controls (roving tabindex).
5. Screen readers: pill state changes go to an `aria-live="polite"` region in the main window too ("Listening", "Pasted", "Error: mic disconnected"). Meters get `role="meter"` with `aria-valuenow` updated at most 4 times per second. Waveform canvas is `aria-hidden`.
6. Reduced motion: honor `prefers-reduced-motion` (tokens set durations; see pill and onboarding notes). Keep opacity fades, remove movement, keep meters live (they are data).
7. Hit targets: 24x24 minimum (pill buttons 20px visual, 28px hit area via padding).
8. Respect macOS "Increase contrast": `@media (prefers-contrast: more)` sets `--line: var(--line-strong)` and `--text-3: var(--text-2)`.

## 10. Motion summary

| What | Duration | Easing | Notes |
|---|---|---|---|
| Button press | 120ms | `--ease-out` | scale 0.97 |
| Hover color | 150ms | `ease` | color/background only, gated `@media (hover: hover)` |
| Popover/tooltip | 150ms | `--ease-out` | scale 0.97 + opacity from trigger |
| Toast | 200ms in / 150ms out | `--ease-out` | transitions, not keyframes |
| Segmented / tab indicator | 200ms | `--ease-out` | transform only |
| History row expand | 200ms | `--ease-out` | occasional |
| Pill morph | 260ms | `--ease-drawer` | width/height, content blur crossfade |
| Sheet / onboarding step | 320ms | `--ease-drawer` | |
| Screen navigation | 0ms | - | frequent, keyboard-driven |
| Stat numbers | 0ms | - | no count-up |
| Recording start | 0ms gate | - | capture never waits for animation |

Never: `transition: all`, `scale(0)` entries, `ease-in` on UI, bounce springs, parallax, skeleton shimmer (local data loads instantly; use nothing or a 12px spinner after 300ms).

## 11. Implementation notes for the frontend

- Plain CSS with the tokens above + CSS modules or one `app.css`. No Tailwind, no component library. Lucide for icons. No Framer Motion: CSS transitions + WAAPI cover everything here; the canvas loops use rAF.
- One Vite entry. `main.tsx` branches on the path: `/overlay` renders the pill, everything else renders the main window. Lazy-import the main window so the pill stays small: no router, no app state store, only the event listener + canvas.
- Theme: `data-theme` on `<html>`, value from `localStorage`, `system` resolved via `matchMedia` with a change listener.
- Body defaults: `font: 13px/20px var(--font-ui); color: var(--text); background: var(--bg); -webkit-font-smoothing: antialiased; user-select: none;` and `user-select: text` on transcript text and inputs.
- Scrollbars: thin, `--line-strong` thumb, transparent track.
