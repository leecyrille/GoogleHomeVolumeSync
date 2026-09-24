import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save, open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";

const DONATE_URL = "https://pactotech.com/products/google-home-volume-sync-tip-jar";
const SITE_URL = "https://googlehomevolumesync.com";
const REPO_URL = "https://github.com/leecyrille/GoogleHomeVolumeSync";

type Backend = "cast" | "roku" | "yamaha" | "lg" | "optoma";

interface MediaInfo {
  state: string;
  title?: string;
  artist?: string;
  app?: string;
  supports_transport: boolean;
  album?: string;
  image?: string;
  position_ms?: number | null;
  duration_ms?: number | null;
  position_at?: number | null;
}
interface Device {
  id: string;
  backend: Backend;
  friendly_name: string;
  custom_name?: string;
  model: string;
  ip: string;
  port: number;
  is_cast_group: boolean;
  online: boolean;
  last_seen: number;
  volume: number;
  muted: boolean;
  can_absolute_volume: boolean;
  sync_gain: number;
  tv?: TvStatus | null;
  members: string[];
  media?: MediaInfo;
}
interface InputOption { id: string; label: string; kind: string; icon?: string | null; }
interface TvStatus {
  restricted: boolean;
  has_power: boolean;
  power?: boolean | null;
  showing?: string | null;
  showing_icon?: string | null;
  showing_detail?: string | null;
  activity?: string | null;
  position_ms?: number | null;
  duration_ms?: number | null;
  position_at?: number | null;
  is_live?: boolean;
  dev_mode?: boolean;
  player_ready?: boolean;
  exact_volume?: boolean;
  inputs: InputOption[];
  headphones: boolean;
  model?: string | null;
  firmware?: string | null;
}
interface Group {
  id: string;
  name: string;
  member_ids: string[];
  sync_enabled: boolean;
  group_volume: number;
}
interface ScheduleTarget { target_id: string; is_group: boolean; volume_pct: number; }
interface Sched { id: string; enabled: boolean; days: boolean[]; time: string; targets: ScheduleTarget[]; }
interface Settings { start_with_windows: boolean; auto_update: boolean; }
interface SyncView { members: string[]; paused: boolean; spread_ms?: number | null; status: string; }
interface CalFeed { url: string; name: string; color: string; enabled: boolean; }
interface CalSchedule {
  id: string; enabled: boolean; days: boolean[]; start: string; duration_min: number; devices: string[];
  theme: string; views: string[]; rotate_secs: number; power_on: boolean; dont_interrupt: boolean; off_after: boolean; idle_off_min: number;
}
interface CalSettings {
  token: string; screensaver_tvs: string[]; pushed: Record<string, string>; theme: string; views: string[]; rotate_secs: number;
  four_k: boolean; feeds: CalFeed[]; photo_folders: string[]; photo_interval_secs: number; refresh_minutes: number;
  schedules: CalSchedule[]; saver_imported: boolean;
}
interface FeedStatus { name: string; color: string; stale: boolean; error?: string | null; enabled: boolean; }
interface CalendarView {
  rendering: boolean; updated?: number | null; error?: string | null; feeds: FeedStatus[]; events: number; photos: number;
  showing: string[]; screensaver_tvs: string[]; outdated: string[]; settings: CalSettings; saver_found: boolean;
}
interface Snapshot { devices: Device[]; groups: Group[]; schedules: Sched[]; settings: Settings; sync?: SyncView | null; calendar?: CalendarView; }

let state: Snapshot = { devices: [], groups: [], schedules: [], settings: { start_with_windows: false, auto_update: false } };
let view = "devices";
const dragging = new Set<string>(); // slider ids being dragged - don't overwrite from state
let logTimer: number | undefined;

const content = document.getElementById("content")!;

function displayName(d: Device): string {
  return d.custom_name || d.friendly_name || d.ip;
}
function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]!));
}
function ago(ts: number): string {
  const s = Math.floor(Date.now() / 1000) - ts;
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return `${Math.floor(s / 86400)} d ago`;
}
const debounces = new Map<string, number>();
function debounce(key: string, ms: number, fn: () => void) {
  const old = debounces.get(key);
  if (old) clearTimeout(old);
  debounces.set(key, window.setTimeout(fn, ms));
}


// ---------------- now playing (sidebar) ----------------

const MAX_SLIDERS = 6;
const sideEl = document.getElementById("np-side")!;

function isPlaying(m: MediaInfo): boolean {
  return m.state === "PLAYING" || m.state === "BUFFERING";
}

/** Active sessions, playing first. Speakers inside a playing cast group only
 *  carry a "via" copy (no transport), so each group cast appears once. */
function nowPlaying(): Device[] {
  return state.devices
    .filter((d) => d.online && d.media && d.media.supports_transport && (isPlaying(d.media) || d.media.state === "PAUSED"))
    .sort((a, b) => Number(isPlaying(b.media!)) - Number(isPlaying(a.media!)) || displayName(a).localeCompare(displayName(b)));
}

interface VolItem { kind: "group" | "device"; id: string; label: string; level: number; }

/** One slider per sync group that holds a speaker in a playing session. A
 *  session with no speaker in any sync group gets its own device slider. */
function sideVolItems(sessions: Device[]): VolItem[] {
  const items = new Map<string, VolItem>();
  for (const d of sessions) {
    const speakers = d.is_cast_group && d.members.length ? d.members : [d.id];
    const groups = state.groups.filter((g) => g.member_ids.some((m) => speakers.includes(m)));
    for (const g of groups) items.set("g:" + g.id, { kind: "group", id: g.id, label: g.name, level: g.group_volume });
    if (groups.length === 0) items.set("d:" + d.id, { kind: "device", id: d.id, label: displayName(d), level: d.volume });
  }
  return [...items.values()];
}

function applyVol(item: VolItem, level: number) {
  const v = Math.max(0, Math.min(1, level));
  if (item.kind === "group") invoke("set_group_volume", { groupId: item.id, level: v });
  else invoke("set_volume", { id: item.id, level: v });
}

function sideSlider(key: string, label: string, level: number, showLabel: boolean): string {
  const pct = Math.round(level * 100);
  return `
    <div class="vcol" data-vkey="${esc(key)}" title="${esc(label)}">
      <span class="vpct">${pct}<small>%</small></span>
      <input type="range" class="vert" min="0" max="100" value="${pct}" aria-label="${esc(label)} volume">
      ${showLabel ? `<span class="vlabel">${esc(label)}</span>` : ""}
    </div>`;
}

function renderSide() {
  const sessions = nowPlaying();
  if (sessions.length === 0 && !state.sync) { sideEl.innerHTML = ""; return; }
  const shown = sessions.slice(0, 3);
  const items = sideVolItems(sessions);
  const averaged = items.length > MAX_SLIDERS;
  const avg = items.reduce((a, i) => a + i.level, 0) / Math.max(1, items.length);

  const sync = state.sync;
  const syncBox = sync ? `
    <div class="side-sync">
      <div><b>⟲ In sync</b> · ${sync.members.length} devices</div>
      <div class="side-sub">${esc(sync.status)}${sync.spread_ms != null && !sync.paused ? ` · within ${(sync.spread_ms / 1000).toFixed(2)} s` : ""}</div>
      <button class="btn mini" id="stop-sync">Stop syncing</button>
    </div>` : "";
  sideEl.innerHTML = `
    <div class="side-head">Now Casting</div>${syncBox}
    ${shown.map((d) => {
      const m = d.media!;
      const playing = isPlaying(m);
      const title = m.title || m.app || "Unknown track";
      const art = m.image ? `<img class="side-art" src="${esc(m.image)}" alt="">` : "";
      return `
      <div class="side-np" data-np="${esc(d.id)}">
        <div class="side-top">
          ${art}
          <div class="side-txt">
            <div class="side-title" title="${esc(title)}">${esc(title)}</div>
            ${m.title && m.artist ? `<div class="side-sub" title="${esc(m.artist)}">${esc(m.artist)}</div>` : ""}
            <div class="side-sub">${playing ? "" : "Paused · "}${esc(displayName(d))}</div>
          </div>
        </div>
        ${m.duration_ms && m.position_ms != null ? `
        <div class="side-prog" data-sprog="${esc(d.id)}">
          <input type="range" min="0" max="${m.duration_ms}" step="1000" value="${Math.round(mediaPos(m))}" data-sseek title="Drag to jump to a point">
          <div class="side-times"><span data-spos>${fmtTime(mediaPos(m))}</span><span>${fmtTime(m.duration_ms)}</span></div>
        </div>` : ""}
        <div class="side-ctl">
          <button class="btn icon" data-np-act="prev" title="Previous">⏮</button>
          <button class="btn icon" data-np-act="${playing ? "pause" : "play"}" title="${playing ? "Pause" : "Play"}">${playing ? "⏸" : "▶"}</button>
          <button class="btn icon" data-np-act="next" title="Next">⏭</button>
        </div>
      </div>`;
    }).join("")}
    ${sessions.length > shown.length ? `<div class="side-sub">+${sessions.length - shown.length} more playing</div>` : ""}
    ${items.length ? `
      <div class="vstrip n${averaged ? 1 : Math.min(items.length, MAX_SLIDERS)}">
        ${averaged
          ? sideSlider("avg", "Average", avg, false)
          : items.map((i) => sideSlider((i.kind === "group" ? "g:" : "d:") + i.id, i.label, i.level, items.length > 1)).join("")}
      </div>
      ${averaged ? `<div class="side-note">Average of ${items.length} sync groups. Moving it scales them all up or down together.</div>` : ""}` : ""}`;

  document.getElementById("stop-sync")?.addEventListener("click", () => invoke("stop_sync"));
  sideEl.querySelectorAll<HTMLElement>(".side-np").forEach((el) => {
    const id = el.dataset.np!;
    el.querySelectorAll<HTMLElement>("[data-np-act]").forEach((b) =>
      b.addEventListener("click", () => invoke("media_cmd", { id, action: b.dataset.npAct })));
    el.querySelector<HTMLImageElement>("img.side-art")?.addEventListener("error", (e) => (e.target as HTMLElement).remove());
    const seek = el.querySelector<HTMLInputElement>("[data-sseek]");
    if (seek) {
      const key = "side:seek:" + id;
      const posEl = el.querySelector<HTMLElement>("[data-spos]")!;
      seek.addEventListener("pointerdown", () => dragging.add(key));
      seek.addEventListener("input", () => { posEl.textContent = fmtTime(Number(seek.value)); });
      seek.addEventListener("change", () => {
        invoke("seek", { id, positionMs: Number(seek.value) });
        setTimeout(() => { dragging.delete(key); renderSide(); renderIfPending(); }, 1200);
      });
    }
  });

  sideEl.querySelectorAll<HTMLElement>(".vcol").forEach((col) => {
    const key = col.dataset.vkey!;
    const input = col.querySelector<HTMLInputElement>("input")!;
    const pctEl = col.querySelector(".vpct")!;
    const dragKey = "side:" + key;
    // The average slider scales every item from where it stood when the drag began.
    let base: VolItem[] = items;
    let baseAvg = avg;
    input.addEventListener("pointerdown", () => {
      dragging.add(dragKey);
      base = items.map((i) => ({ ...i }));
      baseAvg = base.reduce((a, i) => a + i.level, 0) / Math.max(1, base.length);
    });
    input.addEventListener("pointerup", () => setTimeout(() => { dragging.delete(dragKey); renderSide(); renderIfPending(); }, 800));
    input.addEventListener("input", () => {
      const v = Number(input.value) / 100;
      pctEl.innerHTML = `${input.value}<small>%</small>`;
      debounce(dragKey, 180, () => {
        if (key === "avg") {
          for (const i of base) applyVol(i, baseAvg > 0.01 ? i.level * (v / baseAvg) : v);
        } else {
          const item = items.find((i) => (i.kind === "group" ? "g:" : "d:") + i.id === key);
          if (item) applyVol(item, v);
        }
      });
    });
  });
}

function sideDragging(): boolean {
  return [...dragging].some((k) => k.startsWith("side:"));
}

// ---------------- cast media (files / links) ----------------

let castMode: CastMode = "";
const castPicked = new Set<string>();
const CAST_VIDEO = ["mp4", "m4v", "webm", "mkv", "mov"];
const CAST_AUDIO = ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus"];
const CAST_PICTURES = ["jpg", "jpeg", "png", "gif", "bmp", "webp"];
const ROKU_PICTURES = ["jpg", "jpeg", "png", "gif", "bmp"];
/** Cast devices with no screen (by model name). */
const AUDIO_ONLY = /mini|nest audio|chromecast audio|home max|^google home$|speaker/i;

