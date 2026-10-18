// web.js (the script susurro injects into the browser tab, src-tauri/src/web.rs) run for real in a headless page.
// Fixtures are small copies of the page shapes voice control meets: Google results, YouTube, a pricing page,
// nested cards. No network, no real browser of Pablo's.
import { test, expect } from "@playwright/test";
import { readFileSync } from "node:fs";

const JS = readFileSync(new URL("../src-tauri/src/web.js", import.meta.url), "utf8");
const run = (page, p) => page.evaluate(([src, p]) => JSON.parse((0, eval)(src)(p)), [JS, p]);
const scan = async (page) => (await run(page, { op: "scan" })).els;
const named = (els, name) => els.filter((e) => e.name === name);
const sig = async (page) => (await run(page, { op: "sig" })).sig;

const GOOGLE = `
<form role="search"><textarea role="combobox" aria-label="Buscar" name="q">elevenlabs</textarea><button aria-label="Buscar con Google"><svg width="20" height="20"></svg></button></form>
<div id="rso">
  <div class="g" data-hveid="1"><a href="https://elevenlabs.io/"><h3>ElevenLabs: AI Voice Generator</h3><cite>elevenlabs.io</cite></a><div>Create lifelike voiceovers.</div></div>
  <div class="g"><a href="https://elevenlabs.io/pricing"><h3>Pricing | ElevenLabs</h3><cite>elevenlabs.io › pricing</cite></a></div>
  <details><summary>People also ask: Is ElevenLabs free?</summary><p>Yes, a free tier.</p></details>
</div>
<a href="https://example.com/far" style="position:absolute;top:5000px">Far below</a>`;

const YOUTUBE = `
<style>.rich{display:inline-block;width:300px;margin:8px}.menu{display:none}</style>
<button aria-label="Search"><svg width="24" height="24"></svg></button>
<input id="search" placeholder="Search" />
<div class="rich"><a id="thumbnail" href="/watch?v=abc"><img alt="" width="280" height="160"></a>
  <a id="video-title-link" href="/watch?v=abc" aria-label="Lofi beats by Chill 1 day ago">Lofi beats</a>
  <button aria-label="Action menu">⋮</button><div class="menu"><button>Save to playlist</button></div></div>
<div class="rich"><a href="/watch?v=def" title="Rust in 100 seconds"><img alt="Rust in 100 seconds" width="280" height="160"></a></div>`;

const PRICING = `
<style>.card{display:inline-block;width:220px;padding:20px;border:1px solid}</style>
<span id="bill">Yearly billing</span><div role="switch" aria-checked="false" aria-labelledby="bill" tabindex="0" onclick="this.setAttribute('aria-checked', this.getAttribute('aria-checked') === 'true' ? 'false' : 'true')" style="width:40px;height:20px"></div>
<div class="card" role="button" onclick="location.hash='free'"><h2>Free</h2><p>$0</p><button onclick="event.stopPropagation(); location.hash='start-free'">Get started</button></div>
<div class="card" role="button" onclick="location.hash='pro'"><h2>Pro</h2><p>$22</p><button onclick="location.hash='start-pro'">Upgrade</button></div>
<button disabled>Enterprise (soon)</button>
<label for="email">Work email</label><input id="email" type="email" />
<input type="password" aria-label="Password" />
<input type="hidden" name="csrf" value="x" />`;

const NESTED = `
<div class="card" onclick="void 0" style="padding:10px"><a href="/docs">Docs</a><p>All the API reference.</p></div>
<a href="/same" style="display:block;padding:10px"><div role="button">Open the same thing</div></a>
<ul><li onclick="void 0" style="padding:8px"><span>Comercial X</span></li></ul>
<div id="cover-host" style="position:relative;height:60px"><button id="covered">Under the banner</button>
  <div style="position:absolute;inset:0;background:#fff">Cookie banner</div></div>`;

test("Google results: the search box, one link per result, not the far ones", async ({ page }) => {
  await page.setContent(GOOGLE);
  const els = await scan(page);
  const box = named(els, "Buscar")[0];
  expect(box).toMatchObject({ role: "combo box", value: "elevenlabs" });
  expect(named(els, "Buscar con Google")[0].role).toBe("button");
  const links = els.filter((e) => e.role === "link");
  expect(links.map((l) => l.href)).toEqual(["https://elevenlabs.io/", "https://elevenlabs.io/pricing"]);
  expect(links[0].name).toBe("ElevenLabs: AI Voice Generator elevenlabs.io");
  expect(els.some((e) => e.role === "disclosure" && e.name.startsWith("People also ask"))).toBe(true);
  expect(els.some((e) => e.name === "Far below")).toBe(false);
  // reading order: the search row first
  expect(els[0].name).toBe("Buscar");
  // ids are stable across scans
  expect((await scan(page)).map((e) => e.id)).toEqual(els.map((e) => e.id));
});

