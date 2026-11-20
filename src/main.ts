import { getVersion } from "@tauri-apps/api/app";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  api,
  message,
  type Channel,
  type Comment,
  type AppRelease,
  type DownloadItem,
  type Hidden,
  type PlayInfo,
  type Playlist,
  type Video,
  type WatchPage,
  type Watched,
} from "./api";
import { applyIcons, icon, type IconName } from "./icons";
import { isAndroid, player } from "./player";
import { save, settings, SKIP_CATEGORIES } from "./settings";

const $ = <T extends HTMLElement = HTMLElement>(sel: string) => document.querySelector(sel) as T;
const viewEl = $("#view");
const watchEl = $("#watch");
const slotEl = $("#player-slot");
const overlayEl = $("#slot-overlay");
const watchBody = $("#watch-body");
const miniEl = $("#mini");

// ---- helpers ---------------------------------------------------------------

function esc(s: string | null | undefined): string {
  return (s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

function el(html: string): HTMLElement {
  const t = document.createElement("template");
  t.innerHTML = html.trim();
  const node = t.content.firstElementChild as HTMLElement;
  applyIcons(node);
  return node;
}

function fmtTime(s: number): string {
  s = Math.max(0, Math.floor(s));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

function fmtCount(n: number): string {
  if (n < 1000) return String(n);
  const [v, u] = n < 1e6 ? [n / 1e3, "K"] : n < 1e9 ? [n / 1e6, "M"] : [n / 1e9, "B"];
  return `${v < 10 ? Math.floor(v * 10) / 10 : Math.floor(v)}${u}`;
}

function timeAgo(unix: number): string {
  const s = Math.max(0, Date.now() / 1000 - unix);
  const steps: [number, string][] = [[31536000, "year"], [2592000, "month"], [604800, "week"], [86400, "day"], [3600, "hour"], [60, "minute"]];
  for (const [secs, unit] of steps) {
    const n = Math.floor(s / secs);
    if (n >= 1) return `${n} ${unit}${n > 1 ? "s" : ""} ago`;
  }
  return "Just now";
}

/** Avatars go through the app's image cache: Google's image server refuses bursts from the page. */
function avatarSrc(url: string): string {
  return convertFileSrc(`img/${url}`, "media");
}

/** The 1280×720 thumbnail for big pictures; hqdefault is only 480 wide. Old videos lack it (see `thumbFallback`). */
function sharpThumb(url: string): string {
  return url.replace(/^(https:\/\/i\.ytimg\.com\/vi\/[\w-]+\/)hqdefault\.jpg$/, "$1hq720.jpg");
}

/** A missing hq720 comes back as a 120×90 grey placeholder (or an error); fall back to hqdefault. */
function thumbFallback(e: Event) {
  const img = e.target;
  if (img instanceof HTMLImageElement && img.src.endsWith("/hq720.jpg") && (e.type === "error" || img.naturalWidth <= 120)) {
    img.src = img.src.replace(/hq720\.jpg$/, "hqdefault.jpg");
  }
}
document.addEventListener("load", thumbFallback, true);
document.addEventListener("error", thumbFallback, true);

function published(v: Video): string | null {
  return v.publishedAt ? timeAgo(v.publishedAt) : v.published ?? null;
}

let toastTimer = 0;
/** A message along the bottom; with `action`, a button on it (like Undo). */
function toast(text: string, ms = 3000, action?: { label: string; run: () => void }) {
  const t = $("#toast");
  t.textContent = text;
  if (action) {
    const b = el(`<button class="toast-action">${esc(action.label)}</button>`);
    b.addEventListener("click", () => {
      t.hidden = true;
      action.run();
    });
    t.append(b);
  }
  // Above the mini player when it shows, not over it.
  t.style.bottom = miniEl.hidden || watchOpen ? "" : "calc(var(--tabs) + 84px)";
  t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (t.hidden = true), ms);
}

function spinner(text = ""): string {
  return `<div class="loading"><span class="spin"></span>${esc(text)}</div>`;
}

function errorBox(text: string, retry?: () => void): HTMLElement {
  const box = el(`<div class="empty"><p>${esc(text)}</p>${retry ? `<button class="btn">Try again</button>` : ""}</div>`);
  box.querySelector("button")?.addEventListener("click", retry!);
  return box;
}

/** Calls `load` whenever `sentinel` scrolls into view, until it answers false. */
function infinite(sentinel: HTMLElement, root: HTMLElement, load: () => Promise<boolean>) {
  let busy = false;
  const io = new IntersectionObserver(
    async (entries) => {
      if (busy || !entries.some((e) => e.isIntersecting)) return;
      busy = true;
      sentinel.innerHTML = spinner();
      let more = false;
      try {
        more = await load();
      } catch (e) {
        sentinel.replaceChildren(errorBox(message(e)));
      }
      busy = false;
      if (more) {
        sentinel.innerHTML = "";
        // Still in view (a short page): keep going.
        io.unobserve(sentinel);
        io.observe(sentinel);
      } else {
        io.disconnect();
        if (!sentinel.querySelector(".empty")) sentinel.remove();
      }
    },
    { root, rootMargin: "600px" },
  );
  io.observe(sentinel);
}

// ---- sheets (menus along the bottom) ---------------------------------------

interface SheetItem {
  label: string;
  icon?: IconName;
  hint?: string;
  checked?: boolean;
  danger?: boolean;
  run: () => void;
}

const sheetRoot = $("#sheet-root");

function sheet(title: string, items: SheetItem[]) {
  const panel = el(`<div class="sheet"><div class="sheet-grip"></div>${title ? `<h3 class="sheet-title">${esc(title)}</h3>` : ""}<div class="sheet-items"></div></div>`);
  const list = panel.querySelector(".sheet-items")!;
  for (const item of items) {
    const b = el(
      `<button class="sheet-item${item.danger ? " danger" : ""}${item.checked ? " checked" : ""}">${item.icon ? icon(item.icon) : ""}<span><b>${esc(item.label)}</b>${item.hint ? `<small>${esc(item.hint)}</small>` : ""}</span>${item.checked ? icon("check") : ""}</button>`,
    );
    b.addEventListener("click", () => {
      closeSheet();
      item.run();
    });
    list.append(b);
  }
  const backdrop = el(`<div class="sheet-backdrop"></div>`);
  backdrop.addEventListener("click", closeSheet);
  sheetRoot.replaceChildren(backdrop, panel);
  sheetRoot.hidden = false;
}

function closeSheet(): boolean {
  if (sheetRoot.hidden) return false;
  sheetRoot.hidden = true;
  sheetRoot.replaceChildren();
  return true;
}

function confirmSheet(title: string, label: string, run: () => void) {
  sheet(title, [
    { label, icon: "delete", danger: true, run },
    { label: "Cancel", icon: "close", run: () => {} },
  ]);
}

// ---- video cards -----------------------------------------------------------

function badge(v: Video): string {
  if (v.live) return `<span class="badge live">LIVE</span>`;
  return v.duration ? `<span class="badge">${fmtTime(v.duration)}</span>` : "";
}

/** A video in a list: a big card, or a compact row (`compact`). */
function videoCard(v: Video, opts: { compact?: boolean; extra?: SheetItem[]; reason?: string } = {}): HTMLElement {
  const meta = [opts.compact || !v.avatar ? v.channel : null, v.views, published(v)].filter(Boolean).map(esc).join(" · ");
  const card = el(`
    <article class="card${opts.compact ? " compact" : ""}" data-id="${esc(v.id)}" data-channel="${esc(v.channelId ?? "")}">
      <div class="thumb"><img loading="lazy" decoding="async" alt="" src="${esc(opts.compact ? v.thumbnail : sharpThumb(v.thumbnail))}" />${badge(v)}<div class="progress" hidden><i></i></div></div>
      <div class="card-info">
        ${!opts.compact && v.avatar ? `<img class="avatar" loading="lazy" alt="" src="${esc(avatarSrc(v.avatar))}" />` : ""}
        <div class="card-text">
          <h3>${esc(v.title)}</h3>
          ${!opts.compact && v.avatar ? `<p class="card-channel">${esc(v.channel)}</p>` : ""}
          <p>${meta}</p>
          ${opts.reason ? `<p class="card-reason">${esc(opts.reason)}</p>` : ""}
        </div>
        <button class="icon-btn small more" title="More" data-icon="more"></button>
      </div>
    </article>`);
  card.addEventListener("click", (e) => {
    const target = e.target as HTMLElement;
    if (target.closest(".more")) {
      e.stopPropagation();
      videoMenu(v, opts.extra);
    } else if (target.closest(".avatar") && v.channelId) {
      e.stopPropagation();
      openChannel(v.channelId, { id: v.channelId, name: v.channel, avatar: v.avatar });
    } else {
      void openVideo(v);
    }
  });
  // A head start: the touch comes a moment before the tap.
  card.addEventListener("pointerdown", () => prefetch([v.id]), { once: true });
  swipeToQueue(card, v);
  return card;
}

/** How far (px) a card has to be swiped to the right to go in the queue. */
const SWIPE_QUEUE = 90;

/** Swiping a video to the right adds it to the queue (as in Cadence). Only a
 *  clearly sideways move counts, so slanted scrolls don't tug cards; cards
 *  on a sideways shelf scroll it instead. */
function swipeToQueue(card: HTMLElement, v: Video) {
  let x0 = 0;
  let y0 = 0;
  let dx = 0;
  let mode: "" | "swipe" | "scroll" = "";
  card.addEventListener(
    "touchstart",
    (e) => {
      if (card.closest(".shelf")) return;
      x0 = e.touches[0].clientX;
      y0 = e.touches[0].clientY;
      dx = 0;
      mode = "";
      card.style.transition = "";
    },
    { passive: true },
  );
  card.addEventListener(
    "touchmove",
    (e) => {
      if (card.closest(".shelf") || mode === "scroll") return;
      const x = e.touches[0].clientX - x0;
      const y = e.touches[0].clientY - y0;
      if (!mode) {
        if (Math.abs(x) < 12 && Math.abs(y) < 12) return;
        mode = x > 0 && Math.abs(x) > Math.abs(y) * 2 ? "swipe" : "scroll";
        if (mode === "scroll") return;
      }
      dx = Math.max(0, x);
      card.style.transform = `translateX(${dx}px)`;
      card.classList.toggle("swipe-ready", dx > SWIPE_QUEUE);
    },
    { passive: true },
  );
  const end = () => {
    if (mode !== "swipe") return;
    mode = "";
    card.style.transition = "transform 0.2s ease-out";
    card.style.transform = "";
    card.classList.remove("swipe-ready");
    if (dx > SWIPE_QUEUE) queueAdd(v, false);
  };
  card.addEventListener("touchend", end);
  card.addEventListener("touchcancel", end);
}

/** Asks for streams of videos likely to be opened soon (Settings can turn it off). */
function prefetch(ids: string[]) {
  // On mobile data, only the likeliest one.
  if (settings.prefetch && ids.length) void api.prefetch(onWifi ? ids : ids.slice(0, 1)).catch(() => {});
}

/** Wi-Fi (or another connection that isn't metered), checked when it may have changed. */
let onWifi = true;

async function checkNetwork() {
  const was = onWifi;
  onWifi = await api.unmetered().catch(() => true);
  if (onWifi && !was) void downloadLater();
}

/** Settings → Downloads → "Download Watch later": on Wi-Fi, saves what isn't saved yet. */
async function downloadLater() {
  if (!settings.downloadLater || !onWifi) return;
  const [later, downloads] = await Promise.all([api.later().catch(() => [] as Video[]), api.downloads().catch(() => [] as DownloadItem[])]);
  const have = new Set(downloads.map((d) => d.video.id));
  const missing = later.filter((v) => !have.has(v.id));
  for (const v of missing) await api.downloadAdd(v, settings.downloadQuality).catch(() => {});
  if (missing.length) toast(`Downloading ${missing.length} Watch later video${missing.length > 1 ? "s" : ""}`);
}

async function videoMenu(v: Video, extra: SheetItem[] = []) {
  const [later, downloads] = await Promise.all([api.later().catch(() => [] as Video[]), api.downloads().catch(() => [])]);
  const saved = later.some((x) => x.id === v.id);
  const downloaded = downloads.some((d) => d.video.id === v.id);
  sheet(v.title, [
    ...(current ? [{ label: "Play next", icon: "playNext" as IconName, run: () => queueAdd(v, true) }] : []),
    { label: "Add to queue", icon: "addQueue", run: () => queueAdd(v, false) },
    {
      label: saved ? "Remove from Watch later" : "Save to Watch later",
      icon: "later",
      run: () => void setLater(v, !saved),
    },
    downloaded
      ? { label: "Remove download", icon: "delete", run: () => void removeDownload(v) }
      : { label: "Download", icon: "download", hint: `${settings.downloadQuality}p, for watching offline`, run: () => void download(v) },
    ...(v.channelId ? [{ label: `Go to ${v.channel}`, icon: "channel" as IconName, run: () => openChannel(v.channelId!, { id: v.channelId!, name: v.channel, avatar: v.avatar }) }] : []),
    ...(isAndroid ? [{ label: "Share", icon: "share" as IconName, run: () => void shareVideo(v.id) }] : []),
    { label: "Copy link", icon: "link", run: () => copyLink(v.id) },
    { label: "Not interested", icon: "block", hint: "Keeps it out of For you, topics and autoplay", run: () => void setHidden({ id: v.id, kind: "video", name: v.title }, true) },
    ...(v.channelId
      ? [{ label: `Don't recommend ${v.channel}`, icon: "block" as IconName, run: () => void setHidden({ id: v.channelId!, kind: "channel", name: v.channel }, true) }]
      : []),
    ...extra,
  ]);
}

// ---- not interested --------------------------------------------------------

/** Ids of videos and channels kept out of recommendations, for autoplay to skip. */
let hiddenIds = new Set<string>();
void api.hidden().then((h) => (hiddenIds = new Set(h.map((x) => x.id))), () => {});

/** The first of `videos` you haven't said you're not interested in. */
function firstWanted(videos: Video[] | undefined): Video | undefined {
  return videos?.find((v) => !hiddenIds.has(v.id) && !(v.channelId && hiddenIds.has(v.channelId)));
}

/** Settings → Not interested: tapping one lets it into recommendations again. */
async function hiddenSheet() {
  const items = await api.hidden().catch(() => [] as Hidden[]);
  if (!items.length) return toast("Nothing hidden. Pick \"Not interested\" in a video's ⋮ menu to keep it out of recommendations.", 5000);
  sheet(
    "Tap one to see it in recommendations again",
    items.map((h) => ({
      label: h.name,
      icon: (h.kind === "channel" ? "channel" : "play") as IconName,
      hint: h.kind === "channel" ? "Channel" : "Video",
      run: () => {
        void setHidden(h, false).then(() => toast(`${h.name} can show up in recommendations again`));
      },
    })),
  );
}

async function setHidden(item: Hidden, on: boolean) {
  try {
    await api.hide(item, on);
  } catch (e) {
    return toast(message(e));
  }
  if (on) hiddenIds.add(item.id);
  else hiddenIds.delete(item.id);
  // Lists already made leave it out from now on.
  topicCache.clear();
  if (!on) return;
  const attr = item.kind === "video" ? "data-id" : "data-channel";
  // Recommendations on the page now go at once; other lists (search, history) keep theirs.
  document.querySelectorAll<HTMLElement>(`.recs .card[${attr}="${CSS.escape(item.id)}"], .topic .card[${attr}="${CSS.escape(item.id)}"]`).forEach((c) => c.remove());
  toast(item.kind === "video" ? "You won't see this video in recommendations" : `You won't see ${item.name} in recommendations`, 5000, {
    label: "Undo",
    run: () => {
      void setHidden(item, false).then(() => {
        if (top().route.kind === "home") refresh();
      });
    },
  });
}

// ---- queue -----------------------------------------------------------------

const QUEUE_KEY = "vista-queue";
let queue: Video[] = (() => {
  try {
    return JSON.parse(localStorage.getItem(QUEUE_KEY) ?? "[]");
  } catch {
    return [];
  }
})();

function saveQueue() {
  try {
    localStorage.setItem(QUEUE_KEY, JSON.stringify(queue));
  } catch {
    // The queue still works until the app closes.
  }
  drawQueue();
  drawMini();
}

function queueAdd(v: Video, next: boolean) {
  queue = queue.filter((q) => q.id !== v.id);
  if (next) queue.unshift(v);
  else queue.push(v);
  saveQueue();
  if (!current) {
    // Nothing playing: start right away.
    void openVideo(queue.shift()!);
    saveQueue();
    return;
  }
  toast(next ? "Plays next" : `Added to queue (${queue.length})`);
  if (queue[0]?.id === v.id) prefetch([v.id]);
}

/** Plays the next video in the queue; false when it's empty. */
function playNext(): boolean {
  const next = queue.shift();
  if (!next) return false;
  saveQueue();
  void openVideo(next);
  return true;
}

/** The queue on the watch page, above the related videos. */
function drawQueue() {
  const box = watchBody.querySelector<HTMLElement>(".queue");
  if (!box) return;
  box.hidden = !queue.length;
  if (!queue.length) return;
  const list = el(`<div class="list compact-list"></div>`);
  queue.forEach((v, i) =>
    list.append(
      videoCard(v, {
        compact: true,
        extra: [
          {
            label: "Remove from queue",
            icon: "close",
            run: () => {
              queue.splice(i, 1);
              saveQueue();
            },
          },
        ],
      }),
    ),
  );
  // Tapping a queued video plays it now and drops the ones before it.
  list.querySelectorAll<HTMLElement>(".card").forEach((card, i) =>
    card.addEventListener("click", () => {
      queue = queue.slice(i + 1);
      saveQueue();
    }),
  );
  const head = el(`<div class="section-head"><h2 class="section">Queue · ${queue.length}</h2><button class="text-btn">Clear</button></div>`);
  head.querySelector("button")!.addEventListener("click", () => {
    queue = [];
    saveQueue();
  });
  box.replaceChildren(head, list);
}

// ---- downloads -------------------------------------------------------------

async function download(v: Video) {
  try {
    await api.downloadAdd(v, settings.downloadQuality);
    toast("Downloading… see Library → Downloads");
  } catch (e) {
    toast(message(e));
  }
}

async function removeDownload(v: Video) {
  try {
    await api.downloadRemove(v.id);
    toast("Download removed");
  } catch (e) {
    toast(message(e));
  }
}

/** A downloaded video's saved thumbnail, so it shows offline too. */
function offlineVideo(d: DownloadItem): Video {
  return d.thumb ? { ...d.video, thumbnail: convertFileSrc(`thumb/${d.video.id}`, "media") } : d.video;
}

function fmtSize(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(bytes / 1e6))} MB`;
}

async function setLater(v: Video, saved: boolean) {
  try {
    await api.laterSet(v, saved);
    toast(saved ? "Saved to Watch later" : "Removed from Watch later");
    if (saved) void downloadLater();
    if (current?.video.id === v.id) {
      current.saved = saved;
      updateSaveChip();
    }
    if (top().route.kind === "later" || top().route.kind === "library") refresh();
  } catch (e) {
    toast(message(e));
  }
}

function copyLink(id: string, start?: number) {
  navigator.clipboard.writeText(start ? `https://youtu.be/${id}?t=${start}` : `https://youtu.be/${id}`).then(
    () => toast("Link copied"),
    () => toast("Couldn't copy the link"),
  );
}

