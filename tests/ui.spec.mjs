// UI tests for every screen and control, in the browser mock (src/lib/mock.ts). Run: bun run test:ui
// Every test fails on a console error, a page error or a failed request.
import { test as base, expect } from "@playwright/test";
import { readFileSync } from "node:fs";

const test = base.extend({
  guard: [
    async ({ page }, use) => {
      const errs = [];
      page.on("console", (m) => m.type() === "error" && errs.push(`console: ${m.text()}`));
      page.on("pageerror", (e) => errs.push(`pageerror: ${e.message}`));
      page.on("requestfailed", (r) => errs.push(`requestfailed: ${r.url()} ${r.failure()?.errorText}`));
      await use(errs);
      expect(errs, "no console/page errors").toEqual([]);
    },
    { auto: true },
  ],
});

const nav = (page, name) => page.getByRole("navigation", { name: "Screens" }).getByRole("button", { name, exact: true }).click();
const h1 = (page) => page.locator("main h1");
const toastText = (page, text) => page.locator(".toasts .toast", { hasText: text });
const sw = (page, label) => page.getByRole("switch", { name: label, exact: true });
const radio = (page, group, name) => page.getByRole("radiogroup", { name: group }).getByRole("radio", { name, exact: true });

async function open(page, path = "/") {
  await page.goto(path);
  await expect(page.locator(".shell, .onboarding, .pill-root").first()).toBeVisible();
}

// ---------------- shell ----------------

test.describe("shell", () => {
  const screens = [
    ["Home", /Good|Late/],
    ["History", "Every word you said."],
    ["Dictionary", "Your vocabulary."],
    ["Styles", "How you sound."],
    ["Providers", "Where your voice goes."],
    ["Audio", "Your microphone, handled."],
    ["Shortcuts", "Keys, your way."],
    ["Settings", "The quiet details."],
  ];
  test("sidebar opens every screen", async ({ page }) => {
    await open(page);
    for (const [name, title] of screens) {
      await nav(page, name);
      await expect(h1(page)).toHaveText(title);
      await expect(page.getByRole("button", { name, exact: true })).toHaveAttribute("aria-current", "page");
    }
  });
  test("Cmd+1..8 opens every screen", async ({ page }) => {
    await open(page);
    for (let i = screens.length - 1; i >= 0; i--) {
      await page.keyboard.press(`Meta+${i + 1}`);
      await expect(h1(page)).toHaveText(screens[i][1]);
    }
  });
  test("sidebar instruments go to Audio and Providers", async ({ page }) => {
    await open(page);
    await page.locator(".instruments button").first().click();
    await expect(h1(page)).toHaveText("Your microphone, handled.");
    await expect(page.locator(".instruments")).toContainText("MacBook Pro Microphone");
    await page.locator(".instruments button").nth(1).click();
    await expect(h1(page)).toHaveText("Where your voice goes.");
    await expect(page.locator(".instruments")).toContainText("large-v3-turbo q5_0");
  });
  test("screen-reader live region announces session phases", async ({ page }) => {
    await open(page);
    await page.evaluate(() => window.mock.dictate());
    await expect(page.locator(".shell > .sr-only")).toHaveText("Listening");
    await page.evaluate(() => window.mock.stop());
    await expect(page.locator(".shell > .sr-only")).toHaveText("Processing");
    await expect(page.locator(".shell > .sr-only")).toHaveText("Pasted");
    await page.evaluate(() => window.mock.error("Mic gone"));
    await expect(page.locator(".shell > .sr-only")).toHaveText("Error: Mic gone");
  });
});

// ---------------- home ----------------

test.describe("home", () => {
  test("stats, recent rows, copy, re-transcribe, view all", async ({ page }) => {
    await open(page);
    await expect(page.locator(".stats .stat")).toHaveCount(4);
    await expect(page.locator(".stats .num").first()).not.toHaveText("0");
    const rows = page.locator(".recent .row");
    await expect(rows).toHaveCount(5);
    await expect(rows.first()).toContainText("Can you push the release after lunch? Thanks.");
    await rows.first().hover();
    await rows.first().getByRole("button", { name: "Copy" }).click();
    await expect(toastText(page, "Copied to clipboard")).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("Can you push the release after lunch? Thanks.");
    await rows.first().getByRole("button", { name: "Re-transcribe" }).click();
    await expect(toastText(page, "Re-transcribed. Saved in History.")).toBeVisible();
    await page.getByRole("button", { name: "View all" }).click();
    await expect(h1(page)).toHaveText("Every word you said.");
  });
  test("a new dictation shows up in Recent", async ({ page }) => {
    await open(page);
    await expect(page.locator(".recent .row")).toHaveCount(5);
    await page.evaluate(() => window.mock.dictate());
    await page.evaluate(() => window.mock.stop());
    await expect(page.locator(".recent .row").first()).toContainText("The new build is up. Can you test the hot-plug flow?");
  });
  test("empty state", async ({ page }) => {
    await open(page, "/?empty");
    await expect(page.getByText("Nothing said yet.")).toBeVisible();
    await page.getByRole("button", { name: "Try it" }).click();
    await expect(h1(page)).toHaveText("Keys, your way.");
  });
});

// ---------------- history ----------------

test.describe("history", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "History");
    await expect(page.locator(".h-item").first()).toBeVisible();
  });
  test("search highlights, no-match empty state, clear", async ({ page }) => {
    const all = await page.locator(".h-item").count();
    expect(all).toBe(18);
    await page.getByLabel("Search transcripts").fill("Contaflow");
    await expect(page.locator(".h-item")).toHaveCount(1);
    await expect(page.locator(".h-item mark")).toHaveText("Contaflow");
    await page.getByLabel("Search transcripts").fill("zzzz-nothing");
    await expect(page.getByText("No match for “zzzz-nothing”.")).toBeVisible();
    await page.getByRole("button", { name: "Clear search" }).click();
    await expect(page.locator(".h-item")).toHaveCount(18);
  });
  test("Cmd+F focuses search; arrow keys move between rows", async ({ page }) => {
    await page.keyboard.press("Meta+f");
    await expect(page.getByLabel("Search transcripts")).toBeFocused();
    await page.locator(".h-row").first().focus();
    await page.keyboard.press("ArrowDown");
    await expect(page.locator(".h-row").nth(1)).toBeFocused();
    await page.keyboard.press("ArrowUp");
    await expect(page.locator(".h-row").first()).toBeFocused();
  });
  test("expand a row: views, diff, play, copy, retry menu, delete", async ({ page }) => {
    const item = page.locator(".h-item").first();
    await item.locator(".h-row").click();
    await expect(item.locator(".h-row")).toHaveAttribute("aria-expanded", "true");
    await expect(item.locator(".meta")).toContainText("words");
    const tr = item.locator(".transcript");
    await expect(tr).toHaveText("Can you push the release after lunch? Thanks.");
    await radio(page, "Transcript view", "Raw").click();
    await expect(tr).toHaveText("um can you push the release after lunch thanks");
    await radio(page, "Transcript view", "Diff").click();
    await expect(tr.locator("del").first()).toHaveText("um");
    await radio(page, "Transcript view", "Formatted").click();
    // audio player
    await item.getByRole("button", { name: "Play audio" }).click();
    await expect(item.getByRole("button", { name: "Pause audio" })).toBeVisible();
    await item.getByRole("button", { name: "Pause audio" }).click();
    await expect(item.getByRole("button", { name: "Play audio" })).toBeVisible();
    // copy
    await item.getByRole("button", { name: "Copy", exact: true }).click();
    await expect(toastText(page, "Copied to clipboard")).toBeVisible();
    // retry menu
    await item.getByRole("button", { name: /Retry/ }).click();
    await expect(page.getByRole("button", { name: "Paste again" })).toBeVisible();
    await page.getByRole("button", { name: "Paste again" }).click();
    await item.getByRole("button", { name: /Retry/ }).click();
    await page.getByRole("button", { name: "Paste raw" }).click();
    await item.getByRole("button", { name: /Retry/ }).click();
    await page.getByRole("button", { name: /Re-transcribe/ }).click();
    await expect(item.getByRole("button", { name: /Retrying/ })).toBeVisible();
    await expect(toastText(page, "Re-transcribed. Saved in History.")).toBeVisible();
    // delete
    await item.getByRole("button", { name: "Delete", exact: true }).click();
    await expect(page.locator(".h-item")).toHaveCount(17);
    await expect(page.locator(".h-item").first()).not.toContainText("push the release");
  });
  test("failed row: not pasted, copy, retry fixes it", async ({ page }) => {
    const item = page.locator(".h-item.failed");
    await expect(item).toHaveCount(1);
    await expect(item.locator(".status")).toHaveText("not pasted");
    await item.locator(".h-row").click();
    await expect(item.locator(".h-error")).toContainText("Paste failed");
    await item.getByRole("button", { name: "Not pasted - Copy" }).click();
    await expect(toastText(page, "Copied to clipboard")).toBeVisible();
    await item.getByRole("button", { name: /Retry/ }).click();
    // has_audio is false on this seed (index 4 is not the failed one), so re-transcribe is enabled
    await page.getByRole("button", { name: /Re-transcribe/ }).click();
    await expect(page.locator(".h-item.failed")).toHaveCount(0);
  });
  test("row without audio: re-transcribe disabled", async ({ page }) => {
    const item = page.locator(".h-item", { hasText: "Ideas for the weekend" });
    await item.locator(".h-row").click();
    await expect(item.getByRole("button", { name: /Play audio/ })).toHaveCount(0);
    await item.getByRole("button", { name: /Retry/ }).click();
    await expect(page.getByRole("button", { name: /Re-transcribe/ })).toBeDisabled();
  });
  test("copied row shows copied status", async ({ page }) => {
    await expect(page.locator(".h-item", { hasText: "Contaflow is down" }).locator(".status")).toHaveText("copied");
  });
  test("export JSON downloads all items", async ({ page }) => {
    await page.getByRole("button", { name: "More" }).click();
    const [dl] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export JSON" }).click()]);
    expect(dl.suggestedFilename()).toMatch(/^susurro-history-\d{4}-\d\d-\d\d\.json$/);
    const rows = JSON.parse(readFileSync(await dl.path(), "utf8"));
    expect(rows).toHaveLength(18);
  });
  test("delete all: keep, then delete", async ({ page }) => {
    await page.getByRole("button", { name: "More" }).click();
    await page.getByRole("button", { name: "Delete all…" }).click();
    const sheet = page.locator("dialog.sheet[open]");
    await expect(sheet).toContainText("Delete all history?");
    await sheet.getByRole("button", { name: "Keep history" }).click();
    await expect(sheet).toHaveCount(0);
    await expect(page.locator(".h-item")).toHaveCount(18);
    await page.getByRole("button", { name: "More" }).click();
    await page.getByRole("button", { name: "Delete all…" }).click();
    await page.locator("dialog.sheet[open]").getByRole("button", { name: "Delete all history" }).click();
    await expect(page.getByText("Quiet in here.")).toBeVisible();
  });
  test("a new dictation is prepended live", async ({ page }) => {
    await page.evaluate(() => (window.mock.dictate(), window.mock.stop()));
    await expect(page.locator(".h-item").first()).toContainText("The new build is up.");
    await expect(page.locator(".h-item")).toHaveCount(19);
  });
  test("popover closes on Escape and outside click", async ({ page }) => {
    await page.getByRole("button", { name: "More" }).click();
    await expect(page.locator(".popover")).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(page.locator(".popover")).toHaveCount(0);
    await page.getByRole("button", { name: "More" }).click();
    await page.locator("main h1").click();
    await expect(page.locator(".popover")).toHaveCount(0);
  });
});

