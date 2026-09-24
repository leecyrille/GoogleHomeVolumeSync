'use strict';

/* ============================== configuration ============================== */

// 'side' (default) or 'bottom' — the fallback if the side column crowds 1440p.
const TASK_PANEL_POSITION = 'side';

// TV mode: drawn off-screen by Volume Sync and saved as pictures for TVs (?tv=1 to preview).
const IS_TV = !!window.__TV__ || new URLSearchParams(location.search).has('tv');
const IS_HOSTED = !IS_TV && !!(window.chrome && window.chrome.webview); // false when previewed in a plain browser
if (IS_TV) document.documentElement.classList.add('tv');

/* ============================== state ============================== */

let payload = null;          // last data payload from the host
let todayKey = dateKey(new Date());
const slideshow = createSlideshow(document.getElementById('photos'));

/* ============================== helpers ============================== */

function dateKey(d) {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
}
function parseDateOnly(s) {
  const [y, m, d] = s.split('-').map(Number);
  return new Date(y, m - 1, d);
}
function fmtTime(d) {
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`;
}
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}
function shuffle(arr) {
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
}

/* ============================== calendar rendering ============================== */

function monthModel(year, month /* 1-12 */) {
  const first = new Date(year, month - 1, 1);
  const offset = (first.getDay() + 6) % 7;           // Monday-start
  const daysInMonth = new Date(year, month, 0).getDate();
  const weeks = Math.ceil((offset + daysInMonth) / 7);
  const gridStart = new Date(year, month - 1, 1 - offset);
  const days = [];
  for (let i = 0; i < weeks * 7; i++) {
    const d = new Date(gridStart);
    d.setDate(gridStart.getDate() + i);
    days.push(d);
  }
  return { days, weeks };
}

function indexEvents(data) {
  const chipsByDay = new Map();
  const spans = []; // all-day and 24h+ timed events, rendered as continuous bars
  const chip = (k, item) => {
    if (!chipsByDay.has(k)) chipsByDay.set(k, []);
    chipsByDay.get(k).push(item);
  };
  for (const ev of data.events) {
    const color = (data.feeds[ev.feed] && data.feeds[ev.feed].color) || '#7aa2f7';
    if (ev.allDay) {
      const start = parseDateOnly(ev.start);
      spans.push({ start, end: parseDateOnly(ev.end), title: ev.title, color, sort: start.getTime() });
    } else {
      const start = new Date(ev.start);
      const end = new Date(ev.end || ev.start);
      // Timed events lasting a day or more (e.g. week-long events entered with times)
      // become spanning bars; short events that merely cross midnight stay chips.
      if (end - start >= 24 * 3600 * 1000) {
        const startDay = new Date(start.getFullYear(), start.getMonth(), start.getDate());
        const lastDay = new Date(end.getFullYear(), end.getMonth(), end.getDate());
        if (end.getHours() === 0 && end.getMinutes() === 0) lastDay.setDate(lastDay.getDate() - 1);
        spans.push({
          start: startDay, end: lastDay,
          title: `${fmtTime(start)} ${ev.title}`, color, sort: start.getTime(),
        });
      } else {
        chip(dateKey(start), { title: ev.title, color, time: fmtTime(start), endTime: end > start ? fmtTime(end) : '', sort: start.getTime() });
      }
    }
  }
  for (const list of chipsByDay.values()) {
    list.sort((a, b) => a.sort - b.sort || a.title.localeCompare(b.title));
  }
  return { chipsByDay, spans };
}

/* For each week row, give every span segment a stable lane (Google Calendar style):
   a bar keeps its vertical slot for the whole week, and freed slots are padded with
   invisible spacers so bars never jump lanes mid-span. */
function layoutWeekSegments(spans, days, weeks) {
  const DAY = 86400000;
  const weekSegs = [];
  for (let w = 0; w < weeks; w++) {
    const rowStart = days[w * 7];
    const rowEnd = days[w * 7 + 6];
    const segs = [];
    for (const sp of spans) {
      if (sp.end < rowStart || sp.start > rowEnd) continue;
      segs.push({
        sp,
        fromCol: Math.max(0, Math.round((sp.start - rowStart) / DAY)),
        toCol: Math.min(6, Math.round((sp.end - rowStart) / DAY)),
        startsHere: sp.start >= rowStart,
        endsHere: sp.end <= rowEnd,
        lane: 0,
      });
    }
    segs.sort((a, b) => a.fromCol - b.fromCol ||
      (b.toCol - b.fromCol) - (a.toCol - a.fromCol) ||
      a.sp.title.localeCompare(b.sp.title));
    const laneEnds = [];
    for (const seg of segs) {
      let lane = 0;
      while (laneEnds[lane] !== undefined && laneEnds[lane] >= seg.fromCol) lane++;
      laneEnds[lane] = seg.toCol;
      seg.lane = lane;
    }
    weekSegs.push(segs);
  }
  return weekSegs;
}

function setTitle(main, sub) {
  const title = document.getElementById('month-title');
  title.textContent = '';
  title.append(main);
  title.append(Object.assign(el('span', 'year'), { textContent: sub }));
}

/* The seven days (Monday first) of the week holding `d`. */
function weekOf(d) {
  const start = new Date(d.getFullYear(), d.getMonth(), d.getDate() - ((d.getDay() + 6) % 7));
  return Array.from({ length: 7 }, (_, i) => new Date(start.getFullYear(), start.getMonth(), start.getDate() + i));
}

function render() {
  if (!payload) return;
  const { year, month } = payload;
  const now = new Date();
  todayKey = dateKey(now);
  const view = IS_TV ? (payload.view || 'month') : 'month';
  document.getElementById('root').dataset.view = view;

  // header
  const monthName = (d) => d.toLocaleString(undefined, { month: 'long' });
  if (view === 'week') {
    const days = weekOf(now);
    const a = days[0], b = days[6];
    setTitle(`${monthName(a)} ${a.getDate()} – ${a.getMonth() === b.getMonth() ? '' : monthName(b) + ' '}${b.getDate()}`, String(b.getFullYear()));
  } else if (view === 'day') {
    const big = document.documentElement.classList.contains('big-text');
    setTitle(now.toLocaleString(undefined, { weekday: 'long' }), now.toLocaleString(undefined, { month: big ? 'short' : 'long', day: 'numeric' }));
  } else {
    setTitle(monthName(new Date(year, month - 1, 1)), String(year));
  }

  // legend: one dot per visible feed
  const legend = document.getElementById('legend');
  legend.textContent = '';
  for (const feed of payload.feeds) {
    if (feed.enabled === false) continue;
    const item = el('div', 'legend-item');
    const dot = el('div', 'dot');
    dot.style.setProperty('--c', feed.color || '#7aa2f7');
    item.append(dot, el('span', null, feed.name));
    legend.append(item);
  }

  const dowRow = document.getElementById('dow-row');
  dowRow.textContent = '';
  const base = new Date(2024, 0, 1); // a Monday
  for (let i = 0; i < 7; i++) {
    const d = new Date(base);
    d.setDate(base.getDate() + i);
    dowRow.append(el('div', null, d.toLocaleString(undefined, { weekday: 'short' })));
  }

  // grid
  const { chipsByDay, spans } = indexEvents(payload);
  const grid = document.getElementById('grid');
  grid.textContent = '';
  grid.style.gridTemplateColumns = '';
  if (view === 'day') {
    renderDay(grid, now, chipsByDay, spans);
  } else {
    renderGrid(grid, view === 'week' ? { days: weekOf(now), weeks: 1 } : monthModel(year, month), view === 'week' ? -1 : month, chipsByDay, spans);
  }

  renderTasks();
  renderStatus();
  updateClock();
  fitEvents();

  slideshow.update(payload.photos || [], payload.photoIntervalSeconds || 20);
  // On TVs the photo column stays for the clock even without photos.
  document.getElementById('root').classList.toggle('no-photos', !IS_TV && !(payload.photos || []).length);
}

/* Month or week: a grid of day cells with continuous bars for multi-day events. */
function renderGrid(grid, { days, weeks }, month, chipsByDay, spans) {
  const weekSegs = layoutWeekSegments(spans, days, weeks);
  grid.style.gridTemplateRows = `repeat(${weeks}, minmax(0, 1fr))`;

  const frag = document.createDocumentFragment();
  days.forEach((d, i) => {
    const key = dateKey(d);
    const cell = el('div', 'cell');
    const w = Math.floor(i / 7);
    const col = i % 7;
    if (col >= 5) cell.classList.add('weekend');
    if (month > 0 && d.getMonth() !== month - 1) cell.classList.add('out');
    if (key === todayKey) cell.classList.add('today');

    const head = el('div', 'day-head');
    head.append(el('div', 'day-num', String(d.getDate())));
    cell.append(head);

    // spanning bars: one visually-continuous bar per event across the week row
    const cellSegs = weekSegs[w].filter((s) => s.fromCol <= col && col <= s.toCol);
    if (cellSegs.length) {
      const bars = el('div', 'bars');
      const maxLane = Math.max(...cellSegs.map((s) => s.lane));
      for (let lane = 0; lane <= maxLane; lane++) {
        const seg = cellSegs.find((s) => s.lane === lane);
        if (!seg) {
          bars.append(el('div', 'bar spacer', ' '));
          continue;
        }
        const bar = el('div', 'bar');
        bar.style.setProperty('--c', seg.sp.color);
        if (col === seg.fromCol) {
          bar.textContent = seg.sp.title;         // title once per week row
          if (!seg.startsHere) bar.classList.add('sq-left');
        } else {
          bar.textContent = ' ';
          bar.classList.add('cont-left');
        }
        if (col < seg.toCol) bar.classList.add('cont-right');
        else if (!seg.endsHere) bar.classList.add('sq-right');
        bars.append(bar);
      }
      cell.append(bars);
    }

    const events = el('div', 'events');
    const chips = chipsByDay.get(key);
    if (chips) {
      for (const chip of chips) {
        const node = el('div', 'chip');
        node.style.setProperty('--c', chip.color);
        node.append(el('span', 't', chip.time));
        node.append(el('span', 's', chip.title));
        events.append(node);
      }
    }
    cell.append(events);
    frag.append(cell);
  });
  grid.append(frag);
}

/* Day: today's agenda, large, with tomorrow beside it. */
function renderDay(grid, now, chipsByDay, spans) {
  grid.style.gridTemplateRows = 'minmax(0, 1fr)';
  // Small screens (big text): today only, full width.
  const bigText = document.documentElement.classList.contains('big-text');
  grid.style.gridTemplateColumns = bigText ? '1fr' : '2fr 1fr';
  const days = [new Date(now.getFullYear(), now.getMonth(), now.getDate()),
                new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1)].slice(0, bigText ? 1 : 2);
  days.forEach((d, i) => {
    const key = dateKey(d);
    const cell = el('div', 'cell agenda' + (i === 0 ? ' today' : ' tomorrow'));
    const head = el('div', 'agenda-head');
    head.append(el('span', 'agenda-label', i === 0 ? 'Today' : 'Tomorrow'));
    head.append(el('span', 'agenda-date', d.toLocaleString(undefined, { weekday: 'long', month: 'long', day: 'numeric' })));
    cell.append(head);

    const covering = spans.filter((sp) => sp.start <= d && d <= sp.end);
    if (covering.length) {
      const bars = el('div', 'bars');
      for (const sp of covering) {
        const bar = el('div', 'bar', sp.title);
        bar.style.setProperty('--c', sp.color);
        bars.append(bar);
      }
      cell.append(bars);
    }
    const events = el('div', 'events');
    const chips = chipsByDay.get(key) || [];
    for (const chip of chips) {
      const node = el('div', 'chip');
      node.style.setProperty('--c', chip.color);
      node.append(el('span', 't', chip.endTime ? `${chip.time} – ${chip.endTime}` : chip.time));
      node.append(el('span', 's', chip.title));
      events.append(node);
    }
    if (!chips.length && !covering.length) events.append(el('div', 'agenda-empty', 'Nothing scheduled'));
    cell.append(events);
    grid.append(cell);
  });
}

/* ---------- auto-scale so the busiest day still shows every event ---------- */

function overflows() {
  for (const cell of document.querySelectorAll('#grid .cell')) {
    if (cell.scrollHeight > cell.clientHeight + 1) return true; // bars + chips together
    const events = cell.querySelector('.events');
    if (events && events.scrollHeight > events.clientHeight + 1) return true;
  }
  return false;
}

function fitEvents() {
  const root = document.documentElement;
  const setScale = (v) => root.style.setProperty('--scale', String(v));

  setScale(1);
  if (!overflows()) return;

  // On TVs the chosen text size wins: shrink a little at most, then "+N more".
  let lo = IS_TV ? 0.8 : 0.35, hi = 1;   // find the largest scale that fits
  for (let i = 0; i < 9; i++) {
    const mid = (lo + hi) / 2;
    setScale(mid);
    if (overflows()) hi = mid; else lo = mid;
  }
  setScale(lo);
  if (IS_TV) addMoreLabels();
}

/* Days with more events than fit: hide the last ones behind "+N more". */
function addMoreLabels() {
  for (const events of document.querySelectorAll('#grid .events')) {
    const chips = [...events.querySelectorAll('.chip')];
    if (events.scrollHeight <= events.clientHeight + 1 || !chips.length) continue;
    const more = el('div', 'more');
    events.append(more);
    let hidden = 0;
    while (events.scrollHeight > events.clientHeight + 1 && hidden < chips.length) {
      chips[chips.length - 1 - hidden].style.display = 'none';
      hidden++;
      more.textContent = `+${hidden} more`;
    }
  }
}

/* ============================== tasks ============================== */

function renderTasks() {
  const list = document.getElementById('task-list');
  list.textContent = '';
  const tasks = (payload.tasks || []).slice();
  document.getElementById('root').classList.toggle('no-tasks', tasks.length === 0);
  if (!tasks.length) return;

  const today = todayKey;
  const rank = (t) => (t.due ? (t.due < today ? 0 : 1) : 2); // overdue, dated, undated
  tasks.sort((a, b) => rank(a) - rank(b) || (a.due || '9999').localeCompare(b.due || '9999') || a.title.localeCompare(b.title));

  for (const task of tasks) {
    const color = (payload.feeds[task.feed] && payload.feeds[task.feed].color) || '#7aa2f7';
    const node = el('div', 'task');
    const dot = el('div', 'dot');
    dot.style.setProperty('--c', color);
    const body = el('div', 'body');
    body.append(el('div', 'title', task.title));
    if (task.due) {
      const due = parseDateOnly(task.due);
      const overdue = task.due < today;
      if (overdue) node.classList.add('overdue');
      const label = overdue
        ? `Overdue · ${due.toLocaleString(undefined, { day: 'numeric', month: 'short' })}`
        : task.due === today
          ? 'Today'
          : due.toLocaleString(undefined, { weekday: 'short', day: 'numeric', month: 'short' });
      body.append(el('div', 'due', label));
    }
    node.append(dot, body);
    list.append(node);
  }
}

/* ============================== status line & clock ============================== */

function renderStatus() {
  const left = document.getElementById('status-left');
  left.textContent = '';
  const parts = [];

  if (!payload.feeds.length) {
    parts.push(el('span', null, 'No feeds configured — right-click the screensaver file → Install, then open Settings'));
  } else {
    parts.push(el('span', null, payload.lastRefresh ? `Updated ${payload.lastRefresh}` : 'Loading feeds…'));
    const stale = payload.feeds.filter((f) => f.stale).map((f) => f.name);
    const dead = payload.feeds.filter((f) => f.error && !f.stale).map((f) => f.name);
    if (stale.length) parts.push(el('span', 'stale', `⚠ ${stale.join(', ')}: showing cached`));
    if (dead.length) parts.push(el('span', 'err', `✕ ${dead.join(', ')}: unavailable`));
  }
  parts.forEach((p, i) => {
    if (i) left.append(el('span', null, '   ·   '));
    left.append(p);
  });
}

function updateClock() {
  const now = new Date();
  const time = `${String(now.getHours()).padStart(2, '0')}:${String(now.getMinutes()).padStart(2, '0')}`;
  document.getElementById('clock').textContent = time;
  document.getElementById('clock-date').textContent =
    now.toLocaleString(undefined, { weekday: 'long', day: 'numeric', month: 'long' });
  const big = document.getElementById('big-clock-time');
  if (big) {
    big.textContent = time;
    document.getElementById('big-clock-day').textContent = now.toLocaleString(undefined, { weekday: 'long' });
    document.getElementById('big-clock-date').textContent = now.toLocaleString(undefined, { month: 'long', day: 'numeric' });
  }
}

setInterval(() => {
  updateClock();
  const nowKey = dateKey(new Date());
  if (nowKey !== todayKey && payload) {
    todayKey = nowKey;
    if (!IS_HOSTED) payload = mockPayload();  // host re-pushes real data at midnight
    render();
  }
}, 10_000);

/* ============================== photo slideshow ============================== */

function createSlideshow(container) {
  let pool = [];
  let queue = [];
  let tiles = [];
  let timer = null;
  let nextTile = 0;
  let signature = '';
  let intervalSec = 20;

  function nextPhoto() {
    if (!queue.length) queue = shuffle(pool.slice());
    const visible = new Set(tiles.map((t) => t.current));
    for (let i = 0; i < queue.length; i++) {           // prefer one not already on screen
      if (!visible.has(queue[i])) return queue.splice(i, 1)[0];
    }
    return queue.pop();
  }

  function swap(tile) {
    const url = nextPhoto();
    if (!url) return;
    const incoming = tile.imgs[tile.active ^ 1];
    const probe = new Image();
    probe.onload = () => {
      incoming.src = url;   // already decoded via the probe, so the crossfade starts clean
      incoming.classList.add('show');
      tile.imgs[tile.active].classList.remove('show');
      tile.active ^= 1;
      tile.current = url;
    };
    probe.onerror = () => { /* unreadable file — just skip this slot */ };
    probe.src = url;
  }

  function build() {
    container.textContent = '';
    tiles = [];
    nextTile = 0;
    let count = pool.length >= 12 ? 4 : pool.length >= 4 ? 3 : Math.min(pool.length, 2);
    if (IS_TV) {
      // A big clock takes the top photo's place, readable from across the room.
      const clock = el('div', 'tile clock-tile');
      clock.style.setProperty('--grow', 1.05);
      const time = el('div', 'big-time'); time.id = 'big-clock-time';
      const day = el('div', 'big-day'); day.id = 'big-clock-day';
      const date = el('div', 'big-date'); date.id = 'big-clock-date';
      clock.append(time, day, date);
      container.append(clock);
      count = Math.max(0, count - 1);
    }
    const grows = shuffle([1.25, 0.9, 1.1, 0.8]).slice(0, count);
    for (let i = 0; i < count; i++) {
      const tileEl = el('div', 'tile');
      tileEl.style.setProperty('--grow', grows[i]);
      const a = el('img'); const b = el('img');
      tileEl.append(a, b);
      container.append(tileEl);
      tiles.push({ el: tileEl, imgs: [a, b], active: 0, current: null });
    }
    tiles.forEach((tile, i) => setTimeout(() => swap(tile), 350 * i)); // staggered first fill
    if (IS_TV) updateClock();
  }

  function restartTimer() {
    if (timer) clearInterval(timer);
    timer = null;
    if (!tiles.length) return;
    timer = setInterval(() => {          // one tile at a time — never blank the whole panel
      swap(tiles[nextTile]);
      nextTile = (nextTile + 1) % tiles.length;
    }, Math.max(3, intervalSec) * 1000);
  }

  return {
    loaded() {
      return tiles.filter((t) => t.current).length;
    },
    update(photos, seconds) {
      const sig = JSON.stringify(photos) + (IS_TV ? '|tv' : '');
      const intervalChanged = seconds !== intervalSec;
      intervalSec = seconds;
      if (sig !== signature) {
        signature = sig;
        pool = photos.slice();
        queue = [];
        build();
        restartTimer();
      } else if (intervalChanged) {
        restartTimer();
      }
    },
  };
}

/* ============================== host wiring & input-to-exit ============================== */

function applyPayload(data) {
  payload = data;
  // TV mode: text size for the screen it's meant for (1 = a big TV).
  document.documentElement.style.setProperty('--k', String(data.textScale || 1));
  document.documentElement.classList.toggle('big-text', (data.textScale || 1) >= 1.75);
  document.documentElement.classList.toggle('light', payload.theme === 'light');
  render();
}

if (IS_TV) {
  // Volume Sync drives the page: data, which view, and the clock just before each picture.
  window.__tv = {
    apply(data) { applyPayload(data); },
    view(v) { if (payload && payload.view !== v) { payload.view = v; render(); } },
    tick() { updateClock(); },
    photosLoaded() { return slideshow.loaded(); },
  };
  if (!window.__TV__) setTimeout(() => applyPayload(mockPayload()), 30); // ?tv=1 preview
} else if (IS_HOSTED) {
  let exited = false;
  const exit = (reason) => {
    if (exited) return;
    exited = true;
    window.chrome.webview.postMessage({ type: 'exit', reason: String(reason) });
  };

  // Mouse jitter must not kill the saver: only exit after >10px of cumulative travel.
  let lastPos = null;
  let travelled = 0;
  window.addEventListener('mousemove', (e) => {
    if (lastPos) {
      travelled += Math.hypot(e.screenX - lastPos.x, e.screenY - lastPos.y);
      if (travelled > 10) exit(`mousemove travelled=${Math.round(travelled)} at=${e.screenX},${e.screenY}`);
    }
    lastPos = { x: e.screenX, y: e.screenY };
  });
  window.addEventListener('keydown', (e) => exit('keydown:' + e.key));
  window.addEventListener('mousedown', () => exit('mousedown'));
  window.addEventListener('wheel', () => exit('wheel'));
  window.addEventListener('contextmenu', (e) => e.preventDefault());

  window.chrome.webview.addEventListener('message', (e) => {
    if (e.data && e.data.type === 'data') {
      applyPayload(e.data);
      const grid = document.getElementById('grid').getBoundingClientRect();
      window.chrome.webview.postMessage({
        type: 'metrics',
        vw: innerWidth, vh: innerHeight, dpr: devicePixelRatio,
        gridBottom: Math.round(grid.bottom),
        events: (e.data.events || []).length, photos: (e.data.photos || []).length,
      });
      setTimeout(() => {
        const loaded = [...document.querySelectorAll('#photos img')].filter((i) => i.naturalWidth > 0).length;
        window.chrome.webview.postMessage({ type: 'metrics', tilesLoaded: loaded });
      }, 8000);
    }
  });
  window.chrome.webview.postMessage({ type: 'ready' });
} else {
  // Browser preview: render a rich mock so the design can be inspected without the host.
  setTimeout(() => applyPayload(mockPayload()), 30);
}

window.addEventListener('resize', () => render());
if (document.fonts && document.fonts.ready) {
  document.fonts.ready.then(() => fitEvents()); // Inter changes metrics once it loads
}

if (TASK_PANEL_POSITION === 'bottom') {
  document.getElementById('root').classList.add('tasks-bottom');
}

/* ============================== mock data (browser preview only) ============================== */

function mockPayload() {
  const now = new Date();
  const y = now.getFullYear();
  const m = now.getMonth() + 1;
  const day = (d) => `${y}-${String(m).padStart(2, '0')}-${String(d).padStart(2, '0')}`;
  const feeds = [
    { name: 'Family', color: '#7aa2f7', stale: false, error: null, enabled: true },
    { name: 'Work', color: '#f7768e', stale: true, error: null, enabled: true },
    { name: 'School', color: '#9ece6a', stale: false, error: null, enabled: true },
    { name: 'Birthdays', color: '#e0af68', stale: false, error: null, enabled: true },
  ];
  const events = [];
  const add = (feed, title, d, hh, mm = 0) =>
    events.push({ feed, title, allDay: false, start: `${day(d)}T${String(hh).padStart(2, '0')}:${String(mm).padStart(2, '0')}:00`, end: `${day(d)}T${String(hh + 1).padStart(2, '0')}:00:00` });
  const addAllDay = (feed, title, d1, d2) =>
    events.push({ feed, title, allDay: true, start: day(d1), end: day(d2 || d1) });

  // weekly recurrences, as the host would expand them
  for (let d = 1; d <= 28; d += 7) { add(2, 'Soccer practice', d + 1, 17, 30); add(0, 'Swim class', d + 3, 16, 0); }
  add(1, 'Sprint planning', 2, 10); add(1, '1:1 with Sam', 4, 14, 30);
  add(0, 'Dentist — kids', 9, 9, 15); add(1, 'Design review', 10, 11);
  add(3, "Grandma's birthday", 12, 12); addAllDay(3, 'Nana & Pop visiting', 13, 16);
  addAllDay(0, 'School holiday', 20);
  add(0, 'Pizza night', 17, 18, 30); add(1, 'Quarterly review', 18, 9);
  add(2, 'Book fair', 19, 8, 45); add(0, 'Oil change', 24, 15);
  add(1, 'Team offsite', 25, 9); add(0, 'Movie night', 26, 19, 30);

  // one deliberately packed day to exercise the auto-fit
  const busy = Math.min(now.getDate() + 2, 27);
  [['Breakfast run', 7, 30, 0], ['Standup', 9, 0, 1], ['Parent-teacher mtg', 10, 0, 2],
   ['Lunch w/ Alex', 12, 15, 0], ['Vet appointment', 13, 30, 0], ['Code review', 14, 30, 1],
   ['School pickup', 15, 15, 2], ['Groceries', 16, 30, 0], ['Soccer game', 18, 0, 2],
  ].forEach(([t, h, mm, f]) => add(f, t, busy, h, mm));
  addAllDay(1, 'Conference (remote)', busy, busy + 1);

  const tasks = [
    { feed: 0, title: 'Renew car registration', due: day(Math.max(1, now.getDate() - 3)) },
    { feed: 1, title: 'Submit expense report', due: day(Math.max(1, now.getDate() - 1)) },
    { feed: 0, title: 'Book summer camp', due: day(Math.min(28, now.getDate() + 2)) },
    { feed: 2, title: 'Sign permission slip', due: day(Math.min(28, now.getDate() + 4)) },
    { feed: 0, title: 'Fix the fence gate', due: null },
    { feed: 1, title: 'Update team wiki', due: day(Math.min(28, now.getDate() + 9)) },
  ];

  const photos = [];
  for (let i = 0; i < 10; i++) {
    const h1 = (i * 47) % 360, h2 = (h1 + 40) % 360;
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="800" height="1000">` +
      `<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">` +
      `<stop offset="0" stop-color="hsl(${h1},45%,32%)"/><stop offset="1" stop-color="hsl(${h2},55%,16%)"/>` +
      `</linearGradient></defs><rect width="800" height="1000" fill="url(#g)"/>` +
      `<circle cx="${180 + i * 40}" cy="${240 + i * 55}" r="130" fill="hsl(${h2},50%,45%)" opacity="0.35"/></svg>`;
    photos.push('data:image/svg+xml,' + encodeURIComponent(svg));
  }

  return {
    type: 'data', year: y, month: m, events, tasks, feeds, photos,
    photoIntervalSeconds: 6, lastRefresh: fmtTime(now),
    theme: new URLSearchParams(location.search).get('theme') || 'dark', // preview: ?theme=light
    view: new URLSearchParams(location.search).get('view') || 'month',  // TV preview: ?tv=1&view=week
    textScale: Number(new URLSearchParams(location.search).get('k')) || 1,  // TV preview: &k=2.5
  };
}
