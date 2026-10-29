import { test, before, after } from "node:test";
import assert from "node:assert/strict";

import { launchChromium, startStaticServer } from "./source.js";
import {
  UPDATE_DISMISSED_KEY,
  mountUpdateNotice,
  msUntilNextHour,
  updateToShow,
} from "../../web/update-notice.js";

const RELEASE = "https://github.com/sysprog21/codetrial/releases/tag/latest";
const newer = (latest = "def5678") => ({
  enabled: true,
  available: true,
  current: "abc1234",
  latest,
  url: RELEASE,
});

test("only a well-formed newer release from this server is shown", () => {
  assert.deepEqual(updateToShow(newer(), null), {
    current: "abc1234",
    latest: "def5678",
    url: RELEASE,
  });
  for (const answer of [
    null,
    { enabled: false },
    { enabled: true, available: false },
    { ...newer(), available: "true" },
    { ...newer(), latest: "DEF5678" },
    { ...newer(), current: "abc12" },
    { ...newer(), url: "https://example.com/codetrial" },
    { ...newer(), url: "javascript:alert(1)" },
    { ...newer(), url: undefined },
    { ...newer(), url: 7 },
  ]) {
    assert.equal(updateToShow(answer, null), null, JSON.stringify(answer));
  }
});

test("a dismissed release stays away until another one is named", () => {
  assert.equal(updateToShow(newer("def5678"), "def5678"), null);
  assert.equal(updateToShow(newer("0123456"), "def5678").latest, "0123456");
});

test("the hourly check waits for the top of the candidate's hour", () => {
  const at = (hours, minutes, seconds, ms) =>
    new Date(2026, 9, 29, hours, minutes, seconds, ms);
  assert.equal(
    msUntilNextHour(at(10, 15, 30, 250)),
    (44 * 60 + 29) * 1000 + 750,
  );
  assert.equal(msUntilNextHour(at(10, 59, 59, 999)), 1);
  assert.equal(msUntilNextHour(at(11, 0, 0, 0)), 60 * 60 * 1000);
  assert.equal(msUntilNextHour(at(23, 30, 0, 0)), 30 * 60 * 1000);
});

/// The four elements the notice reads, as plain objects, so the checks below
/// run in node and every step can be awaited rather than waited for.
function fakeLobby() {
  const element = (id) => ({
    id,
    hidden: false,
    textContent: "",
    href: "",
    listeners: {},
    addEventListener(type, listener) {
      this.listeners[type] = listener;
    },
    focus() {
      root.activeElement = this;
    },
  });
  const elements = {
    "update-notice": element("update-notice"),
    "update-notice-text": element("update-notice-text"),
    "update-notice-link": element("update-notice-link"),
    "update-notice-dismiss": element("update-notice-dismiss"),
    "practice-title": element("practice-title"),
  };
  const notice = elements["update-notice"];
  notice.hidden = true;
  notice.contains = (other) =>
    [notice, ...Object.values(elements).slice(1, 4)].includes(other);
  const root = {
    activeElement: null,
    querySelector: (selector) => elements[selector.slice(1)],
  };
  return { root, elements, notice };
}

/// Lets a check the mocked clock started run to its end. `setImmediate` is not
/// one of the mocked timers, so it waits out every pending promise.
const settle = () => new Promise((ran) => setImmediate(ran));

const mockedClocks = new WeakSet();