// ---- sleep timer -------------------------------------------------------------

/** When the sleep timer pauses (ms since 1970), if it's set for a time. */
let sleepAt: number | null = null;
/** The sleep timer stops when the playing video ends instead. */
let sleepAtEnd = false;

/** Minutes from now, or "end" for the end of the playing video, or null to turn it off. */
function setSleep(choice: number | "end" | null) {
  sleepAt = typeof choice === "number" ? Date.now() + choice * 60_000 : null;
  sleepAtEnd = choice === "end";
  player.sleep(typeof choice === "number" ? choice * 60 : 0);
  drawSleepChip();
}

function drawSleepChip() {
  // The native player paused when the time came.
  if (sleepAt && Date.now() >= sleepAt) sleepAt = null;
  const chip = document.querySelector<HTMLElement>("#chip-sleep");
  if (!chip) return;
  const label = sleepAt ? `Sleep · ${Math.max(1, Math.ceil((sleepAt - Date.now()) / 60_000))} min` : sleepAtEnd ? "Sleep · end of video" : "Sleep timer";
  chip.classList.toggle("on", !!sleepAt || sleepAtEnd);
  chip.querySelector("span")!.textContent = label;
}

function sleepSheet() {
  const on = !!sleepAt || sleepAtEnd;
  sheet("Sleep timer", [
    ...[15, 30, 45, 60].map((m) => ({
      label: m === 60 ? "1 hour" : `${m} minutes`,
      icon: "sleep" as IconName,
      run: () => {
        setSleep(m);
        toast(`Pausing in ${m === 60 ? "an hour" : `${m} minutes`}`);
      },
    })),
    {
      label: "End of this video",
      icon: "sleep",
      hint: "No queue or autoplay after it",
      checked: sleepAtEnd,
      run: () => {
        setSleep("end");
        toast("Stopping after this video");
      },
    },
    ...(on ? [{ label: "Turn off", icon: "close" as IconName, run: () => (setSleep(null), toast("Sleep timer off")) }] : []),
  ]);
}

/** Android's share sheet with the video's link; the desktop has none, so it copies it. */
async function shareVideo(id: string, start?: number) {
  try {
    if (!(await api.share(id, start))) copyLink(id, start);
  } catch (e) {
    toast(message(e));
  }
}

/** The watch page's Share: the link, from where you are, or copied. */
function shareSheet(v: Video) {
  const at = current?.video.id === v.id && !current.info?.live ? Math.floor(player.state.position) : 0;
  const send = isAndroid ? "Share" : "Copy link";
  sheet("Share", [
    { label: send, icon: isAndroid ? "share" : "link", run: () => void shareVideo(v.id) },
    ...(at >= 10 ? [{ label: `${send} from ${fmtTime(at)}`, icon: "history" as IconName, hint: "Starts where you are now", run: () => void shareVideo(v.id, at) }] : []),
    ...(isAndroid ? [{ label: "Copy link", icon: "link" as IconName, run: () => copyLink(v.id) }] : []),
  ]);
}

/** Fills in the "watched this far" bars on the cards in `root`. */
async function markProgress(root: HTMLElement, videos: Video[]) {
  if (!videos.length) return;
  const positions = await api.positions(videos.map((v) => v.id)).catch(() => ({}) as Record<string, { position: number; duration: number | null }>);
  for (const [id, p] of Object.entries(positions)) {
    const duration = p.duration ?? videos.find((v) => v.id === id)?.duration;
    if (!duration || p.position < 5) continue;
    root.querySelectorAll<HTMLElement>(`.card[data-id="${CSS.escape(id)}"] .progress`).forEach((bar) => {
      bar.hidden = false;
      (bar.firstElementChild as HTMLElement).style.width = `${Math.min(100, (p.position / duration) * 100)}%`;
    });
  }
}

function appendVideos(list: HTMLElement, videos: Video[], opts: Parameters<typeof videoCard>[1] = {}) {
  for (const v of videos) list.append(videoCard(v, opts));
  void markProgress(list, videos);
}

function channelRow(c: Channel, subscribed?: boolean): HTMLElement {
  const sub = [c.handle, c.subscribers].filter(Boolean).map(esc).join(" · ");
  const row = el(`
    <div class="channel-item">
      ${c.avatar ? `<img class="avatar big" loading="lazy" alt="" src="${esc(avatarSrc(c.avatar))}" />` : `<span class="avatar big blank">${esc(c.name.slice(0, 1))}</span>`}
      <div class="channel-text"><b>${esc(c.name)}</b>${sub ? `<small>${sub}</small>` : ""}</div>
    </div>`);
  if (subscribed !== undefined) row.append(subscribeButton(c, subscribed));
  row.addEventListener("click", (e) => {
    if (!(e.target as HTMLElement).closest(".sub-btn")) openChannel(c.id, c);
  });
  return row;
}