function canShowPictures(d: Device): boolean {
  if (d.backend === "roku") return !!d.tv?.player_ready;
  return d.backend === "cast" && !d.is_cast_group && !AUDIO_ONLY.test(d.model);
}

const ROKU_AUDIO = ["mp3", "m4a", "aac", "flac", "wav"];

/** What a device can play: [videos, music, pictures] extension lists. */
function playableKinds(d: Device): [string[], string[], string[]] {
  if (d.backend === "roku") return [VIDEO_EXTS, ROKU_AUDIO, ROKU_PICTURES];
  // Speakers and speaker groups play the sound of a video, but can't show pictures.
  return [CAST_VIDEO, CAST_AUDIO, canShowPictures(d) ? CAST_PICTURES : []];
}

// ---------------- play queue ----------------

const baseName = (p: string) => p.split(/[\\/]/).pop() ?? p;
const extOf = (p: string) => (p.split(".").pop() ?? "").toLowerCase();

/** What was last sent to each device, so its card can show the queue. */
const deviceQueues = new Map<string, string[]>();
const openQueues = new Set<string>();

/**
 * Pick files and folders, put them in order (drag to reorder), then resolve with the list,
 * or null if cancelled. `exts` limits what can be added.
 */
function queueBuilder(title: string, exts: string[], initial: string[] = []): Promise<string[] | null> {
  return new Promise((resolve) => {
    let items = [...initial];
    const wrap = document.createElement("div");
    wrap.className = "dialog-back";
    const done = (r: string[] | null) => { wrap.remove(); resolve(r); };
    const draw = () => {
      wrap.innerHTML = `
        <div class="dialog queue-dialog" role="dialog" aria-modal="true">
          <h3>${esc(title)}</h3>
          <div class="queue-tools">
            <button class="btn" data-q="files">+ Add files…</button>
            <button class="btn" data-q="folder">+ Add a folder…</button>
            <span class="grow"></span>
            <button class="btn mini" data-q="sort" ${items.length > 1 ? "" : "disabled"}>Sort A–Z</button>
            <button class="btn mini" data-q="shuffle" ${items.length > 1 ? "" : "disabled"}>Shuffle</button>
            <button class="btn mini" data-q="clear" ${items.length ? "" : "disabled"}>Clear</button>
          </div>
          <ol class="queue-list">${items.map((p, i) => `
            <li draggable="true" data-i="${i}" title="${esc(p)}">
              <span class="grip" aria-hidden="true">⋮⋮</span>
              <span class="qn">${i + 1}</span>
              <span class="qname">${esc(baseName(p))}</span>
              <button class="btn icon" data-qdel="${i}" title="Remove">✕</button>
            </li>`).join("")}
          </ol>
          ${items.length ? `<div class="hint-inline">Drag to change the order. ${items.length} item${items.length === 1 ? "" : "s"}.</div>` : `<div class="hint">Add files, or a whole folder (with its subfolders), then drag them into the order you want.</div>`}
          <div class="dialog-actions">
            <button class="btn" data-q="cancel">Cancel</button>
            <button class="btn primary" data-q="play" ${items.length ? "" : "disabled"}>▶ Play ${items.length > 1 ? `${items.length} in order` : ""}</button>
          </div>
        </div>`;
      wireList();
    };
    const add = (paths: string[]) => {
      for (const p of paths) if (!items.includes(p) && exts.includes(extOf(p))) items.push(p);
      draw();
    };
    const wireList = () => {
      const list = wrap.querySelector<HTMLOListElement>(".queue-list")!;
      let from = -1;
      list.querySelectorAll<HTMLLIElement>("li").forEach((li) => {
        li.addEventListener("dragstart", (e) => { from = Number(li.dataset.i); li.classList.add("dragging"); e.dataTransfer?.setData("text/plain", String(from)); });
        li.addEventListener("dragend", () => li.classList.remove("dragging"));
        li.addEventListener("dragover", (e) => {
          e.preventDefault();
          const r = li.getBoundingClientRect();
          const after = e.clientY > r.top + r.height / 2;
          list.querySelectorAll("li").forEach((x) => x.classList.remove("drop-before", "drop-after"));
          li.classList.add(after ? "drop-after" : "drop-before");
        });
        li.addEventListener("drop", (e) => {
          e.preventDefault();
          const r = li.getBoundingClientRect();
          let to = Number(li.dataset.i) + (e.clientY > r.top + r.height / 2 ? 1 : 0);
          if (from < 0 || to === from || to === from + 1) { draw(); return; }
          const [moved] = items.splice(from, 1);
          if (to > from) to--;
          items.splice(to, 0, moved);
          draw();
        });
      });
    };
    wrap.addEventListener("click", async (e) => {
      const t = e.target as HTMLElement;
      const del = t.closest<HTMLElement>("[data-qdel]");
      if (del) { items.splice(Number(del.dataset.qdel), 1); draw(); return; }
      const act = t.closest<HTMLElement>("[data-q]")?.dataset.q;
      if (!act) { if (t === wrap) done(null); return; }
      if (act === "cancel") return done(null);
      if (act === "play") return done(items);
      if (act === "clear") { items = []; draw(); }
      if (act === "sort") { items.sort((a, b) => baseName(a).localeCompare(baseName(b), undefined, { numeric: true, sensitivity: "base" })); draw(); }
      if (act === "shuffle") { items = shuffleArr(items); draw(); }
      if (act === "files") {
        const picked = await open({ multiple: true, filters: [{ name: "Playable files", extensions: exts }] });
        add(Array.isArray(picked) ? picked : picked ? [picked] : []);
      }
      if (act === "folder") {
        const folder = await open({ directory: true, multiple: false });
        if (typeof folder !== "string") return;
        try { add(await invoke<string[]>("list_media_files", { folder, extensions: exts })); }
        catch (err) { alert(String(err)); }
      }
    });
    document.body.appendChild(wrap);
    draw();
  });
}

function shuffleArr<T>(a: T[]): T[] {
  const r = [...a];
  for (let i = r.length - 1; i > 0; i--) { const j = Math.floor(Math.random() * (i + 1)); [r[i], r[j]] = [r[j], r[i]]; }
  return r;
}

/** Play an ordered list on one device and remember it for the card's queue. */
async function playQueue(id: string, paths: string[]) {
  try {
    await invoke("play_files", { id, paths });
    deviceQueues.set(id, paths);
    render();
  } catch (e) { alert(String(e)); }
}

/** The queue last sent to a device, with the playing item marked (matched by title). */
function queueView(d: Device): string {
  const q = deviceQueues.get(d.id);
  if (!q || q.length < 2) return "";
  const title = d.media?.title ?? "";
  const cur = q.findIndex((p) => baseName(p).replace(/\.[^.]+$/, "") === title);
  return `
    <details class="dev-queue" ${openQueues.has(d.id) ? "open" : ""}>
      <summary>Queue · ${cur >= 0 ? `${cur + 1} of ${q.length}` : `${q.length} items`}</summary>
      <ol>${q.map((p, i) => `<li class="${i === cur ? "cur" : i < cur ? "past" : ""}">${esc(baseName(p))}</li>`).join("")}</ol>
      <button class="btn mini" data-act="queue-edit">Change order…</button>
    </details>`;
}

/** Pick files or a folder for the device, put them in order, and play them. */
async function chooseAndPlay(d: Device) {
  const [video, audio, pictures] = playableKinds(d);
  const paths = await queueBuilder(`Play on ${displayName(d)}`, [...video, ...audio, ...pictures]);
  if (paths?.length) await playQueue(d.id, paths);
}

/** Reorder what's playing: keep going from the current item, where it was. */
async function editQueue(d: Device) {
  const q = deviceQueues.get(d.id);
  if (!q) return;
  const [video, audio, pictures] = playableKinds(d);
  const title = d.media?.title ?? "";
  const cur = Math.max(0, q.findIndex((p) => baseName(p).replace(/\.[^.]+$/, "") === title));
  const paths = await queueBuilder(`Up next on ${displayName(d)}`, [...video, ...audio, ...pictures], q.slice(cur));
  if (!paths?.length) return;
  const resumeAt = paths[0] === q[cur] && d.media ? mediaPos(d.media) : 0;
  await playQueue(d.id, paths);
  if (resumeAt > 5000) setTimeout(() => invoke("seek", { id: d.id, positionMs: Math.round(resumeAt) }), 3500);
}

type CastMode = "" | "video" | "audio";

/** A column of devices in the cast panel. */
interface CastKind { key: string; title: string; match: (d: Device) => boolean; }

/** Video and pictures need a screen; audio plays anywhere. Speaker groups only take audio. */
function castKinds(mode: CastMode): CastKind[] {
  const roku: CastKind = { key: "roku", title: "Roku TVs", match: (d) => d.backend === "roku" };
  if (mode === "video") return [
    roku,
    { key: "gvideo", title: "Google TVs & displays", match: (d) => d.backend === "cast" && !d.is_cast_group && canShowPictures(d) },
  ];
  return [
    roku,
    { key: "google", title: "Google speakers & displays", match: (d) => d.backend === "cast" && !d.is_cast_group },
    { key: "groups", title: "Speaker groups", match: (d) => d.backend === "cast" && d.is_cast_group },
  ];
}

/** Why a device can't be picked right now, if it can't. */
function castBlocked(d: Device): string {
  if (d.backend === "roku" && !d.tv?.player_ready) return "Set up video playback on this TV's card first";
  return "";
}

function castEligible(mode: CastMode): Device[] {
  const kinds = castKinds(mode);
  return state.devices.filter((d) => d.online && kinds.some((k) => k.match(d)));
}

function castPickedDevices(): Device[] {
  return castEligible(castMode).filter((d) => castPicked.has(d.id) && !castBlocked(d));
}

const nameList = (ds: Device[]) => {
  const n = ds.map((d) => displayName(d));
  return n.length <= 1 ? n.join("") : `${n.slice(0, -1).join(", ")} and ${n[n.length - 1]}`;
};

/** Devices in the selection that won't play in step with the rest, and what to drop to fix it. */
function castConflicts(picked: Device[]): { lines: string[]; remove: string[] } {
  const lines: string[] = [];
  const remove = new Set<string>();
  const groups = picked.filter((d) => d.is_cast_group);
  // A speaker can only play one thing: picking it and a group it's in makes them fight.
  for (const d of picked.filter((x) => !x.is_cast_group)) {
    const g = groups.find((g) => g.members.includes(d.id));
    if (g) { lines.push(`${displayName(d)} is already part of ${displayName(g)}.`); remove.add(d.id); }
  }
  const sorted = [...groups].sort((a, b) => b.members.length - a.members.length);
  sorted.forEach((g, i) => {
    const bigger = sorted.slice(0, i).find((o) => !remove.has(o.id) && g.members.some((m) => o.members.includes(m)));
    if (bigger) { lines.push(`${displayName(g)} and ${displayName(bigger)} share speakers, and a speaker can only play one of them.`); remove.add(g.id); }
  });
  // Roku TVs can't join a Google group, so they're only kept roughly in step.
  const left = picked.filter((d) => !remove.has(d.id));
  const rokus = left.filter((d) => d.backend === "roku");
  const googles = left.filter((d) => d.backend === "cast");
  if (rokus.length && googles.length) {
    const why = castMode === "video" ? "Roku TVs can't be linked with Google screens" : "Roku TVs can't join a Google speaker group";
    lines.push(`${why}, so ${nameList(rokus)} won't stay exactly in sync with ${nameList(googles)}. The app keeps them close with short pauses, but you may hear an echo between them.`);
    const drop = castMode === "video" && rokus.length > googles.length ? googles : rokus;
    drop.forEach((d) => remove.add(d.id));
  }
  return { lines, remove: [...remove] };
}

