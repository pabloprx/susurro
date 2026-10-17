// Adversarial review: failing tests for bugs found in the wispr-parity diff. Run: bunx playwright test tests/review.spec.mjs
import { test, expect } from "@playwright/test";

const step = (page, n) => expect(page.locator(".ticks .count")).toHaveText(`${n}/5`);

async function toPractice(page) {
  await page.goto("/?onboarding");
  await expect(page.locator(".onboarding h1")).toHaveText("Talk. It types.");
  await page.evaluate(() => window.mock.perms({ microphone: "granted", accessibility: true }));
  await page.getByRole("button", { name: "Get started" }).click();
  await page.getByRole("button", { name: "Continue" }).click(); // mic test
  await page.getByRole("button", { name: "Continue" }).click(); // talk key
  await step(page, 5);
  await page.waitForTimeout(500); // TryStep registers its listeners in an effect
}

test("practice: a FAILED transcription must not pass the hold drill", async ({ page }) => {
  await toPractice(page);
  const drills = page.locator(".drills li");
  await expect(drills.nth(0)).toHaveClass(/active/);
  // session.rs Outcome::Failed emits history_added with status "failed" and empty text
  await page.evaluate(async () => {
    window.mock.dictate();
    const m = await import(performance.getEntriesByType("resource").map((e) => e.name).find((n) => n.includes("/src/lib/mock.ts")));
    m.mockEmit("history_added", {
      id: 9999, created_at: Date.now(), app_bundle: null, app_name: null, raw: "", final_text: "",
      provider: "whisper_local", cleanup: null, duration_ms: 1500, words: 0, latency_ms: 0,
      status: "failed", has_audio: true, error: "model not loaded",
    });
    window.mock.phase("idle");
  });
  await page.waitForTimeout(1200);
  await expect(drills.nth(0)).not.toHaveClass(/ok/);
});

test("sanity: the same emit with status pasted passes the drill (proves the emit reaches the page)", async ({ page }) => {
  await toPractice(page);
  const drills = page.locator(".drills li");
  await page.evaluate(async () => {
    window.mock.dictate();
    const m = await import(performance.getEntriesByType("resource").map((e) => e.name).find((n) => n.includes("/src/lib/mock.ts")));
    m.mockEmit("history_added", {
      id: 9998, created_at: Date.now(), app_bundle: null, app_name: null, raw: "x", final_text: "x",
      provider: "whisper_local", cleanup: null, duration_ms: 1500, words: 1, latency_ms: 1,
      status: "pasted", has_audio: true, error: null,
    });
  });
  await expect(drills.nth(0)).toHaveClass(/ok/);
});

test("practice: with 'store history' off, Done alone passes the hold drill", async ({ page }) => {
  await toPractice(page);
  const drills = page.locator(".drills li");
  // session.rs emits history_added only with store_history on; session_state done always comes
  await page.evaluate(() => {
    window.mock.dictate();
    window.mock.phase("done", { message: "pasted" });
  });
  await expect(drills.nth(0)).toHaveClass(/ok/);
});

test("practice: a failed transcription shows the error and keeps the drill active", async ({ page }) => {
  await toPractice(page);
  const drills = page.locator(".drills li");
  await page.evaluate(() => {
    window.mock.dictate();
    window.mock.error("Transcription failed: model not loaded");
  });
  await expect(page.locator(".result-line [role=alert]")).toContainText("model not loaded");
  await expect(drills.nth(0)).toHaveClass(/active/);
  await expect(drills.nth(0)).not.toHaveClass(/ok/);
  // the next try clears the error and passes
  await page.evaluate(() => window.mock.dictate());
  await expect(page.locator(".result-line [role=alert]")).toHaveCount(0);
  await page.evaluate(() => window.mock.stop());
  await expect(drills.nth(0)).toHaveClass(/ok/);
});