function subscribeButton(c: Channel, subscribed: boolean): HTMLElement {
  const b = el(`<button class="sub-btn"></button>`);
  const draw = () => {
    b.classList.toggle("on", subscribed);
    b.textContent = subscribed ? "Subscribed" : "Subscribe";
  };
  draw();
  b.addEventListener("click", async (e) => {
    e.stopPropagation();
    const set = async (on: boolean) => {
      try {
        await api.subscribe(c, on);
        subscribed = on;
        draw();
        toast(on ? `Subscribed to ${c.name}` : `Unsubscribed from ${c.name}`);
      } catch (err) {
        toast(message(err));
      }
    };
    if (subscribed) confirmSheet(`Unsubscribe from ${c.name}?`, "Unsubscribe", () => void set(false));
    else await set(true);
  });
  return b;
}

// ---- navigation ------------------------------------------------------------

type Tab = "home" | "subscriptions" | "library";
type Route =
  | { kind: Tab }
  | { kind: "search"; query: string }
  | { kind: "channel"; id: string; channel?: Channel }
  | { kind: "playlist"; id: string; playlist?: Playlist }
  | { kind: "downloads" }
  | { kind: "history" }
  | { kind: "later" }
  | { kind: "settings" };

interface Entry {
  route: Route;
  el: HTMLElement;
  scroll: number;
}

let stack: Entry[] = [];
const top = () => stack[stack.length - 1];

const TITLES: Partial<Record<Route["kind"], string>> = {
  subscriptions: "Subscriptions",
  library: "Library",
  history: "History",
  later: "Watch later",
  settings: "Settings",
  downloads: "Downloads",
};

function drawTopbar() {
  const route = top().route;
  const title =
    route.kind === "channel" ? route.channel?.name ?? "" : route.kind === "playlist" ? route.playlist?.title ?? "Playlist" : TITLES[route.kind] ?? "";
  const searching = route.kind === "search" || !$("#search-form").hidden;
  $("#back").hidden = stack.length < 2 && !searching;
  $(".topbar").classList.toggle("with-back", !$("#back").hidden);
  $("#brand").hidden = route.kind !== "home" || searching;
  $("#top-title").hidden = !title || searching;
  $("#top-title").textContent = title;
  $("#top-title").classList.toggle("big", stack.length === 1);
  $("#refresh").hidden = !["home", "subscriptions"].includes(route.kind) || searching;
  $("#search-open").hidden = searching;
  document.querySelectorAll<HTMLElement>(".tab").forEach((t) => t.classList.toggle("active", t.dataset.tab === stack[0].route.kind));
}

function render(route: Route): HTMLElement {
  switch (route.kind) {
    case "home":
      return renderHome();
    case "subscriptions":
      return renderSubscriptions();
    case "library":
      return renderLibrary();
    case "search":
      return renderSearch(route.query);
    case "channel":
      return renderChannel(route.id, route.channel);
    case "playlist":
      return renderPlaylist(route.id, route.playlist);
    case "downloads":
      return renderDownloads();
    case "history":
      return renderHistory();
    case "later":
      return renderLater();
    case "settings":
      return renderSettings();
  }
}

function showTop() {
  const entry = top();
  viewEl.replaceChildren(entry.el);
  viewEl.scrollTop = entry.scroll;
  drawTopbar();
}

function navigate(route: Route) {
  if (stack.length) top().scroll = viewEl.scrollTop;
  minimizeWatch();
  closeSearch();
  stack.push({ route, el: render(route), scroll: 0 });
  showTop();
}

function switchTab(tab: Tab) {
  minimizeWatch();
  closeSearch();
  if (stack.length === 1 && stack[0].route.kind === tab) {
    // Tapping the current tab again goes to its top, then refreshes it.
    if (viewEl.scrollTop > 0) viewEl.scrollTo({ top: 0, behavior: "smooth" });
    else refresh();
    return;
  }
  stack = [{ route: { kind: tab }, el: render({ kind: tab }), scroll: 0 }];
  showTop();
}

/** Draws the current page again, keeping its scroll position. */
function refresh() {
  const entry = top();
  entry.scroll = viewEl.scrollTop;
  entry.el = render(entry.route);
  showTop();
}

function openChannel(id: string, channel?: Channel) {
  const route = top().route;
  if (route.kind === "channel" && route.id === id) return minimizeWatch();
  navigate({ kind: "channel", id, channel });
}

/** The back button: closes whatever is on top. False when nothing is left. */
function back(): boolean {
  if (closeSheet()) return true;
  if (closeComments()) return true;
  if (watchOpen) {
    minimizeWatch();
    return true;
  }
  if (!searchPanel.hidden && top().route.kind === "search") {
    searchPanel.hidden = true;
    searchInput.value = (top().route as { query: string }).query;
    searchInput.blur();
    return true;
  }
  if (!$("#search-form").hidden && top().route.kind !== "search") {
    closeSearch();
    return true;
  }
  if (stack.length > 1) {
    stack.pop();
    showTop();
    return true;
  }
  if (stack[0].route.kind !== "home") {
    switchTab("home");
    return true;
  }
  return false;
}
(window as any).__vistaBack = back;

// ---- search ----------------------------------------------------------------

const searchForm = $<HTMLFormElement>("#search-form");
const searchInput = $<HTMLInputElement>("#search-input");

const searchPanel = $("#search-panel");
let suggestTimer = 0;

function openSearch() {
  searchForm.hidden = false;
  const route = top().route;
  searchInput.value = route.kind === "search" ? route.query : "";
  drawTopbar();
  searchInput.focus();
  drawSearchPanel();
}

function closeSearch() {
  searchForm.hidden = true;
  searchPanel.hidden = true;
  searchInput.blur();
}

function runSearch(query: string) {
  searchInput.value = query;
  searchForm.requestSubmit();
}

/** Under the search box while typing: YouTube's suggestions, or your recent searches. */
function drawSearchPanel() {
  searchPanel.hidden = false;
  clearTimeout(suggestTimer);
  const typed = searchInput.value.trim();
  if (!typed) {
    void api.searches().then((recent) => {
      if (searchInput.value.trim() || searchPanel.hidden) return;
      if (!recent.length) {
        searchPanel.replaceChildren();
        return;
      }
      const head = el(`<div class="section-head"><h2 class="section">Recent searches</h2><button class="text-btn">Clear all</button></div>`);
      head.querySelector("button")!.addEventListener("click", () =>
        confirmSheet("Clear your search history?", "Clear searches", async () => {
          await api.searchesClear().catch((e) => toast(message(e)));
          drawSearchPanel();
        }),
      );
      const rows = recent.map((q) => {
        const row = el(`<div class="search-row">${icon("history")}<span>${esc(q)}</span><button class="icon-btn" title="Remove from history" data-icon="close"></button></div>`);
        row.querySelector("button")!.addEventListener("click", async (e) => {
          e.stopPropagation();
          await api.searchRemove(q).catch((err) => toast(message(err)));
          drawSearchPanel();
        });
        row.addEventListener("click", () => runSearch(q));
        return row;
      });
      searchPanel.replaceChildren(head, ...rows);
    });
    return;
  }
  suggestTimer = window.setTimeout(async () => {
    const found = await api.searchSuggestions(typed).catch(() => [] as string[]);
    if (searchInput.value.trim() !== typed || searchPanel.hidden) return;
    const rows = found.slice(0, 8).map((s) => {
      // What's typed stays plain; the rest of the suggestion is bold.
      const same = s.toLowerCase().startsWith(typed.toLowerCase());
      const text = same ? `${esc(s.slice(0, typed.length))}<b>${esc(s.slice(typed.length))}</b>` : esc(s);
      const row = el(`<div class="search-row">${icon("search")}<span>${text}</span><button class="icon-btn" title="Edit this search" data-icon="fill"></button></div>`);
      row.querySelector("button")!.addEventListener("click", (e) => {
        e.stopPropagation();
        searchInput.value = `${s} `;
        searchInput.focus();
        drawSearchPanel();
      });
      row.addEventListener("click", () => runSearch(s));
      return row;
    });
    searchPanel.replaceChildren(...rows);
  }, 150);
}

searchInput.addEventListener("input", drawSearchPanel);
searchInput.addEventListener("focus", () => searchPanel.hidden && drawSearchPanel());

searchForm.addEventListener("submit", (e) => {
  e.preventDefault();
  const query = searchInput.value.trim();
  if (!query) return;
  searchPanel.hidden = true;
  searchInput.blur();
  if (/youtu.?be/i.test(query)) {
    closeSearch();
    drawTopbar();
    void openLink(query);
    return;
  }
  const route = top().route;
  if (route.kind === "search") {
    route.query = query;
    refresh();
  } else {
    navigate({ kind: "search", query });
    searchForm.hidden = false;
    searchInput.value = query;
    drawTopbar();
  }
});

function renderSearch(query: string): HTMLElement {
  const page = el(`<div class="page"><div class="channels"></div><div class="playlists"></div><div class="list"></div><div class="sentinel"></div></div>`);
  const channels = page.querySelector<HTMLElement>(".channels")!;
  const playlists = page.querySelector<HTMLElement>(".playlists")!;
  const list = page.querySelector<HTMLElement>(".list")!;
  const sentinel = page.querySelector<HTMLElement>(".sentinel")!;
  let next: string | null | undefined;
  let subscribed = new Set<string>();
  infinite(sentinel, viewEl, async () => {
    if (next === undefined) {
      const [results, subs] = await Promise.all([api.search(query), api.subscriptions().catch(() => [])]);
      subscribed = new Set(subs.map((c) => c.id));
      // The top channel results, but not every channel that mentions the words.
      for (const c of results.channels.slice(0, 3)) channels.append(channelRow(c, subscribed.has(c.id)));
      for (const p of results.playlists.slice(0, 3)) playlists.append(playlistRow(p));
      if (!results.videos.length && !results.channels.length) {
        list.replaceChildren(el(`<div class="empty"><p>Nothing found for “${esc(query)}”.</p></div>`));
      }
      appendVideos(list, results.videos);
      next = results.next;
    } else if (next) {
      const results = await api.search(query, next);
      appendVideos(list, results.videos);
      next = results.next;
    }
    return !!next;
  });
  return page;
}

// ---- home, and the subscriptions feed ----------------------------------------

/** How old a feed check may be before opening Home checks again. */
const FEED_STALE_SECS = 15 * 60;
let feedChecking = false;

/** Splits a newest-first list into Today, Yesterday, This week… by `at` (seconds since 1970). */
function dayGroups<T>(items: T[], at: (item: T) => number): [string, T[]][] {
  const now = new Date();
  const startOfDay = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime() / 1000;
  const groups: [string, number][] = [["Today", startOfDay], ["Yesterday", startOfDay - 86400], ["This week", startOfDay - 6 * 86400], ["This month", startOfDay - 29 * 86400], ["Older", -Infinity]];
  const out: [string, T[]][] = [];
  for (const item of items) {
    const t = at(item);
    const name = groups.find(([, from]) => t >= from)![0];
    if (out.at(-1)?.[0] !== name) out.push([name, []]);
    out.at(-1)![1].push(item);
  }
  return out;
}

/** Home's chips after "All", each searched for as written. */
const TOPICS = ["Gaming", "News", "Music", "Podcasts", "Anime", "Tech"];
/** The picked chip ("" is All), kept while the app is open. */
let homeTopic = "";

/** Home: "All" (your recommendations) or a topic, picked with chips. */
function renderHome(): HTMLElement {
  const page = el(`
    <div class="page">
      <div class="home-chips">
        <button class="chip" data-topic="">All</button>
        ${TOPICS.map((t) => `<button class="chip" data-topic="${esc(t)}">${esc(t)}</button>`).join("")}
      </div>
      <div class="home-body"></div>
    </div>`);
  const body = page.querySelector<HTMLElement>(".home-body")!;
  const show = (topic: string) => {
    homeTopic = topic;
    page.querySelectorAll<HTMLElement>(".home-chips .chip").forEach((c) => c.classList.toggle("on", c.dataset.topic === topic));
    const view = topic ? topicView(topic) : forYou();
    body.replaceChildren(view);
    (page as any).check = (view as any).check;
  };
  page.querySelectorAll<HTMLElement>(".home-chips .chip").forEach((c) => c.addEventListener("click", () => show(c.dataset.topic!)));
  show(homeTopic);
  return page;
}