function castPanel(): string {
  const kinds = castKinds(castMode);
  const eligible = castEligible(castMode);
  const picked = castPickedDevices();
  const conflicts = castConflicts(picked);
  const video = castMode === "video";
  const column = (k: CastKind) => {
    const ds = eligible.filter(k.match);
    if (ds.length === 0) return "";
    const pickable = ds.filter((d) => !castBlocked(d));
    const allOn = pickable.length > 0 && pickable.every((d) => castPicked.has(d.id));
    return `
      <div class="cast-col">
        <div class="cast-col-head"><span>${k.title}</span>${pickable.length > 1 ? `<button class="linkish" data-cast-all="${k.key}">${allOn ? "None" : "All"}</button>` : ""}</div>
        ${ds.map((d) => {
          const blocked = castBlocked(d);
          return `<label class="chk ${blocked ? "blocked" : ""}" title="${esc(blocked)}"><input type="checkbox" data-cast-pick="${esc(d.id)}" ${castPicked.has(d.id) && !blocked ? "checked" : ""} ${blocked ? "disabled" : ""}> ${esc(displayName(d))}${blocked ? ` <i>needs setup</i>` : ""}</label>`;
        }).join("")}
      </div>`;
  };
  const tipSpeakers = !video && picked.length > 1 && picked.every((d) => d.backend === "cast" && !d.is_cast_group);
  const summary = picked.length === 0 ? "Pick where to play"
    : picked.length === 1 ? `Plays on ${esc(displayName(picked[0]))}`
    : `Plays in sync on ${picked.length} devices`;
  return `
    <div class="cast-panel">
      <div class="cast-panel-title">${video ? "Play video or pictures" : "Play audio"}</div>
      <div class="cast-cols">${kinds.map(column).join("") || `<div class="hint">No devices that can play ${video ? "video" : "audio"} are online.</div>`}</div>
      ${conflicts.lines.length ? `
        <div class="cast-warn">
          <b>⚠ Some of these won't play in sync</b>
          <ul>${conflicts.lines.map((l) => `<li>${esc(l)}</li>`).join("")}</ul>
          <div class="cast-warn-actions">
            <button class="btn mini" id="cast-drop">Remove ${esc(nameList(state.devices.filter((d) => conflicts.remove.includes(d.id))))}</button>
            <span class="hint-inline">or play anyway on all of them.</span>
          </div>
        </div>` : ""}
      ${tipSpeakers ? `<div class="hint">Tip: a speaker group made in the Google Home app plays in perfect sync. Separate speakers are kept in step by this app.</div>` : ""}
      <div class="cast-actions">
        <button class="btn primary" id="cast-files" ${picked.length ? "" : "disabled"}>${video ? "▶ Choose video or pictures…" : "▶ Choose music…"}</button>
        <button class="btn" id="cast-link" ${picked.length ? "" : "disabled"}>🔗 Play a link…</button>
        <span class="cast-summary">${summary}</span>
      </div>
      <div class="hint">${video
        ? "One video plays in sync across several screens: pausing, resuming or skipping on any one moves the rest. Several pictures become a slideshow (8 seconds each), and a same-named .srt or .vtt next to a video becomes subtitles."
        : "One song or recording plays in sync across several devices. On a single device, several files play in order."}
        Files are shared only with the devices you pick, for 12 hours.</div>
    </div>`;
}

/** Ask what to do about devices that won't sync. Resolves "drop", "all" or null (cancel). */
function askConflicts(c: { lines: string[]; remove: string[] }): Promise<"drop" | "all" | null> {
  return new Promise((resolve) => {
    const dropNames = nameList(state.devices.filter((d) => c.remove.includes(d.id)));
    const wrap = document.createElement("div");
    wrap.className = "dialog-back";
    wrap.innerHTML = `
      <div class="dialog" role="dialog" aria-modal="true">
        <h3>These won't play exactly in sync</h3>
        <ul>${c.lines.map((l) => `<li>${esc(l)}</li>`).join("")}</ul>
        <div class="dialog-actions">
          <button class="btn primary" data-r="drop">Remove ${esc(dropNames)}</button>
          <button class="btn" data-r="all">Play on all of them</button>
          <button class="btn" data-r="">Cancel</button>
        </div>
      </div>`;
    const done = (r: "drop" | "all" | null) => { wrap.remove(); resolve(r); };
    wrap.addEventListener("click", (e) => {
      const b = (e.target as HTMLElement).closest<HTMLElement>("[data-r]");
      if (b) done((b.dataset.r || null) as "drop" | "all" | null);
      else if (e.target === wrap) done(null);
    });
    document.body.appendChild(wrap);
    wrap.querySelector<HTMLButtonElement>("[data-r=drop]")?.focus();
  });
}

/** Extensions every picked device can play, for the file picker. */
function commonKinds(ds: Device[]): [string[], string[], string[]] {
  const both = (a: string[], b: string[]) => a.filter((x) => b.includes(x));
  return ds.map(playableKinds).reduce((acc, k) => [both(acc[0], k[0]), both(acc[1], k[1]), both(acc[2], k[2])]);
}

async function castPlay(link: boolean) {
  let picked = castPickedDevices();
  if (picked.length === 0) return;
  const c = castConflicts(picked);
  if (picked.length > 1 && c.lines.length) {
    const r = await askConflicts(c);
    if (!r) return;
    if (r === "drop") {
      c.remove.forEach((id) => castPicked.delete(id));
      picked = picked.filter((d) => !c.remove.includes(d.id));
      render();
    }
  }
  const ids = picked.map((d) => d.id);
  const each = (cmd: string, args: (id: string) => Record<string, unknown>) =>
    Promise.all(ids.map((id) => invoke(cmd, args(id)))).catch((e) => alert(String(e)));

  if (link) {
    const url = prompt(castMode === "video" ? "Video link (MP4, MKV, WebM or an M3U8 live stream):" : "Music or radio link (MP3, AAC or an M3U8 stream):");
    if (url) await each("play_url", (id) => ({ id, url }));
    return;
  }
  const [video, audio, pictures] = commonKinds(picked);
  const single = picked.length === 1;
  const where = single ? displayName(picked[0]) : `${picked.length} devices`;
  if (castMode === "audio") {
    const paths = await queueBuilder(`Play music on ${where}`, audio);
    if (!paths?.length) return;
    if (single) return void playQueue(ids[0], paths);
    if (paths.length > 1) { alert("To play in sync on several devices, choose one song or recording."); return; }
    try { await invoke("play_synced", { ids, path: paths[0] }); } catch (e) { alert(String(e)); }
    return;
  }
  const paths = await queueBuilder(`Play on ${where}`, [...video, ...pictures]);
  if (!paths?.length) return;
  if (single) return void playQueue(ids[0], paths);
  const isPicture = (p: string) => pictures.includes(p.split(".").pop()!.toLowerCase());
  if (single || paths.every(isPicture)) return void each("play_files", (id) => ({ id, paths }));
  if (paths.length > 1) { alert("To play on several screens in sync, choose one video. (Several pictures together make a slideshow.)"); return; }
  try { await invoke("play_synced", { ids, path: paths[0] }); } catch (e) { alert(String(e)); }
}

// ---------------- calendar on TVs ----------------

const calBusy = new Set<string>();
const VIEW_NAMES: Record<string, string> = { month: "Month", week: "Week", day: "Day" };
const ROTATE_CHOICES: [number, string][] = [[0, "Don't switch"], [15, "15 seconds"], [30, "30 seconds"], [60, "1 minute"], [120, "2 minutes"], [300, "5 minutes"], [600, "10 minutes"]];
const DURATIONS: [number, string][] = [[15, "15 min"], [30, "30 min"], [45, "45 min"], [60, "1 hour"], [90, "1½ hours"], [120, "2 hours"], [180, "3 hours"], [240, "4 hours"], [360, "6 hours"], [480, "8 hours"], [720, "12 hours"]];
const IDLE_CHOICES = [10, 15, 20, 30, 45, 60, 90];
const PHOTO_SECS: [number, string][] = [[5, "5 seconds"], [10, "10 seconds"], [20, "20 seconds"], [30, "30 seconds"], [60, "1 minute"], [300, "5 minutes"]];

/** Screens that can show the calendar picture. */
function calendarScreens(includeOffline = false): Device[] {
  return state.devices.filter((d) => (includeOffline || d.online) && (d.backend === "roku" || (d.backend === "cast" && !d.is_cast_group && canShowPictures(d))));
}

function calSettings(): CalSettings | null {
  return state.calendar ? structuredClone(state.calendar.settings) : null;
}

async function saveCal(s: CalSettings) {
  try { await invoke("calendar_save", { settings: s }); } catch (e) { alert(String(e)); }
}

/** Run a calendar command for one screen, showing it as busy meanwhile. */
async function calRun(id: string, cmd: string, args: Record<string, unknown>) {
  calBusy.add(id); render();
  try { await invoke(cmd, args); } catch (e) { alert(String(e)); }
  finally { calBusy.delete(id); render(); }
}

/** Show / stop the calendar from a TV's own card. */
function calendarButton(d: Device, compact = false): string {
  const on = !!state.calendar?.showing.includes(d.id);
  const busy = calBusy.has(d.id);
  const label = busy ? "Starting…" : on ? "📅 Stop calendar" : "📅 Show calendar";
  return `<button class="btn ${compact ? "icon" : ""} ${on ? "cal-active" : ""}" data-act="calendar" ${busy ? "disabled" : ""}
    title="${on ? "Stop showing the calendar on this screen" : "Show your calendar on this screen (updates every minute)"}">${compact ? "📅" : label}</button>`;
}

const seg = (name: string, value: string, options: [string, string][]) => `
  <div class="seg" role="group" data-seg="${name}">${options.map(([v, label]) => `<button type="button" class="${v === value ? "on" : ""}" data-v="${v}">${label}</button>`).join("")}</div>`;

const viewChecks = (name: string, views: string[]) => `
  <span class="view-checks" data-views="${name}">${Object.entries(VIEW_NAMES).map(([v, label]) =>
    `<label class="chk"><input type="checkbox" data-view-opt="${v}" ${views.includes(v) ? "checked" : ""}> ${label}</label>`).join("")}</span>`;

const selectOf = (attr: string, value: number, options: [number, string][]) => `
  <select ${attr}>${options.some(([v]) => v === value) ? "" : `<option value="${value}" selected>${value}</option>`}${options.map(([v, label]) => `<option value="${v}" ${v === value ? "selected" : ""}>${label}</option>`).join("")}</select>`;

