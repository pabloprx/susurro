#!/usr/bin/env bun
// Static IPC contract check (runs in `bun run check`). Fails if:
//  1. a command called in src/lib/api.ts is not in the Tauri invoke_handler, or its args do not match the Rust params;
//  2. a command in api.ts has no handler in src/lib/mock.ts (the browser mock would return undefined);
//  3. an event the frontend listens to (on("x")) is never emitted by Rust, or is missing from the Events type;
//  4. src/lib/contract.gen.ts is stale vs the Rust types (ts-rs export into a temp dir, then diff).
import { readFileSync, readdirSync, statSync, mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { spawnSync } from "node:child_process";

const root = join(import.meta.dir, "..");
const read = (p) => readFileSync(join(root, p), "utf8");
const walk = (dir) =>
  readdirSync(join(root, dir)).flatMap((f) => {
    const p = join(dir, f);
    return statSync(join(root, p)).isDirectory() ? walk(p) : [p];
  });
const errors = [];
const fail = (m) => errors.push(m);

// ---------- commands ----------
const api = read("src/lib/api.ts");
const calls = [...api.matchAll(/call<[^>]*>\("(\w+)"(?:,\s*\{([^}]*)\})?\)/g)].map((m) => ({
  cmd: m[1],
  args: (m[2] ?? "").split(",").map((s) => s.split(":")[0].trim()).filter(Boolean),
}));
if (calls.length < 10) fail(`api.ts: parsed only ${calls.length} commands; the parser is broken`);

const lib = read("src-tauri/src/lib.rs");
const handler = lib.match(/generate_handler!\[([\s\S]*?)\]/)?.[1] ?? "";
const registered = new Map(
  handler.split(",").map((s) => s.trim()).filter(Boolean).map((path) => [path.split("::").pop(), path]),
);
if (!registered.size) fail("lib.rs: no generate_handler! list found");

const rustFiles = walk("src-tauri/src").filter((f) => f.endsWith(".rs"));
const rust = Object.fromEntries(rustFiles.map((f) => [f, read(f)]));
const camel = (s) => s.replace(/_([a-z])/g, (_, c) => c.toUpperCase());
// Params Tauri injects itself (not sent by the frontend).
const injected = /^(State|tauri::State|AppHandle|tauri::AppHandle|Window|tauri::Window|WebviewWindow|tauri::WebviewWindow|Webview|tauri::ipc::Request|Request|Channel)\b/;

function rustParams(name) {
  for (const src of Object.values(rust)) {
    const re = new RegExp(`#\\[tauri::command[^\\]]*\\]\\s*pub\\s+(?:async\\s+)?fn\\s+${name}\\s*\\(`);
    const m = re.exec(src);
    if (!m) continue;
    let depth = 1, i = m.index + m[0].length, start = i;
    while (depth && i < src.length) depth += src[i] === "(" ? 1 : src[i] === ")" ? -1 : 0, i++;
    const params = [];
    let cur = "", d = 0;
    for (const ch of src.slice(start, i - 1) + ",") {
      if ("<([".includes(ch)) d++;
      if (">)]".includes(ch)) d--;
      if (ch === "," && d === 0) {
        const [n, ...t] = cur.split(":");
        if (n.trim() && !injected.test(t.join(":").trim())) params.push(camel(n.trim().replace(/^mut\s+/, "")));
        cur = "";
      } else cur += ch;
    }
    return params;
  }
  return null;
}

const mock = read("src/lib/mock.ts");
const mockBlock = mock.match(/const handlers[^\n]*\{([\s\S]*?)\n\};/)?.[1] ?? "";
const mockHandlers = new Set([...mockBlock.matchAll(/^\s{2}(\w+):\s/gm)].map((m) => m[1]));
for (const { cmd, args } of calls) {
  if (!registered.has(cmd)) {
    fail(`command "${cmd}" is called in api.ts but not registered in invoke_handler (lib.rs)`);
    continue;
  }
  const params = rustParams(cmd);
  if (!params) fail(`command "${cmd}": no #[tauri::command] fn ${cmd} found in src-tauri/src`);
  else {
    const want = [...params].sort().join(",");
    const got = [...args].sort().join(",");
    if (want !== got) fail(`command "${cmd}": api.ts sends {${got}} but Rust expects {${want}}`);
  }
  if (!mockHandlers.has(cmd)) fail(`command "${cmd}" has no handler in mock.ts`);
}
const called = new Set(calls.map((c) => c.cmd));
for (const name of registered.keys()) if (!called.has(name)) console.warn(`warn: command "${name}" is registered but api.ts never calls it`);

// ---------- events ----------
const frontend = walk("src").filter((f) => /\.(ts|tsx)$/.test(f) && !f.endsWith("mock.ts"));
const listened = new Set(frontend.flatMap((f) => [...read(f).matchAll(/\bon\(\s*"(\w+)"/g)].map((m) => m[1])));
const eventsType = api.match(/export type Events = \{([\s\S]*?)\};/)?.[1] ?? "";
const typed = new Set([...eventsType.matchAll(/^\s*(\w+):/gm)].map((m) => m[1]));
const emitted = new Set(
  Object.values(rust).flatMap((src) => [...src.matchAll(/\.emit(?:_to)?\(\s*(?:[^,()]+,\s*)?"(\w+)"/g)].map((m) => m[1])),
);
if (!listened.size) fail("frontend: no on(\"event\") calls found; the parser is broken");
for (const e of listened) {
  if (!emitted.has(e)) fail(`event "${e}" is listened to in the frontend but Rust never emits it`);
  if (!typed.has(e)) fail(`event "${e}" is listened to but missing from the Events type in api.ts`);
}
for (const e of typed) if (!emitted.has(e)) fail(`event "${e}" is in the Events type but Rust never emits it`);
for (const e of emitted) if (!listened.has(e)) console.warn(`warn: Rust emits "${e}" but the frontend never listens`);

// ---------- contract.gen.ts freshness ----------
if (!process.argv.includes("--no-gen")) {
  const tmp = mkdtempSync(join(tmpdir(), "susurro-tsrs-"));
  const r = spawnSync("cargo", ["test", "--manifest-path", "src-tauri/Cargo.toml", "--lib", "export_bindings", "-q"], {
    cwd: root,
    env: { ...process.env, TS_RS_EXPORT_DIR: tmp },
    encoding: "utf8",
  });
  if (r.status !== 0) fail(`ts-rs export failed:\n${r.stderr.slice(-2000)}`);
  else {
    let fresh = "";
    try {
      fresh = readFileSync(join(tmp, "contract.gen.ts"), "utf8");
    } catch {
      fail("ts-rs export wrote no contract.gen.ts");
    }
    if (fresh && fresh !== read("src/lib/contract.gen.ts"))
      fail("src/lib/contract.gen.ts is stale vs the Rust types: run `cargo test --manifest-path src-tauri/Cargo.toml` and commit it");
  }
  rmSync(tmp, { recursive: true, force: true });
}

if (errors.length) {
  console.error(`contract check FAILED (${errors.length}):\n- ${errors.join("\n- ")}`);
  process.exit(1);
}
console.log(`contract check ok: ${calls.length} commands, ${listened.size} events, contract.gen.ts fresh`);