// ---------------- dictionary ----------------

test.describe("dictionary: words", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Dictionary");
    await expect(page.locator(".d-row").first()).toBeVisible();
  });
  const add = (page) => page.getByLabel("New word");
  test("add, duplicate, too long, symbols only", async ({ page }) => {
    await expect(add(page)).toBeFocused();
    await add(page).fill("Susurro");
    await add(page).press("Enter");
    await expect(page.locator(".d-row.fresh")).toContainText("Susurro");
    await expect(add(page)).toHaveValue("");
    // duplicate (accent and case insensitive)
    await add(page).fill("pablo perez");
    await expect(page.locator(".add-row .field-error")).toHaveText("Already in Words");
    await expect(add(page)).toHaveAttribute("aria-invalid", "true");
    // alias of another entry
    await add(page).fill("contra flow");
    await expect(page.locator(".add-row .field-error")).toHaveText("Already in Corrections (→ Contaflow)");
    // snippet trigger
    await add(page).fill("my calendly");
    await expect(page.locator(".add-row .field-error")).toHaveText("Already in Snippets");
    await add(page).fill("x".repeat(61));
    await expect(page.locator(".add-row .field-error")).toHaveText("Max 60 characters");
    await add(page).fill("!!!");
    await expect(page.locator(".add-row .field-error")).toHaveCount(0);
    await page.locator(".add-row").getByRole("button", { name: /Add/ }).click();
    await expect(page.locator(".add-row .field-error")).toHaveText("Needs at least one letter or number");
    await add(page).fill("   ");
    await expect(page.locator(".add-row").getByRole("button", { name: /Add/ })).toBeDisabled();
  });
  test("edit inline: save, duplicate error, escape", async ({ page }) => {
    const row = page.locator(".d-row", { hasText: "Tauri" });
    await row.hover();
    await row.getByRole("button", { name: "Edit Tauri" }).click();
    const ed = page.getByLabel("Edit entry");
    await ed.fill("Kubernetes");
    await ed.press("Enter");
    await expect(page.locator(".d-row", { hasText: "Kubernetes" })).toHaveCount(1);
    await expect(page.locator(".d-row .term", { hasText: /^Tauri$/ })).toHaveCount(0);
    await page.locator(".d-row", { hasText: "Kubernetes" }).getByRole("button", { name: "Edit Kubernetes" }).click();
    const k = page.locator(".d-row", { has: page.getByLabel("Edit entry") });
    await page.getByLabel("Edit entry").fill("Nadia");
    await page.getByLabel("Edit entry").press("Enter");
    await expect(k.locator(".field-error")).toHaveText("Already in Words");
    await page.getByLabel("Edit entry").press("Escape");
    await expect(page.getByLabel("Edit entry")).toHaveCount(0);
    await expect(page.locator(".d-row .term", { hasText: /^Kubernetes$/ })).toHaveCount(1);
  });
  test("star toggles and reorders", async ({ page }) => {
    const row = page.locator(".d-row", { hasText: "Nadia" });
    const star = row.getByRole("button", { name: /Star|Unstar/ });
    await expect(star).toHaveAttribute("aria-pressed", "false");
    await star.click();
    await expect(page.locator(".d-row", { hasText: "Nadia" }).getByRole("button", { name: "Unstar" })).toHaveAttribute("aria-pressed", "true");
    // starred rows come first: Contaflow (31 uses), Pablo Pérez (11), Nadia (6)
    await expect(page.locator(".d-row .term").nth(2)).toHaveText("Nadia");
  });
  test("delete with undo", async ({ page }) => {
    const n = await page.locator(".d-row").count();
    const row = page.locator(".d-row", { hasText: "Karpathy" });
    await row.hover();
    await row.getByRole("button", { name: "Delete Karpathy" }).click();
    await expect(page.locator(".d-row")).toHaveCount(n - 1);
    await toastText(page, "Deleted “Karpathy”").getByRole("button", { name: "Undo" }).click();
    await expect(page.locator(".d-row", { hasText: "Karpathy" })).toHaveCount(1);
    await expect(page.locator(".d-row")).toHaveCount(n);
  });
  test("filter, no match, clear; Cmd+F and Cmd+N", async ({ page }) => {
    await page.keyboard.press("Meta+f");
    await expect(page.getByLabel("Filter entries")).toBeFocused();
    await page.getByLabel("Filter entries").fill("kube");
    await expect(page.locator(".d-row")).toHaveCount(1);
    await page.getByLabel("Filter entries").fill("qqqq");
    await expect(page.getByText("No match for “qqqq”.")).toBeVisible();
    await page.getByRole("button", { name: "Clear filter" }).click();
    await expect(page.locator(".d-row").first()).toBeVisible();
    await page.keyboard.press("Meta+n");
    await expect(add(page)).toBeFocused();
  });
  test("biasing footer", async ({ page }) => {
    await expect(page.locator(".biasing")).toContainText("Whisper prompt ~14/14 fit");
    await expect(page.locator(".biasing")).toContainText("ElevenLabs keyterms off");
    await expect(page.locator(".biasing")).toContainText("LLM hints on");
  });
  // Was a bug: switching tab ran addRef.focus(), so focus left the radiogroup after one arrow.
  test("tabs by keyboard", async ({ page }) => {
    await radio(page, "Dictionary section", /Words/).focus();
    await page.keyboard.press("ArrowRight");
    await expect(radio(page, "Dictionary section", /Corrections/)).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("ArrowRight");
    await expect(radio(page, "Dictionary section", /Snippets/)).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("ArrowRight");
    await expect(radio(page, "Dictionary section", /Words/)).toHaveAttribute("aria-checked", "true");
  });
});

test.describe("dictionary: corrections", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Dictionary");
    await radio(page, "Dictionary section", /Corrections/).click();
  });
  test("add to new and existing word, preview, errors", async ({ page }) => {
    const n = await page.locator(".d-row").count();
    await expect(page.locator(".preview-line")).toHaveText("Preview: type both sides to see the fix.");
    await page.getByLabel("Heard as").fill("sue sorro");
    await page.getByLabel("Write as").fill("Susurro");
    await expect(page.locator(".preview-line")).toContainText("“sue sorro is great” → “Susurro is great”");
    await page.getByLabel("Write as").press("Enter");
    await expect(page.locator(".d-row")).toHaveCount(n + 1);
    await expect(page.locator(".d-row.fresh")).toContainText("Susurro");
    await expect(page.getByLabel("Heard as")).toHaveValue("");
    // to an existing word: no new word appears in Words
    await page.getByLabel("Heard as").fill("tow ree");
    await page.getByLabel("Write as").fill("tauri");
    await page.getByLabel("Write as").press("Enter");
    await expect(page.locator(".d-row", { hasText: "tow ree" })).toContainText("Tauri");
    // duplicate heard
    await page.getByLabel("Heard as").fill("super base");
    await expect(page.locator(".add-row .field-error")).toHaveText("Already in Corrections (→ Supabase)");
    // alias equal to the word
    await page.getByLabel("Heard as").fill("New Thing");
    await page.getByLabel("Write as").fill("new thing");
    await page.getByLabel("Write as").press("Enter");
    await expect(page.locator(".add-row .field-error")).toContainText(/An alias cannot equal|is the same as/);
  });
  test("edit and delete with undo", async ({ page }) => {
    const row = page.locator(".d-row", { hasText: "sea lion" });
    await row.hover();
    await row.getByRole("button", { name: "Edit sea lion" }).click();
    await page.getByLabel("Edit entry").fill("see ell eye");
    await page.getByLabel("Edit entry").press("Enter");
    await expect(page.locator(".d-row", { hasText: "see ell eye" })).toContainText("CLI");
    const r2 = page.locator(".d-row", { hasText: "see ell eye" });
    await r2.getByRole("button", { name: "Delete see ell eye" }).click();
    await expect(page.locator(".d-row", { hasText: "see ell eye" })).toHaveCount(0);
    await toastText(page, "Deleted “see ell eye → CLI”").getByRole("button", { name: "Undo" }).click();
    await expect(page.locator(".d-row", { hasText: "see ell eye" })).toHaveCount(1);
  });
});