/** Big cards for recommendations, each with why it's there (if it says). */
function recsList(items: { video: Video; reason: string }[]): HTMLElement {
  const list = el(`<div class="list"></div>`);
  for (const r of items) list.append(videoCard(r.video, { reason: r.reason || undefined }));
  void markProgress(list, items.map((r) => r.video));
  prefetch(items.slice(0, 2).map((r) => r.video.id));
  return list;
}

/** How old a topic's list may be before opening it makes a new one. */
const TOPIC_STALE_SECS = 30 * 60;
const topicCache = new Map<string, { items: { video: Video; reason: string }[]; made: number }>();

function topicView(topic: string): HTMLElement {
  const box = el(`<div class="topic"><div class="topic-list">${spinner(`Finding ${topic.toLowerCase()} videos…`)}</div></div>`);
  const listBox = box.querySelector<HTMLElement>(".topic-list")!;
  const load = async (fresh: boolean) => {
    const cached = topicCache.get(topic);
    if (cached && !fresh && Date.now() / 1000 - cached.made < TOPIC_STALE_SECS) {
      listBox.replaceChildren(recsList(cached.items));
      return;
    }
    if (fresh) listBox.innerHTML = spinner(`Finding ${topic.toLowerCase()} videos…`);
    try {
      const items = await api.topic(topic);
      topicCache.set(topic, { items, made: Date.now() / 1000 });
      listBox.replaceChildren(items.length ? recsList(items) : el(`<div class="empty"><p>Nothing found for ${esc(topic)} right now.</p></div>`));
    } catch (e) {
      listBox.replaceChildren(errorBox(message(e), () => void load(true)));
    }
  };
  void load(false);
  (box as any).check = () => load(true);
  return box;
}

/** How old the For you list may be before opening Home makes a new one. */
const RECS_STALE_SECS = 30 * 60;
let recsMaking = false;

function forYou(): HTMLElement {
  const box = el(`
    <div>
      <div class="feed-status" hidden></div>
      <div class="recs">${spinner()}</div>
    </div>`);
  const status = box.querySelector<HTMLElement>(".feed-status")!;
  const recsBox = box.querySelector<HTMLElement>(".recs")!;

  const draw = (items: { video: Video; reason: string }[]) => {
    if (!items.length) {
      recsBox.replaceChildren(
        el(`<div class="empty"><p>Nothing to suggest yet.</p><small>Watch or search for a few videos, and Vista will suggest more like them here.</small></div>`),
      );
      return;
    }
    recsBox.replaceChildren(el(`<h2 class="section">Recommended</h2>`), recsList(items));
  };

  const make = async () => {
    if (recsMaking) return;
    recsMaking = true;
    status.hidden = false;
    status.innerHTML = spinner("Finding videos for you…");
    try {
      draw((await api.recommendations(true)).items);
    } catch (e) {
      if (!recsBox.querySelector(".card")) recsBox.replaceChildren(errorBox(message(e), () => void make()));
      else toast(message(e));
    } finally {
      recsMaking = false;
      status.hidden = true;
    }
  };

  void (async () => {
    try {
      const [recs, subs, history] = await Promise.all([api.recommendations(false), api.subscriptions(), api.history()]);
      // A brand new install: nothing to go on yet.
      if (!subs.length && !history.length && !recs.items.length) {
        recsBox.replaceChildren(welcome());
        return;
      }
      if (recs.items.length) draw(recs.items);
      if (!recs.made || Date.now() / 1000 - recs.made > RECS_STALE_SECS) await make();
      else if (!recs.items.length) draw([]);
    } catch (e) {
      recsBox.replaceChildren(errorBox(message(e), refresh));
    }
  })();
  // The refresh button: new suggestions.
  (box as any).check = make;
  return box;
}

function subsFeed(): HTMLElement {
  const page = el(`<div><div class="feed-status" hidden></div><div class="feed">${spinner()}</div></div>`);
  const status = page.querySelector<HTMLElement>(".feed-status")!;
  const feed = page.querySelector<HTMLElement>(".feed")!;

  const draw = (videos: Video[], subs: number) => {
    if (!videos.length) {
      feed.replaceChildren(
        subs
          ? el(`<div class="empty"><p>No new videos yet.</p><small>Uploads from your ${subs} subscription${subs > 1 ? "s" : ""} show up here.</small></div>`)
          : welcome(),
      );
      return;
    }
    feed.replaceChildren();
    // The newest uploads are the likeliest to be opened.
    prefetch(videos.slice(0, 2).map((v) => v.id));
    for (const [name, group] of dayGroups(videos, (v) => v.publishedAt ?? 0)) {
      feed.append(el(`<h2 class="section">${esc(name)}</h2>`));
      const list = el(`<div class="list"></div>`);
      appendVideos(list, group);
      feed.append(list);
    }
  };

  const check = async (subs: number) => {
    if (!subs) return toast("Subscribe to some channels first.");
    if (feedChecking) return;
    feedChecking = true;
    status.hidden = false;
    status.innerHTML = spinner(`Checking ${subs} channel${subs > 1 ? "s" : ""} for new videos…`);
    try {
      const f = await api.feed(true, settings.shorts);
      draw(f.videos, subs);
      if (f.failed) toast(`${f.failed} channel${f.failed > 1 ? "s" : ""} couldn't be checked. Try again later.`);
    } catch (e) {
      toast(message(e));
    } finally {
      feedChecking = false;
      status.hidden = true;
    }
  };

  void (async () => {
    try {
      const [f, subs] = await Promise.all([api.feed(false, settings.shorts), api.subscriptions()]);
      draw(f.videos, subs.length);
      if (subs.length && (!f.checked || Date.now() / 1000 - f.checked > FEED_STALE_SECS)) await check(subs.length);
    } catch (e) {
      feed.replaceChildren(errorBox(message(e), refresh));
    }
  })();
  (page as any).check = async () => check((await api.subscriptions()).length);
  return page;
}

function welcome(): HTMLElement {
  const box = el(`
    <div class="welcome">
      <span class="brand-mark big"></span>
      <h2>Welcome to Vista</h2>
      <p>Subscribe to channels, or watch and search for a few videos, and Vista fills up with new uploads and suggestions. No account needed, and nothing leaves your phone.</p>
      <button class="btn primary" data-icon="search">Find channels</button>
      <button class="btn" data-icon="import">Import from YouTube</button>
      <small>Your YouTube subscriptions come from Google Takeout: takeout.google.com → YouTube → subscriptions.csv</small>
    </div>`);
  const [find, imp] = box.querySelectorAll("button");
  find.addEventListener("click", openSearch);
  imp.addEventListener("click", pickTakeout);
  return box;
}

$("#refresh").addEventListener("click", () => {
  const page = top().el as any;
  if (page.check) void page.check();
});

/** How far (px) Home or Subscriptions has to be pulled down to refresh. */
const PULL_REFRESH = 70;

// Pulling down from the top of Home or Subscriptions does what refresh does.
{
  const pull = $("#pull");
  let y0: number | null = null;
  let dy = 0;
  const pullable = () => stack.length === 1 && ["home", "subscriptions"].includes(top().route.kind) && searchPanel.hidden;
  const draw = () => {
    const shown = Math.min(dy, PULL_REFRESH * 1.5);
    pull.style.opacity = String(Math.min(1, shown / PULL_REFRESH));
    pull.style.transform = `translateY(${shown * 0.6 - 48 + 8}px) rotate(${shown * 3}deg)`;
    pull.classList.toggle("ready", dy > PULL_REFRESH);
  };
  viewEl.addEventListener(
    "touchstart",
    (e) => {
      y0 = pullable() && viewEl.scrollTop <= 0 ? e.touches[0].clientY : null;
      dy = 0;
    },
    { passive: true },
  );
  viewEl.addEventListener(
    "touchmove",
    (e) => {
      if (y0 === null) return;
      dy = Math.max(0, e.touches[0].clientY - y0);
      // Scrolled away from the top: not a pull after all.
      if (viewEl.scrollTop > 0) dy = 0;
      draw();
    },
    { passive: true },
  );
  viewEl.addEventListener("touchend", () => {
    if (y0 === null) return;
    const page = top().el as any;
    if (dy > PULL_REFRESH && page.check) void page.check();
    y0 = null;
    dy = 0;
    pull.style.transition = "opacity 0.2s, transform 0.2s";
    draw();
    window.setTimeout(() => (pull.style.transition = ""), 200);
  });
}

// ---- subscriptions ---------------------------------------------------------

/**
 * Phones: hands Android your subscriptions, so it can check them for new
 * videos every few hours and notify, with Vista closed too (NewVideos.kt).
 * The first time there's a channel to watch, Android 13 and newer ask whether
 * Vista may notify.
 */
async function watchUploads() {
  if (!isAndroid) return;
  const subs = await api.subscriptions().catch(() => [] as Channel[]);
  const on = settings.newVideos && subs.length > 0;
  const json = JSON.stringify({ channels: subs.map((c) => ({ id: c.id, name: c.name })), shorts: settings.shorts });
  await api.uploadsWatch(json, on).catch(() => {});
  if (on && !settings.askedNotifications) {
    save({ askedNotifications: true });
    await api.notificationsAllow().catch(() => false);
  }
}

const fileInput = $<HTMLInputElement>("#file-input");

function pickTakeout() {
  fileInput.value = "";
  fileInput.click();
}

fileInput.addEventListener("change", async () => {
  const file = fileInput.files?.[0];
  if (!file) return;
  try {
    const r = await api.importSubscriptions(await file.text());
    toast(r.added ? `Added ${r.added} channel${r.added > 1 ? "s" : ""}. Fetching their videos…` : "You already had all of these channels.", 4000);
    if (["subscriptions", "home"].includes(top().route.kind)) refresh();
  } catch (e) {
    toast(message(e), 5000);
  }
});

/** Channels shown before "Show all". */
const CHANNELS_SHOWN = 4;

/** Subscriptions: your channels, then their new videos by day. */
function renderSubscriptions(): HTMLElement {
  const page = el(`
    <div class="page">
      <div class="page-head"><span class="muted" id="sub-count"></span><button class="btn small" data-icon="import">Import</button></div>
      <div class="channels">${spinner()}</div>
      <div class="subs-feed"></div>
    </div>`);
  page.querySelector(".page-head button")!.addEventListener("click", pickTakeout);
  const list = page.querySelector<HTMLElement>(".channels")!;
  void api.subscriptions().then(
    (subs) => {
      page.querySelector("#sub-count")!.textContent = subs.length ? `${subs.length} channel${subs.length > 1 ? "s" : ""}` : "";
      if (!subs.length) {
        list.replaceChildren(welcome());
        return;
      }
      list.classList.add("box");
      const rows = subs.map((c) => channelRow(c, true));
      list.replaceChildren(...rows.slice(0, CHANNELS_SHOWN));
      if (rows.length > CHANNELS_SHOWN) {
        const all = el(`<button class="more-channels">Show all ${subs.length} channels</button>`);
        all.addEventListener("click", () => list.replaceChildren(...rows));
        list.append(all);
      }
      const feed = subsFeed();
      page.querySelector(".subs-feed")!.replaceChildren(feed);
      (page as any).check = (feed as any).check;
    },
    (e) => list.replaceChildren(errorBox(message(e), refresh)),
  );
  return page;
}

// ---- channel page ----------------------------------------------------------