function renderCalendar() {
  const cal = state.calendar;
  if (!cal) { content.innerHTML = `<h2>Calendar</h2><div class="hint">Loading…</div>`; return; }
  const s = cal.settings;
  const when = cal.updated ? new Date(cal.updated).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" }) : "";
  const feedProblems = cal.feeds.filter((f) => f.enabled && (f.error || f.stale));
  const status = cal.rendering
    ? `<span class="cal-on">● Drawing for your TVs</span> · updated ${esc(when || "…")} · ${cal.events} events · ${cal.photos} photos`
    : `Not drawing right now: it starts when a TV shows the calendar.`;

  // ----- screens -----
  const screens = calendarScreens();
  const row = (d: Device) => {
    const showing = cal.showing.includes(d.id);
    const blocked = castBlocked(d);
    const busy = calBusy.has(d.id);
    const saver = d.backend === "roku" ? `
        <label class="chk" title="Keep the calendar on this TV as its Roku screensaver">
          <input type="checkbox" data-cal-ss="${esc(d.id)}" ${cal.screensaver_tvs.includes(d.id) ? "checked" : ""} ${blocked || busy ? "disabled" : ""}> Screensaver</label>
        ${cal.outdated.includes(d.id) ? `<button class="btn mini" data-cal-update="${esc(d.id)}" title="The TV's saved screensaver settings are out of date (theme, views, 4K or this PC's address changed)">Update TV</button>` : ""}` : "";
    return `
      <div class="cal-row">
        <span class="cal-name">${esc(displayName(d))}${blocked ? ` <i>needs setup</i>` : ""}</span>
        ${showing ? `<span class="cal-on">● Showing</span>` : ""}
        <span class="cal-acts">
          ${saver}
          ${showing
            ? `<button class="btn mini" data-cal-stop="${esc(d.id)}" ${busy ? "disabled" : ""}>Stop</button>`
            : `<button class="btn mini primary" data-cal-show="${esc(d.id)}" ${blocked || busy ? "disabled" : ""} title="${esc(blocked)}">${busy ? "Starting…" : "Show"}</button>`}
        </span>
      </div>`;
  };
  const column = (title: string, ds: Device[]) => ds.length === 0 ? "" : `
      <div class="cast-col"><div class="cast-col-head"><span>${title}</span></div>${ds.map(row).join("")}</div>`;

  // ----- previews -----
  const previews = cal.updated ? `
      <div class="cal-previews">${Object.entries(VIEW_NAMES).map(([v, label]) => `
        <figure data-cal-preview="${v}" title="Open the ${label.toLowerCase()} view">
          <img src="http://calphoto.localhost/preview/calendar-${s.theme}-${v}.jpg?v=${cal.updated}" alt="${label} view" onerror="this.closest('figure').classList.add('missing')">
          <figcaption>${label}</figcaption>
        </figure>`).join("")}
      </div>` : "";

  // ----- schedules -----
  const allScreens = calendarScreens(true);
  const schedCardHtml = (sc: CalSchedule) => `
    <div class="card cal-sched" data-csid="${esc(sc.id)}">
      <div class="row wrap">
        <label class="chk"><input type="checkbox" data-cs="enabled" ${sc.enabled ? "checked" : ""}> On</label>
        ${timePicker(sc.start)}
        <span class="lbl">for</span> ${selectOf('data-cs="duration_min"', sc.duration_min, DURATIONS)}
        <div class="day-row">${DAY_NAMES.map((n, i) => `<label>${n}<input type="checkbox" data-csday="${i}" ${sc.days[i] ? "checked" : ""} /></label>`).join("")}</div>
        <div class="grow"></div>
        <button class="btn danger mini" data-cs="delete">Delete</button>
      </div>
      <div class="cal-sched-grid">
        <div class="lbl">Screens</div>
        <div class="cal-devs">${allScreens.map((d) => `<label class="chk"><input type="checkbox" data-csdev="${esc(d.id)}" ${sc.devices.includes(d.id) ? "checked" : ""}> ${esc(displayName(d))}</label>`).join("") || `<span class="hint-inline">No TVs or Google screens found yet.</span>`}</div>
        <div class="lbl">Look</div>
        <div class="row wrap">${seg("theme", sc.theme || "default", [["default", "Usual"], ["light", "Light"], ["dark", "Dark"]])}
          ${viewChecks("views", sc.views)} <span class="lbl">switch</span> ${selectOf('data-cs="rotate_secs"', sc.rotate_secs, ROTATE_CHOICES)}</div>
        <div class="lbl">TV</div>
        <div class="cal-opts">
          <label class="chk"><input type="checkbox" data-cs="power_on" ${sc.power_on ? "checked" : ""}> Turn the TV on if it's off</label>
          <label class="chk"><input type="checkbox" data-cs="dont_interrupt" ${sc.dont_interrupt ? "checked" : ""}> Don't interrupt a show or movie (the screensaver and home screen are fine); it waits until it's over</label>
          <label class="chk"><input type="checkbox" data-cs="off_after" ${sc.off_after ? "checked" : ""}> When the time's up, turn the TV off if it's still showing the calendar</label>
          <label class="chk"><input type="checkbox" data-cs="idle_on" ${sc.idle_off_min > 0 ? "checked" : ""}> Turn it off early if nobody presses a remote button for
            ${selectOf('data-cs="idle_off_min"', sc.idle_off_min || 30, IDLE_CHOICES.map((m) => [m, `${m} minutes`]))}</label>
        </div>
      </div>
    </div>`;

  // ----- calendars -----
  const feedRow = (f: CalFeed, i: number) => {
    const st = cal.feeds[i];
    const problem = st && st.enabled ? (st.error ? `✕ ${st.error}` : st.stale ? "⚠ showing the saved copy" : "") : "";
    return `
      <div class="feed-row" data-feed="${i}">
        <input type="checkbox" data-f="enabled" ${f.enabled ? "checked" : ""} title="Show this calendar">
        <input type="color" data-f="color" value="${esc(f.color || "#7aa2f7")}" title="Colour">
        <input type="text" data-f="name" value="${esc(f.name)}" placeholder="Name" class="feed-name">
        <input type="text" data-f="url" value="${esc(f.url)}" placeholder="Secret address in iCal format (…/basic.ics)" class="feed-url" spellcheck="false">
        <button class="btn mini" data-f="test">Test</button>
        <button class="btn icon danger" data-f="del" title="Remove">✕</button>
        ${problem ? `<div class="feed-problem">${esc(problem)}</div>` : ""}
        <div class="feed-test" data-test="${i}"></div>
      </div>`;
  };

  content.innerHTML = `
    <h2>Calendar <span class="sub">your calendars and photos, on your TVs</span></h2>
    <div class="cal-status">${status}${cal.error ? ` · <span class="warn">⚠ ${esc(cal.error)}</span>` : ""}${feedProblems.length ? ` · <span class="warn">⚠ ${feedProblems.map((f) => esc(f.name)).join(", ")}</span>` : ""}</div>

    <section class="cal-sec">
      <h3>On your TVs</h3>
      <div class="cast-cols">${column("Roku TVs", screens.filter((d) => d.backend === "roku"))}${column("Google TVs & displays", screens.filter((d) => d.backend === "cast"))}
        ${screens.length === 0 ? `<div class="hint">No TVs or Google screens are online.</div>` : ""}</div>
      <div class="hint">For a Roku screensaver, tick <b>Screensaver</b>, then on the TV choose <b>Settings › Theme › Screensaver › Calendar (Volume Sync)</b>. On a Roku, <b>Up</b> and <b>Down</b> on the remote switch between month, week and day. This PC needs to be on: it redraws the calendar every minute.</div>
    </section>

    <section class="cal-sec">
      <h3>Look</h3>
      <div class="cal-look">
        <div class="lbl">Theme</div><div>${seg("main-theme", s.theme, [["dark", "🌙 Dark"], ["light", "☀️ Light"]])}</div>
        <div class="lbl">Shows</div><div class="row wrap">${viewChecks("main-views", s.views)} <span class="lbl">switch</span> ${selectOf('id="cal-rotate"', s.rotate_secs, ROTATE_CHOICES)}</div>
        <div class="lbl">Sharpness</div><div><label class="chk"><input type="checkbox" id="cal-4k" ${s.four_k ? "checked" : ""}> 4K on Roku TVs <span class="hint-inline">(sent as a one-frame video, since Roku apps draw pictures at 1080p)</span></label></div>
      </div>
      ${previews}
    </section>

    <section class="cal-sec">
      <h3>Scheduled times <button class="btn mini primary" id="cal-add-sched">+ New</button></h3>
      ${s.schedules.map(schedCardHtml).join("") || `<div class="hint">For example: weekdays at 7:00 AM for 1½ hours in light mode, turning the TV on and back off.</div>`}
    </section>

    <section class="cal-sec">
      <h3>Calendars <button class="btn mini primary" id="cal-add-feed">+ Add</button>
        ${cal.saver_found ? `<button class="btn mini" id="cal-import">Copy from PactoTech Calendar Saver</button>` : ""}</h3>
      ${s.feeds.map(feedRow).join("") || `<div class="hint">Add a calendar's secret iCal address. In Google Calendar: Settings › your calendar › Integrate calendar › Secret address in iCal format.</div>`}
    </section>

    <section class="cal-sec">
      <h3>Photos <button class="btn mini primary" id="cal-add-folder">+ Add folder</button></h3>
      ${s.photo_folders.map((p, i) => `<div class="folder-row"><span title="${esc(p)}">📁 ${esc(p)}</span><button class="btn icon danger" data-folder-del="${i}" title="Remove">✕</button></div>`).join("") || `<div class="hint">Photos from these folders (and their subfolders) fill the side of the calendar.</div>`}
      <div class="row" style="margin-top:8px"><span class="lbl">Change a photo every</span> ${selectOf('id="cal-photo-secs"', s.photo_interval_secs, PHOTO_SECS)}</div>
    </section>
    <div class="hint">The calendar's design comes from the PactoTech Calendar Saver (calendarsaver.com).</div>
  `;
  wireCalendarPage();
}

function wireCalendarPage() {
  const q = <T extends Element = HTMLElement>(sel: string) => content.querySelector<T>(sel);
  const all = <T extends Element = HTMLElement>(sel: string, root: ParentNode = content) => [...root.querySelectorAll<T>(sel)];
  const edit = (f: (s: CalSettings) => void) => { const s = calSettings(); if (!s) return; f(s); saveCal(s); };
  const segValue = (root: ParentNode, name: string, set: (v: string) => void) =>
    all<HTMLElement>(`[data-seg="${name}"] [data-v]`, root).forEach((b) => b.addEventListener("click", () => set(b.dataset.v!)));
  const viewsFrom = (root: ParentNode, name: string) => all<HTMLInputElement>(`[data-views="${name}"] [data-view-opt]`, root).filter((c) => c.checked).map((c) => c.dataset.viewOpt!);

  all("[data-cal-show]").forEach((b) => b.addEventListener("click", () => calRun(b.dataset.calShow!, "calendar_show", { ids: [b.dataset.calShow] })));
  all("[data-cal-stop]").forEach((b) => b.addEventListener("click", () => calRun(b.dataset.calStop!, "calendar_stop", { ids: [b.dataset.calStop] })));
  all<HTMLInputElement>("[data-cal-ss]").forEach((cb) => cb.addEventListener("change", () => calRun(cb.dataset.calSs!, "calendar_screensaver", { id: cb.dataset.calSs, on: cb.checked })));
  all("[data-cal-update]").forEach((b) => b.addEventListener("click", () => calRun(b.dataset.calUpdate!, "calendar_screensaver", { id: b.dataset.calUpdate, on: true })));
  all("[data-cal-preview]").forEach((f) => f.addEventListener("click", () =>
    invoke("calendar_preview", { theme: state.calendar!.settings.theme, view: f.dataset.calPreview }).catch((e) => alert(String(e)))));

  // look
  segValue(content, "main-theme", (v) => edit((s) => { s.theme = v; }));
  all<HTMLInputElement>('[data-views="main-views"] input').forEach((cb) => cb.addEventListener("change", () => edit((s) => { s.views = viewsFrom(content, "main-views"); })));
  q<HTMLSelectElement>("#cal-rotate")?.addEventListener("change", (e) => edit((s) => { s.rotate_secs = Number((e.target as HTMLSelectElement).value); }));
  q<HTMLInputElement>("#cal-4k")?.addEventListener("change", (e) => edit((s) => { s.four_k = (e.target as HTMLInputElement).checked; }));
  q<HTMLSelectElement>("#cal-photo-secs")?.addEventListener("change", (e) => edit((s) => { s.photo_interval_secs = Number((e.target as HTMLSelectElement).value); }));

  // schedules
  q("#cal-add-sched")?.addEventListener("click", () => edit((s) => {
    s.schedules.push({
      id: crypto.randomUUID(), enabled: true, days: [true, true, true, true, true, false, false], start: "07:00", duration_min: 90,
      devices: calendarScreens(true).filter((d) => d.backend === "roku").map((d) => d.id), theme: "light", views: ["month"], rotate_secs: 0,
      power_on: true, dont_interrupt: true, off_after: true, idle_off_min: 0,
    });
  }));
  all(".cal-sched").forEach((card) => {
    const id = (card as HTMLElement).dataset.csid!;
    const upd = (f: (sc: CalSchedule) => void) => edit((s) => { const sc = s.schedules.find((x) => x.id === id); if (sc) f(sc); });
    const cs = (name: string) => card.querySelector<HTMLInputElement & HTMLSelectElement>(`[data-cs="${name}"]`);
    cs("enabled")?.addEventListener("change", (e) => upd((sc) => { sc.enabled = (e.target as HTMLInputElement).checked; }));
    cs("duration_min")?.addEventListener("change", (e) => upd((sc) => { sc.duration_min = Number((e.target as HTMLSelectElement).value); }));
    cs("rotate_secs")?.addEventListener("change", (e) => upd((sc) => { sc.rotate_secs = Number((e.target as HTMLSelectElement).value); }));
    for (const k of ["power_on", "dont_interrupt", "off_after"] as const)
      cs(k)?.addEventListener("change", (e) => upd((sc) => { sc[k] = (e.target as HTMLInputElement).checked; }));
    const idleSel = cs("idle_off_min");
    cs("idle_on")?.addEventListener("change", (e) => upd((sc) => { sc.idle_off_min = (e.target as HTMLInputElement).checked ? Number(idleSel?.value || 30) : 0; }));
    idleSel?.addEventListener("change", () => upd((sc) => { sc.idle_off_min = Number(idleSel.value); }));
    cs("delete")?.addEventListener("click", () => edit((s) => { s.schedules = s.schedules.filter((x) => x.id !== id); }));
    all<HTMLInputElement>("[data-csday]", card).forEach((cb) => cb.addEventListener("change", () => upd((sc) => { sc.days[Number(cb.dataset.csday)] = cb.checked; })));
    all<HTMLInputElement>("[data-csdev]", card).forEach((cb) => cb.addEventListener("change", () => upd((sc) => {
      sc.devices = sc.devices.filter((d) => d !== cb.dataset.csdev);
      if (cb.checked) sc.devices.push(cb.dataset.csdev!);
    })));
    segValue(card, "theme", (v) => upd((sc) => { sc.theme = v; }));
    all<HTMLInputElement>('[data-views="views"] input', card).forEach((cb) => cb.addEventListener("change", () => upd((sc) => { sc.views = viewsFrom(card, "views"); })));
    // time
    const tp = card.querySelector<HTMLElement>(".tpick")!;
    const cur = () => { const sc = state.calendar!.settings.schedules.find((x) => x.id === id)!; const [h, m] = parseTime(sc.start); return { h12: h % 12 || 12, m, pm: h >= 12 }; };
    const setTime = (h12: number, m: number, pm: boolean) => upd((sc) => { sc.start = `${String((h12 % 12) + (pm ? 12 : 0)).padStart(2, "0")}:${String(m).padStart(2, "0")}`; });
    tp.querySelector<HTMLSelectElement>('[data-t="h"]')!.addEventListener("change", (e) => { const c = cur(); setTime(Number((e.target as HTMLSelectElement).value), c.m, c.pm); });
    tp.querySelector<HTMLSelectElement>('[data-t="m"]')!.addEventListener("change", (e) => { const c = cur(); setTime(c.h12, Number((e.target as HTMLSelectElement).value), c.pm); });
    all<HTMLElement>("[data-ampm]", tp).forEach((b) => b.addEventListener("click", () => { const c = cur(); setTime(c.h12, c.m, b.dataset.ampm === "pm"); }));
  });

  // calendars
  q("#cal-add-feed")?.addEventListener("click", () => edit((s) => { s.feeds.push({ url: "", name: "", color: ["#7aa2f7", "#f7768e", "#9ece6a", "#e0af68", "#bb9af7"][s.feeds.length % 5], enabled: true }); }));
  q("#cal-import")?.addEventListener("click", async () => {
    try { alert(await invoke<string>("calendar_import_saver")); } catch (e) { alert(String(e)); }
  });
  all(".feed-row").forEach((rowEl) => {
    const i = Number((rowEl as HTMLElement).dataset.feed);
    const f = (name: string) => rowEl.querySelector<HTMLInputElement>(`[data-f="${name}"]`)!;
    const upd = (fn: (feed: CalFeed) => void) => edit((s) => { if (s.feeds[i]) fn(s.feeds[i]); });
    f("enabled").addEventListener("change", (e) => upd((x) => { x.enabled = (e.target as HTMLInputElement).checked; }));
    f("color").addEventListener("change", (e) => upd((x) => { x.color = (e.target as HTMLInputElement).value; }));
    f("name").addEventListener("change", (e) => upd((x) => { x.name = (e.target as HTMLInputElement).value.trim(); }));
    f("url").addEventListener("change", (e) => upd((x) => { x.url = (e.target as HTMLInputElement).value.trim(); }));
    f("del").addEventListener("click", () => { if (confirm("Remove this calendar?")) edit((s) => { s.feeds.splice(i, 1); }); });
    f("test").addEventListener("click", async () => {
      const out = rowEl.querySelector<HTMLElement>(".feed-test")!;
      out.textContent = "Checking…";
      try { out.textContent = "✓ " + await invoke<string>("calendar_test_feed", { url: f("url").value.trim() }); }
      catch (e) { out.textContent = "✕ " + String(e); }
    });
  });

  // photos
  q("#cal-add-folder")?.addEventListener("click", async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string") edit((s) => { if (!s.photo_folders.includes(picked)) s.photo_folders.push(picked); });
  });
  all("[data-folder-del]").forEach((b) => b.addEventListener("click", () => edit((s) => { s.photo_folders.splice(Number(b.dataset.folderDel), 1); })));
}

function wireCastPanel() {
  document.querySelectorAll<HTMLInputElement>("[data-cast-pick]").forEach((cb) => cb.addEventListener("change", () => {
    if (cb.checked) castPicked.add(cb.dataset.castPick!); else castPicked.delete(cb.dataset.castPick!);
    render();
  }));
  document.querySelectorAll<HTMLElement>("[data-cast-all]").forEach((b) => b.addEventListener("click", () => {
    const kind = castKinds(castMode).find((k) => k.key === b.dataset.castAll)!;
    const ds = castEligible(castMode).filter((d) => kind.match(d) && !castBlocked(d));
    const allOn = ds.every((d) => castPicked.has(d.id));
    ds.forEach((d) => allOn ? castPicked.delete(d.id) : castPicked.add(d.id));
    render();
  }));
  document.getElementById("cast-drop")?.addEventListener("click", () => {
    castConflicts(castPickedDevices()).remove.forEach((id) => castPicked.delete(id));
    render();
  });
  document.getElementById("cast-files")?.addEventListener("click", () => castPlay(false));
  document.getElementById("cast-link")?.addEventListener("click", () => castPlay(true));
}

// ---------------- devices view ----------------

function renderDevices() {
  const stale = (d: Device) => !d.online && Date.now() / 1000 - d.last_seen > 86400;
  const html = `
    <h2>Devices</h2>
    <div class="toolbar">
      <button class="btn" id="scan-roku">Scan for Roku TVs</button>
      <button class="btn" id="add-manual">Add device by IP…</button>
      <span class="toolbar-gap"></span>
      <button class="btn ${castMode === "video" ? "primary" : ""}" data-cast-mode="video" title="Play a video or pictures from this PC on one or more screens">🎬 Play Video or Pictures</button>
      <button class="btn ${castMode === "audio" ? "primary" : ""}" data-cast-mode="audio" title="Play music from this PC on one or more speakers or TVs">🎵 Play Audio</button>
      <button class="btn" id="go-calendar" title="Your calendar on TVs: show it, schedule it, or use it as a Roku screensaver">📅 Calendar</button>
    </div>
    ${castMode ? castPanel() : ""}
    ${section("Roku TVs", state.devices.filter((d) => d.backend === "roku"), stale)}
    ${section("Other Devices", state.devices.filter((d) => !["cast", "roku"].includes(d.backend)), stale)}
    ${section("Google Devices", state.devices.filter((d) => !d.is_cast_group && d.backend === "cast"), stale)}
    ${section("Google Cast Groups", state.devices.filter((d) => d.is_cast_group), stale)}
    ${state.devices.length === 0 ? `<div class="hint">Searching for Google Cast devices on your network…</div>` : ""}
  `;
  content.innerHTML = html;

  document.getElementById("scan-roku")!.addEventListener("click", async (e) => {
    const btn = e.target as HTMLButtonElement;
    btn.disabled = true; btn.textContent = "Scanning…";
    try { const n = await invoke<number>("scan_roku"); btn.textContent = `Found ${n} Roku(s)`; }
    catch { btn.textContent = "Scan failed"; }
    setTimeout(() => { btn.disabled = false; btn.textContent = "Scan for Roku TVs"; }, 2500);
  });
  document.getElementById("add-manual")!.addEventListener("click", showAddManual);
  document.getElementById("go-calendar")?.addEventListener("click", () => { view = "calendar"; render(); });
  document.querySelectorAll<HTMLElement>("[data-cast-mode]").forEach((b) => b.addEventListener("click", () => {
    const mode = b.dataset.castMode as CastMode;
    castMode = castMode === mode ? "" : mode;
    // Keep only the picks that still make sense (no speakers for video).
    const ok = new Set(castEligible(castMode).map((d) => d.id));
    [...castPicked].forEach((id) => { if (!ok.has(id)) castPicked.delete(id); });
    render();
  }));
  wireCastPanel();

  for (const d of state.devices) wireDeviceCard(d);
}

function section(title: string, devices: Device[], stale: (d: Device) => boolean): string {
  if (devices.length === 0) return "";
  return `<div class="section-title">${title} <span class="sub">${devices.filter((d) => d.online).length} online</span></div>
    ${devices.map((d) => deviceCard(d, stale(d))).join("")}`;
}

function deviceCard(d: Device, stale: boolean): string {
  const pct = Math.round(d.volume * 100);
  const media = d.media
    ? `<div class="media-info">${d.media.state === "PLAYING" ? `<span class="playing">▶ Playing</span>` : d.media.state === "PAUSED" ? "⏸ Paused" : d.media.state}
       ${d.media.title ? " — " + esc(d.media.title) : ""}${d.media.artist ? " · " + esc(d.media.artist) : ""}${d.media.app ? ` <span class="via">${esc(d.media.app)}</span>` : ""}</div>`
    : "";
  const remote = d.backend === "roku" && d.tv;
  return `
  <div class="card compact ${d.online ? "" : "offline"} ${remote ? "has-remote" : ""}" data-id="${esc(d.id)}">
    <div class="card-body">
    <div class="row">
      <span class="dot ${d.online ? "on" : ""}" title="${d.online ? "Online" : "Offline"}"></span>
      <div class="dev-id" title="${esc(d.friendly_name)} · ${esc(d.model)} · ${esc(d.ip)}">
        <div class="dev-name"><input value="${esc(displayName(d))}" data-act="rename" title="Click to rename (blank = reset to device name)" /></div>
        <div class="dev-meta">${d.is_cast_group ? "cast group · " : ""}${esc(d.model)}${d.tv?.firmware ? ` · ${esc(d.tv.firmware)}` : ""}${!d.online ? ` · last seen ${ago(d.last_seen)}` : ""}</div>
      </div>
      <button class="btn icon ${d.muted ? "muted-on" : ""}" data-act="mute" title="Mute">${d.muted ? "🔇" : "🔊"}</button>
      <input type="range" min="0" max="100" value="${pct}" data-act="vol" />
      <span class="vol-pct">${pct}%</span>
      ${d.media?.supports_transport ? `
        <button class="btn icon" data-act="prev">⏮</button>
        <button class="btn icon" data-act="${d.media.state === "PLAYING" ? "pause" : "play"}">${d.media.state === "PLAYING" ? "⏸" : "▶"}</button>
        <button class="btn icon" data-act="next">⏭</button>` : ""}
      ${d.online && d.backend === "cast" && !d.is_cast_group && canShowPictures(d) ? calendarButton(d, true) : ""}
      ${stale ? `<button class="btn danger icon" data-act="delete" title="Remove until seen again">✕</button>` : ""}
      <label class="gain-wrap" title="Sync Gain (0–200%, default 100%): balance this device inside sync groups. Its actual volume = group volume × gain; it reports volume ÷ gain back to the group. Example: at 50% gain, group volume 30% puts this device at 15%.">
        <span>Sync Gain</span>
        <div class="gain-col">
          <input type="number" min="0" max="200" step="5" value="${Math.round((d.sync_gain ?? 1) * 100)}" data-act="gain" />
          <div class="gain-bar"><div class="gain-fill ${((d.sync_gain ?? 1) > 1) ? "hot" : ""}" style="width:${Math.min(100, ((d.sync_gain ?? 1) * 100) / 2)}%"></div><div class="gain-mid"></div></div>
        </div>
      </label>
    </div>
    ${d.tv ? tvRow(d) : ""}
    ${media}
    ${queueView(d)}
    </div>
    ${remote ? rokuRemote() : ""}
  </div>`;
}

/** Activity pill: [label, tone, tooltip]. */
const ACTIVITY: Record<string, [string, string, string]> = {
  playing: ["Playing", "play", "An app is playing video or audio"],
  "live-tv": ["Watching Live TV", "play", "Tuned to an antenna channel"],
  paused: ["Paused", "pause", "Playback in the app is paused"],
  loading: ["Loading", "pause", "The app is starting or buffering"],
  screensaver: ["Idle · screensaver", "idle", "Nobody has touched the remote for a while"],
  home: ["Idle · home screen", "idle", "Sitting on the Roku home screen"],
  input: ["On an input", "unknown", "Showing an HDMI source; the TV can't tell whether that device is playing"],
  app: ["App open", "unknown", "An app is open but isn't reporting playback. Some apps, like ambient or fireplace videos, play without reporting it"],
};

function fmtTime(ms: number): string {
  const t = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(t / 3600), m = Math.floor((t % 3600) / 60), s = t % 60;
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}

/** A cast session's position now, advanced since its last status while playing. */
function mediaPos(m: MediaInfo): number {
  const base = m.position_ms ?? 0;
  if (!(m.state === "PLAYING") || !m.position_at) return base;
  return Math.min(m.duration_ms ?? Infinity, base + (Date.now() - m.position_at));
}

/** Position now, advanced from the last poll while playing. */
function livePosition(tv: TvStatus): number {
  const base = tv.position_ms ?? 0;
  if (tv.activity !== "playing" || !tv.position_at) return base;
  return Math.min(tv.duration_ms ?? Infinity, base + (Date.now() - tv.position_at));
}

const VIDEO_EXTS = ["mp4", "m4v", "mov", "mkv", "ts", "webm"];
const setupOpen = new Set<string>();
const setupMsg = new Map<string, string>();

/** Play buttons (or the setup button) for a Roku TV. */
function playButtons(d: Device): string {
  const tv = d.tv!;
  if (d.backend !== "roku" || tv.power == null) return "";
  if (tv.player_ready) {
    return `
      <button class="btn" data-act="play-files" title="Videos, music or pictures from this PC. Several play in order (pictures as a slideshow); a same-named .srt or .vtt comes along as subtitles.">▶ Play Audio, Video or Image</button>
      <button class="btn" data-act="play-url" title="Paste a video link (MP4, MKV, TS or an M3U8 live stream)">🔗 Play a link…</button>
      ${calendarButton(d)}`;
  }
  return `<button class="btn" data-act="setup-toggle" title="Play files from this PC on this TV. One-time setup.">${setupOpen.has(d.id) ? "Hide setup" : "Set up video playback"}</button>`;
}

/** One-time setup steps for the player channel. */
function setupPanel(d: Device): string {
  const tv = d.tv!;
  if (d.backend !== "roku" || tv.player_ready || !setupOpen.has(d.id)) return "";
  const msg = setupMsg.get(d.id);
  return `
    <div class="setup">
      <p>Roku doesn't let apps send videos to the TV anymore, so Volume Sync installs its own small player channel. That needs the TV's developer mode, which you switch on once:</p>
      <ol>
        <li><button class="btn mini" data-act="dev-settings">Open developer settings on the TV</button> (or press Home ×3, Up ×2, Right, Left, Right, Left, Right on the remote)</li>
        <li>On the TV choose <b>Enable installer and restart</b>, accept the agreement, and pick a password.</li>
        <li>After the TV restarts, enter that password here:
          <span class="setup-pw"><input type="password" data-act="dev-pw" placeholder="Developer password" autocomplete="off"><button class="btn primary" data-act="install-player">Install player</button></span>
        </li>
      </ol>
      ${msg ? `<div class="setup-msg">${esc(msg)}</div>` : ""}
      <p class="hint">Developer mode only lets the TV accept apps installed from your own network. Roku allows one such app at a time.</p>
    </div>`;
}

function progressRow(d: Device): string {
  const tv = d.tv!;
  if (tv.position_ms == null || !(tv.activity === "playing" || tv.activity === "paused")) return "";
  if (tv.is_live || !tv.duration_ms) return `<div class="tv-prog"><span class="live">● Live</span></div>`;
  const pos = livePosition(tv);
  return `
    <div class="tv-prog" data-prog="${esc(d.id)}" title="Drag to seek. The TV has no direct seek, so this fast-forwards or rewinds to near the spot.">
      <span class="t" data-pos>${fmtTime(pos)}</span>
      <input type="range" min="0" max="${tv.duration_ms}" step="1000" value="${Math.round(pos)}" data-seek>
      <span class="t">${fmtTime(tv.duration_ms)}</span>
    </div>`;
}

/** Icons for the remote, drawn to match the Roku remote's keys. */
const ICON: Record<string, string> = {
  back: `<svg viewBox="0 0 24 24"><path d="M20 12H6.5M11.5 6.5 6 12l5.5 5.5" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  home: `<svg viewBox="0 0 24 24"><path d="M4.5 11 12 4.8l7.5 6.2V19.5h-5.2v-5h-4.6v5H4.5z" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linejoin="round"/></svg>`,
  up: `<svg viewBox="0 0 24 24"><path d="M6 15l6-6 6 6" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  down: `<svg viewBox="0 0 24 24"><path d="M6 9l6 6 6-6" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  left: `<svg viewBox="0 0 24 24"><path d="M15 6l-6 6 6 6" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  right: `<svg viewBox="0 0 24 24"><path d="M9 6l6 6-6 6" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  replay: `<svg viewBox="0 0 24 24"><path d="M6.2 9.2A7 7 0 1 1 5 13" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"/><path d="M4.5 4.8v4.9h4.9" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"/></svg>`,
  sleep: `<svg viewBox="0 0 24 24"><path d="M18.5 14.8A7.5 7.5 0 0 1 9.2 5.5a7.5 7.5 0 1 0 9.3 9.3z" fill="currentColor"/></svg>`,
  options: `<svg viewBox="0 0 24 24"><path d="M12 4.5v15M5.5 8.2l13 7.6M18.5 8.2l-13 7.6" fill="none" stroke="currentColor" stroke-width="2.6" stroke-linecap="round"/></svg>`,
  rew: `<svg viewBox="0 0 24 24"><path d="M11.5 6.5v11L4 12zM20 6.5v11L12.5 12z" fill="currentColor"/></svg>`,
  ff: `<svg viewBox="0 0 24 24"><path d="M12.5 6.5v11L20 12zM4 6.5v11l7.5-5.5z" fill="currentColor"/></svg>`,
  playpause: `<svg viewBox="0 0 30 24"><path d="M3 6.5v11l8.5-5.5z" fill="currentColor"/><rect x="15" y="6.5" width="3.4" height="11" rx="0.6" fill="currentColor"/><rect x="21" y="6.5" width="3.4" height="11" rx="0.6" fill="currentColor"/></svg>`,
};