test.describe("dictionary: snippets", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Dictionary");
    await radio(page, "Dictionary section", /Snippets/).click();
  });
  const sheet = (page) => page.locator("dialog.sheet[open]");
  test("new snippet: validation, variables, preview, save", async ({ page }) => {
    const n = await page.locator(".snip-row").count();
    await page.getByRole("button", { name: "New snippet" }).first().click();
    await expect(sheet(page).locator("h2")).toHaveText("New snippet");
    const save = sheet(page).getByRole("button", { name: "Save snippet" });
    await expect(save).toBeDisabled();
    const trig = sheet(page).getByPlaceholder("my calendly");
    await trig.fill("sign off");
    await expect(sheet(page).locator(".field-error")).toHaveText("Already in Snippets");
    await trig.fill("Contaflow");
    await expect(sheet(page).locator(".field-error")).toHaveText("Already in Words");
    await trig.fill("my zoom");
    await expect(sheet(page).locator(".field-error")).toHaveCount(0);
    const area = sheet(page).locator("textarea");
    await area.fill("Join: https://zoom.us/j/1 on ");
    await sheet(page).getByRole("button", { name: "{date}" }).click();
    await expect(area).toHaveValue(/on \{date\}$/);
    await expect(sheet(page).locator(".preview-box")).toHaveText(/on \d{4}-\d\d-\d\d$/);
    await expect(sheet(page).locator(".field-foot .mono")).toHaveText(/^\d+ \/ 4000$/);
    await save.click();
    await expect(sheet(page)).toHaveCount(0);
    await expect(page.locator(".snip-row")).toHaveCount(n + 1);
    await expect(page.locator(".snip-row.fresh")).toContainText("my zoom");
  });
  test("edit by row click, cancel, delete with undo", async ({ page }) => {
    await page.locator(".snip-row", { hasText: "my address" }).click();
    await expect(sheet(page).locator("h2")).toHaveText("Edit snippet");
    await expect(sheet(page).getByPlaceholder("my calendly")).toHaveValue("my address");
    // same trigger as itself is fine
    await expect(sheet(page).locator(".field-error")).toHaveCount(0);
    await sheet(page).locator("textarea").fill("Somewhere else 123");
    await sheet(page).getByRole("button", { name: "Save snippet" }).click();
    await expect(page.locator(".snip-row", { hasText: "my address" })).toContainText("Somewhere else 123");
    await page.locator(".snip-row", { hasText: "my address" }).getByRole("button", { name: "Edit my address" }).click();
    await sheet(page).getByRole("button", { name: "Cancel" }).click();
    await expect(sheet(page)).toHaveCount(0);
    await page.locator(".snip-row", { hasText: "my calendly" }).click();
    await sheet(page).getByRole("button", { name: "Close" }).click();
    await expect(sheet(page)).toHaveCount(0);
    await page.locator(".snip-row", { hasText: "my calendly" }).click();
    await page.keyboard.press("Escape");
    await expect(sheet(page)).toHaveCount(0);
    const row = page.locator(".snip-row", { hasText: "standup template" });
    await row.getByRole("button", { name: "Delete standup template" }).click();
    await expect(page.locator(".snip-row", { hasText: "standup template" })).toHaveCount(0);
    await toastText(page, "Deleted “standup template”").getByRole("button", { name: "Undo" }).click();
    await expect(page.locator(".snip-row", { hasText: "standup template" })).toHaveCount(1);
  });
  test("Cmd+N opens a new snippet", async ({ page }) => {
    await page.keyboard.press("Meta+n");
    await expect(sheet(page).locator("h2")).toHaveText("New snippet");
  });
});

test.describe("dictionary: import/export", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Dictionary");
    await expect(page.locator(".d-row").first()).toBeVisible();
  });
  test("export downloads every entry", async ({ page }) => {
    const [dl] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export" }).click()]);
    expect(dl.suggestedFilename()).toBe("susurro-dictionary.json");
    const rows = JSON.parse(readFileSync(await dl.path(), "utf8"));
    expect(rows).toHaveLength(18);
    expect(rows.filter((r) => r.kind === "snippet")).toHaveLength(4);
  });
  test("import counts new, duplicate and invalid", async ({ page }) => {
    const rows = [
      { kind: "word", text: "Brand New", aliases: ["brand nu"] },
      { kind: "word", text: "Contaflow", aliases: [] },
      { kind: "word", text: "!!!", aliases: [] },
      { kind: "snippet", text: "my phone", expansion: "+56 9 1234 5678" },
    ];
    await page.locator('input[type="file"]').setInputFiles({ name: "d.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(rows)) });
    await expect(toastText(page, "2 new · 1 duplicates skipped · 1 invalid")).toBeVisible();
    await expect(page.locator(".d-row", { hasText: "Brand New" })).toHaveCount(1);
  });
  test("import rejects a non-list file", async ({ page }) => {
    await page.locator('input[type="file"]').setInputFiles({ name: "d.json", mimeType: "application/json", buffer: Buffer.from("{nope") });
    await expect(toastText(page, "That file is not a susurro dictionary export (JSON list).")).toBeVisible();
  });
  // Was a mock seed bug: "whisper.cpp" had the alias "whisper cpp", which validate() rejects as equal to the text.
  test("export then import round-trips as all duplicates", async ({ page }) => {
    const [dl] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export" }).click()]);
    await page.locator('input[type="file"]').setInputFiles(await dl.path());
    await expect(toastText(page, "0 new · 18 duplicates skipped · 0 invalid")).toBeVisible();
  });
  test("empty install: History and Styles empty states", async ({ page }) => {
    await open(page, "/?empty");
    await nav(page, "History");
    await expect(page.getByText("Quiet in here.")).toBeVisible();
    await nav(page, "Styles");
    await expect(page.getByText("No raw apps.")).toBeVisible();
    await page.getByRole("button", { name: "Add app" }).click();
    await expect(page.locator(".popover .label")).toHaveText("Suggestions");
  });
  test("empty dictionary: empty states, export disabled", async ({ page }) => {
    await open(page, "/?empty");
    await nav(page, "Dictionary");
    await expect(page.getByText("Teach it your words.")).toBeVisible();
    await expect(page.getByRole("button", { name: "Export" })).toBeDisabled();
    await radio(page, "Dictionary section", /Corrections/).click();
    await expect(page.getByText("Fix what it mishears.")).toBeVisible();
    await radio(page, "Dictionary section", /Snippets/).click();
    await expect(page.getByText("Say less, paste more.")).toBeVisible();
    await page.locator(".empty").getByRole("button", { name: "New snippet" }).click();
    await expect(page.locator("dialog.sheet[open] h2")).toHaveText("New snippet");
  });
});

// ---------------- styles ----------------

test.describe("styles", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Styles");
  });
  test("cleanup level, style radios, sample text", async ({ page }) => {
    await radio(page, "Cleanup level", "Medium").click();
    await expect(page.getByText("Also rewords for clarity.")).toBeVisible();
    await radio(page, "Cleanup level", "None").click();
    await expect(page.getByText("Pastes what Whisper heard")).toBeVisible();
    await expect(page.locator(".note")).toContainText("Tone rewrites need an LLM");
    await radio(page, "Cleanup level", "Light").click();
    await expect(page.locator(".note")).toHaveCount(0);
    for (const [name, sample] of [
      ["Formal", "Hello, are you free tonight?"],
      ["Very casual", "hey are you free tonight?"],
      ["Excited", "grab dinner!"],
      ["Casual", "Hey, are you free tonight?"],
    ]) {
      await page.getByRole("radiogroup", { name: "Style" }).getByRole("radio", { name: new RegExp(`^${name}`) }).click();
      await expect(page.locator(".becomes")).toContainText(sample);
    }
  });
  test("raw apps: add suggestion, add by bundle id, validation, remove", async ({ page }) => {
    const rows = page.locator(".app-row");
    await expect(rows).toHaveCount(2);
    await page.getByRole("button", { name: "Add app" }).click();
    await page.locator(".popover .menu-item", { hasText: "Slack" }).click();
    await expect(rows).toHaveCount(3);
    await expect(page.locator(".app-row", { hasText: "com.tinyspeck.slackmacgap" })).toContainText("Slack");
    await page.getByRole("button", { name: "Add app" }).click();
    const bid = page.getByLabel("Bundle id");
    await bid.fill("notabundle");
    await expect(page.locator(".popover").getByRole("button", { name: "Add", exact: true })).toBeDisabled();
    await bid.fill("com.example.thing");
    await page.locator(".popover").getByRole("button", { name: "Add", exact: true }).click();
    await expect(page.locator(".app-row", { hasText: "com.example.thing" })).toContainText("thing");
    await page.getByRole("button", { name: "Remove com.example.thing" }).click();
    await expect(rows).toHaveCount(3);
  });
  test("needs-LLM link goes to Providers", async ({ page }) => {
    await nav(page, "Providers");
    await page.getByRole("textbox", { name: "OpenRouter key" }).fill("sk-or-v1-abcdefghijklmnop");
    await page.getByRole("textbox", { name: "OpenRouter key" }).press("Enter");
    await page.getByRole("button", { name: "Remove OpenRouter key" }).click();
    await nav(page, "Styles");
    await page.getByRole("button", { name: "needs an LLM provider" }).click();
    await expect(h1(page)).toHaveText("Where your voice goes.");
  });
});