function renderChannel(id: string, known?: Channel): HTMLElement {
  const page = el(`<div class="page"><div class="channel-head">${known ? "" : spinner()}</div><div class="list"></div><div class="sentinel"></div></div>`);
  const head = page.querySelector<HTMLElement>(".channel-head")!;
  const list = page.querySelector<HTMLElement>(".list")!;
  const drawHead = (c: Channel, description: string, subscribed?: boolean) => {
    head.replaceChildren(
      el(`
        <div class="channel-hero">
          ${c.avatar ? `<img class="avatar huge" alt="" src="${esc(avatarSrc(c.avatar))}" />` : `<span class="avatar huge blank">${esc(c.name.slice(0, 1))}</span>`}
          <h2>${esc(c.name)}</h2>
          <p class="muted">${[c.handle, c.subscribers].filter(Boolean).map(esc).join(" · ")}</p>
          ${description ? `<p class="channel-desc">${esc(description)}</p>` : ""}
        </div>`),
    );
    if (subscribed !== undefined) head.firstElementChild!.append(subscribeButton(c, subscribed));
    head.querySelector(".channel-desc")?.addEventListener("click", (e) => (e.currentTarget as HTMLElement).classList.toggle("open"));
  };
  if (known) drawHead(known, "");
  let channel: Channel | undefined;
  let next: string | null | undefined;
  infinite(page.querySelector(".sentinel")!, viewEl, async () => {
    if (next === undefined) {
      const view = await api.channel(id);
      channel = view.channel;
      drawHead(view.channel, view.description, view.subscribed);
      if (top().route.kind === "channel") {
        (top().route as { channel?: Channel }).channel = view.channel;
        drawTopbar();
      }
      if (!view.videos.items.length) list.append(el(`<div class="empty"><p>This channel has no videos.</p></div>`));
      appendVideos(list, view.videos.items);
      next = view.videos.next;
    } else if (next) {
      const more = await api.more("channel", next, channel);
      appendVideos(list, more.items);
      next = more.next;
    }
    return !!next;
  });
  return page;
}

// ---- playlists -------------------------------------------------------------

function playlistRow(p: Playlist): HTMLElement {
  const row = el(`
    <div class="playlist-item">
      <div class="thumb stack">${p.thumbnail ? `<img loading="lazy" alt="" src="${esc(p.thumbnail)}" />` : ""}<span class="badge">${icon("playlist")}${esc(p.count ?? "")}</span></div>
      <div class="channel-text"><b>${esc(p.title)}</b><small>${esc([p.channel, "Playlist"].filter(Boolean).join(" · "))}</small></div>
    </div>`);
  row.addEventListener("click", () => navigate({ kind: "playlist", id: p.id, playlist: p }));
  return row;
}

/** Plays `videos` in order (or shuffled), the rest queued after the first. */
function playAll(videos: Video[], shuffle: boolean) {
  if (!videos.length) return;
  const list = [...videos];
  if (shuffle) {
    for (let i = list.length - 1; i > 0; i--) {
      const j = Math.floor(Math.random() * (i + 1));
      [list[i], list[j]] = [list[j], list[i]];
    }
  }
  queue = list.slice(1);
  saveQueue();
  void openVideo(list[0]);
}

function renderPlaylist(id: string, known?: Playlist): HTMLElement {
  const page = el(`<div class="page"><div class="playlist-head">${spinner()}</div><div class="list compact-list"></div><div class="sentinel"></div></div>`);
  const head = page.querySelector<HTMLElement>(".playlist-head")!;
  const list = page.querySelector<HTMLElement>(".list")!;
  const videos: Video[] = [];
  let next: string | null | undefined;
  const drawHead = (p: Playlist, saved: boolean) => {
    head.replaceChildren(
      el(`
        <div class="playlist-hero">
          ${p.thumbnail ? `<img class="hero-thumb" alt="" src="${esc(p.thumbnail)}" />` : ""}
          <h2>${esc(p.title)}</h2>
          <p class="muted">${[p.channel, p.count].filter(Boolean).map(esc).join(" · ")}</p>
          <div class="hero-actions">
            <button class="btn primary" data-icon="play">Play all</button>
            <button class="btn" data-icon="shuffle">Shuffle</button>
            <button class="btn icon-only${saved ? " on" : ""}" title="Save to Library" data-icon="bookmark"></button>
          </div>
        </div>`),
    );
    const [all, mix, keep] = head.querySelectorAll("button");
    all.addEventListener("click", () => playAll(videos, false));
    mix.addEventListener("click", () => playAll(videos, true));
    keep.addEventListener("click", async () => {
      saved = !saved;
      try {
        await api.playlistSave(p, saved);
        keep.classList.toggle("on", saved);
        toast(saved ? "Saved to your Library" : "Removed from your Library");
      } catch (e) {
        toast(message(e));
      }
    });
  };
  if (known) drawHead(known, false);
  infinite(page.querySelector(".sentinel")!, viewEl, async () => {
    if (next === undefined) {
      const view = await api.playlist(id);
      drawHead({ ...view.playlist, thumbnail: view.playlist.thumbnail ?? known?.thumbnail }, view.saved);
      const route = top().route;
      if (route.kind === "playlist" && route.id === id) {
        route.playlist = view.playlist;
        drawTopbar();
      }
      if (!view.videos.items.length) list.append(el(`<div class="empty"><p>This playlist is empty.</p></div>`));
      videos.push(...view.videos.items);
      appendVideos(list, view.videos.items, { compact: true });
      next = view.videos.next;
    } else if (next) {
      const more = await api.more("channel", next);
      videos.push(...more.items);
      appendVideos(list, more.items, { compact: true });
      next = more.next;
    }
    return !!next;
  });
  return page;
}

// ---- shared links ----------------------------------------------------------

/** Opens a YouTube link: shared to Vista, tapped elsewhere, or pasted into search. */
async function openLink(text: string) {
  try {
    const link = await api.resolveLink(text);
    if (link.kind === "channel") return openChannel(link.id);
    if (link.kind === "playlist") {
      minimizeWatch();
      return navigate({ kind: "playlist", id: link.id });
    }
    // The video page has the title and channel; the player wants them from the start.
    const page = await api.watchInfo(link.id).catch(() => null);
    const video: Video = {
      id: link.id,
      title: page?.title ?? "",
      channel: page?.channel.name ?? "",
      channelId: page?.channel.id,
      avatar: page?.channel.avatar,
      thumbnail: `https://i.ytimg.com/vi/${link.id}/hqdefault.jpg`,
    };
    await openVideo(video, link.start ?? undefined);
  } catch (e) {
    toast(message(e), 4000);
  }
}
(window as any).__vistaOpen = (text: string) => void openLink(text);

// ---- library ---------------------------------------------------------------

function historyExtra(w: Watched): SheetItem[] {
  return [
    {
      label: "Remove from history",
      icon: "delete",
      run: async () => {
        await api.historyRemove(w.video.id).catch((e) => toast(message(e)));
        refresh();
      },
    },
  ];
}

function renderLibrary(): HTMLElement {
  const page = el(`
    <div class="page">
      <div class="section-head"><h2 class="section">History</h2><button class="text-btn" data-go="history">See all</button></div>
      <div class="shelf" id="lib-history">${spinner()}</div>
      <div class="tiles">
        <button class="tile" data-go="later">${icon("later")}<span><b>Watch later</b><small id="lib-later"></small></span></button>
        <button class="tile" data-go="downloads">${icon("download")}<span><b>Downloads</b><small id="lib-downloads"></small></span></button>
      </div>
      <div id="lib-playlists" hidden>
        <h2 class="section">Playlists</h2>
        <div class="channels"></div>
      </div>
      <div class="menu-list">
        <button class="menu-row" data-go="settings">${icon("settings")}Settings${icon("expand")}</button>
      </div>
      <p class="about" id="about"></p>
    </div>`);
  page.querySelectorAll<HTMLElement>("[data-go]").forEach((b) => b.addEventListener("click", () => navigate({ kind: b.dataset.go as "history" | "later" | "settings" | "downloads" })));
  const shelf = (id: string, videos: Video[], empty: string, extra?: (i: number) => SheetItem[]) => {
    const box = page.querySelector<HTMLElement>(id)!;
    if (!videos.length) {
      box.replaceChildren(el(`<p class="muted shelf-empty">${esc(empty)}</p>`));
      return;
    }
    box.replaceChildren();
    videos.slice(0, 12).forEach((v, i) => box.append(videoCard(v, { compact: true, extra: extra?.(i) })));
    void markProgress(box, videos.slice(0, 12));
  };
  void api.history().then(
    (h) => shelf("#lib-history", h.map((w) => w.video), "Videos you watch show up here.", (i) => historyExtra(h[i])),
    (e) => toast(message(e)),
  );
  const videos = (n: number) => `${n} video${n === 1 ? "" : "s"}`;
  void api.later().then(
    (l) => (page.querySelector("#lib-later")!.textContent = videos(l.length)),
    (e) => toast(message(e)),
  );
  void api.savedPlaylists().then((lists) => {
    const box = page.querySelector<HTMLElement>("#lib-playlists")!;
    box.hidden = !lists.length;
    box.querySelector(".channels")!.replaceChildren(...lists.map(playlistRow));
  });
  void api.downloads().then((d) => {
    const done = d.filter((x) => x.done);
    page.querySelector("#lib-downloads")!.textContent = d.length ? `${videos(done.length)} · ${fmtSize(done.reduce((n, x) => n + x.size, 0))}` : "Nothing yet";
  });
  void Promise.all([getVersion(), api.ytdlpStatus()]).then(([version, s]) => {
    page.querySelector("#about")!.textContent = `Vista ${version} · yt-dlp ${s.version ?? "not installed"}`;
  });
  return page;
}

function renderDownloads(): HTMLElement {
  const page = el(`<div class="page"><div class="box quality-box"></div><h2 class="label"></h2><div class="list compact-list downloads">${spinner()}</div></div>`);
  page.querySelector(".quality-box")!.append(downloadQualityRow());
  const list = page.querySelector<HTMLElement>(".list")!;
  void api.downloads().then(
    (items) => {
      list.replaceChildren();
      if (!items.length) {
        page.querySelector(".label")!.remove();
        const empty = el(`
          <div class="empty-big">
            <div class="empty-icon">${icon("download")}</div>
            <h2>Nothing downloaded yet</h2>
            <p>Tap Download under any video, or pick Download from its ⋮ menu, to watch it without a connection.</p>
            <button class="btn primary">Browse videos</button>
          </div>`);
        empty.querySelector("button")!.addEventListener("click", () => switchTab("home"));
        list.replaceWith(empty);
        return;
      }
      page.querySelector(".label")!.textContent = `${items.length} video${items.length === 1 ? "" : "s"}`;
      for (const d of items) {
        const card = videoCard(offlineVideo(d), {
          compact: true,
          extra: [
            ...(d.error || d.missing ? [{ label: "Try again", icon: "refresh" as IconName, run: () => void download(d.video) }] : []),
            { label: d.done ? "Remove download" : "Cancel download", icon: "delete", danger: true, run: () => void removeDownload(d.video) },
          ],
        });
        const status = d.done
          ? d.missing
            ? "Files missing: try again"
            : `${d.quality}p · ${fmtSize(d.size)}`
          : d.error
            ? `Failed: ${d.error}`
            : d.active
              ? "Downloading…"
              : "Waiting";
        card.querySelector(".card-text")!.append(el(`<p class="download-status${d.error ? " failed" : ""}" data-download="${esc(d.video.id)}">${esc(status)}</p>`));
        list.append(card);
      }
    },
    (e) => list.replaceChildren(errorBox(message(e), refresh)),
  );
  return page;
}

/** "23 min left", or "Watched" near the end. */
function watchState(w: Watched): string | null {
  const duration = w.duration ?? w.video.duration;
  if (!duration || w.position < 5) return null;
  const left = duration - w.position;
  return left < 30 ? "Watched" : `${Math.max(1, Math.round(left / 60))} min left`;
}

function renderHistory(): HTMLElement {
  const page = el(`<div class="page"><div class="page-head"><span></span><button class="btn small" data-icon="delete">Clear all</button></div><div class="history">${spinner()}</div></div>`);
  const box = page.querySelector<HTMLElement>(".history")!;
  page.querySelector(".page-head button")!.addEventListener("click", () =>
    confirmSheet("Clear your whole watch history?", "Clear history", async () => {
      await api.historyRemove().catch((e) => toast(message(e)));
      refresh();
    }),
  );
  void api.history().then(
    (h) => {
      box.replaceChildren();
      if (!h.length) box.append(el(`<div class="empty"><p>Videos you watch show up here.</p></div>`));
      for (const [name, group] of dayGroups(h, (w) => w.watched)) {
        const list = el(`<div class="list compact-list"></div>`);
        for (const w of group) {
          const card = videoCard(w.video, { compact: true, extra: historyExtra(w) });
          const state = watchState(w);
          if (state) card.querySelector(".card-text")!.append(el(`<p class="watch-state">${esc(state)}</p>`));
          list.append(card);
        }
        box.append(el(`<h2 class="label">${esc(name)}</h2>`), list);
        void markProgress(list, group.map((w) => w.video));
      }
    },
    (e) => box.replaceChildren(errorBox(message(e), refresh)),
  );
  return page;
}