test("YouTube: aria-label names, image alt for a bare thumbnail, hidden menus skipped", async ({ page }) => {
  await page.setContent(YOUTUBE);
  const els = await scan(page);
  expect(named(els, "Search").map((e) => e.role).sort()).toEqual(["button", "text field"]);
  expect(named(els, "Lofi beats by Chill 1 day ago")[0].href).toMatch(/\/watch\?v=abc$/);
  expect(named(els, "Rust in 100 seconds")[0].role).toBe("link");
  expect(named(els, "Action menu")).toHaveLength(1);
  expect(named(els, "Save to playlist")).toHaveLength(0);
  for (const e of els) expect(e.w).toBeGreaterThanOrEqual(4);
});

test("pricing page: buttons inside clickable cards, a switch with state, no secrets", async ({ page }) => {
  await page.setContent(PRICING);
  const els = await scan(page);
  const names = els.map((e) => e.name);
  expect(names).toContain("Get started");
  expect(names).toContain("Upgrade");
  // the role=button cards wrap a real button: the leaf stays, the card goes
  expect(names.some((n) => n.startsWith("Free"))).toBe(false);
  expect(names).not.toContain("Enterprise (soon)");
  expect(names).toContain("Work email");
  expect(els.some((e) => /password/i.test(e.name))).toBe(false);
  expect(els.some((e) => e.name === "csrf")).toBe(false);
  const sw = named(els, "Yearly billing")[0];
  expect(sw).toMatchObject({ role: "switch", state: "off" });
  const before = await sig(page);
  await run(page, { op: "click", id: sw.id, name: sw.name });
  expect(await sig(page)).not.toBe(before);
  expect(named(await scan(page), "Yearly billing")[0].state).toBe("on");
});

test("nested cards: the link inside, the leaf of a same-name wrapper, a plain clickable row, nothing covered", async ({ page }) => {
  await page.setContent(NESTED);
  const els = await scan(page);
  expect(named(els, "Docs")[0].role).toBe("link");
  expect(els.some((e) => e.name.startsWith("Docs All"))).toBe(false);
  const same = named(els, "Open the same thing");
  expect(same).toHaveLength(1);
  expect(same[0].role).toBe("button");
  expect(named(els, "Comercial X")).toHaveLength(1);
  expect(named(els, "Under the banner")).toHaveLength(0);
});

