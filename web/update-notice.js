/// The lobby's update notice. The server decides whether there is anything to
/// look for and asks GitHub itself (src/web/update.rs); this page only asks the
/// server, when the lobby loads and on the hour, and shows what it answers.
///
/// Both of the moments the notice is for are a lobby load: starting the binary
/// opens the lobby, and leaving an interview navigates back to it. An interview
/// replaces this page, so nothing here runs while one is under way.

/// The release the candidate last closed the notice on. The notice stays away
/// until the server names a different one.
export const UPDATE_DISMISSED_KEY = "codetrial:updateDismissed";

const COMMIT = /^[0-9a-f]{7}$/;
const RELEASE_PAGE = "https://github.com/sysprog21/codetrial/releases/";

/// The release to show for one `/api/update` answer, or null.
///
/// The server only ever sends its own constants and two commits, so anything
/// else means something answered that is not this server, and it gets no link.
export function updateToShow(answer, dismissed) {
  if (answer?.available !== true) return null;
  const { current, latest, url } = answer;
  if (!COMMIT.test(current) || !COMMIT.test(latest)) return null;
  if (typeof url !== "string" || !url.startsWith(RELEASE_PAGE)) return null;
  if (latest === dismissed) return null;
  return { current, latest, url };
}

/// How long until the next top of the hour on the candidate's clock.
export function msUntilNextHour(now) {
  const next = new Date(now);
  next.setMinutes(60, 0, 0);
  return next.getTime() - now.getTime();
}

/// Wires the notice in `root` and returns `refresh`, which checks now and then
/// keeps checking on the hour.
///
/// Only a server that answered with the check on is asked again. GitHub
/// failing is the server's answer `available: false`, so that keeps the
/// schedule, as finding nothing does. A server with the check off answers
/// `enabled: false`, and a lobby that cannot reach its own server has nothing
/// to show on the hour either, so neither is asked again until the next load.
export function mountUpdateNotice({
  root = document,
  fetchJson,
  readStored,
  writeStored,
  now = () => new Date(),
  nextFrame = (callback) => requestAnimationFrame(callback),
} = {}) {
  const notice = root.querySelector("#update-notice");
  const text = root.querySelector("#update-notice-text");
  const link = root.querySelector("#update-notice-link");
  const dismiss = root.querySelector("#update-notice-dismiss");
  const after = root.querySelector("#practice-title");
  let shown = null;
  // Also kept here, because a refused write (private browsing, a full or
  // blocked storage) is swallowed, and the next check would then read no
  // dismissal and show the same release again.
  let dismissed = null;
  let timer = null;

  function hide() {
    shown = null;
    // The button is about to disappear, and focus with it. Not scrolled to:
    // the title is at the top, and the candidate may be reading further down.
    if (notice.contains(root.activeElement))
      after.focus({ preventScroll: true });
    notice.hidden = true;
    // Emptied so the next notice is a change the status line announces.
    text.textContent = "";
  }

  dismiss.addEventListener("click", () => {
    if (!shown) return;
    dismissed = shown.latest;
    writeStored(UPDATE_DISMISSED_KEY, shown.latest);
    hide();
  });

  function show(release) {
    shown = release;
    link.href = release.url;
    notice.hidden = false;
    // Written a frame after the notice is visible: a status line filled in the
    // task that reveals it is not reliably announced. The link and the button
    // are found by moving to them, not read out.
    nextFrame(() => {
      if (shown !== release) return;
      text.textContent = `This copy is build ${release.current}, and the latest release is build ${release.latest}. Replace the executable and keep config/ beside it.`;
    });
  }

  async function check() {
    let answer;
    try {
      answer = await fetchJson("/api/update");
    } catch {
      return false;
    }
    // A server restarted with the check off takes the notice down too, since
    // nothing will ask again until the next load.
    if (answer?.enabled !== true) {
      if (shown) hide();
      return false;
    }
    // Either copy of the dismissal counts: a write refused over an older
    // stored one leaves storage naming that older release.
    let release = updateToShow(answer, readStored(UPDATE_DISMISSED_KEY));
    if (release?.latest === dismissed) release = null;
    // A release that is no longer newer, or was dismissed in another tab,
    // takes the notice down rather than leaving a stale one up.
    if (!release) {
      if (shown) hide();
    } else if (release.latest !== shown?.latest) {
      show(release);
    }
    return true;
  }

  // A restored page can ask while the hourly check still waits on the server,
  // and both then schedule one. The first of the two to fire cancels the other.
  async function refresh() {
    clearTimeout(timer);
    if (!(await check())) return;
    timer = setTimeout(refresh, msUntilNextHour(now()));
  }

  return { refresh };
}