/** A Roku remote: Back / Home, the blue pad, replay / sleep / options, and transport. */
function rokuRemote(): string {
  const k = (key: string, icon: string, title: string, cls = "") =>
    `<button class="rk ${cls}" data-key="${key}" title="${title}" aria-label="${title}">${ICON[icon]}</button>`;
  return `
    <div class="rremote" aria-label="Remote control">
      <div class="rrow two">${k("Back", "back", "Back")}${k("Home", "home", "Home")}</div>
      <div class="rpad">
        <div class="rpad-v"></div><div class="rpad-h"></div>
        <button class="rpad-btn up" data-key="Up" title="Up" aria-label="Up">${ICON.up}</button>
        <button class="rpad-btn down" data-key="Down" title="Down" aria-label="Down">${ICON.down}</button>
        <button class="rpad-btn left" data-key="Left" title="Left" aria-label="Left">${ICON.left}</button>
        <button class="rpad-btn right" data-key="Right" title="Right" aria-label="Right">${ICON.right}</button>
        <button class="rpad-ok" data-key="Select" title="OK">OK</button>
      </div>
      <div class="rrow three">
        ${k("InstantReplay", "replay", "Instant replay")}
        <button class="rk" disabled title="Sleep timer: Roku only allows setting this from the remote itself">${ICON.sleep}</button>
        ${k("Info", "options", "Options (✱)")}
      </div>
      <div class="rrow three transport">
        ${k("Rev", "rew", "Rewind")}${k("Play", "playpause", "Play / Pause", "wide")}${k("Fwd", "ff", "Fast forward")}
      </div>
    </div>`;
}