// ---------------- providers ----------------

test.describe("providers", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Providers");
    await expect(page.locator(".models .row").first()).toBeVisible();
  });
  const prov = (page, name) => page.getByRole("radiogroup", { name: "Speech to text provider" }).getByRole("radio", { name: new RegExp(`^${name}`) });
  const testBtn = (page) => page.locator(".prov-body .test-btn");
  const result = (page) => page.locator(".prov-body .test-result");

  test("local whisper: test ok, model use/delete", async ({ page }) => {
    await expect(prov(page, "Local Whisper")).toHaveAttribute("aria-checked", "true");
    await testBtn(page).click();
    await expect(result(page)).toContainText("487 ms");
    const base = page.locator(".models .row", { hasText: "base.en q5_1" });
    await base.getByRole("button", { name: "Use" }).click();
    await expect(base).toContainText("in use");
    await expect(page.locator(".instruments")).toContainText("base.en q5_1");
    await expect(base.getByRole("button", { name: "Delete base.en q5_1" })).toBeDisabled();
    const turbo = page.locator(".models .row", { hasText: "large-v3-turbo q5_0" });
    await turbo.getByRole("button", { name: "Delete large-v3-turbo q5_0" }).click();
    await expect(turbo.getByRole("button", { name: "Download" })).toBeVisible();
  });
  test("English-only model warns for other languages", async ({ page }) => {
    await page.locator(".models .row", { hasText: "base.en q5_1" }).getByRole("button", { name: "Use" }).click();
    await expect(page.getByRole("status").filter({ hasText: "only understands English" })).toContainText("other languages");
    await page.getByRole("combobox", { name: "Language" }).selectOption("es");
    await expect(page.getByRole("status").filter({ hasText: "only understands English" })).toContainText("Español");
    await page.getByRole("combobox", { name: "Language" }).selectOption("en");
    await expect(page.getByText("only understands English")).toHaveCount(0);
  });
  test("model download shows progress, then Use", async ({ page }) => {
    test.setTimeout(45_000);
    const row = page.locator(".models .row", { hasText: "base q5_1" }).filter({ hasNotText: "base.en" });
    await row.getByRole("button", { name: "Download" }).click();
    await expect(row.getByRole("progressbar")).toBeVisible();
    await expect(row.locator(".progress .mono")).toContainText("MB/s");
    await expect(row.getByRole("button", { name: "Use" })).toBeVisible({ timeout: 20_000 });
    await row.getByRole("button", { name: "Use" }).click();
    await expect(row).toContainText("in use");
  });
  // Was a bug: after leaving and coming back mid-download, the row stuck at 100% with no Use button.
  test("download survives leaving the screen", async ({ page }) => {
    test.setTimeout(45_000);
    const row = () => page.locator(".models .row", { hasText: "base q5_1" }).filter({ hasNotText: "base.en" });
    await row().getByRole("button", { name: "Download" }).click();
    await expect(row().getByRole("progressbar")).toBeVisible();
    await nav(page, "Home");
    await nav(page, "Providers");
    await expect(row().getByRole("button", { name: "Use" })).toBeVisible({ timeout: 20_000 });
  });
  // Was a bug: new URL(base_url).host threw during render and blanked the window, and the bad URL was saved.
  test("invalid local server URL does not crash the screen", async ({ page }) => {
    await prov(page, "Local server").click();
    const url = page.locator(".prov-body input.mono").first();
    const before = await url.inputValue();
    for (const bad of ["http://[bad", "localhost:8000", "ftp://host/v1", "http://"]) {
      await url.fill(bad);
      await url.press("Enter");
      await expect(toastText(page, "Enter a full URL").last()).toBeVisible();
      await expect(url).toHaveValue(before);
    }
    await expect(h1(page)).toHaveText("Where your voice goes.");
    const saved = await page.evaluate(async () => (await import("/src/lib/mock.ts")).mockInvoke("get_settings"));
    expect(saved.stt.openai_compat.base_url).toBe(before);
    await url.fill("http://127.0.0.1:9000/v1");
    await url.press("Enter");
    await expect(prov(page, "Local server")).toContainText("127.0.0.1:9000");
  });
  test("a stored invalid URL (hand-edited settings.json) does not crash the screen", async ({ page }) => {
    await nav(page, "Home");
    await page.evaluate(async () => {
      const { mockInvoke } = await import("/src/lib/mock.ts");
      const s = await mockInvoke("get_settings");
      s.stt.openai_compat.base_url = "http://[bad";
      s.stt.provider = "openai_compat";
      await mockInvoke("save_settings", { settings: s });
      window.mock.plug(); // mic_state makes the app re-read settings
    });
    await nav(page, "Providers");
    await expect(h1(page)).toHaveText("Where your voice goes.");
    await expect(prov(page, "Local server")).toContainText("invalid URL");
  });
  // Was a bug: a second save read the backend before the first landed and wrote the old value back.
  test("two quick saves both stick", async ({ page }) => {
    await page.evaluate(() => {
      [...document.querySelectorAll(".models .row")].find((r) => r.textContent.includes("base.en q5_1")).querySelector("button.btn").click();
      const sel = document.querySelector('select[aria-label="Language"]');
      sel.value = "es";
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    await expect(page.locator(".models .row", { hasText: "base.en q5_1" })).toContainText("in use");
    await expect(page.getByRole("status").filter({ hasText: "only understands English" })).toContainText("Español");
    const saved = await page.evaluate(async () => (await import("/src/lib/mock.ts")).mockInvoke("get_settings"));
    expect([saved.stt.whisper_model, saved.language]).toEqual(["ggml-base.en-q5_1.bin", "es"]);
  });
  test("key masks keep only the last 4 characters", async ({ page }) => {
    await page.getByRole("textbox", { name: "OpenRouter key" }).fill("sk-or-v1-secretsecretABCD");
    await page.getByRole("textbox", { name: "OpenRouter key" }).press("Enter");
    await expect(page.getByRole("textbox", { name: "OpenRouter key" })).toHaveAttribute("placeholder", "••••ABCD");
    const all = await page.evaluate(() => Object.keys(localStorage).map((k) => localStorage.getItem(k)).join(" "));
    expect(all).not.toContain("sk-or-");
  });
  test("an old 6+4 key mask is cut to the last 4 on load", async ({ page }) => {
    await page.addInitScript(() => localStorage.setItem("susurro.keymask.elevenlabs", "el-012…cdef"));
    await page.reload();
    await expect.poll(() => page.evaluate(() => localStorage.getItem("susurro.keymask.elevenlabs"))).toBe("••••cdef");
  });
  test("model download failure shows the error and Retry", async ({ page }) => {
    test.setTimeout(45_000);
    const row = page.locator(".models .row", { hasText: "large-v3-turbo q8_0" });
    await row.getByRole("button", { name: "Download" }).click();
    await expect(row.getByRole("alert")).toHaveText("Download failed: disk full. 1.1 GB needed.", { timeout: 20_000 });
    await row.getByRole("button", { name: "Retry" }).click();
    await expect(row.getByRole("progressbar")).toBeVisible();
  });
  test("local server: URL/model fields, hotwords, test fails", async ({ page }) => {
    await prov(page, "Local server").click();
    await expect(prov(page, "Local server")).toHaveAttribute("aria-checked", "true");
    const url = page.locator(".prov-body input.mono").first();
    await expect(url).toHaveValue("http://127.0.0.1:8080/v1");
    await url.fill("http://127.0.0.1:8000/v1");
    await url.press("Enter");
    await expect(prov(page, "Local server")).toContainText("127.0.0.1:8000");
    await sw(page, "Send hotwords").click();
    await expect(sw(page, "Send hotwords")).toHaveAttribute("aria-checked", "true");
    await testBtn(page).click();
    await expect(result(page)).toContainText("Not reachable");
    await expect(prov(page, "Local server")).toContainText("not reachable");
    await sw(page, "Fall back to Local Whisper").click();
    await expect(sw(page, "Fall back to Local Whisper")).toHaveAttribute("aria-checked", "false");
    await expect(page.locator(".instruments")).toContainText("Local server");
  });
  test("ElevenLabs: needs key, then test ok; model and keyterms", async ({ page }) => {
    await prov(page, "ElevenLabs").click();
    await expect(testBtn(page)).toBeDisabled();
    await expect(page.locator(".instruments")).toContainText("needs key");
    await page.locator(".prov-body select").selectOption("scribe_v1");
    await expect(page.locator(".prov-body select")).toHaveValue("scribe_v1");
    await sw(page, "Send keyterms").click();
    await expect(sw(page, "Send keyterms")).toHaveAttribute("aria-checked", "true");
    await page.getByRole("textbox", { name: "ElevenLabs key" }).fill("el-0123456789abcdef");
    await page.locator(".keys .row", { hasText: "ElevenLabs" }).getByRole("button", { name: "Save" }).click();
    await expect(page.getByRole("textbox", { name: "ElevenLabs key" })).toHaveAttribute("placeholder", "••••cdef");
    await expect(testBtn(page)).toBeEnabled();
    await testBtn(page).click();
    await expect(result(page)).toContainText("938 ms");
    await expect(page.locator(".instruments")).not.toContainText("needs key");
  });
  test("OpenRouter STT: model picker, test", async ({ page }) => {
    await prov(page, "OpenRouter").click();
    await expect(prov(page, "OpenRouter")).toContainText("pick a model");
    const pick = page.locator(".prov-body").getByRole("combobox", { name: "Model" });
    await expect(pick).toHaveValue("");
    await pick.selectOption("openai/gpt-4o-transcribe");
    await expect(pick).toHaveValue("openai/gpt-4o-transcribe");
    await expect(page.locator(".instruments")).toContainText("openai/gpt-4o-transcribe");
    await testBtn(page).click();
    await expect(result(page)).toContainText("1204 ms");
  });
  test("provider radios by keyboard", async ({ page }) => {
    await prov(page, "Local Whisper").focus();
    await page.keyboard.press("ArrowDown");
    await expect(prov(page, "Local server")).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("ArrowDown");
    await expect(prov(page, "ElevenLabs")).toHaveAttribute("aria-checked", "true");
    await page.keyboard.press("ArrowUp");
    await expect(prov(page, "Local server")).toHaveAttribute("aria-checked", "true");
  });
  test("cleanup: off, local, openrouter + test", async ({ page }) => {
    const sel = page.getByRole("combobox", { name: "Cleanup provider" });
    const card = page.locator(".card.pad");
    await expect(sel).toHaveValue("openrouter");
    await card.locator(".test-btn").click();
    await expect(card.locator(".test-result")).toContainText("612 ms");
    await card.getByRole("combobox", { name: "Model" }).selectOption("anthropic/claude-haiku-4.5");
    await expect(card.getByRole("combobox", { name: "Model" })).toHaveValue("anthropic/claude-haiku-4.5");
    await sel.selectOption("local");
    await expect(card).toContainText("LM Studio");
    await card.getByPlaceholder("qwen3-8b").fill("qwen3-8b");
    await card.getByPlaceholder("qwen3-8b").press("Enter");
    await card.locator(".test-btn").click();
    await expect(card.locator(".test-result")).toContainText("Not reachable");
    await sel.selectOption("off");
    await expect(card).toContainText("no rewrite");
    await expect(card.locator(".test-btn")).toHaveCount(0);
    await nav(page, "Styles");
    await expect(radio(page, "Cleanup level", "None")).toHaveAttribute("aria-checked", "true");
  });
  test("keys: show/hide, save, mask, test, remove", async ({ page }) => {
    const row = page.locator(".keys .row", { hasText: "OpenRouter" });
    await expect(row.getByRole("button", { name: "Show key" })).toBeDisabled();
    await expect(row.locator(".test-btn")).toBeVisible(); // seeded key, cleanup uses it
    await row.locator(".test-btn").click();
    await expect(row.locator(".test-result")).toContainText("612 ms");
    await row.getByRole("button", { name: "Remove OpenRouter key" }).click();
    await expect(row).toContainText("needs key");
    await expect(page.locator(".card.pad")).toContainText("Add an OpenRouter key below");
    const inp = page.getByRole("textbox", { name: "OpenRouter key" });
    await inp.fill("sk-or-v1-0123456789abcdefXYZ");
    await expect(inp).toHaveAttribute("type", "password");
    await row.getByRole("button", { name: "Show key" }).click();
    await expect(inp).toHaveAttribute("type", "text");
    await row.getByRole("button", { name: "Hide key" }).click();
    await expect(inp).toHaveAttribute("type", "password");
    await inp.press("Enter");
    await expect(inp).toHaveValue("");
    await expect(inp).toHaveAttribute("placeholder", "••••fXYZ");
    // short key masks fully
    const l = page.getByRole("textbox", { name: "Local STT server key" });
    await l.fill("abc");
    await page.locator(".keys .row", { hasText: "Local STT server" }).getByRole("button", { name: "Save" }).click();
    await expect(l).toHaveAttribute("placeholder", "••••");
    await expect(page.locator(".keys .row", { hasText: "Local STT server" })).toContainText("not in use");
  });
});