/// A lobby whose `/api/update` answers whatever `answers` holds next, with a
/// storage it can read back.
function mounted(
  t,
  answers,
  { stored = null, now, refuseWrites = false } = {},
) {
  // Every check schedules the next one, and a real timer up to an hour out
  // would hold the test process open after the last test. Once per test: the
  // mock refuses a second enable.
  if (!mockedClocks.has(t)) {
    t.mock.timers.enable({ apis: ["setTimeout"] });
    mockedClocks.add(t);
  }
  const lobby = fakeLobby();
  const storage = new Map(stored ? [[UPDATE_DISMISSED_KEY, stored]] : []);
  const asked = [];
  const frames = [];
  const notice = mountUpdateNotice({
    root: lobby.root,
    fetchJson: async (url) => {
      asked.push(url);
      const answer = answers.shift();
      if (answer instanceof Error) throw answer;
      return answer;
    },
    readStored: (key) => storage.get(key) ?? null,
    // `writeStored` swallows a refused write, as private browsing refuses one.
    writeStored: (key, value) => {
      if (!refuseWrites) storage.set(key, value);
    },
    now: now ?? (() => new Date(2026, 9, 29, 10, 58, 0)),
    nextFrame: (callback) => frames.push(callback),
  });
  // Paints the frames the notice asked for, as the browser would next.
  const paint = () => frames.splice(0).forEach((callback) => callback());
  // A check, and the frame after it.
  const refresh = async () => {
    await notice.refresh();
    paint();
  };
  return {
    ...lobby,
    storage,
    asked,
    paint,
    pending: () => frames.length,
    check: notice.refresh,
    refresh,
    t,
  };
}

test("a newer release shows the notice with its link", async (t) => {
  const lobby = mounted(t, [newer()]);
  await lobby.refresh();
  assert.deepEqual(lobby.asked, ["/api/update"]);
  assert.equal(lobby.notice.hidden, false);
  assert.equal(lobby.elements["update-notice-link"].href, RELEASE);
  assert.match(lobby.elements["update-notice-text"].textContent, /abc1234/);
  assert.match(lobby.elements["update-notice-text"].textContent, /def5678/);
  // It announces itself; it does not take the candidate's focus.
  assert.equal(lobby.root.activeElement, null);
});

test("the message is written a frame after the notice is visible", async (t) => {
  const lobby = mounted(t, [newer()]);
  const text = lobby.elements["update-notice-text"];
  await lobby.check();
  // Shown, and the status line still empty in the task that showed it.
  assert.equal(lobby.notice.hidden, false);
  assert.equal(text.textContent, "");
  lobby.paint();
  assert.match(text.textContent, /def5678/);
});

test("a notice that no longer names a newer release is taken down", async (t) => {
  const lobby = mounted(t, [
    newer("def5678"),
    { enabled: true, available: false },
    newer("def5678"),
  ]);
  await lobby.refresh();
  assert.equal(lobby.notice.hidden, false);
  lobby.elements["update-notice-link"].focus();

  // The candidate replaced the binary, and the same lobby asked on the hour.
  t.mock.timers.tick(2 * 60_000);
  await settle();
  lobby.paint();
  assert.equal(lobby.notice.hidden, true);
  assert.equal(lobby.elements["update-notice-text"].textContent, "");
  assert.equal(lobby.root.activeElement, lobby.elements["practice-title"]);

  // Forgotten, not dismissed: the same release offered again shows again.
  t.mock.timers.tick(2 * 60_000);
  await settle();
  lobby.paint();
  assert.equal(lobby.notice.hidden, false);
  assert.match(lobby.elements["update-notice-text"].textContent, /def5678/);
});

test("a release dismissed in another tab is taken down here on the next check", async (t) => {
  const lobby = mounted(t, [newer("def5678"), newer("def5678")]);
  await lobby.refresh();
  assert.equal(lobby.notice.hidden, false);
  lobby.storage.set(UPDATE_DISMISSED_KEY, "def5678");
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.notice.hidden, true);
});

test("a notice already up follows the release the server names next", async (t) => {
  const lobby = mounted(t, [
    newer("def5678"),
    newer("def5678"),
    newer("0123456"),
  ]);
  const text = lobby.elements["update-notice-text"];
  await lobby.refresh();
  assert.match(text.textContent, /def5678/);

  // The same release on the hour leaves the message alone, so it is not
  // announced again.
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.pending(), 0);

  t.mock.timers.tick(2 * 60_000);
  await settle();
  lobby.paint();
  assert.match(text.textContent, /0123456/);
});