function renderLater(): HTMLElement {
  const page = el(`<div class="page"><div class="list compact-list">${spinner()}</div></div>`);
  const list = page.querySelector<HTMLElement>(".list")!;
  void api.later().then(
    (videos) => {
      list.replaceChildren();
      if (!videos.length) list.append(el(`<div class="empty"><p>Save videos from their ⋮ menu to watch them later.</p></div>`));
      appendVideos(list, videos, { compact: true });
    },
    (e) => list.replaceChildren(errorBox(message(e), refresh)),
  );
  return page;
}

// ---- settings --------------------------------------------------------------

const QUALITIES = [2160, 1440, 1080, 720, 480, 360];

function toggleRow(label: string, hint: string, on: boolean, change: (on: boolean) => void): HTMLElement {
  const row = el(`<label class="setting"><span><b>${esc(label)}</b><small>${esc(hint)}</small></span><input type="checkbox" class="switch" ${on ? "checked" : ""} /></label>`);
  const input = row.querySelector("input")!;
  input.addEventListener("change", () => change(input.checked));
  return row;
}

/** A pick-one setting: its name, what it means now, and the choice in a pill. */
function qualityRow(title: string, get: () => number, set: (q: number) => void, hint: (q: number) => string | undefined): HTMLElement {
  const row = el(`<button class="setting"><span><b>${esc(title)}</b><small></small></span><span class="value"><i></i>${icon("expand")}</span></button>`);
  const draw = () => {
    row.querySelector("small")!.textContent = `${get()}p, or the closest below it`;
    row.querySelector(".value i")!.textContent = `${get()}p`;
  };
  draw();
  row.addEventListener("click", () =>
    sheet(
      title,
      QUALITIES.map((q) => ({
        label: `${q}p`,
        hint: hint(q),
        checked: get() === q,
        run: () => {
          set(q);
          draw();
        },
      })),
    ),
  );
  return row;
}

function downloadQualityRow(): HTMLElement {
  return qualityRow(
    "Download quality",
    () => settings.downloadQuality,
    (q) => save({ downloadQuality: q }),
    (q) => (q >= 1080 ? "Big files" : q <= 480 ? "Small files" : undefined),
  );
}

function renderSettings(): HTMLElement {
  const page = el(`<div class="page settings"></div>`);
  const group = (title: string) => {
    page.append(el(`<h2 class="label">${esc(title)}</h2>`));
    const g = el(`<div class="box"></div>`);
    page.append(g);
    return g;
  };

  const playback = group("Playback");
  playback.append(
    qualityRow(
      "Preferred quality",
      () => settings.quality,
      (q) => save({ quality: q }),
      (q) => (q > 1080 ? "Uses more data and battery" : q <= 480 ? "Saves data" : undefined),
    ),
    toggleRow("Prepare videos ahead", "Starts videos sooner by getting ready for the ones you're likely to open next. Uses a little more battery and data.", settings.prefetch, (on) =>
      save({ prefetch: on }),
    ),
    toggleRow("Autoplay", "When a video ends and the queue is empty, play the first related video", settings.autoplay, (on) => save({ autoplay: on })),
  );
  if (isAndroid) {
    playback.append(
      toggleRow("Background play", "Keep playing when you leave the app or turn the screen off", settings.background, (on) => {
        save({ background: on });
        player.applySettings();
      }),
      toggleRow("Picture-in-picture", "Going home while a video plays shrinks it into a small window", settings.pip, (on) => {
        save({ pip: on });
        player.applySettings();
      }),
    );
  }

  const feed = group("Subscriptions");
  feed.append(
    toggleRow("Show Shorts", "Include Shorts from your subscriptions", settings.shorts, (on) => {
      save({ shorts: on });
      void watchUploads();
    }),
  );
  if (isAndroid) {
    feed.append(
      toggleRow("New video notifications", "Checks your subscriptions every few hours, even with Vista closed", settings.newVideos, async (on) => {
        save({ newVideos: on, askedNotifications: true });
        await watchUploads();
        if (on) {
          // Asks Android again if needed; past a second no, it only answers, so say where to allow it.
          const allowed = await api.notificationsAllow().catch(() => true);
          if (!allowed) toast("Notifications are off for Vista. Allow them in Android's app settings.", 5000);
        }
      }),
    );
  }

  const skip = group("SponsorBlock: skip these");
  for (const c of SKIP_CATEGORIES) {
    skip.append(
      toggleRow(c.label, c.hint, settings.skip.includes(c.id), (on) => {
        save({ skip: on ? [...settings.skip, c.id] : settings.skip.filter((s) => s !== c.id) });
      }),
    );
  }
  page.append(el(`<p class="note">Skips use segments marked by SponsorBlock's community. Vista asks for them without telling SponsorBlock which video you're watching.</p>`));

  group("Downloads").append(
    downloadQualityRow(),
    toggleRow("Download Watch later", "Videos you save to Watch later download by themselves on Wi-Fi", settings.downloadLater, (on) => {
      save({ downloadLater: on });
      void downloadLater();
    }),
  );

  if (isAndroid) {
    const check = el(`<button class="setting"><span><b>Check for updates</b><small></small></span></button>`);
    const hint = check.querySelector("small")!;
    void getVersion().then((v) => (hint.textContent = `You have Vista ${v}`));
    check.addEventListener("click", async () => {
      hint.textContent = "Checking…";
      try {
        const c = await api.appUpdateCheck();
        hint.textContent = c.update ? `Vista ${c.update.version} is out` : `Vista ${c.current} is the newest`;
        if (c.update) updateSheet(c.update);
      } catch (e) {
        hint.textContent = message(e);
      }
    });
    group("App").append(check);
  }

  const backups = group("Backup");
  const backupNow = el(`<button class="setting"><span><b>Back up now</b><small>Saves your subscriptions, history, Watch later, playlists and settings to ${BACKUP_PLACE}</small></span></button>`);
  backupNow.addEventListener("click", () => void backup(false));
  const restoreRow = el(`<button class="setting"><span><b>Restore from a backup…</b><small>Replaces what's in Vista now; a copy of it is kept first</small></span></button>`);
  restoreRow.addEventListener("click", pickBackup);
  backups.append(
    toggleRow("Back up automatically", "Once a week, when Vista opens. The newest four are kept.", settings.autoBackup, (on) => save({ autoBackup: on })),
    backupNow,
    restoreRow,
  );

  const data = group("Your data");
  const clear = el(`<button class="setting danger"><span><b>Clear watch history</b><small>Also forgets where you stopped in each video</small></span></button>`);
  clear.addEventListener("click", () =>
    confirmSheet("Clear your whole watch history?", "Clear history", async () => {
      await api.historyRemove().catch((e) => toast(message(e)));
      toast("History cleared");
    }),
  );
  const clearSearches = el(`<button class="setting danger"><span><b>Clear search history</b><small>Your searches help pick your recommendations</small></span></button>`);
  clearSearches.addEventListener("click", () =>
    confirmSheet("Clear your search history?", "Clear searches", async () => {
      await api.searchesClear().catch((e) => toast(message(e)));
      toast("Search history cleared");
    }),
  );
  const hiddenRow = el(`<button class="setting"><span><b>Not interested</b><small>Videos and channels kept out of your recommendations</small></span></button>`);
  hiddenRow.addEventListener("click", () => void hiddenSheet());
  data.append(hiddenRow, clear, clearSearches);
  const about = el(`<p class="about"></p>`);
  page.append(about);
  void Promise.all([getVersion(), api.ytdlpStatus()]).then(([version, s]) => {
    about.textContent = `Vista ${version} · yt-dlp ${s.version ?? "not installed"}`;
  });
  return page;
}

// ---- app updates --------------------------------------------------------------

/** Release notes: plain text with **bold** and "- " bullets, as GitHub has them. */
function notesHtml(notes: string): string {
  const bold = (s: string) => esc(s).replace(/\*\*(.+?)\*\*/g, "<b>$1</b>");
  const out: string[] = [];
  let list: string[] = [];
  const flush = () => {
    if (list.length) out.push(`<ul>${list.map((l) => `<li>${bold(l)}</li>`).join("")}</ul>`);
    list = [];
  };
  for (const line of notes.split(/\r?\n/)) {
    const item = line.match(/^\s*[-*]\s+(.*)/);
    if (item) list.push(item[1]);
    else {
      flush();
      if (line.trim()) out.push(`<p>${bold(line.trim().replace(/^#+\s*/, ""))}</p>`);
    }
  }
  flush();
  return out.join("");
}

/** The sheet offering a newer version, with what's new in it. */
function updateSheet(r: AppRelease) {
  sheet(`Vista ${r.version} is out`, [
    { label: "Update now", icon: "download", hint: r.size ? `${fmtSize(r.size)} download` : undefined, run: () => void installUpdate() },
    { label: "Later", icon: "close", hint: "Settings → App can update any time", run: () => {} },
  ]);
  if (r.notes) sheetRoot.querySelector(".sheet-title")?.insertAdjacentElement("afterend", el(`<div class="sheet-notes">${notesHtml(r.notes)}</div>`));
}

let updating = false;

async function installUpdate() {
  if (updating) return;
  updating = true;
  toast("Downloading the update…", 60_000);
  try {
    const opened = await api.appUpdateInstall();
    if (opened) toast("Tap Update in Android's installer", 5000);
    else toast("Allow Vista to install apps on the screen that opened, then tap Update again.", 8000);
  } catch (e) {
    toast(message(e), 5000);
  } finally {
    updating = false;
  }
}

void listen<{ downloaded: number; total: number }>("update-progress", ({ payload: p }) => {
  if (!updating || !p.total) return;
  toast(`Downloading the update… ${Math.floor((p.downloaded / p.total) * 100)}%`, 60_000);
});

/** On launch: a newer version is offered once; Settings → App has it any time after. */
async function offerUpdate() {
  if (!isAndroid) return;
  const c = await api.appUpdateCheck().catch(() => null);
  if (!c?.update || settings.updateOffered === c.update.version) return;
  save({ updateOffered: c.update.version });
  updateSheet(c.update);
}

// ---- backups ----------------------------------------------------------------

/** Where backups go (backup_save), as people find it. */
const BACKUP_PLACE = isAndroid ? "Downloads → Vista backups" : "Documents → Vista backups";
const AUTO_BACKUP_SECS = 7 * 86400;

function dateStamp(d = new Date()): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** Saves a backup; automatic ones are named so only the newest few are kept. */
async function backup(auto: boolean) {
  const name = `${auto ? "Vista auto backup" : "Vista backup"} ${dateStamp()}.vista-backup`;
  try {
    const place = await api.backupSave(JSON.stringify(settings), name, auto);
    if (auto) save({ lastAutoBackup: Math.floor(Date.now() / 1000) });
    else toast(`Backup saved in ${place.replace(/\//g, " → ")}`, 4000);
  } catch (e) {
    if (!auto) toast(`Couldn't save the backup: ${message(e)}`, 5000);
  }
}

/** Once a week, when Vista opens or comes back, if there's anything to keep. */
async function autoBackup() {
  if (!settings.autoBackup || Date.now() / 1000 - settings.lastAutoBackup < AUTO_BACKUP_SECS) return;
  const [subs, history] = await Promise.all([api.subscriptions().catch(() => []), api.history().catch(() => [])]);
  if (subs.length || history.length) await backup(true);
}