// ---------------- audio ----------------

test.describe("audio", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Audio");
    await expect(page.locator(".prio .row").first()).toBeVisible();
  });
  const prioNames = (page) => page.locator(".prio .rows .dname").allInnerTexts();
  test("test mic: live meter, stop", async ({ page }) => {
    await page.getByRole("button", { name: "Test mic" }).click();
    await expect(page.locator(".inuse .status")).toContainText("live");
    await expect(page.locator(".level .db")).toHaveText(/^-?\d+ dB$/);
    await expect(page.locator(".level [role=meter] i.low").first()).toBeVisible();
    await page.getByRole("button", { name: "Stop" }).click();
    await expect(page.locator(".inuse .status")).toContainText("closed at rest");
    await expect(page.locator(".level .db")).toHaveText("- dB");
  });
  test("priority reorder by keyboard announces the move", async ({ page }) => {
    expect(await prioNames(page)).toEqual(["MacBook Pro Microphone", "AirPods Pro"]);
    await page.getByRole("button", { name: /Reorder MacBook Pro Microphone/ }).focus();
    await page.keyboard.press("Alt+ArrowDown");
    await expect.poll(() => prioNames(page)).toEqual(["AirPods Pro", "MacBook Pro Microphone"]);
    await expect(page.locator(".content .sr-only")).toHaveText("MacBook Pro Microphone moved to position 2");
    await page.getByRole("button", { name: /Reorder MacBook Pro Microphone/ }).focus();
    await page.keyboard.press("Alt+ArrowUp");
    await expect.poll(() => prioNames(page)).toEqual(["MacBook Pro Microphone", "AirPods Pro"]);
    // plain arrows do nothing
    await page.keyboard.press("ArrowDown");
    expect(await prioNames(page)).toEqual(["MacBook Pro Microphone", "AirPods Pro"]);
  });
  test("priority reorder by drag", async ({ page }) => {
    const h = page.getByRole("button", { name: /Reorder MacBook Pro Microphone/ });
    const b = await h.boundingBox();
    await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
    await page.mouse.down();
    await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2 + 30, { steps: 5 });
    await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2 + 50, { steps: 5 });
    await page.mouse.up();
    await expect.poll(() => prioNames(page)).toEqual(["AirPods Pro", "MacBook Pro Microphone"]);
  });
  test("disconnected device: forget", async ({ page }) => {
    const air = page.locator(".prio .row", { hasText: "AirPods Pro" });
    await expect(air).toContainText("disconnected");
    await expect(page.locator(".warnline")).toHaveText("Bluetooth lowers mic quality while in use.");
    await air.getByRole("button", { name: "Forget" }).click();
    await expect(page.locator(".prio .row", { hasText: "AirPods Pro" })).toHaveCount(0);
  });
  test("selection: always use, then auto", async ({ page }) => {
    const auto = page.getByRole("radiogroup", { name: "Microphone selection" }).getByRole("radio", { name: /^Auto/ });
    const fixed = page.getByRole("radio", { name: "Always use" });
    await expect(auto).toHaveAttribute("aria-checked", "true");
    await page.getByRole("button", { name: /Show virtual devices/ }).click();
    await page.getByRole("combobox", { name: "Always use this microphone" }).selectOption("BlackHole2ch_UID");
    await expect(fixed).toHaveAttribute("aria-checked", "true");
    await expect(page.locator(".inuse .dname")).toHaveText("BlackHole 2ch");
    await expect(page.locator(".inuse .meta")).toContainText("fixed");
    await expect(page.locator(".instruments")).toContainText("BlackHole 2ch");
    await auto.click();
    await expect(auto).toHaveAttribute("aria-checked", "true");
    await expect(page.locator(".inuse .dname")).toHaveText("MacBook Pro Microphone");
    await fixed.click();
    await expect(fixed).toHaveAttribute("aria-checked", "true");
    await expect(page.locator(".inuse .dname")).toHaveText("MacBook Pro Microphone");
    await auto.focus();
    await page.keyboard.press(" ");
    await expect(auto).toHaveAttribute("aria-checked", "true");
  });
  test("virtual devices disclosure", async ({ page }) => {
    const d = page.getByRole("button", { name: /virtual devices \(2\)/ });
    await expect(d).toHaveAttribute("aria-expanded", "false");
    await expect(page.locator(".prio .row", { hasText: "BlackHole 2ch" })).toHaveCount(0);
    await d.click();
    await expect(page.getByRole("button", { name: /Hide virtual devices/ })).toHaveAttribute("aria-expanded", "true");
    await expect(page.locator(".prio .row", { hasText: "BlackHole 2ch" })).toHaveCount(1);
    await page.getByRole("button", { name: /Hide virtual devices/ }).click();
    await expect(page.locator(".prio .row", { hasText: "BlackHole 2ch" })).toHaveCount(0);
  });
  test("hot-plug toast: Switch", async ({ page }) => {
    await page.evaluate(() => window.mock.plug());
    const t = toastText(page, "New mic: Shure MV7. Switch to it for dictation?");
    await expect(t).toBeVisible();
    await t.getByRole("button", { name: "Switch" }).click();
    await expect(t).toHaveCount(0);
    await expect(page.locator(".inuse .dname")).toHaveText("Shure MV7");
    await expect(page.locator(".instruments")).toContainText("Shure MV7");
    await page.evaluate(() => window.mock.unplug());
    await expect(toastText(page, "Shure MV7 disconnected - using MacBook Pro Microphone")).toBeVisible();
    await expect(page.locator(".note", { hasText: "Shure MV7 is disconnected" })).toBeVisible();
  });
  test("hot-plug toast: Switch after the mic is already gone", async ({ page }) => {
    await page.evaluate(() => window.mock.plug());
    await expect(toastText(page, "New mic: Shure MV7")).toBeVisible();
    await page.evaluate(() => {
      [...document.querySelectorAll(".toasts .toast button")].find((b) => b.textContent === "Switch").click();
      window.mock.dictate();
      window.mock.unplug();
    });
    await expect(toastText(page, "New mic: Shure MV7")).toHaveCount(0);
    await expect(page.locator(".inuse .dname")).toHaveText("MacBook Pro Microphone");
    await page.waitForTimeout(300); // the toast action runs after unplug(); its error would show as a toast
    expect(await page.locator(".toasts .toast.error").count()).toBe(0); // no retry: the toast would expire
  });
  test("hot-plug toast: Keep current", async ({ page }) => {
    await page.evaluate(() => window.mock.plug());
    const t = toastText(page, "New mic: Shure MV7");
    await t.getByRole("button", { name: "Keep current" }).click();
    await expect(page.locator(".inuse .dname")).toHaveText("MacBook Pro Microphone");
    await expect.poll(() => prioNames(page)).toEqual(["MacBook Pro Microphone", "AirPods Pro", "Shure MV7"]);
  });
  test("toast stack keeps the newest three", async ({ page }) => {
    await page.evaluate(() => ["A1", "A2", "A3", "A4", "A5"].forEach((t) => window.mock.toast(t)));
    await expect(page.locator(".toasts .toast")).toHaveText(["A3", "A4", "A5"]);
  });
  test("hot-plug toast: dismiss", async ({ page }) => {
    await page.evaluate(() => window.mock.plug());
    const t = toastText(page, "New mic: Shure MV7");
    await t.getByRole("button", { name: "Dismiss" }).click();
    await expect(t).toHaveCount(0);
  });
  test("mic lost mid-dictation toast", async ({ page }) => {
    await page.evaluate(() => window.mock.plug());
    await toastText(page, "New mic").getByRole("button", { name: "Switch" }).click();
    await expect(page.locator(".inuse .dname")).toHaveText("Shure MV7");
    await page.evaluate(() => (window.mock.dictate(), window.mock.unplug()));
    await expect(toastText(page, "Mic lost - continuing on MacBook Pro Microphone")).toBeVisible();
  });
});

