import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save, open } from "@tauri-apps/plugin-dialog";

type Backend = "cast" | "roku" | "yamaha" | "lg" | "optoma";

interface MediaInfo {
  state: string;
  title?: string;
  artist?: string;
  app?: string;
  supports_transport: boolean;
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
  media?: MediaInfo;
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
        <div class="dev-meta">${d.is_cast_group ? "cast group · " : ""}${esc(d.model)}${!d.online ? ` · last seen ${ago(d.last_seen)}` : ""}</div>
      </div>
      <button class="btn icon ${d.muted ? "muted-on" : ""}" data-act="mute" title="Mute">${d.muted ? "🔇" : "🔊"}</button>
      <input type="range" min="0" max="100" value="${pct}" data-act="vol" />
      <span class="vol-pct">${pct}%</span>
      ${d.media?.supports_transport ? `
        <button class="btn icon" data-act="prev">⏮</button>
        <button class="btn icon" data-act="${d.media.state === "PLAYING" ? "pause" : "play"}">${d.media.state === "PLAYING" ? "⏸" : "▶"}</button>
        <button class="btn icon" data-act="next">⏭</button>` : ""}
      ${d.backend === "roku" ? `<button class="btn icon" data-act="recal" title="Re-zero volume calibration on next set">🎯</button>` : ""}
      ${stale ? `<button class="btn danger icon" data-act="delete" title="Remove until seen again">✕</button>` : ""}
      <label class="gain-wrap" title="Sync Gain (0–200%, default 100%): balance this device inside sync groups. Its actual volume = group volume × gain; it reports volume ÷ gain back to the group. Example: at 50% gain, group volume 30% puts this device at 15%.">
        <span>Sync Gain</span>
        <div class="gain-col">
          <input type="number" min="0" max="200" step="5" value="${Math.round((d.sync_gain ?? 1) * 100)}" data-act="gain" />
          <div class="gain-bar"><div class="gain-fill ${((d.sync_gain ?? 1) > 1) ? "hot" : ""}" style="width:${Math.min(100, ((d.sync_gain ?? 1) * 100) / 2)}%"></div><div class="gain-mid"></div></div>
        </div>
      </label>
    </div>
    ${media}
  </div>`;
}

function wireDeviceCard(d: Device) {
  const card = content.querySelector(`.card[data-id="${CSS.escape(d.id)}"]`);
  if (!card) return;
  const q = (sel: string) => card.querySelector(sel) as HTMLElement | null;

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
    <div class="member-list">
      ${state.devices.filter((d) => !d.is_cast_group).map((d) => `
        <label class="chk"><input type="checkbox" data-member="${esc(d.id)}" ${g.member_ids.includes(d.id) ? "checked" : ""} /> ${esc(displayName(d))}</label>
      `).join("")}
    </div>
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
      <input type="time" data-act="time" value="${esc(s.time)}" />
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
  (card.querySelector('[data-act="time"]') as HTMLInputElement).addEventListener("change", (e) => {
    s.time = (e.target as HTMLInputElement).value; saveAll();
  });
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
    <h2>Log</h2>
    <div class="toolbar">
      <input type="text" id="log-filter" placeholder="Filter…" style="width:240px" />
      <button class="btn" id="open-logs">Open log folder</button>
    </div>
    <div class="log-box" id="log-box">Loading…</div>`;
  document.getElementById("open-logs")!.addEventListener("click", () => invoke("open_log_folder"));
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
    </div>`;
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
  const editing = active instanceof HTMLInputElement && (active.type === "text" || active.type === "time" || active.type === "number");
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

invoke<Snapshot>("get_state").then((s) => { state = s; render(); maybeCheckUpdates(); });

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