test("taking the notice down leaves focus that was elsewhere alone", async (t) => {
  const lobby = mounted(t, [newer(), { enabled: true, available: false }]);
  await lobby.refresh();
  const field = { id: "candidate-name" };
  lobby.root.activeElement = field;
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.notice.hidden, true);
  assert.equal(lobby.root.activeElement, field);
});

test("a notice dismissed before its frame writes nothing", async (t) => {
  const lobby = mounted(t, [newer()]);
  await lobby.check();
  lobby.elements["update-notice-dismiss"].listeners.click();
  lobby.paint();
  assert.equal(lobby.elements["update-notice-text"].textContent, "");
});

test("a page restored while a check is scheduled keeps one schedule", async (t) => {
  const answers = Array.from({ length: 4 }, () => ({
    enabled: true,
    available: false,
  }));
  const lobby = mounted(t, answers);
  await lobby.refresh();
  await lobby.refresh();
  assert.equal(lobby.asked.length, 2);
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.asked.length, 3);
});

test("a dismissal storage refused still holds on the next check", async (t) => {
  const lobby = mounted(t, [newer("def5678"), newer("def5678")], {
    refuseWrites: true,
  });
  await lobby.refresh();
  lobby.elements["update-notice-dismiss"].listeners.click();
  assert.equal(lobby.storage.size, 0);
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.asked.length, 2);
  assert.equal(lobby.notice.hidden, true);
});

test("a refused write over an older dismissal still holds the new one", async (t) => {
  const lobby = mounted(t, [newer("def5678"), newer("def5678")], {
    stored: "abc0000",
    refuseWrites: true,
  });
  await lobby.refresh();
  assert.equal(lobby.notice.hidden, false);
  lobby.elements["update-notice-dismiss"].listeners.click();
  assert.equal(lobby.storage.get(UPDATE_DISMISSED_KEY), "abc0000");
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.notice.hidden, true);
});

test("a second Dismiss on a hidden notice writes nothing", async (t) => {
  const lobby = mounted(t, [newer("def5678")]);
  await lobby.refresh();
  const click = lobby.elements["update-notice-dismiss"].listeners.click;
  click();
  lobby.storage.clear();
  click();
  assert.equal(lobby.storage.size, 0);
});

test("a server restarted with the check off takes the notice down", async (t) => {
  const lobby = mounted(t, [newer(), { enabled: false }]);
  await lobby.refresh();
  assert.equal(lobby.notice.hidden, false);
  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.notice.hidden, true);
  assert.equal(lobby.elements["update-notice-text"].textContent, "");
});

test("the released build, a failed check and a server with it off show nothing", async (t) => {
  for (const answer of [
    { enabled: true, available: false },
    new Error("/api/update returned 500"),
    { enabled: false },
    null,
  ]) {
    const lobby = mounted(t, [answer]);
    await lobby.refresh();
    assert.equal(lobby.notice.hidden, true, String(answer));
  }
});

test("dismissing keeps the notice away until another release is named", async (t) => {
  const lobby = mounted(t, [newer("def5678")]);
  await lobby.refresh();
  const dismiss = lobby.elements["update-notice-dismiss"];
  dismiss.focus();
  dismiss.listeners.click();
  assert.equal(lobby.notice.hidden, true);
  assert.equal(lobby.storage.get(UPDATE_DISMISSED_KEY), "def5678");
  // The focused button disappeared, so focus moved rather than fell away.
  assert.equal(lobby.root.activeElement, lobby.elements["practice-title"]);

  const again = mounted(t, [newer("def5678"), newer("0123456")], {
    stored: "def5678",
  });
  await again.refresh();
  assert.equal(again.notice.hidden, true);
  await again.refresh();
  assert.equal(again.notice.hidden, false);
  assert.match(again.elements["update-notice-text"].textContent, /0123456/);
});