// ---------------- shortcuts ----------------

test.describe("shortcuts", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Shortcuts");
  });
  const bind = (page, name) => page.getByRole("button", { name: new RegExp(`^${name}:`) });
  const row = (page, label) => page.locator(".sc-row", { has: page.locator(".label", { hasText: new RegExp(`^${label}$`) }) });
  test("record push to talk; reset; quick pick fn", async ({ page }) => {
    await bind(page, "Push to talk").click();
    await expect(row(page, "Push to talk").locator(".rec-hint")).toHaveText("Press a key alone or a combo… Esc to cancel");
    await page.keyboard.press("Control+Alt+KeyK");
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: Ctrl\+Alt\+K\./);
    await expect(page.locator(".hint-kbd")).toHaveCount(0);
    await expect(page.getByLabel("Practice field")).toHaveAttribute("placeholder", /hold .*K and talk/);
    await row(page, "Push to talk").getByRole("button", { name: "Reset" }).click();
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightOption\./);
    await expect(row(page, "Push to talk").getByRole("button", { name: "Reset" })).toBeDisabled();
    await bind(page, "Push to talk").click();
    await page.keyboard.press("Control+Shift+F5");
    await bind(page, "Push to talk").click();
    await row(page, "Push to talk").getByRole("button", { name: "Use fn" }).click();
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: Fn\./);
    await expect(page.getByLabel("Practice field")).toHaveAttribute("placeholder", "Click here, hold fn and talk. Your words land in this box.");
  });
  test("validation messages", async ({ page }) => {
    const msg = row(page, "Push to talk").locator(".recorder-msg[role=alert]");
    await bind(page, "Push to talk").click();
    await page.keyboard.press("KeyK");
    await expect(msg).toContainText("Add a modifier: a single key types text");
    await page.keyboard.press("Meta+KeyV");
    await expect(msg).toContainText("⌘V is reserved by the system");
    await page.keyboard.press("Control+Meta+KeyV");
    await expect(msg).toContainText("Already used by Paste last");
    await page.keyboard.down("Control");
    await page.keyboard.down("Alt");
    await page.keyboard.up("Alt");
    await page.keyboard.up("Control");
    await expect(msg).toContainText("Add a key, or press one modifier alone");
    await page.keyboard.press("Control+Alt+Shift+KeyK");
    await expect(msg).toContainText("Use up to 3 keys");
    await page.keyboard.press("Escape");
    await expect(msg).toHaveCount(0);
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightOption\./);
    // one modifier alone is push to talk only
    await bind(page, "Cancel").click();
    await page.keyboard.press("ShiftLeft");
    await expect(row(page, "Cancel").locator(".recorder-msg[role=alert]")).toContainText("only push to talk works with one key alone");
  });
  // Was a bug: every modifier keydown re-ran the effect and reset the pressed codes.
  test("left+right modifier is rejected", async ({ page }) => {
    await bind(page, "Cancel").click();
    await page.keyboard.down("ShiftLeft");
    await page.keyboard.down("ShiftRight");
    await page.keyboard.press("KeyK");
    await page.keyboard.up("ShiftRight");
    await page.keyboard.up("ShiftLeft");
    await expect(row(page, "Cancel").locator(".recorder-msg")).toContainText("Left and right of one modifier");
  });
  test("hands-free combo add/remove, double-tap toggle", async ({ page }) => {
    await expect(bind(page, "Hands-free")).toContainText("+ combo");
    await bind(page, "Hands-free").click();
    await page.keyboard.press("Control+Alt+Space");
    await expect(bind(page, "Hands-free")).toHaveAttribute("aria-label", /^Hands-free: Ctrl\+Alt\+Space\./);
    await page.getByRole("button", { name: "Remove hands-free combo" }).click();
    await expect(bind(page, "Hands-free")).toContainText("+ combo");
    const dt = sw(page, "Double-tap push to talk");
    await expect(dt).toHaveAttribute("aria-checked", "true");
    await dt.click();
    await expect(dt).toHaveAttribute("aria-checked", "false");
  });
  test("cancel and paste-last: record and reset", async ({ page }) => {
    await bind(page, "Cancel").click();
    await page.keyboard.press("Control+Period");
    await expect(bind(page, "Cancel")).toHaveAttribute("aria-label", /^Cancel: Ctrl\+\.\./);
    await row(page, "Cancel").getByRole("button", { name: "Reset" }).click();
    await expect(bind(page, "Cancel")).toHaveAttribute("aria-label", /^Cancel: Escape\./);
    await bind(page, "Paste last").click();
    await page.keyboard.press("Alt+Shift+KeyV");
    await expect(bind(page, "Paste last")).toHaveAttribute("aria-label", /^Paste last: Alt\+Shift\+V\./);
    await row(page, "Paste last").getByRole("button", { name: "Reset" }).click();
    await expect(bind(page, "Paste last")).toHaveAttribute("aria-label", /^Paste last: Ctrl\+Cmd\+V\./);
  });
  test("recording swallows app shortcuts (Cmd+2 does not navigate)", async ({ page }) => {
    await bind(page, "Hands-free").click();
    await page.keyboard.press("Meta+Digit2");
    await expect(h1(page)).toHaveText("Keys, your way.");
    await expect(bind(page, "Hands-free")).toHaveAttribute("aria-label", /^Hands-free: Cmd\+2\./);
  });
  test("recorder: a modifier alone binds that side; a combo binds the combo; Escape cancels", async ({ page }) => {
    await bind(page, "Push to talk").click();
    await page.keyboard.down("MetaRight");
    await expect(row(page, "Push to talk").locator(".bind kbd")).toHaveText("right ⌘");
    await page.keyboard.up("MetaRight");
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightCommand\./);
    await expect(page.getByLabel("Practice field")).toHaveAttribute("placeholder", /hold Right ⌘ and talk/);
    await expect(page.locator(".small", { hasText: "double-tap" })).toHaveText("double-tap Right ⌘");
    // combo: modifier + key is an accelerator, not the modifier alone
    await bind(page, "Push to talk").click();
    await page.keyboard.down("AltRight");
    await page.keyboard.press("KeyJ");
    await page.keyboard.up("AltRight");
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: Alt\+J\./);
    // Escape cancels and keeps the binding
    await bind(page, "Push to talk").click();
    await page.keyboard.down("ControlRight");
    await page.keyboard.up("ControlRight");
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightControl\./);
    await bind(page, "Push to talk").click();
    await page.keyboard.press("Escape");
    await expect(bind(page, "Push to talk")).not.toHaveClass(/recording/);
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightControl\./);
  });
  test("quick picks per action", async ({ page }) => {
    await bind(page, "Push to talk").click();
    const picks = page.getByLabel("Quick picks for Push to talk").getByRole("button");
    await expect(picks).toHaveText(["right ⌥", "right ⌘", "fn"]);
    await expect(picks.first()).toBeDisabled(); // the current binding
    await page.getByRole("button", { name: "Use Right ⌘" }).click();
    await expect(bind(page, "Push to talk")).toHaveAttribute("aria-label", /^Push to talk: RightCommand\./);
    await expect(page.getByLabel("Quick picks for Push to talk")).toHaveCount(0);
    await bind(page, "Hands-free").click();
    await page.getByLabel("Quick picks for Hands-free").getByRole("button", { name: "Use ⌥ Space" }).click();
    await expect(bind(page, "Hands-free")).toHaveAttribute("aria-label", /^Hands-free: Alt\+Space\./);
    await bind(page, "Paste last").click();
    await page.getByLabel("Quick picks for Paste last").getByRole("button", { name: "Use ⌥ ⇧ V" }).click();
    await expect(bind(page, "Paste last")).toHaveAttribute("aria-label", /^Paste last: Alt\+Shift\+V\./);
  });
  test("holding the talk key dictates, a chord cancels, recording a binding pauses it", async ({ page }) => {
    const live = page.locator(".shell > .sr-only");
    await bind(page, "Cancel").click();
    await page.keyboard.down("AltRight");
    await page.keyboard.press("KeyK");
    await page.keyboard.up("AltRight");
    await expect(bind(page, "Cancel")).toHaveAttribute("aria-label", /^Cancel: Alt\+K\./);
    await expect(live).toHaveText("");
    await page.keyboard.down("AltRight");
    await expect(live).toHaveText("Listening");
    await page.keyboard.press("Digit2"); // Option+2 types @ on a LatAm layout: cancel, paste nothing
    await expect(live).toHaveText("");
    await page.keyboard.up("AltRight");
    await page.keyboard.down("AltRight");
    await expect(live).toHaveText("Listening");
    await page.keyboard.up("AltRight");
    await expect(live).toHaveText("Pasted");
  });
});

