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
interface Snapshot { devices: Device[]; groups: Group[]; schedules: Sched[]; settings: Settings; }

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
  if (sessions.length === 0) { sideEl.innerHTML = ""; return; }
  const shown = sessions.slice(0, 3);
  const items = sideVolItems(sessions);
  const averaged = items.length > MAX_SLIDERS;
  const avg = items.reduce((a, i) => a + i.level, 0) / Math.max(1, items.length);

  sideEl.innerHTML = `
    <div class="side-head">Now Playing</div>
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

  sideEl.querySelectorAll<HTMLElement>(".side-np").forEach((el) => {
    const id = el.dataset.np!;
    el.querySelectorAll<HTMLElement>("[data-np-act]").forEach((b) =>
      b.addEventListener("click", () => invoke("media_cmd", { id, action: b.dataset.npAct })));
    el.querySelector<HTMLImageElement>("img.side-art")?.addEventListener("error", (e) => (e.target as HTMLElement).remove());
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

// ---------------- devices view ----------------

function renderDevices() {
  const stale = (d: Device) => !d.online && Date.now() / 1000 - d.last_seen > 86400;
  const html = `
    <h2>Devices <span class="sub">${state.devices.filter((d) => d.online).length} online / ${state.devices.length} known</span></h2>
    <div class="toolbar">
      <button class="btn" id="scan-roku">Scan for Roku TVs</button>
      <button class="btn" id="add-manual">Add device by IP…</button>
    </div>
    ${section("Devices", state.devices.filter((d) => !d.is_cast_group && d.backend === "cast"), stale)}
    ${section("Google Cast Groups", state.devices.filter((d) => d.is_cast_group), stale)}
    ${section("Roku TVs", state.devices.filter((d) => d.backend === "roku"), stale)}
    ${section("Other Devices", state.devices.filter((d) => !["cast", "roku"].includes(d.backend)), stale)}
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
  return `
  <div class="card compact ${d.online ? "" : "offline"}" data-id="${esc(d.id)}">
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
  </div>`;
}

/** Screen, input picker and remote for TVs that report them (Roku). */
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
  const showing = tv.power === false ? `<span class="tv-now off">Screen off</span>` : tv.showing ? `
      <span class="tv-now" title="Now showing">
        ${tv.showing_icon ? `<img src="${esc(tv.showing_icon)}" alt="">` : ""}
        <span><b>${esc(tv.showing)}</b>${tv.showing_detail ? `<i>${esc(tv.showing_detail)}</i>` : ""}</span>
      </span>` : "";
  return `
    <div class="tv-row">
      ${tv.has_power ? `
      <div class="pwr" title="${tv.power == null ? "This device doesn't report whether it's on" : ""}">
        <button class="${tv.power === true ? "on" : ""}" data-power="on">⏻ On</button><button class="${tv.power === false ? "off" : ""}" data-power="off">Off</button>
      </div>` : ""}
      ${showing}
      ${tv.headphones ? `<span class="tv-badge" title="Headphones are connected (private listening), so the TV speakers are silent">🎧 Private listening</span>` : ""}
      <span class="grow"></span>
      ${inputs}
      ${d.backend === "roku" ? `<button class="btn" data-act="recal" title="Re-zero the volume calibration on the next volume change">Recalibrate volume</button>` : ""}
    </div>${tv.restricted ? `
    <div class="tv-warn">This TV only allows limited control from apps, so it blocks power and input changes. To fix it, on the TV go to
      <b>Settings → System → Advanced system settings → Control by mobile apps → Network access</b> and choose <b>Default</b> (or <b>Permissive</b> if that still doesn't work).</div>` : ""}${d.backend !== "roku" ? "" : `
    <div class="remote">
      <div class="dpad">
        <span></span><button class="btn" data-key="Up" title="Up">▲</button><span></span>
        <button class="btn" data-key="Left" title="Left">◀</button><button class="btn ok" data-key="Select">OK</button><button class="btn" data-key="Right" title="Right">▶</button>
        <span></span><button class="btn" data-key="Down" title="Down">▼</button><span></span>
      </div>
      <div class="rkeys">
        <div><button class="btn" data-key="Back">Back</button><button class="btn" data-key="Home">Home</button><button class="btn" data-key="Info" title="Options">✱</button></div>
        <div><button class="btn" data-key="Rev" title="Rewind">⏪</button><button class="btn" data-key="Play" title="Play/Pause">⏯</button><button class="btn" data-key="Fwd" title="Fast forward">⏩</button></div>
        <div><button class="btn" data-key="InstantReplay" title="Instant replay">↺ Replay</button><button class="btn" data-key="ChannelUp" title="Channel up">CH ▲</button><button class="btn" data-key="ChannelDown" title="Channel down">CH ▼</button></div>
      </div>
    </div>`}`;
}

function wireDeviceCard(d: Device) {
  const card = content.querySelector(`.card[data-id="${CSS.escape(d.id)}"]`);
  if (!card) return;
  const q = (sel: string) => card.querySelector(sel) as HTMLElement | null;

  card.querySelectorAll<HTMLElement>("[data-power]").forEach((b) =>
    b.addEventListener("click", () => invoke("set_power", { id: d.id, on: b.dataset.power === "on" })));
  const inputSel = q('[data-act="input"]') as HTMLSelectElement | null;
  inputSel?.addEventListener("change", () => { invoke("set_input", { id: d.id, input: inputSel.value }); inputSel.blur(); });
  card.querySelectorAll<HTMLImageElement>(".tv-now img").forEach((img) => img.addEventListener("error", () => img.remove()));
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
  { key: "cast", title: "Google speakers & displays", match: (d) => d.backend === "cast" },
  { key: "roku", title: "Roku TVs", match: (d) => d.backend === "roku" },
  { key: "other", title: "Other devices", match: (d) => d.backend !== "cast" && d.backend !== "roku" },
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
  const editing = (active instanceof HTMLInputElement && (active.type === "text" || active.type === "time" || active.type === "number"))
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