test("acts by id: click navigates, focus, fill fires input + change through the value setter, scroll, gone", async ({ page }) => {
  await page.setContent(`${PRICING}<div style="height:3000px"></div><a href="#bottom" id="b">Bottom link</a>`);
  const els = await scan(page);
  const go = named(els, "Get started")[0];
  const before = await sig(page);
  const r = await run(page, { op: "click", id: go.id, name: go.name });
  expect(r.url).toMatch(/#start-free$/);
  expect(await sig(page)).not.toBe(before);

  const email = named(els, "Work email")[0];
  await page.evaluate(() => {
    const el = document.getElementById("email");
    window.events = [];
    for (const t of ["input", "change"]) el.addEventListener(t, () => window.events.push(t + ":" + el.value));
  });
  const f = await run(page, { op: "focus", id: email.id });
  expect(f.focused).toBe(true);
  expect(typeof f.hasFocus).toBe("boolean");
  await run(page, { op: "fill", id: email.id, value: "pablo@example.com" });
  expect(await page.evaluate(() => window.events)).toEqual(["input:pablo@example.com", "change:pablo@example.com"]);

  // the bottom link is not on screen: no mark, but an act by id scrolls it into view
  expect(els.some((e) => e.name === "Bottom link")).toBe(false);
  await page.evaluate(() => document.getElementById("b").setAttribute("data-susurro-id", "999"));
  const s = await run(page, { op: "scroll", id: 999 });
  expect(s.rect.y).toBeGreaterThanOrEqual(0);
  expect(s.rect.y).toBeLessThan(s.ih);

  expect(await run(page, { op: "click", id: 12345 })).toEqual({ err: "gone" });
  // an id is digits only: nothing else reaches the selector
  expect(await run(page, { op: "click", id: '1"]' })).toEqual({ err: "gone" });
});

test("a click that changes nothing keeps the same signature (the Rust side then clicks for real)", async ({ page }) => {
  await page.setContent(`<button onclick="">Does nothing</button>`);
  const [b] = await scan(page);
  const before = await sig(page);
  const r = await run(page, { op: "click", id: b.id, name: b.name });
  expect(await sig(page)).toBe(before);
  expect(r.rect.w).toBeGreaterThan(0);
});

test("stale ids: a re-rendered row with another name is gone, never clicked", async ({ page }) => {
  await page.setContent(`<ul id="l"><li><button onclick="window.hit='Ana'">Ana</button></li><li><button onclick="window.hit='Beto'">Beto</button></li></ul>`);
  const els = await scan(page);
  const ana = named(els, "Ana")[0];
  // a list keyed by index: the same node now shows another person
  await page.evaluate(() => { const b = document.querySelector("button"); b.textContent = "Carla"; b.onclick = () => (window.hit = "Carla"); });
  expect(await run(page, { op: "click", id: ana.id, name: ana.name })).toEqual({ err: "gone" });
  expect(await page.evaluate(() => window.hit)).toBeUndefined();
  // a new page reuses the counter: id 1 there is another element, the name check stops it
  await page.setContent(`<button onclick="window.hit='Borrar'">Borrar todo</button>`);
  const fresh = await scan(page);
  expect(fresh[0].id).toBe(ana.id);
  expect(await run(page, { op: "click", id: ana.id, name: ana.name })).toEqual({ err: "gone" });
  expect(await page.evaluate(() => window.hit)).toBeUndefined();
});

test("shadow DOM: elements inside open shadow roots are marks and acts find them", async ({ page }) => {
  await page.setContent(`<div id="host"></div><script>
    const r = document.getElementById("host").attachShadow({ mode: "open" });
    r.innerHTML = '<a href="#in-shadow">Comment</a><div style="position:relative;height:40px"><button>Hidden vote</button><div style="position:absolute;inset:0;background:#fff">cover</div></div>';
  </script>`);
  const els = await scan(page);
  const c = named(els, "Comment")[0];
  expect(c.role).toBe("link");
  expect(named(els, "Hidden vote")).toHaveLength(0);
  await run(page, { op: "click", id: c.id, name: c.name });
  expect(page.url()).toMatch(/#in-shadow$/);
});

test("a navigation that has started counts as changed (no real click on top, no double submit)", async ({ page }) => {
  await page.route("http://slow.test/**", async (route) => {
    await new Promise((r) => setTimeout(r, 3000));
    await route.fulfill({ body: "<p>later</p>", contentType: "text/html" });
  });
  await page.goto("about:blank");
  await page.setContent(`<form action="http://slow.test/pay" method="get"><button>Pay</button></form>`);
  const [b] = await scan(page);
  const before = await sig(page);
  // the click and the next sig in one evaluate: while the navigation is pending Playwright holds new evaluates (the
  // real osascript then times out, which web.rs also counts as changed)
  const after = await page.evaluate(async ([src, b]) => {
    const f = (0, eval)(src);
    f({ op: "click", id: b.id, name: b.name });
    await new Promise((r) => setTimeout(r, 250)); // exec's SETTLE_MS
    return JSON.parse(f({ op: "sig" })).sig;
  }, [JS, b]);
  expect(after).not.toBe(before);
});

test("a big iframe sends the page to the AX tree; a small one does not", async ({ page }) => {
  await page.setContent(`<button>Pay</button><iframe srcdoc="<input placeholder=card>" style="width:90vw;height:80vh"></iframe>`);
  expect((await run(page, { op: "scan" })).frames).toBe(1);
  await page.setContent(`<button>Pay</button><iframe srcdoc="ad" style="width:120px;height:60px"></iframe>`);
  expect((await run(page, { op: "scan" })).frames).toBe(0);
});

test("CSP + Trusted Types: the script uses no DOM sink they block", async ({ page }) => {
  await page.setContent(`<meta http-equiv="Content-Security-Policy" content="require-trusted-types-for 'script'; trusted-types none"><button>Go</button>`);
  const [b] = await scan(page);
  expect(b.name).toBe("Go");
  expect(await run(page, { op: "click", id: b.id, name: b.name })).not.toHaveProperty("err");
});

test("fill refuses a password field even with its id", async ({ page }) => {
  await page.setContent(`<input type="password" data-susurro-id="5">`);
  expect(await run(page, { op: "fill", id: 5, value: "x" })).toEqual({ err: "secure" });
  expect(await run(page, { op: "bogus", id: 5 })).toEqual({ err: "op" });
});

// vc-feedback 26: JS Paint's palette is divs with data-color and no text; each swatch is a mark named by its color.
const PALETTE = `
<style>.palette{position:absolute;bottom:0;left:40px}.swatch{display:inline-block;width:16px;height:16px;margin:1px}</style>
<div role="menubar"><div role="menuitem">Colors</div></div>
<div class="palette">
  <div class="swatch color-button" data-color="rgb(0,0,0)"></div><div class="swatch color-button" data-color="rgb(128,0,0)"></div>
  <div class="swatch color-button" data-color="rgb(255,0,0)" onpointerdown="window.picked=this.dataset.color"></div>
  <div class="swatch color-button" data-color="#0000ff"></div><div class="swatch color-button" title="Custom"></div>
</div>`;

test("paint palette: swatches are marks named by color, the click reaches pointerdown", async ({ page }) => {
  await page.setContent(PALETTE);
  const els = await scan(page);
  expect(els.map((e) => e.name)).toEqual(["Colors", "black color", "dark red color", "red color", "blue color", "Custom"]);
  const red = named(els, "red color")[0];
  expect(red.role).toBe("button");
  const r = await run(page, { op: "click", id: red.id, name: "red color" });
  expect(r.err).toBeUndefined();
  expect(await page.evaluate(() => window.picked)).toBe("rgb(255,0,0)");
});