// ---------------- settings ----------------

test.describe("settings", () => {
  test.beforeEach(async ({ page }) => {
    await open(page);
    await nav(page, "Settings");
  });
  test("every toggle flips and survives a screen change", async ({ page }) => {
    const toggles = ["Launch at login", "Sounds", "Show idle pill", "Spoken punctuation", "Fuzzy word matching", "Restore clipboard", "Save history", "Press-enter command"];
    const before = {};
    for (const t of toggles) before[t] = await sw(page, t).getAttribute("aria-checked");
    for (const t of toggles) {
      await sw(page, t).click();
      await expect(sw(page, t)).toHaveAttribute("aria-checked", before[t] === "true" ? "false" : "true");
    }
    await nav(page, "Home");
    await nav(page, "Settings");
    for (const t of toggles) await expect(sw(page, t)).toHaveAttribute("aria-checked", before[t] === "true" ? "false" : "true");
  });
  test("theme", async ({ page }) => {
    await radio(page, "Theme", "Light").click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await radio(page, "Theme", "Dark").click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await page.reload();
    await nav(page, "Settings");
    await expect(radio(page, "Theme", "Dark")).toHaveAttribute("aria-checked", "true");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
    await page.emulateMedia({ colorScheme: "light" });
    await radio(page, "Theme", "System").click();
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    await page.emulateMedia({ colorScheme: "dark" });
    await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  });
  test("max length and keep audio", async ({ page }) => {
    const sel = page.getByRole("combobox", { name: "Max recording length" });
    await sel.selectOption("300");
    await expect(sel).toHaveValue("300");
    await radio(page, "Keep audio", "Never").click();
    await expect(radio(page, "Keep audio", "Never")).toHaveAttribute("aria-checked", "true");
    await radio(page, "Keep audio", "30 days").click();
    await expect(radio(page, "Keep audio", "30 days")).toHaveAttribute("aria-checked", "true");
    await nav(page, "Home");
    await nav(page, "Settings");
    await expect(radio(page, "Keep audio", "30 days")).toHaveAttribute("aria-checked", "true");
    await expect(page.getByRole("combobox", { name: "Max recording length" })).toHaveValue("300");
  });
  test("copy data path", async ({ page }) => {
    await page.getByRole("button", { name: "Copy path" }).click();
    await expect(toastText(page, "Copied to clipboard")).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("~/Library/Application Support/app.susurro.desktop");
  });
  test("delete all history", async ({ page }) => {
    await page.getByRole("button", { name: "Delete all history…" }).click();
    await page.locator("dialog.sheet[open]").getByRole("button", { name: "Keep history" }).click();
    await expect(page.locator("dialog.sheet[open]")).toHaveCount(0);
    await page.getByRole("button", { name: "Delete all history…" }).click();
    await page.locator("dialog.sheet[open]").getByRole("button", { name: "Delete all history" }).click();
    await expect(toastText(page, "History deleted")).toBeVisible();
    await nav(page, "History");
    await expect(page.getByText("Quiet in here.")).toBeVisible();
  });
});

// ---------------- onboarding ----------------