function pickBackup() {
  const input = document.createElement("input");
  input.type = "file";
  input.addEventListener("change", async () => {
    const file = input.files?.[0];
    if (!file) return;
    try {
      const path = await api.backupStage(await file.arrayBuffer());
      const s = await api.backupPeek(path);
      const n = (count: number, one: string, after = "") => (count ? `${count} ${one}${count === 1 ? "" : "s"}${after}` : "");
      const holds = [n(s.subscriptions, "channel"), n(s.history, "watched video"), n(s.later, "video", " in Watch later"), n(s.playlists, "playlist")].filter(Boolean);
      const made = new Date(s.createdAt * 1000).toLocaleDateString(undefined, { day: "numeric", month: "long", year: "numeric" });
      sheet(`Restore the backup from ${made}?`, [
        {
          label: "Restore",
          icon: "history",
          hint: `${holds.length ? holds.join(", ") : "Only settings"}. What's in Vista now is replaced.`,
          run: () => void restoreBackup(path),
        },
        { label: "Cancel", icon: "close", run: () => {} },
      ]);
    } catch (e) {
      toast(message(e), 5000);
    }
  });
  input.click();
}

async function restoreBackup(path: string) {
  try {
    const r = await api.backupRestore(path, JSON.stringify(settings));
    // The backup's settings, but this phone keeps its own bookkeeping.
    const restored = { ...JSON.parse(r.settings), askedNotifications: settings.askedNotifications, lastAutoBackup: settings.lastAutoBackup };
    save(restored);
    toast("Backup restored");
    // Every page shows the restored data after starting over.
    window.setTimeout(() => location.reload(), 800);
  } catch (e) {
    toast(message(e), 5000);
  }
}

// ---- watching ---------------------------------------------------------------

interface Current {
  video: Video;
  token: number;
  info?: PlayInfo;
  page?: WatchPage;
  saved: boolean;
  /** Whether the player already retried with fresh streams after an error. */
  retried: boolean;
  /** The end was handled (next in queue, autoplay), so it happens once. */
  ended: boolean;
}

let current: Current | null = null;
let watchOpen = false;
let token = 0;
let lastSaved = 0;
const SPEEDS = [0.25, 0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];

const CATEGORY_NAMES: Record<string, string> = {
  sponsor: "sponsor",
  selfpromo: "self-promotion",
  interaction: "subscribe reminder",
  intro: "intro",
  outro: "endcard",
  preview: "preview",
  music_offtopic: "non-music part",
  filler: "filler",
};

function expandWatch() {
  if (!current) return;
  closeSearch();
  watchOpen = true;
  watchEl.hidden = false;
  miniEl.hidden = true;
  if (current.info) player.setVisible(true);
}

function minimizeWatch() {
  if (!watchOpen) return;
  watchOpen = false;
  watchEl.hidden = true;
  player.setVisible(false);
  saveProgress();
  drawMini();
}

function closeVideo() {
  saveProgress();
  token++;
  current = null;
  player.stop();
  // Nothing left to pause.
  if (sleepAt || sleepAtEnd) setSleep(null);
  watchOpen = false;
  watchEl.hidden = true;
  miniEl.hidden = true;
}

function saveProgress() {
  if (!current || !current.info || current.info.live) return;
  const { position, duration } = player.state;
  if (position > 0) void api.historyProgress(current.video.id, position, duration ?? current.info.duration).catch(() => {});
}

function drawMini() {
  if (!current || watchOpen) {
    miniEl.hidden = true;
    return;
  }
  miniEl.hidden = false;
  $<HTMLImageElement>("#mini-thumb").src = current.video.thumbnail;
  $("#mini-title").textContent = current.video.title;
  $("#mini-channel").textContent = current.video.channel;
  const b = $("#mini-toggle");
  b.innerHTML = icon(player.state.playing ? "pause" : "play");
  $("#mini-next").hidden = !queue.length;
}

function setOverlay(html: string | null, retry?: () => void) {
  overlayEl.hidden = html === null;
  overlayEl.innerHTML = html ?? "";
  overlayEl.querySelector("button")?.addEventListener("click", retry!);
}

async function openVideo(v: Video, startAt?: number) {
  if (current?.video.id === v.id) {
    expandWatch();
    if (startAt !== undefined) player.seek(startAt);
    return;
  }
  saveProgress();
  closeComments();
  const myToken = ++token;
  current = { video: v, token: myToken, saved: false, retried: false, ended: false };
  player.stop();
  expandWatch();
  watchBody.scrollTop = 0;
  drawWatch(v);
  void api.history(v).catch(() => {});
  let start = 0;
  try {
    const p = (await api.positions([v.id]))[v.id];
    const duration = p?.duration ?? v.duration;
    if (p && p.position > 5 && (!duration || p.position < duration - 15)) start = p.position;
  } catch {
    // Starts from the beginning.
  }
  if (startAt !== undefined) start = startAt;
  void loadWatchInfo(v, myToken);
  void startPlayback(v, start, myToken);
}

async function startPlayback(v: Video, start: number, myToken: number, quality = settings.quality, fresh = false) {
  setOverlay(`<img class="slot-thumb" alt="" src="${esc(sharpThumb(v.thumbnail))}" />${spinner("Getting the video…")}`);
  // The very first video after installing also unpacks yt-dlp.
  const slow = window.setTimeout(() => {
    const text = overlayEl.querySelector(".loading");
    if (text) text.lastChild!.textContent = "Still working… the first video takes longer.";
  }, 9000);
  try {
    const info = await player.start(v, quality, start, slotEl, fresh);
    if (myToken !== token || !current) return;
    current.info = info;
    setOverlay(null);
    if (!watchOpen) player.setVisible(false);
    drawPlayInfo();
    // Get the next video ready while this one plays.
    window.setTimeout(() => {
      if (myToken !== token) return;
      const next = queue[0] ?? (settings.autoplay ? firstWanted(current?.page?.related.items) : undefined);
      if (next) prefetch([next.id]);
    }, 15_000);
  } catch (e) {
    const text = message(e);
    if (text === "superseded" || myToken !== token) return;
    player.setVisible(false);
    setOverlay(`<img class="slot-thumb dim" alt="" src="${esc(v.thumbnail)}" /><div class="slot-error"><p>${esc(text)}</p><button class="btn">Try again</button></div>`, () =>
      void startPlayback(v, start, myToken, quality, true),
    );
  } finally {
    clearTimeout(slow);
  }
}

function drawWatch(v: Video) {
  watchBody.replaceChildren(
    el(`
      <div class="watch-info">
        <h1 class="watch-title">${esc(v.title)}</h1>
        <p class="watch-meta muted">${[v.views, published(v)].filter(Boolean).map(esc).join(" · ")}</p>
        <div class="watch-channel"></div>
        <div class="chips">
          <button class="chip" id="chip-like" data-icon="like"><span>…</span></button>
          <button class="chip" id="chip-dislike" data-icon="dislike"><span>…</span></button>
          <button class="chip" id="chip-save" data-icon="later"><span>Watch later</span></button>
          <button class="chip" id="chip-download" data-icon="download"><span>Download</span></button>
          <button class="chip" id="chip-quality" data-icon="quality" hidden><span></span></button>
          <button class="chip" id="chip-speed" data-icon="speed"><span>1×</span></button>
          <button class="chip" id="chip-share" data-icon="share"><span>Share</span></button>
          <button class="chip" id="chip-sleep" data-icon="sleep"><span>Sleep timer</span></button>
        </div>
        <div class="chapters" hidden></div>
        <div class="desc"><p class="sb-note" hidden></p><div class="desc-text muted">${spinner()}</div><button class="text-btn desc-more" hidden>Show more</button></div>
        <button class="comments-entry" hidden>${icon("comment")}<span>Comments</span>${icon("expand")}</button>
      </div>`),
    el(`<div class="queue" hidden></div>`),
    el(`<div class="section-head"><h2 class="section">Up next</h2><button class="text-btn autoplay"></button></div>`),
    el(`<div class="list compact-list related"></div>`),
    el(`<div class="sentinel"></div>`),
  );
  if (v.channelId) $(".watch-channel").append(channelRow({ id: v.channelId, name: v.channel, avatar: v.avatar }));
  // Autoplay can be switched right where it matters.
  const autoplay = watchBody.querySelector<HTMLElement>(".autoplay")!;
  const drawAutoplay = () => (autoplay.textContent = `Autoplay ${settings.autoplay ? "on" : "off"}`);
  drawAutoplay();
  autoplay.addEventListener("click", () => {
    save({ autoplay: !settings.autoplay });
    drawAutoplay();
    toast(settings.autoplay ? "Autoplay on: a related video plays when the queue is empty" : "Autoplay off");
  });
  $("#chip-speed span").textContent = `${player.state.rate}×`;
  $("#chip-save").addEventListener("click", () => current && void setLater(current.video, !current.saved));
  $("#chip-share").addEventListener("click", () => current && shareSheet(current.video));
  $("#chip-sleep").addEventListener("click", sleepSheet);
  drawSleepChip();
  $("#chip-download").addEventListener("click", () => current && void downloadChip(current.video));
  void drawDownloadChip(v.id);
  drawQueue();
  $("#chip-speed").addEventListener("click", () =>
    sheet(
      "Playback speed",
      SPEEDS.map((s) => ({
        label: s === 1 ? "Normal" : `${s}×`,
        checked: player.state.rate === s,
        run: () => {
          player.setRate(s);
          $("#chip-speed span").textContent = `${s}×`;
        },
      })),
    ),
  );
  $("#chip-quality").addEventListener("click", qualitySheet);
  void api.votes(v.id).then(
    (votes) => {
      if (current?.video.id !== v.id) return;
      if (!current.page?.likes) $("#chip-like span").textContent = fmtCount(votes.likes);
      $("#chip-dislike span").textContent = fmtCount(votes.dislikes);
    },
    () => {
      if (current?.video.id !== v.id) return;
      $("#chip-dislike").hidden = true;
      if (!current.page?.likes) $("#chip-like").hidden = true;
    },
  );
}

async function loadWatchInfo(v: Video, myToken: number) {
  let page: WatchPage;
  try {
    page = await api.watchInfo(v.id);
  } catch (e) {
    if (myToken !== token) return;
    // Offline (a download) or a hiccup: the video can still play.
    watchBody.querySelector(".desc")!.classList.add("open");
    watchBody.querySelector(".desc-text")!.replaceChildren(errorBox(message(e), () => void loadWatchInfo(v, myToken)));
    if ($("#chip-like span").textContent === "…") $("#chip-like").hidden = true;
    return;
  }
  if (myToken !== token || !current) return;
  current.page = page;
  current.saved = page.saved;
  // The page knows more than the list the video came from.
  current.video = {
    ...v,
    title: page.title || v.title,
    channel: page.channel.name || v.channel,
    channelId: page.channel.id || v.channelId,
    avatar: page.channel.avatar ?? v.avatar,
  };
  $(".watch-title").textContent = current.video.title;
  $(".watch-meta").textContent = [page.views, page.date].filter(Boolean).join(" · ");
  if (page.likes) {
    $("#chip-like span").textContent = page.likes;
    $("#chip-like").hidden = false;
  }
  updateSaveChip();
  if (page.channel.id) $(".watch-channel").replaceChildren(channelRow(page.channel, page.subscribed));

  const text = watchBody.querySelector<HTMLElement>(".desc-text")!;
  text.innerHTML = linkify(page.description) || `<span class="faint">No description.</span>`;
  const more = watchBody.querySelector<HTMLElement>(".desc-more")!;
  const desc = watchBody.querySelector<HTMLElement>(".desc")!;
  requestAnimationFrame(() => {
    more.hidden = text.scrollHeight <= text.clientHeight + 4;
  });
  const toggle = () => {
    desc.classList.toggle("open");
    more.textContent = desc.classList.contains("open") ? "Show less" : "Show more";
  };
  more.addEventListener("click", toggle);
  $(".watch-title").addEventListener("click", () => !more.hidden && toggle());

  const entry = watchBody.querySelector<HTMLElement>(".comments-entry")!;
  entry.hidden = !page.comments;
  entry.addEventListener("click", () => openComments(page.comments!));

  const related = watchBody.querySelector<HTMLElement>(".related")!;
  appendVideos(related, page.related.items, { compact: true });
  let next = page.related.next;
  const sentinel = watchBody.querySelector<HTMLElement>(".sentinel")!;
  if (!next) sentinel.remove();
  else
    infinite(sentinel, watchBody, async () => {
      if (!next || myToken !== token) return false;
      const p = await api.more("related", next);
      appendVideos(related, p.items.filter((x) => x.id !== v.id), { compact: true });
      next = p.next;
      return !!next;
    });
}