/** Screen status, power, inputs and playback for a TV or projector. */
function tvRow(d: Device): string {
  const tv = d.tv;
  if (!tv) return "";
  const optionList = (kind: string) => tv.inputs.filter((i) => i.kind === kind)
    .map((i) => `<option value="${esc(i.id)}">${esc(i.label)}</option>`).join("");
  const inputs = tv.inputs.length === 0 ? "" : `
      <select data-act="input" title="Switch input or open an app">
        <option value="" selected disabled>Switch to…</option>
        ${tv.inputs.some((i) => i.kind === "app")
          ? `<optgroup label="Inputs">${optionList("input")}</optgroup><optgroup label="Apps">${optionList("app")}</optgroup>`
          : optionList("input")}
      </select>`;
  // "Playing"/"Paused" already shows in the activity label; keep details like the Live TV channel.
  const detail = tv.showing_detail && !["Playing", "Paused"].includes(tv.showing_detail) ? tv.showing_detail : "";
  const off = tv.power === false;
  const title = off ? "Screen off" : tv.showing ?? (tv.power == null ? displayName(d) : "—");
  const activity = tv.power !== false && tv.activity && ACTIVITY[tv.activity]
    ? `<span class="tv-act ${ACTIVITY[tv.activity][1]}" title="${ACTIVITY[tv.activity][2]}">${ACTIVITY[tv.activity][0]}</span>` : "";
  const power = tv.has_power ? `
      <div class="pwr" title="${tv.power == null ? "This device doesn't report whether it's on" : ""}">
        <button class="${tv.power === true ? "on" : ""}" data-power="on">⏻ On</button><button class="${tv.power === false ? "off" : ""}" data-power="off">Off</button>
      </div>` : "";
  const recal = d.backend === "roku" && !tv.exact_volume
    ? `<button class="btn" data-act="recal" title="This TV doesn't report its volume, so the app estimates it. Recalibrate re-zeros that estimate on the next change.">Recalibrate volume</button>` : "";
  const actions = `${inputs}${playButtons(d)}${recal}`;
  return `
    <div class="tv-area">
      <div class="tv-main">
        <div class="tv-head">
          <div class="tv-icon ${off ? "off" : ""}">${!off && tv.showing_icon ? `<img src="${esc(tv.showing_icon)}" alt="">` : `<span>${off ? "⏻" : "▭"}</span>`}</div>
          <div class="tv-info">
            <div class="tv-label">${off ? "" : "Now showing"}</div>
            <div class="tv-title">${esc(title)}</div>
            ${detail ? `<div class="tv-sub">${esc(detail)}</div>` : ""}
            <div class="tv-badges">${activity}${tv.headphones ? `<span class="tv-badge" title="Headphones are connected (private listening), so the TV speakers are silent">🎧 Private listening</span>` : ""}</div>
          </div>
          ${power}
        </div>
        ${actions.trim() ? `<div class="tv-line">${actions}</div>` : ""}
        ${progressRow(d)}
        ${tv.restricted ? `
        <div class="tv-warn">This TV only allows limited control from apps, so it blocks power and input changes. To fix it, on the TV go to
          <b>Settings → System → Advanced system settings → Control by mobile apps → Network access</b> and choose <b>Default</b> (or <b>Permissive</b> if that still doesn't work).</div>` : ""}
        ${setupPanel(d)}
      </div>
    </div>`;
}

function wireDeviceCard(d: Device) {
  const card = content.querySelector(`.card[data-id="${CSS.escape(d.id)}"]`);
  if (!card) return;
  const q = (sel: string) => card.querySelector(sel) as HTMLElement | null;

  card.querySelectorAll<HTMLElement>("[data-power]").forEach((b) =>
    b.addEventListener("click", () => invoke("set_power", { id: d.id, on: b.dataset.power === "on" })));
  const inputSel = q('[data-act="input"]') as HTMLSelectElement | null;
  inputSel?.addEventListener("change", () => { invoke("set_input", { id: d.id, input: inputSel.value }); inputSel.blur(); });
  card.querySelectorAll<HTMLImageElement>(".tv-icon img").forEach((img) => img.addEventListener("error", () => img.remove()));
  q('[data-act="setup-toggle"]')?.addEventListener("click", () => {
    if (setupOpen.has(d.id)) setupOpen.delete(d.id); else setupOpen.add(d.id);
    render();
  });
  q('[data-act="dev-settings"]')?.addEventListener("click", async () => {
    setupMsg.set(d.id, "Pressing the buttons on the TV… watch the screen.");
    render();
    try { await invoke("roku_dev_settings", { id: d.id }); setupMsg.set(d.id, "The developer settings screen should be open on the TV."); }
    catch (e) { setupMsg.set(d.id, String(e)); }
    render();
  });
  q('[data-act="install-player"]')?.addEventListener("click", async () => {
    const pw = (card.querySelector('[data-act="dev-pw"]') as HTMLInputElement).value;
    if (!pw) { setupMsg.set(d.id, "Enter the password you chose on the TV."); render(); return; }
    setupMsg.set(d.id, "Installing the player on the TV…");
    render();
    try {
      await invoke("roku_install_player", { id: d.id, password: pw });
      setupMsg.delete(d.id);
      setupOpen.delete(d.id);
    } catch (e) { setupMsg.set(d.id, String(e)); }
    render();
  });
  q('[data-act="play-files"]')?.addEventListener("click", () => chooseAndPlay(d));
  q('[data-act="queue-edit"]')?.addEventListener("click", () => editQueue(d));
  card.querySelector<HTMLDetailsElement>(".dev-queue")?.addEventListener("toggle", (e) => {
    if ((e.target as HTMLDetailsElement).open) openQueues.add(d.id); else openQueues.delete(d.id);
  });
  q('[data-act="calendar"]')?.addEventListener("click", () => state.calendar?.showing.includes(d.id)
    ? calRun(d.id, "calendar_stop", { ids: [d.id] })
    : calRun(d.id, "calendar_show", { ids: [d.id] }));
  q('[data-act="play-url"]')?.addEventListener("click", async () => {
    const url = prompt("Video link (MP4, MKV, TS or an M3U8 live stream):");
    if (!url) return;
    try { await invoke("play_url", { id: d.id, url }); } catch (e) { alert(String(e)); }
  });
  const seekEl = card.querySelector<HTMLInputElement>("[data-seek]");
  if (seekEl) {
    const posEl = card.querySelector<HTMLElement>("[data-pos]")!;
    const key = "seek:" + d.id;
    seekEl.addEventListener("pointerdown", () => dragging.add(key));
    seekEl.addEventListener("input", () => { posEl.textContent = fmtTime(Number(seekEl.value)); });
    seekEl.addEventListener("change", () => {
      invoke("seek", { id: d.id, positionMs: Number(seekEl.value) });
      setTimeout(() => { dragging.delete(key); renderIfPending(); }, 1500);
    });
  }
  card.querySelectorAll<HTMLElement>("[data-key]").forEach((b) =>
    b.addEventListener("click", () => invoke("device_key", { id: d.id, key: b.dataset.key })));

  const slider = q('[data-act="vol"]') as HTMLInputElement | null;
  if (slider) {
    const pctEl = card.querySelector(".vol-pct")!;
    slider.addEventListener("pointerdown", () => dragging.add(d.id));
    slider.addEventListener("pointerup", () => setTimeout(() => { dragging.delete(d.id); renderIfPending(); }, 800));
    slider.addEventListener("input", () => {
      pctEl.textContent = `${slider.value}%`;
      debounce(`vol:${d.id}`, 180, () => invoke("set_volume", { id: d.id, level: Number(slider.value) / 100 }));
    });
  }
  q('[data-act="mute"]')?.addEventListener("click", () => invoke("set_muted", { id: d.id, muted: !d.muted }));
  q('[data-act="delete"]')?.addEventListener("click", () => invoke("delete_device", { id: d.id }));
  q('[data-act="recal"]')?.addEventListener("click", () => invoke("recalibrate_roku", { id: d.id }));
  const gainInput = q('[data-act="gain"]') as HTMLInputElement | null;
  gainInput?.addEventListener("change", () => {
    const pct = Math.max(0, Math.min(200, Number(gainInput.value) || 100));
    invoke("set_sync_gain", { id: d.id, gain: pct / 100 });
  });
  for (const act of ["play", "pause", "next", "prev"]) {
    q(`[data-act="${act}"]`)?.addEventListener("click", () => invoke("media_cmd", { id: d.id, action: act }));
  }
  const nameInput = q('[data-act="rename"]') as HTMLInputElement | null;
  nameInput?.addEventListener("change", () => {
    const v = nameInput.value.trim();
    invoke("rename_device", { id: d.id, name: v === d.friendly_name || v === "" ? null : v });
  });
  nameInput?.addEventListener("keydown", (e) => { if ((e as KeyboardEvent).key === "Enter") nameInput.blur(); });
}

function showAddManual() {
  const ip = prompt("Device IP address:");
  if (!ip) return;
  const kind = prompt("Type: roku / yamaha / lg / optoma", "yamaha");
  if (!kind || !["roku", "yamaha", "lg", "optoma"].includes(kind)) return;
  const name = prompt("Name for this device:", `${kind} ${ip}`) || `${kind} ${ip}`;
  const port = kind === "roku" ? 8060 : kind === "lg" ? 3000 : kind === "optoma" ? 23 : 80;
  invoke("add_manual_device", { device: { backend: kind, ip, port, name } });
}

// ---------------- groups view ----------------

/** Device types in a sync group's member picker. Google cast groups are left out on purpose. */
const MEMBER_KINDS: { key: string; title: string; match: (d: Device) => boolean }[] = [
  { key: "roku", title: "Roku TVs", match: (d) => d.backend === "roku" },
  { key: "other", title: "Other devices", match: (d) => d.backend !== "cast" && d.backend !== "roku" },
  { key: "cast", title: "Google speakers & displays", match: (d) => d.backend === "cast" },
];

function renderGroups() {
  content.innerHTML = `
    <h2>Sync Groups <span class="sub">members' volumes stay matched — independent of Google cast groups</span></h2>
    <div class="toolbar"><button class="btn primary" id="add-group">+ New Sync Group</button></div>
    ${state.groups.map((g) => groupCard(g)).join("")}
    ${state.groups.length === 0 ? `<div class="hint">Create a sync group to control several devices with one slider, keep their volumes in sync, and get quick tray presets.</div>` : ""}
  `;
  document.getElementById("add-group")!.addEventListener("click", () => {
    const name = prompt("Sync group name:");
    if (!name) return;
    const groups = [...state.groups, { id: crypto.randomUUID(), name, member_ids: [], sync_enabled: true, group_volume: 0.5 }];
    invoke("save_groups", { groups });
  });
  for (const g of state.groups) wireGroupCard(g);
}

function groupCard(g: Group): string {
  const pct = Math.round(g.group_volume * 100);
  return `
  <div class="card" data-gid="${esc(g.id)}">
    <div class="row">
      <div class="dev-name grow"><input value="${esc(g.name)}" data-act="gname" /></div>
      <button class="btn danger" data-act="gdel">Delete</button>
    </div>
    <div class="row" style="margin-top:10px">
      <input type="range" min="0" max="100" value="${pct}" data-act="gvol" />
      <span class="vol-pct">${pct}%</span>
      <button class="btn icon" data-act="gprev">⏮</button>
      <button class="btn icon" data-act="gplay">▶</button>
      <button class="btn icon" data-act="gpause">⏸</button>
      <button class="btn icon" data-act="gnext">⏭</button>
    </div>
    ${MEMBER_KINDS.map((k) => {
      const devs = state.devices.filter((d) => !d.is_cast_group && k.match(d));
      if (devs.length === 0) return "";
      const all = devs.every((d) => g.member_ids.includes(d.id));
      return `
      <div class="member-kind">
        <div class="member-head">
          <span>${k.title}</span>
          <button class="btn mini" data-select-kind="${k.key}">${all ? "Clear all" : "Select all"}</button>
        </div>
        <div class="member-list">
          ${devs.map((d) => `
            <label class="chk"><input type="checkbox" data-member="${esc(d.id)}" ${g.member_ids.includes(d.id) ? "checked" : ""} /> ${esc(displayName(d))}</label>`).join("")}
        </div>
      </div>`;
    }).join("")}
  </div>`;
}

function wireGroupCard(g: Group) {
  const card = content.querySelector(`.card[data-gid="${CSS.escape(g.id)}"]`)!;
  const saveGroups = () => invoke("save_groups", { groups: state.groups });

  (card.querySelector('[data-act="gname"]') as HTMLInputElement).addEventListener("change", (e) => {
    g.name = (e.target as HTMLInputElement).value.trim() || g.name;
    saveGroups();
  });
  card.querySelector('[data-act="gdel"]')!.addEventListener("click", () => {
    if (!confirm(`Delete group "${g.name}"?`)) return;
    invoke("save_groups", { groups: state.groups.filter((x) => x.id !== g.id) });
  });
  const slider = card.querySelector('[data-act="gvol"]') as HTMLInputElement;
  const pctEl = card.querySelector(".vol-pct")!;
  slider.addEventListener("pointerdown", () => dragging.add("g:" + g.id));
  slider.addEventListener("pointerup", () => setTimeout(() => { dragging.delete("g:" + g.id); renderIfPending(); }, 800));
  slider.addEventListener("input", () => {
    pctEl.textContent = `${slider.value}%`;
    debounce(`gvol:${g.id}`, 200, () => invoke("set_group_volume", { groupId: g.id, level: Number(slider.value) / 100 }));
  });
  for (const [act, action] of [["gplay", "play"], ["gpause", "pause"], ["gnext", "next"], ["gprev", "prev"]] as const) {
    card.querySelector(`[data-act="${act}"]`)!.addEventListener("click", () => invoke("group_media", { groupId: g.id, action }));
  }
  card.querySelectorAll<HTMLElement>("[data-select-kind]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const kind = MEMBER_KINDS.find((k) => k.key === btn.dataset.selectKind)!;
      const ids = state.devices.filter((d) => !d.is_cast_group && kind.match(d)).map((d) => d.id);
      const all = ids.every((id) => g.member_ids.includes(id));
      g.member_ids = all ? g.member_ids.filter((m) => !ids.includes(m)) : [...new Set([...g.member_ids, ...ids])];
      saveGroups();
    });
  });
  card.querySelectorAll("[data-member]").forEach((el) => {
    el.addEventListener("change", () => {
      const id = (el as HTMLElement).dataset.member!;
      const checked = (el as HTMLInputElement).checked;
      g.member_ids = checked ? [...new Set([...g.member_ids, id])] : g.member_ids.filter((m) => m !== id);
      saveGroups();
    });
  });
}

// ---------------- schedule view ----------------

function parseTime(t: string): [number, number] {
  const [h, m] = t.split(":").map(Number);
  return [Number.isFinite(h) ? h : 0, Number.isFinite(m) ? m : 0];
}

/** Hour / minute / AM-PM picker; the schedule keeps storing 24-hour "HH:MM". */
function timePicker(time: string): string {
  const [h, m] = parseTime(time);
  const h12 = h % 12 || 12;
  const pm = h >= 12;
  const minutes = Array.from({ length: 12 }, (_, i) => i * 5);
  if (!minutes.includes(m)) minutes.push(m), minutes.sort((a, b) => a - b);
  return `
    <div class="tpick" title="Time this event runs">
      <select data-t="h" aria-label="Hour">${Array.from({ length: 12 }, (_, i) => i + 1).map((v) => `<option value="${v}" ${v === h12 ? "selected" : ""}>${v}</option>`).join("")}</select>
      <span class="tsep">:</span>
      <select data-t="m" aria-label="Minute">${minutes.map((v) => `<option value="${v}" ${v === m ? "selected" : ""}>${String(v).padStart(2, "0")}</option>`).join("")}</select>
      <div class="ampm"><button type="button" class="${pm ? "" : "on"}" data-ampm="am">AM</button><button type="button" class="${pm ? "on" : ""}" data-ampm="pm">PM</button></div>
    </div>`;
}

const DAY_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function renderSchedule() {
  content.innerHTML = `
    <h2>Schedule <span class="sub">volume changes fire at the set time on checked days</span></h2>
    <div class="toolbar"><button class="btn primary" id="add-sched">+ New Scheduled Event</button></div>
    ${state.schedules.map((s) => schedCard(s)).join("")}
    ${state.schedules.length === 0 ? `<div class="hint">No scheduled events yet.</div>` : ""}
  `;
  document.getElementById("add-sched")!.addEventListener("click", () => {
    const s: Sched = { id: crypto.randomUUID(), enabled: true, days: [true, true, true, true, true, false, false], time: "22:00", targets: [] };
    invoke("save_schedules", { schedules: [...state.schedules, s] });
  });
  for (const s of state.schedules) wireSchedCard(s);
}

function schedCard(s: Sched): string {
  const targetName = (t: ScheduleTarget) =>
    t.is_group ? state.groups.find((g) => g.id === t.target_id)?.name ?? "(deleted group)"
               : (() => { const d = state.devices.find((d) => d.id === t.target_id); return d ? displayName(d) : "(unknown device)"; })();
  return `
  <div class="card" data-sid="${esc(s.id)}">
    <div class="row wrap">
      <label class="chk"><input type="checkbox" data-act="en" ${s.enabled ? "checked" : ""} /> Enabled</label>
      ${timePicker(s.time)}
      <div class="day-row">
        ${DAY_NAMES.map((n, i) => `<label>${n}<input type="checkbox" data-day="${i}" ${s.days[i] ? "checked" : ""} /></label>`).join("")}
      </div>
      <div class="grow"></div>
      <button class="btn danger" data-act="sdel">Delete</button>
    </div>
    <div style="margin-top:10px">
      ${s.targets.map((t, i) => `
        <div class="row" style="margin-top:6px">
          <span class="grow">${t.is_group ? "👥" : "🔈"} ${esc(targetName(t))}</span>
          <input type="number" min="0" max="100" step="5" value="${t.volume_pct}" data-tvol="${i}" style="width:70px" /> %
          <button class="btn icon danger" data-tdel="${i}">✕</button>
        </div>`).join("")}
      <div class="row" style="margin-top:8px">
        <select data-act="addsel">
          <option value="">Add target…</option>
          ${state.groups.map((g) => `<option value="g:${esc(g.id)}">Group: ${esc(g.name)}</option>`).join("")}
          ${state.devices.map((d) => `<option value="d:${esc(d.id)}">Device: ${esc(displayName(d))}</option>`).join("")}
        </select>
      </div>
    </div>
  </div>`;
}

function wireSchedCard(s: Sched) {
  const card = content.querySelector(`.card[data-sid="${CSS.escape(s.id)}"]`)!;
  const saveAll = () => invoke("save_schedules", { schedules: state.schedules });

  (card.querySelector('[data-act="en"]') as HTMLInputElement).addEventListener("change", (e) => {
    s.enabled = (e.target as HTMLInputElement).checked; saveAll();
  });
  const tp = card.querySelector<HTMLElement>(".tpick")!;
  const setTime = (h12: number, m: number, pm: boolean) => {
    s.time = `${String((h12 % 12) + (pm ? 12 : 0)).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
    saveAll();
  };
  const cur = () => {
    const [h, m] = parseTime(s.time);
    return { h12: h % 12 || 12, m, pm: h >= 12 };
  };
  tp.querySelector<HTMLSelectElement>('[data-t="h"]')!.addEventListener("change", (e) => {
    const c = cur(); setTime(Number((e.target as HTMLSelectElement).value), c.m, c.pm);
  });
  tp.querySelector<HTMLSelectElement>('[data-t="m"]')!.addEventListener("change", (e) => {
    const c = cur(); setTime(c.h12, Number((e.target as HTMLSelectElement).value), c.pm);
  });
  tp.querySelectorAll<HTMLElement>("[data-ampm]").forEach((b) => b.addEventListener("click", () => {
    const c = cur(); setTime(c.h12, c.m, b.dataset.ampm === "pm");
  }));
  card.querySelectorAll("[data-day]").forEach((el) => el.addEventListener("change", () => {
    s.days[Number((el as HTMLElement).dataset.day)] = (el as HTMLInputElement).checked; saveAll();
  }));
  card.querySelector('[data-act="sdel"]')!.addEventListener("click", () =>
    invoke("save_schedules", { schedules: state.schedules.filter((x) => x.id !== s.id) }));
  card.querySelectorAll("[data-tvol]").forEach((el) => el.addEventListener("change", () => {
    s.targets[Number((el as HTMLElement).dataset.tvol)].volume_pct = Math.max(0, Math.min(100, Number((el as HTMLInputElement).value))); saveAll();
  }));
  card.querySelectorAll("[data-tdel]").forEach((el) => el.addEventListener("click", () => {
    s.targets.splice(Number((el as HTMLElement).dataset.tdel), 1); saveAll();
  }));
  (card.querySelector('[data-act="addsel"]') as HTMLSelectElement).addEventListener("change", (e) => {
    const v = (e.target as HTMLSelectElement).value;
    if (!v) return;
    s.targets.push({ target_id: v.slice(2), is_group: v.startsWith("g:"), volume_pct: 50 });
    saveAll();
  });
}

// ---------------- log view ----------------

async function renderLog() {
  content.innerHTML = `
    <h2>Log <span class="sub">discoveries, observed changes and sent commands</span></h2>
    <div class="toolbar">
      <button class="btn" id="log-back">← Settings</button>
      <input type="text" id="log-filter" placeholder="Filter…" style="width:240px" />
      <button class="btn" id="open-logs">Open log folder</button>
    </div>
    <div class="log-box" id="log-box">Loading…</div>`;
  document.getElementById("open-logs")!.addEventListener("click", () => invoke("open_log_folder"));
  document.getElementById("log-back")!.addEventListener("click", () => { view = "settings"; render(); });
  const filterEl = document.getElementById("log-filter") as HTMLInputElement;
  const refresh = async () => {
    const lines = await invoke<string[]>("get_log_tail", { lines: 500 });
    const f = filterEl.value.toLowerCase();
    const box = document.getElementById("log-box");
    if (!box) return;
    const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 40;
    box.textContent = lines.filter((l) => !f || l.toLowerCase().includes(f)).join("\n");
    if (atBottom) box.scrollTop = box.scrollHeight;
  };
  filterEl.addEventListener("input", refresh);
  await refresh();
  logTimer = window.setInterval(refresh, 2000);
}

// ---------------- settings view ----------------

function renderSettings() {
  content.innerHTML = `
    <h2>Settings</h2>
    <div class="card">
      <label class="chk"><input type="checkbox" id="set-autostart" ${state.settings.start_with_windows ? "checked" : ""} /> Start with Windows (minimized to tray)</label>
    </div>
    <div class="card">
      <label class="chk"><input type="checkbox" id="set-update" ${state.settings.auto_update ? "checked" : ""} /> Automatic updates (checks GitHub releases)</label>
      <div class="hint">Checks this project's GitHub releases on startup and offers to install newer versions.</div>
    </div>
    <div class="card">
      <div class="row">
        <button class="btn" id="cfg-export">Export config…</button>
        <button class="btn" id="cfg-import">Import config…</button>
      </div>
      <div class="hint">Exports devices, custom names, groups and schedules as a JSON file.</div>
    </div>
    <div class="card">
      <div class="support-row">
        <button class="btn donate" id="support-donate">&#9749; Buy me a coffee</button>
        <button class="btn" id="support-site">Website</button>
        <button class="btn" id="support-repo">Source on GitHub</button>
      </div>
      <div class="hint">This app is free and open source. If it made your house sound better, a small tip keeps it that way. Links open in your browser.</div>
    </div>
    <div class="card">
      <div class="credits-title">Troubleshooting</div>
      <div class="support-row" style="margin-top:6px">
        <button class="btn" id="view-log">View log</button>
        <button class="btn" id="open-log-folder">Open log folder</button>
      </div>
      <div class="hint">Every discovery, observed volume change and sent command, with each device's Cast ID and address.</div>
    </div>
    <div class="card">
      <div class="credits-title">Open-source credits</div>
      <p class="credits">Built with <b>Tauri</b>, <b>Tokio</b>, <b>Serde</b>, <b>mdns-sd</b>, <b>prost</b>, <b>reqwest</b>, <b>rustls</b>, <b>native-tls</b>, <b>tungstenite</b>, <b>tracing</b> and <b>chrono</b>, plus about 350 other open-source packages. Thank you to everyone who maintains them.</p>
      <p class="credits">The Google Cast message format comes from Chromium's <i>cast_channel.proto</i> (BSD-3-Clause, The Chromium Authors). Roku control follows Roku's published External Control Protocol documentation.</p>
      <p class="credits">Casting files and links, queues and subtitles were inspired by <b>Web Video Caster</b> (webvideocaster.com), which does this across many kinds of devices. Volume Sync isn't affiliated with it and uses none of its code.</p>
      <div class="support-row" style="margin-top:10px"><button class="btn" id="open-notices">View all licenses</button></div>
    </div>`;
  document.getElementById("open-notices")!.addEventListener("click", () => invoke("open_notices"));
  document.getElementById("view-log")!.addEventListener("click", () => { view = "log"; render(); });
  document.getElementById("open-log-folder")!.addEventListener("click", () => invoke("open_log_folder"));
  document.getElementById("support-donate")!.addEventListener("click", () => openUrl(DONATE_URL));
  document.getElementById("support-site")!.addEventListener("click", () => openUrl(SITE_URL));
  document.getElementById("support-repo")!.addEventListener("click", () => openUrl(REPO_URL));
  const push = () => invoke("set_settings", { settings: {
    start_with_windows: (document.getElementById("set-autostart") as HTMLInputElement).checked,
    auto_update: (document.getElementById("set-update") as HTMLInputElement).checked,
  }});
  document.getElementById("set-autostart")!.addEventListener("change", push);
  document.getElementById("set-update")!.addEventListener("change", push);
  document.getElementById("cfg-export")!.addEventListener("click", async () => {
    const path = await save({ defaultPath: "volume-sync-config.json", filters: [{ name: "JSON", extensions: ["json"] }] });
    if (path) await invoke("export_config", { path });
  });
  document.getElementById("cfg-import")!.addEventListener("click", async () => {
    const path = await open({ multiple: false, filters: [{ name: "JSON", extensions: ["json"] }] });
    if (typeof path === "string") await invoke("import_config", { path });
  });
}

// ---------------- shell ----------------

function render() {
  const navView = view === "log" ? "settings" : view;
  document.querySelectorAll<HTMLElement>(".nav-btn").forEach((b) => b.classList.toggle("active", b.dataset.view === navView));
  if (logTimer) { clearInterval(logTimer); logTimer = undefined; }
  switch (view) {
    case "devices": return renderDevices();
    case "groups": return renderGroups();
    case "schedule": return renderSchedule();
    case "log": return void renderLog();
    case "settings": return renderSettings();
    case "calendar": return renderCalendar();
  }
}

document.querySelectorAll(".nav-btn").forEach((btn) => {
  btn.addEventListener("click", () => {
    document.querySelectorAll(".nav-btn").forEach((b) => b.classList.remove("active"));
    btn.classList.add("active");
    view = (btn as HTMLElement).dataset.view!;
    render();
  });
});

let pendingRender = false;

listen<Snapshot>("state", (e) => {
  state = e.payload;
  // Avoid clobbering UI while user drags a slider or edits text - but still
  // live-update the OTHER sliders so sync is visible during a drag.
  const active = document.activeElement;
  const editing = (active instanceof HTMLInputElement && (active.type === "text" || active.type === "password" || active.type === "time" || active.type === "number"))
    || active instanceof HTMLSelectElement;
  if (!sideDragging()) renderSide();
  if (dragging.size === 0 && !editing && view !== "log") {
    pendingRender = false;
    render();
  } else if (view !== "log") {
    pendingRender = true;
    updateSlidersInPlace();
  }
});

function updateSlidersInPlace() {
  for (const d of state.devices) {
    if (dragging.has(d.id)) continue;
    const card = content.querySelector(`.card[data-id="${CSS.escape(d.id)}"]`);
    if (!card) continue;
    const slider = card.querySelector('[data-act="vol"]') as HTMLInputElement | null;
    const pctEl = card.querySelector(".vol-pct");
    if (slider && document.activeElement !== slider) {
      const pct = Math.round(d.volume * 100);
      slider.value = String(pct);
      if (pctEl) pctEl.textContent = `${pct}%`;
    }
  }
}

function renderIfPending() {
  if (pendingRender && dragging.size === 0) {
    pendingRender = false;
    render();
  }
}

invoke<Snapshot>("get_state").then((s) => { state = s; render(); renderSide(); maybeCheckUpdates(); });

async function maybeCheckUpdates() {
  if (!state.settings.auto_update) return;
  try {
    const { check } = await import("@tauri-apps/plugin-updater");
    const update = await check();
    if (update) {
      if (confirm(`Update ${update.version} is available. Install now?`)) {
        await update.downloadAndInstall();
      }
    }
  } catch {
    // No releases published yet or offline - silently ignore.
  }
}

// sidebar footer links (present on every view)
document.getElementById("nav-donate")?.addEventListener("click", () => openUrl(DONATE_URL));
document.getElementById("nav-site")?.addEventListener("click", () => openUrl(SITE_URL));

// Keep progress bars moving between status updates.
setInterval(() => {
  document.querySelectorAll<HTMLElement>("[data-sprog]").forEach((el) => {
    const d = state.devices.find((x) => x.id === el.dataset.sprog);
    if (!d?.media || dragging.has("side:seek:" + d.id)) return;
    const pos = mediaPos(d.media);
    const input = el.querySelector<HTMLInputElement>("[data-sseek]");
    if (input) input.value = String(Math.round(pos));
    const t = el.querySelector<HTMLElement>("[data-spos]");
    if (t) t.textContent = fmtTime(pos);
  });
  document.querySelectorAll<HTMLElement>("[data-prog]").forEach((el) => {
    const d = state.devices.find((x) => x.id === el.dataset.prog);
    if (!d?.tv || dragging.has("seek:" + d.id)) return;
    const pos = livePosition(d.tv);
    const input = el.querySelector<HTMLInputElement>("[data-seek]");
    if (input) input.value = String(Math.round(pos));
    const t = el.querySelector<HTMLElement>("[data-pos]");
    if (t) t.textContent = fmtTime(pos);
  });
}, 1000);