test("an open lobby checks again on the hour, through a GitHub failure", async (t) => {
  // The second answer is what the server sends when GitHub did not answer.
  const lobby = mounted(t, [
    { enabled: true, available: false },
    { enabled: true, available: false },
    newer(),
  ]);
  await lobby.refresh();
  assert.equal(lobby.asked.length, 1);

  // 10:58 to 11:00. The clock the notice reads stays at 10:58, so each check
  // schedules the next one two minutes on.
  t.mock.timers.tick(2 * 60_000 - 1);
  await settle();
  assert.equal(lobby.asked.length, 1);
  t.mock.timers.tick(1);
  await settle();
  assert.equal(lobby.asked.length, 2);
  assert.equal(lobby.notice.hidden, true);

  t.mock.timers.tick(2 * 60_000);
  await settle();
  assert.equal(lobby.asked.length, 3);
  assert.equal(lobby.notice.hidden, false);
});

test("a server with the check off, or one that cannot answer, is not asked again", async (t) => {
  for (const answer of [
    { enabled: false },
    new Error("/api/update returned 500"),
    null,
  ]) {
    const lobby = mounted(t, [answer, newer()]);
    await lobby.refresh();
    t.mock.timers.tick(3 * 60 * 60_000);
    await settle();
    assert.equal(lobby.asked.length, 1, String(answer));
    assert.equal(lobby.notice.hidden, true, String(answer));
  }
});

// The page itself, in Chromium: the markup, app.js wiring it on load, and the
// keyboard path a candidate takes through it.

let browser = null;
let server = null;
let base = "";
let update = newer();

before(async () => {
  browser = await launchChromium();
  if (!browser) return;
  ({ server, base } = await startStaticServer({
    handle: async (request, response, url) => {
      if (url.pathname !== "/api/update") return false;
      response.setHeader("content-type", "application/json");
      response.end(JSON.stringify(update));
      return true;
    },
  }));
});

after(async () => {
  await browser?.close();
  await new Promise((closed) => (server ? server.close(closed) : closed()));
});

test("the lobby shows a newer release on load and dismisses it by keyboard", async (t) => {
  if (!browser) return t.skip("playwright chromium unavailable");
  const context = await browser.newContext();
  t.after(() => context.close());
  const page = await context.newPage();
  update = newer("def5678");
  await page.goto(base);

  const notice = page.getByRole("complementary", {
    name: "A newer CodeTrial build is available",
  });
  await notice.waitFor({ state: "visible" });
  const link = notice.getByRole("link", { name: "Open the latest release" });
  assert.equal(await link.getAttribute("href"), RELEASE);
  assert.equal(await link.getAttribute("target"), "_blank");
  // The message is what is announced, and only the message. It is written a
  // frame after the notice shows, so it is waited for.
  await notice.getByRole("status").filter({ hasText: "def5678" }).waitFor();

  const dismiss = notice.getByRole("button", { name: "Dismiss" });
  await dismiss.focus();
  await page.keyboard.press("Enter");
  await notice.waitFor({ state: "hidden" });
  assert.equal(
    await page.evaluate(() => document.activeElement?.id),
    "practice-title",
  );

  // The next release brings it back on the next load.
  update = newer("0123456");
  await page.reload();
  await notice.getByRole("status").filter({ hasText: "0123456" }).waitFor();
});

test("coming back to the lobby from an interview checks again", async (t) => {
  if (!browser) return t.skip("playwright chromium unavailable");
  const context = await browser.newContext();
  t.after(() => context.close());
  const page = await context.newPage();
  update = { enabled: true, available: false };
  // The load's own check has to be answered before the answer changes, or a
  // late one would show the release without the restore doing anything.
  const answered = page.waitForResponse((response) =>
    response.url().endsWith("/api/update"),
  );
  await page.goto(base);
  await answered;
  const notice = page.getByRole("complementary", {
    name: "A newer CodeTrial build is available",
  });
  assert.equal(await notice.isVisible(), false);

  // Leaving an interview restores the cached lobby rather than loading it.
  update = newer("0123456");
  await page.evaluate(() =>
    window.dispatchEvent(
      new PageTransitionEvent("pageshow", { persisted: true }),
    ),
  );
  await notice.getByRole("status").filter({ hasText: "0123456" }).waitFor();
});