/** Timestamps in descriptions jump there; everything else stays plain text. */
function linkify(text: string): string {
  return esc(text).replace(/(^|[\s(])((?:\d{1,2}:)?\d{1,2}:\d{2})(?=$|[\s),.\-–])/gm, (_, pre: string, t: string) => {
    const secs = t.split(":").reduce((acc, p) => acc * 60 + Number(p), 0);
    return `${pre}<a class="ts" data-seek="${secs}">${t}</a>`;
  });
}

watchBody.addEventListener("click", (e) => {
  const ts = (e.target as HTMLElement).closest<HTMLElement>("[data-seek]");
  if (!ts) return;
  e.stopPropagation();
  player.seek(Number(ts.dataset.seek));
  player.play();
});

async function drawDownloadChip(id: string) {
  const d = (await api.downloads().catch(() => [] as DownloadItem[])).find((x) => x.video.id === id);
  const chip = document.querySelector<HTMLElement>("#chip-download");
  if (!chip || current?.video.id !== id) return;
  const [name, label] = !d ? (["download", "Download"] as const) : d.done && !d.missing ? (["downloaded", "Downloaded"] as const) : (["download", d.error ? "Retry download" : "Downloading…"] as const);
  chip.innerHTML = `${icon(name)}<span>${label}</span>`;
  chip.classList.toggle("on", !!d?.done && !d.missing);
}

async function downloadChip(v: Video) {
  const d = (await api.downloads().catch(() => [] as DownloadItem[])).find((x) => x.video.id === v.id);
  if (!d || d.error || d.missing) return void download(v);
  confirmSheet(d.done ? "Remove this download?" : "Stop this download?", d.done ? "Remove download" : "Stop download", () => void removeDownload(v));
}

// ---- comments --------------------------------------------------------------

let commentsPanel: HTMLElement | null = null;

function commentEl(c: Comment): HTMLElement {
  const box = el(`
    <div class="comment">
      ${c.avatar ? `<img class="avatar" loading="lazy" alt="" src="${esc(avatarSrc(c.avatar))}" />` : `<span class="avatar blank">${esc(c.author.replace("@", "").slice(0, 1))}</span>`}
      <div class="comment-body">
        <p class="comment-head"><b class="${c.creator ? "creator" : ""}">${esc(c.author)}</b> · ${esc(c.published)}</p>
        <p class="comment-text">${linkify(c.text)}</p>
        <p class="comment-foot muted">${c.likes ? `${icon("like")}${esc(c.likes)}` : ""}${c.replies ? `<button class="text-btn replies">${esc(c.replyCount ?? "")} ${c.replyCount === "1" ? "reply" : "replies"}</button>` : ""}</p>
        <div class="replies-list"></div>
      </div>
    </div>`);
  const repliesBtn = box.querySelector<HTMLElement>(".replies");
  repliesBtn?.addEventListener("click", async () => {
    const list = box.querySelector<HTMLElement>(".replies-list")!;
    if (list.childElementCount) {
      list.replaceChildren();
      return;
    }
    list.innerHTML = spinner();
    let token: string | null = c.replies;
    list.replaceChildren();
    // Replies come a few at a time; fetch them all (threads rarely run long).
    for (let i = 0; token && i < 10; i++) {
      try {
        const part = await api.comments(token);
        list.append(...part.items.map(commentEl));
        token = part.next;
      } catch (e) {
        list.append(errorBox(message(e)));
        break;
      }
    }
  });
  return box;
}

function openComments(token: string) {
  closeComments();
  const panel = el(`
    <div class="comments-panel">
      <header class="comments-head"><h2>Comments</h2><span class="muted"></span><span class="spacer"></span><button class="icon-btn" title="Close" data-icon="close"></button></header>
      <div class="comments-list"></div>
      <div class="sentinel"></div>
    </div>`);
  // Below the player, which stays visible.
  panel.style.top = `${slotEl.offsetHeight}px`;
  panel.querySelector(".comments-head button")!.addEventListener("click", closeComments);
  panel.addEventListener("click", (e) => {
    const ts = (e.target as HTMLElement).closest<HTMLElement>("[data-seek]");
    if (ts) {
      player.seek(Number(ts.dataset.seek));
      player.play();
    }
  });
  const list = panel.querySelector<HTMLElement>(".comments-list")!;
  let next: string | null = token;
  infinite(panel.querySelector(".sentinel")!, list.parentElement!, async () => {
    if (!next) return false;
    const part = await api.comments(next);
    if (part.count) panel.querySelector(".comments-head .muted")!.textContent = part.count.replace(/ Comments?$/, "");
    if (!part.items.length && !list.childElementCount) list.append(el(`<div class="empty"><p>No comments yet.</p></div>`));
    list.append(...part.items.map(commentEl));
    next = part.next;
    return !!next;
  });
  watchEl.append(panel);
  commentsPanel = panel;
}

function closeComments(): boolean {
  if (!commentsPanel) return false;
  commentsPanel.remove();
  commentsPanel = null;
  return true;
}

function updateSaveChip() {
  const chip = document.querySelector<HTMLElement>("#chip-save");
  if (!chip || !current) return;
  chip.classList.toggle("on", current.saved);
  chip.querySelector("span")!.textContent = current.saved ? "Saved" : "Watch later";
}

function drawPlayInfo() {
  const info = current?.info;
  if (!info) return;
  const q = $("#chip-quality");
  q.hidden = !info.quality || info.qualities.length < 2;
  q.querySelector("span")!.textContent = info.quality ? `${info.quality}p` : "";
  const note = watchBody.querySelector<HTMLElement>(".sb-note")!;
  const notes = [
    info.offline ? `${icon("downloaded")} Playing your download` : "",
    info.segments.length ? `${icon("skip")} SponsorBlock will skip ${info.segments.length} part${info.segments.length > 1 ? "s" : ""} of this video.` : "",
  ].filter(Boolean);
  note.hidden = !notes.length;
  note.innerHTML = notes.join("<br />");
  const chapters = watchBody.querySelector<HTMLElement>(".chapters")!;
  chapters.hidden = info.chapters.length < 2;
  chapters.replaceChildren(
    ...info.chapters.map((c) => el(`<button class="chapter" data-seek="${c.start}"><b>${fmtTime(c.start)}</b><span>${esc(c.title)}</span></button>`)),
  );
}

function qualitySheet() {
  const c = current;
  if (!c?.info) return;
  sheet(
    "Quality for this video",
    c.info.qualities.map((q) => ({
      label: `${q}p`,
      hint: q === settings.quality ? "Your preferred quality" : undefined,
      checked: c.info!.quality === q,
      run: () => void startPlayback(c.video, player.state.position, c.token, q),
    })),
  );
}

/** Highlights the chapter being watched. */
function markChapter(position: number) {
  const buttons = watchBody.querySelectorAll<HTMLElement>(".chapter");
  if (!buttons.length) return;
  let active: HTMLElement | null = null;
  buttons.forEach((b) => {
    if (Number(b.dataset.seek) <= position + 0.5) active = b;
  });
  buttons.forEach((b) => b.classList.toggle("active", b === active));
}

player.on((e) => {
  if (!current) return;
  switch (e.type) {
    case "state":
      drawMini();
      if (!e.state.playing) saveProgress();
      if (e.state.ended && current.info && !current.ended) {
        current.ended = true;
        if (!current.info.live) void api.historyProgress(current.video.id, e.state.duration ?? e.state.position, e.state.duration).catch(() => {});
        // The sleep timer said this video is the last one.
        if (sleepAtEnd) {
          setSleep(null);
          toast("Sleep timer: stopped after this video");
          break;
        }
        // Next in the queue, else (with Autoplay on) the first related video.
        const related = firstWanted(current.page?.related.items);
        if (!playNext() && settings.autoplay && related) void openVideo(related);
      }
      drawSleepChip();
      break;
    case "time": {
      const now = Date.now();
      if (now - lastSaved > 10_000) {
        lastSaved = now;
        saveProgress();
      }
      const d = e.duration ?? current.info?.duration;
      $("#mini-progress").style.width = d ? `${(e.position / d) * 100}%` : "0";
      markChapter(e.position);
      drawSleepChip();
      break;
    }
    case "skip":
      toast(`Skipped ${CATEGORY_NAMES[e.category] ?? "a segment"}`, 2000);
      break;
    case "error": {
      const c = current;
      // Stream URLs expire; get fresh ones once and carry on from the same spot.
      if (!c.retried) {
        c.retried = true;
        void startPlayback(c.video, e.position || player.state.position, c.token, c.info?.quality ?? settings.quality, true);
      } else {
        player.setVisible(false);
        setOverlay(`<div class="slot-error"><p>The video stopped: ${esc(e.message)}</p><button class="btn">Try again</button></div>`, () => {
          c.retried = false;
          void startPlayback(c.video, player.state.position, c.token, c.info?.quality ?? settings.quality, true);
        });
      }
      break;
    }
  }
});

$("#mini").addEventListener("click", (e) => {
  const target = e.target as HTMLElement;
  if (target.closest("#mini-toggle")) player.toggle();
  else if (target.closest("#mini-next")) playNext();
  else if (target.closest("#mini-close")) closeVideo();
  else expandWatch();
});

// ---- startup ---------------------------------------------------------------

$("#back").addEventListener("click", () => {
  if (!searchForm.hidden && top().route.kind !== "search") {
    closeSearch();
    drawTopbar();
  } else back();
});
$("#search-open").addEventListener("click", openSearch);
searchInput.addEventListener("blur", () => {
  // Leaving an empty search box closes it again.
  if (!searchInput.value.trim() && top().route.kind !== "search") {
    closeSearch();
    drawTopbar();
  }
});
document.querySelectorAll<HTMLElement>(".tab").forEach((t) => t.addEventListener("click", () => switchTab(t.dataset.tab as Tab)));

document.addEventListener("keydown", (e) => {
  const typing = (e.target as HTMLElement).closest("input, textarea");
  if (e.key === "Escape" || (e.altKey && e.key === "ArrowLeft")) {
    e.preventDefault();
    back();
  } else if (e.key === " " && !typing && current) {
    e.preventDefault();
    player.toggle();
  } else if (e.key === "/" && !typing) {
    e.preventDefault();
    openSearch();
  }
});
// The mouse's back button.
document.addEventListener("mouseup", (e) => {
  if (e.button === 3) back();
});

// After subscribing, unsubscribing or an import.
void listen("feed-changed", () => {
  if (stack[0].route.kind === "subscriptions" && stack.length === 1) refresh();
  else if (stack[0].route.kind === "subscriptions") stack[0].el = renderSubscriptions();
  void watchUploads();
});
void listen("downloads-changed", () => {
  if (top().route.kind === "downloads" || top().route.kind === "library") refresh();
  if (current) void drawDownloadChip(current.video.id);
});
void listen<{ id: string; done: number; total: number }>("download-progress", ({ payload: p }) => {
  const status = document.querySelector<HTMLElement>(`[data-download="${CSS.escape(p.id)}"]`);
  if (status) status.textContent = p.total ? `Downloading… ${Math.floor((p.done / p.total) * 100)}% of ${fmtSize(p.total)}` : `Downloading… ${fmtSize(p.done)}`;
});
void listen("subscriptions-changed", () => {
  if (top().route.kind === "subscriptions") refresh();
});

applyIcons();
switchTab("home");
player.applySettings();
void watchUploads();
void autoBackup();
void checkNetwork().then(downloadLater);
void offerUpdate();
document.addEventListener("visibilitychange", () => {
  if (document.hidden) return;
  void autoBackup();
  void checkNetwork().then(downloadLater);
});
(navigator as any).connection?.addEventListener?.("change", () => void checkNetwork());

// Desktop needs yt-dlp downloaded once; phones have it built in.
void api.ytdlpStatus().then(async (s) => {
  if (s.installed || isAndroid) return;
  toast("Downloading yt-dlp, which Vista needs to play videos…", 60_000);
  try {
    const done = await api.ytdlpInstall();
    toast(`yt-dlp ${done.version ?? ""} is ready`);
  } catch (e) {
    toast(`Couldn't download yt-dlp: ${message(e)}`, 8000);
  }
});