test.describe("onboarding", () => {
  const step = (page, n) => expect(page.locator(".ticks .count")).toHaveText(`${n}/7`);
  const notNow = (page) => page.getByRole("button", { name: "Not now" }).click();
  test("all seven steps, grants, finish", async ({ page }) => {
    test.setTimeout(60_000);
    await open(page, "/?onboarding");
    await step(page, 1);
    await expect(page.locator(".onboarding h1")).toHaveText("Talk. It types.");
    await page.getByRole("button", { name: "Replay" }).click();
    await page.getByRole("button", { name: "Get started" }).click();
    // 2 mic
    await step(page, 2);
    await page.getByRole("button", { name: "Allow microphone" }).click();
    await expect(page.locator(".ob-panel [role=meter]")).toBeVisible({ timeout: 4000 });
    await expect(page.getByText("Heard you.")).toBeVisible({ timeout: 8000 });
    await page.getByRole("button", { name: "Continue" }).click();
    // 3 accessibility: grant, then it advances by itself
    await step(page, 3);
    await expect(page.locator(".eyebrow")).toHaveText("Accessibility");
    await page.getByRole("button", { name: "Open System Settings" }).click();
    await expect(page.getByText("Granted")).toBeVisible({ timeout: 4000 });
    // 4 input monitoring, worded with the real key (default Right Option)
    await step(page, 4);
    await expect(page.locator(".onboarding h1")).toHaveText("Hold Right ⌥ to talk.");
    await page.getByRole("button", { name: "Open System Settings" }).click();
    await expect(page.getByText("Granted")).toBeVisible({ timeout: 4000 });
    // 5 talk key: live check turns green once holding the key starts a dictation
    await step(page, 5);
    await expect(page.locator(".keycap")).toHaveText("right ⌥");
    await expect(page.locator(".live-check")).toContainText("Right ⌥ is ready. Hold it and say something.");
    await page.keyboard.down("AltRight");
    await expect(page.locator(".live-check")).toContainText("Right ⌥ works.");
    await page.keyboard.up("AltRight");
    await page.getByRole("button", { name: "Continue" }).click();
    // 6 rhythm
    await step(page, 6);
    await page.getByRole("radio", { name: /Hold to talk/ }).click();
    await expect(page.getByRole("radio", { name: /Hold to talk/ })).toHaveAttribute("aria-checked", "true");
    await page.getByRole("radio", { name: /Tap to toggle/ }).click();
    await expect(page.getByRole("radio", { name: /Tap to toggle/ })).toHaveAttribute("aria-checked", "true");
    await page.getByRole("combobox", { name: "Language" }).selectOption("es");
    await expect(page.getByRole("combobox", { name: "Language" })).toHaveValue("es");
    await page.getByRole("button", { name: "Continue" }).click();
    // 7 try: no model yet, so it downloads base q5_1 (multilingual since es)
    await step(page, 7);
    await expect(page.locator(".ob-panel")).toContainText("Downloading base q5_1");
    await expect(page.locator(".ob-panel")).toHaveCount(0, { timeout: 20_000 });
    await page.evaluate(() => (window.mock.dictate(), window.mock.stop()));
    await expect(page.getByLabel("Practice field")).toHaveValue("The new build is up. Can you test the hot-plug flow?");
    await expect(page.locator(".result-line")).toContainText("642 ms to text");
    await page.getByRole("button", { name: "Finish" }).click();
    await expect(page.locator(".shell")).toBeVisible();
    await nav(page, "Providers");
    await expect(page.locator(".models .row", { hasText: "base q5_1" }).filter({ hasNotText: "base.en" })).toContainText("in use");
  });
  test("talk key: quick picks, record your own, Escape cancels", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    for (let i = 0; i < 3; i++) await notNow(page);
    await step(page, 5);
    const pick = (name) => page.getByRole("radiogroup", { name: "Talk key" }).getByRole("radio", { name });
    await expect(pick(/right ⌥/)).toHaveAttribute("aria-checked", "true");
    await expect(pick(/right ⌥/)).toContainText("Recommended");
    await expect(page.locator(".live-check")).toContainText("Right ⌥ needs Input Monitoring.");
    await pick(/right ⌘/).click();
    await expect(page.locator(".keycap")).toHaveText("right ⌘");
    await pick(/fn/).click();
    await expect(page.locator(".keycap")).toHaveText("fn");
    // record a modifier alone
    await page.getByRole("button", { name: /Record your own/ }).click();
    await expect(page.locator(".keycap .rec-hint")).toHaveText("Press any key or combo");
    await page.keyboard.down("ShiftRight");
    await expect(page.locator(".keycap")).toHaveText("right ⇧");
    await page.keyboard.up("ShiftRight");
    await expect(page.locator(".keycap")).not.toHaveClass(/recording/);
    await expect(page.getByRole("button", { name: /Record your own/ })).toHaveClass(/on/);
    // record a combo
    await page.locator(".keycap").click();
    await page.keyboard.press("Control+Alt+Space");
    await expect(page.locator(".keycap")).toHaveText("⌃⌥Space");
    await expect(page.locator(".live-check")).not.toContainText("Input Monitoring");
    // Escape cancels, the binding stays
    await page.locator(".keycap").click();
    await page.keyboard.press("Escape");
    await expect(page.locator(".keycap")).not.toHaveClass(/recording/);
    await expect(page.locator(".keycap")).toHaveText("⌃⌥Space");
    await page.getByRole("button", { name: "Continue" }).click();
    await step(page, 6);
    await expect(page.locator(".choice")).toContainText("⌃⌥Space");
  });
  test("permission step re-checks on window focus (back from System Settings)", async ({ page }) => {
    await page.clock.install();
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    await notNow(page);
    await step(page, 3);
    await page.clock.pauseAt(Date.now() + 60_000); // the 1 s poll stops: only the focus check can see the grant
    await page.evaluate(() => window.mock.perms({ accessibility: true }));
    await expect(page.getByText("Granted")).toHaveCount(0);
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByText("Granted")).toBeVisible();
    await page.clock.runFor(1000);
    await step(page, 4);
  });
  test("grant not seen 3 s after coming back: Restart susurro reopens on the same step", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.evaluate(() => (window.mock.stale = true));
    await page.getByRole("button", { name: "Get started" }).click();
    await notNow(page);
    await step(page, 3);
    await page.getByRole("button", { name: "Open System Settings" }).click();
    await page.waitForTimeout(1600); // the mock "System Settings" grant lands, but this process cannot see it
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
    await expect(page.getByRole("button", { name: "Restart susurro" })).toBeVisible({ timeout: 4500 });
    await expect(page.locator(".ob-restart")).toContainText("macOS needs a restart to apply Accessibility.");
    await page.getByRole("button", { name: "Restart susurro" }).click();
    await step(page, 3);
    await expect(page.getByText("Granted")).toBeVisible();
    await step(page, 4);
  });
  test("needs_relaunch shows the restart button; I already enabled it restarts too", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    await notNow(page);
    await notNow(page);
    await step(page, 4);
    await expect(page.getByRole("button", { name: "Restart susurro" })).toHaveCount(0);
    await page.evaluate(() => window.mock.perms({ input_monitoring: true, needs_relaunch: true }));
    await expect(page.getByRole("button", { name: "Restart susurro" })).toBeVisible({ timeout: 2500 });
    await expect(page.getByText("Granted")).toHaveCount(0);
    // a fresh case: the user flipped it before; the secondary link restarts right away
    await page.evaluate(() => window.mock.perms({ input_monitoring: false, needs_relaunch: false }));
    await expect(page.getByRole("button", { name: "I already enabled it" })).toBeVisible({ timeout: 2500 });
    await page.getByRole("button", { name: "I already enabled it" }).click();
    await step(page, 4); // reopened on the same step after the relaunch
    await expect(page.locator(".eyebrow")).toHaveText("Input Monitoring");
  });
  test("back, not now, alt+space, skip", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    await step(page, 2);
    await page.getByRole("button", { name: "Back" }).click();
    await step(page, 1);
    await page.getByRole("button", { name: "Get started" }).click();
    await notNow(page);
    await step(page, 3);
    await notNow(page);
    await step(page, 4);
    await page.getByRole("button", { name: "Use ⌥ Space instead (no permission)" }).click();
    await step(page, 5);
    await expect(page.locator(".keycap")).toHaveText("⌥Space");
    await page.getByRole("button", { name: "Continue" }).click();
    await step(page, 6);
    await expect(page.locator(".choice")).toContainText("⌥");
    await page.getByRole("button", { name: "Skip for now" }).click();
    await expect(page.locator(".shell")).toBeVisible();
    await expect(page.locator(".hint-kbd")).toContainText("hold to talk");
  });
  test("cloud provider link from the try step", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Get started" }).click();
    for (let i = 0; i < 3; i++) await notNow(page);
    await page.getByRole("button", { name: "Continue" }).click();
    await page.getByRole("button", { name: "Continue" }).click();
    await step(page, 7);
    await page.getByRole("button", { name: "Use a cloud provider instead" }).click();
    await expect(page.locator(".shell")).toBeVisible();
    await expect(h1(page)).toHaveText("Where your voice goes.");
  });
  test("Shortcuts Grant link opens onboarding at Input Monitoring", async ({ page }) => {
    await open(page, "/?onboarding");
    await page.getByRole("button", { name: "Skip for now" }).click();
    await nav(page, "Shortcuts");
    await expect(page.locator(".recorder-msg.warn-text")).toContainText("Right ⌥ needs Input Monitoring");
    await page.getByRole("button", { name: "Grant" }).click();
    await step(page, 4);
    await expect(page.locator(".eyebrow")).toHaveText("Input Monitoring");
  });
});

// ---------------- overlay ----------------

test.describe("overlay", () => {
  const pill = (page) => page.locator(".pill");
  const cases = [
    ["idle", "v-idle", "right ⌥ to talk"],
    ["hover", "v-idle", "right ⌥ to talk"],
    ["listening", "v-listening", null],
    ["handsfree", "v-handsfree", /0:4\d/],
    ["warning", "v-handsfree", /left/],
    ["processing", "v-processing", null],
    ["done", "v-done", "Pasted"],
    ["copied", "v-copied", "Copied - grant Accessibility"],
    ["error", "v-error", "Mic disconnected"],
    ["notice", "v-notice", "Switched to MacBook Pro Microphone"],
  ];
  for (const [state, cls, text] of cases)
    test(`state ${state}`, async ({ page }) => {
      await page.goto(`/overlay?state=${state}`);
      await expect(pill(page)).toHaveClass(new RegExp(`\\b${cls}\\b`));
      if (text) await expect(pill(page).locator(".layer.in")).toContainText(text);
      if (state === "hover") await expect(pill(page)).toHaveClass(/\bhover\b/);
      if (state === "warning") await expect(pill(page)).toHaveClass(/\bwarning\b/);
      if (["listening", "handsfree"].includes(state)) {
        await expect(pill(page).getByRole("button", { name: "Cancel" })).toBeVisible();
        await expect(pill(page).getByRole("button", { name: "Stop and paste" })).toBeVisible();
        await expect(pill(page).locator("canvas.wave.on")).toHaveCount(1);
      }
      await page.screenshot({ path: test.info().outputPath(`overlay-${state}.png`) });
    });
  test("a long error message ends in an ellipsis inside 300 px", async ({ page }) => {
    await page.goto("/overlay?state=error");
    await expect(pill(page)).toHaveClass(/\bv-error\b/);
    const long = "No audio from Wireless Mic Rx - check the mic is on and not muted in System Settings";
    await page.evaluate((m) => window.mock.error(m), long);
    const msg = pill(page).locator(".layer.in .msg");
    await expect(msg).toHaveText(long);
    await expect.poll(async () => (await pill(page).boundingBox()).width).toBeLessThanOrEqual(300); // after the morph
    await page.waitForTimeout(600); // let the pill settle in place before measuring the text against it
    const m = await msg.evaluate((e) => ({ over: getComputedStyle(e).textOverflow, clipped: e.scrollWidth > e.clientWidth, right: e.getBoundingClientRect().right }));
    expect(m).toMatchObject({ over: "ellipsis", clipped: true });
    const box = await pill(page).boundingBox();
    expect(m.right).toBeLessThanOrEqual(box.x + box.width);
  });
  test("listening: Cancel goes idle", async ({ page }) => {
    await page.goto("/overlay?state=listening");
    await pill(page).getByRole("button", { name: "Cancel" }).click();
    await expect(pill(page)).toHaveClass(/\bv-notice\b/);
    await expect(pill(page).locator(".layer.in")).toContainText("Cancelled");
    await expect(pill(page).getByRole("button", { name: "Undo" })).toBeVisible();
    await expect(pill(page)).toHaveClass(/\bv-idle\b/, { timeout: 8000 });
  });
  test("listening: Stop goes processing then done", async ({ page }) => {
    await page.goto("/overlay?state=listening");
    await pill(page).getByRole("button", { name: "Stop and paste" }).click();
    await expect(pill(page)).toHaveClass(/\bv-processing\b/);
    await expect(pill(page)).toHaveClass(/\bv-done\b/);
    await expect(pill(page)).toHaveClass(/\bv-idle\b/);
  });
  test("idle: click starts hands-free", async ({ page }) => {
    await page.goto("/overlay?state=idle");
    await expect(pill(page)).toHaveClass(/\bv-idle\b/);
    await pill(page).click();
    await expect(pill(page)).toHaveClass(/\bv-handsfree\b/);
  });
  test("demo loop runs through states", async ({ page }) => {
    await page.goto("/overlay");
    await expect(pill(page)).toHaveClass(/\bv-listening\b/, { timeout: 4000 });
    await expect(pill(page)).toHaveClass(/\bv-processing\b/, { timeout: 5000 });
    await expect(pill(page)).toHaveClass(/\bv-done\b/, { timeout: 4000 });
    await expect(pill(page)).toHaveClass(/\bv-handsfree\b/, { timeout: 6000 });
  });
});
