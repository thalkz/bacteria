'use strict';

/* ------------------------------------------------------------------ helpers */

const $ = (sel, root = document) => root.querySelector(sel);
const app = $('#app');
const COLORS = ['green', 'red'];
const NAME = { green: 'Green', red: 'Red' };
const CH = { green: 'G', red: 'R' };
const COLOR_OF = { G: 'green', R: 'red' };
const other = c => (c === 'green' ? 'red' : 'green');
const cap = s => s.charAt(0).toUpperCase() + s.slice(1);
const now = () => performance.now();
const countOf = (cells, ch) => cells.split(ch).length - 1;
const fmt = secs => `0:${String(secs).padStart(2, '0')}`;
const buzz = pattern => { try { navigator.vibrate && navigator.vibrate(pattern); } catch (_) { /* unsupported */ } };

/** Only touches the DOM when the markup actually changes. */
function setHTML(el, html) {
  if (el._html !== html) {
    el._html = html;
    el.innerHTML = html;
  }
}

/* ------------------------------------------------------------------ storage */

const memory = {};
const store = {
  get(key, fallback) {
    try {
      const v = localStorage.getItem('bacteria.' + key);
      return v === null ? (key in memory ? memory[key] : fallback) : JSON.parse(v);
    } catch (_) {
      return key in memory ? memory[key] : fallback;
    }
  },
  set(key, value) {
    memory[key] = value;
    try { localStorage.setItem('bacteria.' + key, JSON.stringify(value)); } catch (_) { /* private mode */ }
  },
};

const getToken = id => store.get('tokens', {})[id] || null;
function setToken(id, token) {
  const tokens = store.get('tokens', {});
  tokens[id] = token;
  const ids = Object.keys(tokens);
  if (ids.length > 40) delete tokens[ids[0]];
  store.set('tokens', tokens);
}

/** The colour this browser played in each game, for "My games". */
const playedSeats = () => store.get('played', {});
function rememberSeat(id, seat) {
  const all = playedSeats();
  if (all[id] === seat) return;
  all[id] = seat;
  const ids = Object.keys(all);
  if (ids.length > 200) delete all[ids[0]];
  store.set('played', all);
}

/* ------------------------------------------------------------------ sound */

const sound = (() => {
  let ctx = null;
  let muted = store.get('muted', false);
  const btn = $('#sound');
  const paint = () => {
    btn.classList.toggle('muted', muted);
    btn.setAttribute('aria-label', muted ? 'Sound off' : 'Sound on');
  };
  paint();

  function audio() {
    if (!ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return null;
      ctx = new AC();
    }
    if (ctx.state === 'suspended') ctx.resume();
    return ctx;
  }
  // Browsers only allow audio after a user gesture.
  window.addEventListener('pointerdown', () => { if (!muted) audio(); }, { once: true });

  function tone(f0, f1, dur, type = 'sine', vol = 0.1, delay = 0) {
    const a = audio();
    if (!a) return;
    const t = a.currentTime + delay;
    const osc = a.createOscillator();
    const gain = a.createGain();
    osc.type = type;
    osc.frequency.setValueAtTime(f0, t);
    osc.frequency.exponentialRampToValueAtTime(f1, t + dur);
    gain.gain.setValueAtTime(0.0001, t);
    gain.gain.exponentialRampToValueAtTime(vol, t + 0.012);
    gain.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    osc.connect(gain).connect(a.destination);
    osc.start(t);
    osc.stop(t + dur + 0.03);
  }

  const sounds = {
    select: () => tone(660, 900, 0.05, 'triangle', 0.05),
    copy: () => tone(260, 640, 0.12, 'sine', 0.16),
    jump: () => { tone(220, 560, 0.1, 'sine', 0.12); tone(560, 280, 0.14, 'sine', 0.1, 0.1); },
    convert: k => tone(520 + k * 70, 760 + k * 90, 0.07, 'triangle', 0.07),
    fill: () => tone(300, 900, 0.6, 'triangle', 0.05),
    start: () => { tone(440, 440, 0.12, 'triangle', 0.09); tone(660, 660, 0.18, 'triangle', 0.09, 0.12); },
    tick: () => tone(1250, 1250, 0.03, 'square', 0.025),
    win: () => [523, 659, 784, 1046].forEach((f, i) => tone(f, f, 0.18, 'triangle', 0.09, i * 0.1)),
    lose: () => [440, 370, 311].forEach((f, i) => tone(f, f * 0.97, 0.22, 'triangle', 0.08, i * 0.15)),
    nope: () => tone(190, 140, 0.08, 'square', 0.03),
  };

  btn.addEventListener('click', () => {
    muted = !muted;
    store.set('muted', muted);
    paint();
    play('select');
  });

  function play(name, ...args) {
    if (muted) return;
    try { sounds[name](...args); } catch (_) { /* audio unavailable */ }
  }
  return { play };
})();

/* ------------------------------------------------------------------ toast */

let toastTimer = 0;
function toast(text, ms = 2400) {
  const el = $('#toast');
  if (!el) return;
  el.textContent = text;
  el.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('show'), ms);
}

/* ------------------------------------------------------------------ rules (mirror of src/game.rs) */

const AROUND = [[-1, -1], [0, -1], [1, -1], [-1, 0], [1, 0], [-1, 1], [0, 1], [1, 1]];
// Copies to the 8 neighbours, then the 4 straight jumps.
const STEPS = [...AROUND, [2, 0], [-2, 0], [0, 2], [0, -2]];

function offset(b, i, dx, dy) {
  const x = (i % b.w) + dx;
  const y = Math.floor(i / b.w) + dy;
  return x < 0 || y < 0 || x >= b.w || y >= b.h ? -1 : y * b.w + x;
}

/** Empty cells the bacterium at `i` can reach. `b` is `{w, h, cells}`. */
function destinations(b, i) {
  const out = [];
  STEPS.forEach(([dx, dy], k) => {
    const j = offset(b, i, dx, dy);
    if (j >= 0 && b.cells[j] === '.') out.push({ to: j, jump: k >= 8 });
  });
  return out;
}

function captures(b, color, to) {
  const enemy = CH[other(color)];
  return AROUND.map(([dx, dy]) => offset(b, to, dx, dy)).filter(j => j >= 0 && b.cells[j] === enemy);
}

function hasMoves(b, color) {
  for (let i = 0; i < b.cells.length; i++) {
    if (b.cells[i] === CH[color] && destinations(b, i).length) return true;
  }
  return false;
}

/* ------------------------------------------------------------------ board view */

const PIECE_ANIMS = ['spawning', 'jumping', 'converting', 'filling', 'nope', 'hop'];

function anim(el, cls) {
  el.classList.remove(...PIECE_ANIMS, cls);
  void el.offsetWidth; // restart the animation
  el.classList.add(cls);
}

/** Renders a board and animates moves on it. Knows nothing about the network. */
class BoardView {
  constructor(el, handlers = {}) {
    this.el = el;
    this.pending = [];
    this.pieces = [];
    this.cellsEl = [];
    if (handlers.onTap) {
      el.addEventListener('pointerdown', e => {
        if (e.button > 0) return;
        const cell = e.target.closest('.cell');
        if (!cell) return;
        this.fx(+cell.dataset.i, 'tap');
        handlers.onTap(+cell.dataset.i);
      });
    }
    if (handlers.onHover) {
      el.addEventListener('pointerover', e => {
        if (e.pointerType !== 'mouse') return;
        const cell = e.target.closest('.cell');
        handlers.onHover(cell ? +cell.dataset.i : -1);
      });
      el.addEventListener('pointerleave', () => handlers.onHover(-1));
    }
  }

  /** `coords`: letters along the bottom and numbers along the left, as in the move notation. */
  setup(w, h, cells, coords = false) {
    this.cols = w;
    this.rows = h;
    this.el.style.setProperty('--w', w);
    this.el.style.setProperty('--h', h);
    this.pieces = new Array(w * h).fill(null);
    this.cellsEl = [];
    const frag = document.createDocumentFragment();
    for (let i = 0; i < w * h; i++) {
      const c = document.createElement('div');
      const alt = ((i % w) + Math.floor(i / w)) % 2 === 1;
      c.className = 'cell' + (cells[i] === '#' ? ' blocked' : alt ? ' alt' : '');
      c.dataset.i = i;
      if (coords && i % w === 0) c.insertAdjacentHTML('beforeend', `<span class="coord rank">${h - Math.floor(i / w)}</span>`);
      if (coords && i >= w * (h - 1)) c.insertAdjacentHTML('beforeend', `<span class="coord file">${FILES[i % w]}</span>`);
      frag.appendChild(c);
      this.cellsEl.push(c);
    }
    this.el.replaceChildren(frag);
  }

  piece(color) {
    const p = document.createElement('div');
    p.className = 'bac ' + color;
    p.style.setProperty('--d', (-Math.random() * 7).toFixed(2) + 's');
    p.appendChild(document.createElement('i'));
    p.addEventListener('animationend', e => { if (e.target === p) p.classList.remove(...PIECE_ANIMS); });
    return p;
  }

  /** Puts the board in the given position, without animation. */
  sync(cells) {
    for (let i = 0; i < this.pieces.length; i++) {
      const want = COLOR_OF[cells[i]];
      const p = this.pieces[i];
      if (!want) {
        if (p) { p.remove(); this.pieces[i] = null; }
      } else if (!p) {
        const n = this.piece(want);
        this.cellsEl[i].appendChild(n);
        this.pieces[i] = n;
      } else if (!p.classList.contains(want)) {
        p.classList.remove(other(want));
        p.classList.add(want);
      }
    }
  }

  later(fn, ms) {
    const task = { fn };
    task.id = setTimeout(() => {
      this.pending = this.pending.filter(t => t !== task);
      fn();
    }, ms);
    this.pending.push(task);
  }

  /** Drops every scheduled step (the caller puts the board in a new position). */
  cancel() {
    this.pending.forEach(t => clearTimeout(t.id));
    this.pending = [];
  }

  /** Runs every scheduled step right now (a new position is coming). */
  flush() {
    const tasks = this.pending;
    this.pending = [];
    tasks.forEach(t => { clearTimeout(t.id); t.fn(); });
  }

  /** Animates a move. Returns how long the whole sequence takes, in ms. */
  animate(m, color, sounds = true) {
    const w = this.cols;
    const dx = (m.from % w) - (m.to % w);
    const dy = Math.floor(m.from / w) - Math.floor(m.to / w);
    let el = null;
    if (m.jump) {
      el = this.pieces[m.from];
      this.pieces[m.from] = null;
      this.fx(m.from, 'puff ' + color);
    }
    if (!el) el = this.piece(color);
    if (this.pieces[m.to]) this.pieces[m.to].remove();
    el.classList.remove('selected', 'threat');
    this.cellsEl[m.to].appendChild(el);
    this.pieces[m.to] = el;
    el.style.setProperty('--dx', dx);
    el.style.setProperty('--dy', dy);
    anim(el, m.jump ? 'jumping' : 'spawning');
    if (sounds) sound.play(m.jump ? 'jump' : 'copy');

    const start = m.jump ? 290 : 190;
    const step = 60;
    m.converted.forEach((i, k) => this.later(() => {
      this.convert(i, color);
      if (sounds) sound.play('convert', k);
    }, start + k * step));
    let end = start + m.converted.length * step + 260;
    if (m.filled && m.filled.length) end = this.fillAll(m.filled, color, m.to, end + 200, sounds);
    return end;
  }

  /** Auto-complete: empty cells fill up in a wave starting from `origin`. */
  fillAll(list, color, origin, delay, sounds = true) {
    const w = this.cols;
    const ox = origin % w;
    const oy = Math.floor(origin / w);
    const dist = i => Math.hypot((i % w) - ox, Math.floor(i / w) - oy);
    const order = [...list].sort((a, b) => dist(a) - dist(b));
    const step = Math.max(12, Math.min(50, 900 / order.length));
    if (sounds) this.later(() => sound.play('fill'), delay);
    order.forEach((i, k) => this.later(() => this.fill(i, color), delay + k * step));
    return delay + order.length * step + 350;
  }

  convert(i, color) {
    const p = this.pieces[i];
    if (!p) return;
    p.classList.remove(other(color));
    p.classList.add(color);
    anim(p, 'converting');
    this.fx(i, 'ring ' + color);
  }

  fill(i, color) {
    if (this.pieces[i]) return;
    const p = this.piece(color);
    this.cellsEl[i].appendChild(p);
    this.pieces[i] = p;
    anim(p, 'filling');
  }

  fx(i, cls) {
    const s = document.createElement('span');
    s.className = 'fx ' + cls;
    s.addEventListener('animationend', () => s.remove());
    this.cellsEl[i].appendChild(s);
  }

  celebrate(color) {
    this.pieces.forEach((p, i) => {
      if (!p || !p.classList.contains(color)) return;
      p.style.setProperty('--k', ((i % this.cols) + Math.floor(i / this.cols)) * 45 + 'ms');
      anim(p, 'hop');
    });
  }
}

/* ------------------------------------------------------------------ router */

let current = null;

function route() {
  if (current) current.destroy();
  current = null;
  $('#toast').classList.remove('show');
  const path = location.pathname;
  const game = path.match(/^\/g\/([a-z0-9]+)\/?$/);
  const replay = path.match(/^\/r\/([a-z0-9]+)\/?$/);
  document.body.classList.toggle('wide', !!replay);
  if (game) current = new GameView(game[1]);
  else if (replay) current = new ReplayView(replay[1]);
  else if (/^\/history\/?$/.test(path)) current = new HistoryView();
  else current = new HomeView();
  window.scrollTo(0, 0);
}

function navigate(path, replace = false) {
  history[replace ? 'replaceState' : 'pushState'](null, '', path);
  route();
}

window.addEventListener('popstate', route);
document.addEventListener('click', e => {
  const a = e.target.closest('a[data-link]');
  if (a && !e.metaKey && !e.ctrlKey) {
    e.preventDefault();
    navigate(a.getAttribute('href'));
  }
});

/* ------------------------------------------------------------------ home */

const ICONS = {
  link: '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/></svg>',
  cpu: '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="6" width="12" height="12" rx="2.5"/><path d="M10 2.5v3M14 2.5v3M10 18.5v3M14 18.5v3M2.5 10h3M2.5 14h3M18.5 10h3M18.5 14h3"/></svg>',
  clock: '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 12a8.5 8.5 0 1 0 2.5-6"/><path d="M3 4v4h4"/><path d="M12 7.5V12l3 2"/></svg>',
  duo: '<svg viewBox="0 0 24 24" width="24" height="24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="8.5" cy="8" r="3"/><circle cx="16.5" cy="9.5" r="2.5"/><path d="M3 19.5c.4-3.2 2.7-5.5 5.5-5.5s5.1 2.3 5.5 5.5M14.5 14.3c.6-.2 1.3-.3 2-.3 2.4 0 4.2 1.9 4.5 4.5"/></svg>',
};

const RULES = `
  <ol>
    <li>Tap one of your bacteria, then a highlighted cell.</li>
    <li>Next door, diagonals included: your bacterium <b>copies</b> itself.</li>
    <li>Two cells away in a straight line (no diagonals): it <b>jumps</b>, leaving its old cell empty.</li>
    <li>Where it lands, every enemy bacterium touching it <b>converts</b> to your colour, diagonals included.</li>
    <li>You have <b>30 seconds</b> per move. Run out and you lose.</li>
    <li>When a player can't move, the other one takes every empty cell. Most cells wins.</li>
  </ol>`;

class HomeView {
  constructor() {
    const level = (name, n) => `<button type="button" data-level="${name}"><span class="pips">${'<i class="on"></i>'.repeat(n)}${'<i></i>'.repeat(3 - n)}</span>${cap(name)}</button>`;
    app.innerHTML = `
      <section class="home">
        <div class="hero">
          <div class="board demo" aria-hidden="true"></div>
          <h1>Bacteria</h1>
          <p class="tagline">Spread your bacteria. Convert your rivals. Take the board.</p>
        </div>
        <nav class="modes">
          <button type="button" class="mode" data-mode="online">
            <span class="mode-icon">${ICONS.link}</span>
            <span class="mode-text"><b>Play with a friend</b><small>Send a link, play anywhere</small></span>
          </button>
          <div class="mode-group">
            <button type="button" class="mode" data-mode="solo" aria-expanded="false">
              <span class="mode-icon">${ICONS.cpu}</span>
              <span class="mode-text"><b>Play the computer</b><small>Easy, medium or hard</small></span>
            </button>
            <div class="levels">${level('easy', 1)}${level('medium', 2)}${level('hard', 3)}</div>
          </div>
          <button type="button" class="mode" data-mode="local">
            <span class="mode-icon">${ICONS.duo}</span>
            <span class="mode-text"><b>Pass and play</b><small>Two players, one screen</small></span>
          </button>
          <a href="/history" data-link class="mode">
            <span class="mode-icon">${ICONS.clock}</span>
            <span class="mode-text"><b>Past games</b><small>Replay them and learn from each move</small></span>
          </a>
        </nav>
        <details class="rules">
          <summary>How to play</summary>
          ${RULES}
        </details>
      </section>`;

    const group = $('.mode-group');
    app.querySelector('.modes').addEventListener('click', e => {
      const lvl = e.target.closest('[data-level]');
      if (lvl) return this.create('solo', lvl.dataset.level, lvl);
      const btn = e.target.closest('[data-mode]');
      if (!btn) return;
      if (btn.dataset.mode === 'solo') {
        const open = group.classList.toggle('open');
        btn.setAttribute('aria-expanded', String(open));
        sound.play('select');
        return;
      }
      this.create(btn.dataset.mode, undefined, btn);
    });

    this.demo = new Demo($('.board.demo'));
  }

  async create(mode, difficulty, btn) {
    if (this.busy) return;
    this.busy = true;
    btn.classList.add('busy');
    try {
      const res = await fetch('/api/games', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ mode, difficulty }),
      });
      if (!res.ok) throw new Error(await res.text());
      const { id, token } = await res.json();
      setToken(id, token);
      navigate('/g/' + id);
    } catch (_) {
      toast("Couldn't start a game. Try again.");
      btn.classList.remove('busy');
      this.busy = false;
    }
  }

  destroy() {
    this.demo.stop();
  }
}

/** The little board on the home page that plays against itself. */
class Demo {
  constructor(el) {
    this.view = new BoardView(el);
    this.reset();
    if (!matchMedia('(prefers-reduced-motion: reduce)').matches) {
      this.timer = setInterval(() => this.step(), 1150);
    }
  }

  reset() {
    this.cells = ('G...R' + '.....' + '..#..' + '.....' + 'R...G').split('');
    this.turn = 'green';
    this.rest = 0;
    this.view.setup(5, 5, this.cells);
    this.view.sync(this.cells);
    anim(this.view.el, 'appear');
  }

  step() {
    if (document.hidden) return;
    if (this.rest > 0) {
      if (--this.rest === 0) this.reset();
      return;
    }
    const b = { w: 5, h: 5, cells: this.cells };
    const me = CH[this.turn];
    let best = null;
    let bestScore = -1;
    this.cells.forEach((c, i) => {
      if (c !== me) return;
      for (const d of destinations(b, i)) {
        const score = captures(b, this.turn, d.to).length * 2 + (d.jump ? 0 : 1) + Math.random() * 2.5;
        if (score > bestScore) { bestScore = score; best = { from: i, ...d }; }
      }
    });
    if (!best) { this.rest = 3; return; }
    const converted = captures(b, this.turn, best.to);
    if (best.jump) this.cells[best.from] = '.';
    this.cells[best.to] = me;
    converted.forEach(j => (this.cells[j] = me));
    const filled = [];
    if (!hasMoves(b, other(this.turn))) {
      this.cells.forEach((c, i) => { if (c === '.') { filled.push(i); this.cells[i] = me; } });
      this.rest = 4;
    }
    this.view.flush();
    this.view.animate({ ...best, converted, filled }, this.turn, false);
    this.turn = other(this.turn);
  }

  stop() {
    clearInterval(this.timer);
    this.view.flush();
  }
}

/* ------------------------------------------------------------------ game */

const panelHTML = c => `
  <div class="panel" data-c="${c}">
    <span class="swatch"><span class="bac ${c}"><i></i></span></span>
    <span class="who">
      <span class="name"></span>
      <span class="presence" hidden></span>
      <span class="thinking" aria-hidden="true"><i></i><i></i><i></i></span>
    </span>
    <span class="count">0</span>
    <span class="clock">0:30</span>
    <span class="bar"><i></i></span>
  </div>`;

class GameView {
  constructor(id) {
    this.id = id;
    this.seat = null; // 'green' | 'red' | 'both' | null (spectator)
    this.s = null; // last state from the server
    this.board = null;
    this.selected = -1;
    this.dests = [];
    this.threats = [];
    this.lastCells = [];
    this.optimistic = null;
    this.retry = 0;
    app.innerHTML = '<div class="loading"><span class="spinner"></span></div>';
    this.connect();
    this.ping = setInterval(() => this.send({ t: 'ping' }), 25000);
    this.onVisible = () => {
      if (document.hidden) { this.hiddenAt = now(); return; }
      // Phones freeze background tabs: resync after a while away.
      if (this.hiddenAt && now() - this.hiddenAt > 5000 && this.ws) this.ws.close();
    };
    document.addEventListener('visibilitychange', this.onVisible);
  }

  destroy() {
    this.closed = true;
    clearInterval(this.ping);
    clearTimeout(this.retryTimer);
    clearTimeout(this.disarm);
    cancelAnimationFrame(this.raf);
    document.removeEventListener('visibilitychange', this.onVisible);
    if (this.ws) this.ws.close();
    if (this.board) this.board.flush();
  }

  /* --- network --- */

  connect() {
    const token = getToken(this.id);
    const proto = location.protocol === 'https:' ? 'wss' : 'ws';
    const ws = new WebSocket(`${proto}://${location.host}/ws/${this.id}` + (token ? `?token=${encodeURIComponent(token)}` : ''));
    this.ws = ws;
    ws.onopen = () => {
      if (this.retry > 1) toast('Reconnected');
      this.retry = 0;
    };
    ws.onmessage = e => {
      let msg;
      try { msg = JSON.parse(e.data); } catch (_) { return; }
      this.onMessage(msg);
    };
    ws.onclose = () => {
      if (this.closed || this.notFound || ws !== this.ws) return;
      if (this.retry === 1) toast('Connection lost. Reconnecting…', 4000);
      const delay = this.retry === 0 ? 0 : Math.min(5000, 400 * 2 ** this.retry);
      this.retry++;
      this.retryTimer = setTimeout(() => this.connect(), delay);
    };
  }

  send(msg) {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) this.ws.send(JSON.stringify(msg));
  }

  onMessage(m) {
    if (m.t === 'welcome') {
      this.seat = m.seat;
      if (m.token) setToken(this.id, m.token);
      if (m.seat) rememberSeat(this.id, m.seat);
    } else if (m.t === 'state') {
      this.onState(m);
    } else if (m.t === 'error') {
      if (m.code === 'not_found') {
        this.notFound = true;
        // The room is gone, but the finished game may be in the history.
        fetch('/api/history/' + this.id, { method: 'HEAD' })
          .then(r => { if (this.closed) return; if (r.ok) navigate('/r/' + this.id, true); else this.renderNotFound(); })
          .catch(() => { if (!this.closed) this.renderNotFound(); });
      } else if (this.optimistic) {
        this.optimistic.rejected = true;
      }
    }
  }

  /* --- seats --- */

  mine() {
    if (this.seat === 'both') return COLORS;
    return this.seat === 'green' || this.seat === 'red' ? [this.seat] : [];
  }

  isMine(c) {
    return this.mine().includes(c);
  }

  /** The single colour this screen plays, or null for pass and play / spectators. */
  me() {
    const m = this.mine();
    return m.length === 1 ? m[0] : null;
  }

  canMove() {
    const s = this.s;
    return !!s && s.status === 'playing' && this.isMine(s.turn) && !s.players[s.turn].ai && !this.optimistic;
  }

  label(c) {
    const s = this.s;
    if (s.mode === 'local' || !this.me()) return NAME[c];
    if (this.isMine(c)) return 'You';
    if (s.players[c].ai) return `Computer · ${cap(s.difficulty)}`;
    return 'Opponent';
  }

  /* --- state --- */

  onState(s) {
    if (s.next) {
      // Rematch accepted: follow the players to the new room.
      const token = getToken(this.id);
      if (token) setToken(s.next, token);
      navigate('/g/' + s.next, true);
      return;
    }
    const prev = this.s;
    this.s = s;
    this.deadline = s.remaining_ms == null ? null : now() + s.remaining_ms;
    if (s.status === 'waiting') {
      this.renderLobby(s);
      return;
    }

    let wait = 0;
    if (!this.board) {
      this.renderGame(s);
    } else {
      const b = this.board;
      const o = this.optimistic;
      const L = s.last;
      this.optimistic = null;
      if (o && !o.rejected && L && s.ply === o.ply && L.from === o.from && L.to === o.to) {
        // Our own move, already animated: only the auto-complete is new.
        wait = Math.max(0, o.endAt - now());
        if (L.filled.length) wait = b.fillAll(L.filled, L.by, L.to, wait + 200);
      } else if (!o && prev && L && s.ply === prev.ply + 1) {
        b.flush();
        b.sync(prev.cells);
        wait = b.animate(L, L.by);
      } else {
        b.flush();
      }
      if (wait) b.later(() => b.sync(s.cells), wait);
      else b.sync(s.cells);
    }
    this.afterState(s, prev, wait);
  }

  afterState(s, prev, wait) {
    const b = this.board;
    if (s.status === 'playing' && s.ply === 0 && (!prev || prev.status === 'waiting')) this.announce(s, prev);
    this.paint();
    if (wait) b.later(() => this.paintScores(s), wait);
    else this.paintScores(s);
    if (s.status === 'finished' && (!prev || prev.status !== 'finished')) {
      if (prev) b.later(() => this.finished(s), wait + 350);
      else this.openModal();
    }
    if (this.modalOpen) this.renderModal();
  }

  announce(s, prev) {
    const me = this.me();
    sound.play('start');
    buzz(15);
    if (s.mode === 'local') toast('Green starts');
    else if (me && prev) toast(`Your opponent is here. You play ${NAME[me]}`, 3000);
    else if (me) toast(s.turn === me ? `You play ${NAME[me]}. Your move!` : `You play ${NAME[me]}`, 3000);
  }

  finished(s) {
    const r = s.result;
    const me = this.me();
    if (r.reason !== 'aborted') {
      sound.play(me && r.winner && r.winner !== me ? 'lose' : 'win');
      if (r.winner) this.board.celebrate(r.winner);
    }
    buzz([20, 60, 20]);
    this.openModal();
  }

  /* --- rendering --- */

  renderNotFound() {
    app.innerHTML = `
      <section class="center">
        <div class="card">
          <h1>Game not found</h1>
          <p class="muted">This game doesn't exist or has expired.</p>
          <a href="/" data-link class="btn primary">New game</a>
        </div>
      </section>`;
  }

  renderLobby(s) {
    if (this.lobby) return; // nothing changes until the opponent shows up
    this.lobby = true;
    const me = this.me();
    const url = `${location.origin}/g/${this.id}`;
    app.innerHTML = `
      <section class="center">
        <div class="card lobby">
          <div class="board mini" aria-hidden="true"></div>
          <h1>Invite a friend</h1>
          <p class="muted">Send this link to your opponent.<br>The game starts as soon as they open it.</p>
          <div class="share">
            <input readonly value="${url}" aria-label="Invite link" spellcheck="false">
            <button type="button" class="btn" data-copy>Copy</button>
          </div>
          ${navigator.share ? '<button type="button" class="btn primary wide" data-share>Share link</button>' : ''}
          <p class="waiting"><span class="dots"><i></i><i></i><i></i></span>Waiting for your opponent</p>
          <p class="meta">${me ? `You play <b class="tag ${me}">${NAME[me]}</b> · ` : ''}${s.map} · 30 s per move</p>
          <a href="/" data-link class="link">Cancel</a>
        </div>
      </section>`;
    const mini = new BoardView($('.board.mini'));
    mini.setup(s.w, s.h, s.cells);
    mini.sync(s.cells);
    const input = $('.share input');
    input.addEventListener('focus', () => input.select());
    $('[data-copy]').addEventListener('click', e => copyText(url, e.currentTarget));
    const share = $('[data-share]');
    if (share) {
      share.addEventListener('click', () => {
        navigator.share({ title: 'Bacteria', text: 'Come play Bacteria with me!', url }).catch(() => {});
      });
    }
  }

  renderGame(s) {
    this.lobby = false;
    const bottom = this.seat === 'red' ? 'red' : 'green';
    app.innerHTML = `
      <section class="game">
        ${panelHTML(other(bottom))}
        <div class="scorebar" aria-hidden="true"><i class="g"></i><i class="r"></i></div>
        <div class="board"></div>
        ${panelHTML(bottom)}
        <p class="status-line" aria-live="polite"></p>
        <div class="actions"></div>
        <div class="modal" hidden></div>
      </section>`;
    const root = $('.game');
    this.panels = {};
    for (const c of COLORS) {
      const el = $(`.panel[data-c="${c}"]`, root);
      this.panels[c] = {
        el,
        name: $('.name', el),
        presence: $('.presence', el),
        count: $('.count', el),
        clock: $('.clock', el),
        bar: $('.bar i', el),
      };
    }
    this.scorebar = { g: $('.scorebar .g', root), r: $('.scorebar .r', root) };
    this.statusEl = $('.status-line', root);
    this.actions = $('.actions', root);
    this.modal = $('.modal', root);
    this.board = new BoardView($('.board', root), { onTap: i => this.onTap(i), onHover: i => this.onHover(i) });
    this.board.setup(s.w, s.h, s.cells);
    this.board.sync(s.cells);
    root.addEventListener('click', e => {
      const a = e.target.closest('[data-act]');
      if (a && !a.disabled) this.act(a.dataset.act);
    });
    this.modal.addEventListener('click', e => { if (e.target === this.modal) this.closeModal(); });
    this.loop();
  }

  paint() {
    const s = this.s;
    const playing = s.status === 'playing';
    for (const c of COLORS) {
      const p = this.panels[c];
      const active = playing && s.turn === c;
      p.el.classList.toggle('active', active);
      p.el.classList.toggle('is-thinking', active && s.players[c].ai);
      p.el.classList.toggle('winner', s.status === 'finished' && s.result.winner === c);
      setHTML(p.name, this.label(c));
      p.presence.hidden = s.mode !== 'online' || this.isMine(c);
      p.presence.classList.toggle('on', s.players[c].online);
      p.presence.title = s.players[c].online ? 'Connected' : 'Disconnected';
    }
    const el = this.board.el;
    el.classList.toggle('turn-green', s.turn === 'green');
    el.classList.toggle('turn-red', s.turn === 'red');
    el.classList.toggle('playable', this.canMove());
    this.markLast(s.last ? [s.last.from, s.last.to] : []);
    setHTML(this.statusEl, this.statusText());
    this.renderActions();
    if (!this.canMove() && this.selected >= 0) this.select(-1);
  }

  paintScores(s) {
    for (const c of COLORS) {
      const el = this.panels[c].count;
      const v = String(s.scores[c]);
      if (el.textContent !== v) {
        el.textContent = v;
        anim(el, 'bump');
      }
    }
    this.scorebar.g.style.flexGrow = s.scores.green;
    this.scorebar.r.style.flexGrow = s.scores.red;
  }

  markLast(cells) {
    this.lastCells.forEach(i => this.board.cellsEl[i] && this.board.cellsEl[i].classList.remove('last'));
    this.lastCells = cells;
    cells.forEach(i => this.board.cellsEl[i].classList.add('last'));
  }

  statusText() {
    const s = this.s;
    if (s.status === 'finished') return this.resultText().title;
    const t = s.turn;
    if (s.mode === 'local') return `<span class="tag ${t}">${NAME[t]}</span> to play`;
    if (this.isMine(t)) return s.ply < 2 ? 'Your move! You have 30 seconds' : 'Your move';
    if (s.players[t].ai) return 'The computer is thinking…';
    if (!this.me()) return `<span class="tag ${t}">${NAME[t]}</span> to play`;
    return s.players[t].online ? "Your opponent's move" : 'Your opponent is disconnected…';
  }

  renderActions() {
    const s = this.s;
    let html = '';
    if (s.status === 'playing' && this.mine().length) {
      html = `<button type="button" class="btn ghost${this.resignArmed ? ' armed' : ''}" data-act="resign">${this.resignArmed ? 'Tap again to resign' : 'Resign'}</button>`;
    } else if (s.status === 'finished') {
      html = this.rematchButton() + '<button type="button" class="btn" data-act="home">New game</button>';
      if (this.reviewable()) html += '<button type="button" class="btn ghost" data-act="review">Review moves</button>';
      if (!this.modalOpen) html += '<button type="button" class="btn ghost" data-act="result">Result</button>';
    }
    setHTML(this.actions, html);
  }

  /** Finished games with moves are saved: they can be replayed and analysed. */
  reviewable() {
    const s = this.s;
    return s.status === 'finished' && s.ply > 0 && s.result.reason !== 'aborted';
  }

  rematchButton() {
    const s = this.s;
    if (!this.mine().length) return '';
    if (s.mode !== 'online') return '<button type="button" class="btn primary" data-act="rematch">Play again</button>';
    const me = this.me();
    if (s.rematch[me]) return '<button type="button" class="btn primary" disabled>Waiting for opponent…</button>';
    if (s.rematch[other(me)]) return '<button type="button" class="btn primary pulse" data-act="rematch">Accept rematch</button>';
    return '<button type="button" class="btn primary" data-act="rematch">Rematch</button>';
  }

  resultText() {
    const s = this.s;
    const r = s.result;
    const me = this.me();
    const w = r.winner;
    const loser = w && other(w);
    const who = c => (me ? (c === me ? 'You' : this.label(c)) : NAME[c]);

    let title;
    let tone = 'neutral';
    if (r.reason === 'aborted') title = 'Game aborted';
    else if (!w) title = 'Draw';
    else if (me) { title = w === me ? 'You won!' : 'You lost'; tone = w === me ? 'win' : 'lose'; }
    else title = `${NAME[w]} wins`;

    let detail;
    if (r.reason === 'timeout') detail = `${who(loser)} ran out of time`;
    else if (r.reason === 'resign') detail = `${who(loser)} resigned`;
    else if (r.reason === 'aborted') detail = 'The first move wasn’t played in time';
    else {
      const n = s.last ? s.last.filled.length : 0;
      const stuck = s.last && other(s.last.by);
      if (n && s.scores[stuck] === 0) detail = `${NAME[stuck]} has no bacteria left`;
      else if (n) detail = `${NAME[stuck]} can’t move: the last ${n} empty cell${n > 1 ? 's go' : ' goes'} to ${NAME[s.last.by]}`;
      else detail = 'The board is full';
    }
    return { title, detail, tone };
  }

  openModal() {
    this.modalOpen = true;
    this.renderModal();
  }

  renderModal() {
    const s = this.s;
    if (!s || s.status !== 'finished') return;
    const { title, detail, tone } = this.resultText();
    const r = s.result;
    const mark = r.winner
      ? `<span class="bac ${r.winner}"><i></i></span>`
      : '<span class="bac green small"><i></i></span><span class="bac red small"><i></i></span>';
    const me = this.me();
    const asks = s.mode === 'online' && me && s.rematch[other(me)] && !s.rematch[me];
    setHTML(this.modal, `
      <div class="modal-card ${tone}" role="dialog" aria-label="${title}">
        <div class="result-mark${r.winner ? '' : ' pair'}">${mark}</div>
        <h2>${title}</h2>
        <p class="final"><span class="g">${s.scores.green}</span><span class="dash">–</span><span class="r">${s.scores.red}</span></p>
        <p class="detail">${detail}</p>
        ${asks ? '<p class="ask">Your opponent wants a rematch</p>' : ''}
        <div class="row">${this.rematchButton()}<button type="button" class="btn" data-act="home">New game</button></div>
        <div class="links">
          <button type="button" class="link" data-act="close">View board</button>
          ${this.reviewable() ? '<button type="button" class="link strong" data-act="review">Review moves</button>' : ''}
        </div>
      </div>`);
    if (this.modal.hidden) {
      this.modal.hidden = false;
      requestAnimationFrame(() => requestAnimationFrame(() => this.modal.classList.add('open')));
    }
    this.renderActions();
  }

  closeModal() {
    this.modalOpen = false;
    this.modal.classList.remove('open');
    setTimeout(() => { if (!this.modalOpen) this.modal.hidden = true; }, 250);
    this.renderActions();
  }

  /** Clocks and time bars, every frame. */
  loop() {
    this.raf = requestAnimationFrame(() => this.loop());
    const s = this.s;
    if (!s || !this.panels) return;
    for (const c of COLORS) {
      const p = this.panels[c];
      const active = s.status === 'playing' && s.turn === c;
      const ms = active && this.deadline != null ? Math.max(0, this.deadline - now()) : s.move_ms;
      const secs = Math.ceil(ms / 1000);
      let text = fmt(secs);
      if (s.status === 'finished') text = s.result.reason === 'timeout' && s.result.winner !== c ? '0:00' : '–';
      if (p.clock._text !== text) {
        p.clock._text = text;
        p.clock.textContent = text;
      }
      p.bar.style.transform = `scaleX(${active ? ms / s.move_ms : 0})`;
      p.el.classList.toggle('low', active && secs <= 10);
      const tick = `${s.ply}:${secs}`;
      if (active && secs <= 5 && secs > 0 && this.isMine(c) && this.tick !== tick) {
        this.tick = tick;
        sound.play('tick');
      }
    }
  }

  /* --- input --- */

  onTap(i) {
    const s = this.s;
    if (!s || s.status !== 'playing') return;
    if (this.selected >= 0) {
      const d = this.dests.find(x => x.to === i);
      if (d) return this.play(this.selected, d);
    }
    const c = s.cells[i];
    if (this.canMove() && c === CH[s.turn]) {
      this.select(i === this.selected ? -1 : i);
      return;
    }
    if (c === 'G' || c === 'R') {
      const p = this.board.pieces[i];
      if (p) anim(p, 'nope');
    }
    this.select(-1);
  }

  select(i) {
    const b = this.board;
    if (this.selected >= 0) {
      b.cellsEl[this.selected].classList.remove('sel');
      const p = b.pieces[this.selected];
      if (p) p.classList.remove('selected');
    }
    this.dests.forEach(d => b.cellsEl[d.to].classList.remove('dest', 'jump', 'copy'));
    this.onHover(-1);
    this.selected = -1;
    this.dests = [];
    if (i < 0) return;

    const s = this.s;
    const dests = destinations(s, i);
    if (!dests.length) {
      anim(b.pieces[i], 'nope');
      sound.play('nope');
      return;
    }
    this.selected = i;
    this.dests = dests;
    b.cellsEl[i].classList.add('sel');
    b.pieces[i].classList.add('selected');
    dests.forEach((d, k) => {
      const cell = b.cellsEl[d.to];
      cell.style.setProperty('--k', k);
      cell.classList.add('dest', d.jump ? 'jump' : 'copy');
    });
    sound.play('select');
    buzz(6);
  }

  /** Desktop only: hovering a target shows which bacteria would convert. */
  onHover(i) {
    const b = this.board;
    this.threats.forEach(j => b.pieces[j] && b.pieces[j].classList.remove('threat'));
    this.threats = [];
    if (i < 0 || this.selected < 0 || !this.dests.some(d => d.to === i)) return;
    this.threats = captures(this.s, this.s.turn, i);
    this.threats.forEach(j => b.pieces[j] && b.pieces[j].classList.add('threat'));
  }

  /** Plays a move: animate right away, the server confirms a moment later. */
  play(from, d) {
    const s = this.s;
    const color = s.turn;
    const ch = CH[color];
    const converted = captures(s, color, d.to);
    const cells = s.cells.split('');
    if (d.jump) cells[from] = '.';
    cells[d.to] = ch;
    converted.forEach(j => (cells[j] = ch));
    const next = cells.join('');

    this.select(-1);
    const b = this.board;
    b.flush();
    const dur = b.animate({ from, to: d.to, jump: d.jump, converted, filled: [] }, color);
    this.optimistic = { ply: s.ply + 1, from, to: d.to, endAt: now() + dur };
    this.s = {
      ...s,
      cells: next,
      turn: other(color),
      scores: { green: countOf(next, 'G'), red: countOf(next, 'R') },
      last: { by: color, from, to: d.to, jump: d.jump, converted, filled: [] },
    };
    this.deadline = now() + s.move_ms;
    this.send({ t: 'move', from, to: d.to });
    buzz(12);
    this.paint();
    const optimistic = this.s;
    b.later(() => this.paintScores(optimistic), dur);
  }

  act(a) {
    if (a === 'resign') {
      clearTimeout(this.disarm);
      if (this.resignArmed) {
        this.resignArmed = false;
        this.send({ t: 'resign' });
      } else {
        this.resignArmed = true;
        this.disarm = setTimeout(() => { this.resignArmed = false; this.renderActions(); }, 3000);
      }
      this.renderActions();
    } else if (a === 'rematch') {
      this.send({ t: 'rematch' });
    } else if (a === 'home') {
      navigate('/');
    } else if (a === 'close') {
      this.closeModal();
    } else if (a === 'result') {
      this.openModal();
    } else if (a === 'review') {
      navigate('/r/' + this.id);
    }
  }
}

/* ------------------------------------------------------------------ recorded games */

const FILES = 'abcdefghijklmnopqrstuvwxyz';
const MODE_NAME = { online: 'Online', local: 'Pass and play', solo: 'Computer' };
/** Cell names like chess: files a, b, c… from the left, ranks from the bottom. */
const sq = (i, w, h) => FILES[i % w] + (h - Math.floor(i / w));
const isJump = (w, from, to) =>
  Math.max(Math.abs((from % w) - (to % w)), Math.abs(Math.floor(from / w) - Math.floor(to / w))) === 2;
/** Ataxx notation: a copy is just its target (any neighbour gives the same board), a jump is from–to. */
const moveName = (m, w, h) => (isJump(w, m.from, m.to) ? `${sq(m.from, w, h)}–${sq(m.to, w, h)}` : sq(m.to, w, h));
/** Turn number as in the move list: "12." before Green's move, "12…" before Red's. */
const turnOf = k => `${Math.floor(k / 2) + 1}${k % 2 ? '…' : '.'}`;
const signed = n => (n > 0 ? '+' : n < 0 ? '−' : '') + Math.abs(n);
const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

const rtf = typeof Intl !== 'undefined' && Intl.RelativeTimeFormat ? new Intl.RelativeTimeFormat('en', { numeric: 'auto' }) : null;
function ago(t) {
  const s = Math.max(0, Date.now() / 1000 - t);
  const units = [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60]];
  for (const [unit, n] of units) {
    if (s >= n) return rtf ? rtf.format(-Math.floor(s / n), unit) : `${Math.floor(s / n)} ${unit}s ago`;
  }
  return 'just now';
}

/** Plays a move on `b` ({w, h, cells: array}) in place, like Game::play on the server. */
function applyMove(b, color, from, to) {
  const ch = CH[color];
  const jump = isJump(b.w, from, to);
  const converted = captures(b, color, to);
  if (jump) b.cells[from] = '.';
  b.cells[to] = ch;
  converted.forEach(j => (b.cells[j] = ch));
  // The opponent is stuck: the empty cells are ours and the game is over.
  const filled = [];
  if (!hasMoves(b, other(color))) {
    b.cells.forEach((c, i) => { if (c === '.') { filled.push(i); b.cells[i] = ch; } });
  }
  return { by: color, from, to, jump, converted, filled };
}

/** Every position of a recorded game: `positions[k]` is the board before move `k`. Green always starts. */
function timeline(rec) {
  const b = { w: rec.w, h: rec.h, cells: rec.start.split('') };
  const positions = [rec.start];
  const moves = rec.moves.map((m, k) => {
    const mv = applyMove(b, k % 2 ? 'red' : 'green', m.from, m.to);
    positions.push(b.cells.join(''));
    return mv;
  });
  return { positions, moves };
}

function mySeat(rec) {
  const seat = playedSeats()[rec.id];
  return seat === 'green' || seat === 'red' ? seat : null;
}

function playerName(rec, c) {
  if (rec.ai[c]) return `Computer · ${cap(rec.difficulty || '')}`;
  return mySeat(rec) === c ? 'You' : NAME[c];
}

function resultOf(rec) {
  const w = rec.winner;
  const me = mySeat(rec);
  let title = 'Draw';
  let tone = '';
  if (w && me) { title = w === me ? 'You won' : 'You lost'; tone = w === me ? 'win' : 'lose'; }
  else if (w) { title = `${NAME[w]} won`; tone = w; }
  const loser = w && other(w);
  const how = rec.reason === 'timeout' ? `${NAME[loser]} ran out of time` : rec.reason === 'resign' ? `${NAME[loser]} resigned` : '';
  const short = { timeout: 'on time', resign: 'resigned' }[rec.reason] || '';
  return { title, tone, how, short };
}

function thumbHTML(w, cells) {
  const cls = { '#': 'x', '.': 'e', G: 'g', R: 'r' };
  return `<span class="thumb" style="--w:${w}" aria-hidden="true">${[...cells].map(c => `<i class="${cls[c]}"></i>`).join('')}</span>`;
}

/* ------------------------------------------------------------------ history list */

class HistoryView {
  constructor() {
    app.innerHTML = `
      <section class="history">
        <div class="history-head">
          <h1>Past games</h1>
          <div class="tabs" role="tablist">
            <button type="button" role="tab" data-tab="all">All games</button>
            <button type="button" role="tab" data-tab="mine">My games</button>
          </div>
        </div>
        <ul class="games"></ul>
        <div class="history-foot"></div>
      </section>`;
    this.list = $('.games');
    this.foot = $('.history-foot');
    $('.tabs').addEventListener('click', e => {
      const b = e.target.closest('[data-tab]');
      if (b) this.show(b.dataset.tab);
    });
    this.foot.addEventListener('click', e => { if (e.target.closest('[data-more]')) this.load(); });
    this.show(store.get('historyTab', 'all'));
  }

  show(tab) {
    this.tab = tab;
    store.set('historyTab', tab);
    app.querySelectorAll('[data-tab]').forEach(b => b.setAttribute('aria-selected', String(b.dataset.tab === tab)));
    this.list.innerHTML = '';
    this.next = null;
    this.load();
  }

  async load() {
    const tab = this.tab;
    let url = '/api/history?limit=20' + (this.next != null ? `&before=${this.next}` : '');
    if (tab === 'mine') {
      const ids = Object.keys(playedSeats());
      if (!ids.length) return this.render([], null);
      url = '/api/history?ids=' + encodeURIComponent(ids.join(','));
    }
    this.foot.innerHTML = '<span class="spinner"></span>';
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(res.statusText);
      const data = await res.json();
      if (!this.closed && tab === this.tab) this.render(data.games, data.next);
    } catch (_) {
      if (!this.closed && tab === this.tab) this.foot.innerHTML = '<p class="muted">Couldn’t load the games.</p><button type="button" class="btn ghost" data-more>Retry</button>';
    }
  }

  render(games, next) {
    this.next = next;
    this.list.insertAdjacentHTML('beforeend', games.map(rec => this.row(rec)).join(''));
    if (!this.list.children.length) {
      this.foot.innerHTML = this.tab === 'mine'
        ? '<p class="muted">Games you finish in this browser show up here.</p><a href="/" data-link class="btn primary">Play a game</a>'
        : '<p class="muted">No finished games yet.</p><a href="/" data-link class="btn primary">Play a game</a>';
    } else {
      this.foot.innerHTML = next != null ? '<button type="button" class="btn ghost" data-more>Load more</button>' : '';
    }
  }

  row(rec) {
    const { positions } = timeline(rec);
    const r = resultOf(rec);
    const meta = [MODE_NAME[rec.mode], rec.map, plural(rec.moves.length, 'move'), ago(rec.ended_at)].join(' · ');
    return `
      <li><a class="game-row" href="/r/${rec.id}" data-link>
        ${thumbHTML(rec.w, positions[positions.length - 1])}
        <span class="game-info">
          <span class="vs"><span class="tag green">${playerName(rec, 'green')}</span> <span class="muted">vs</span> <span class="tag red">${playerName(rec, 'red')}</span></span>
          <small>${meta}</small>
        </span>
        <span class="game-result ${r.tone}">
          <b>${r.title}</b>
          <small>${r.short ? `${r.short} · ` : ''}<span class="g">${rec.scores.green}</span>–<span class="r">${rec.scores.red}</span></small>
        </span>
      </a></li>`;
  }

  destroy() {
    this.closed = true;
  }
}

/* ------------------------------------------------------------------ replay and analysis */

const GRADES = {
  best: { label: 'Best move', glyph: '★' },
  good: { label: 'Good move', glyph: '✓' },
  inaccuracy: { label: 'Inaccuracy', glyph: '?!' },
  mistake: { label: 'Mistake', glyph: '?' },
  blunder: { label: 'Blunder', glyph: '??' },
  forced: { label: 'Only move', glyph: '□' },
};
const FLAWS = ['inaccuracy', 'mistake', 'blunder'];
const NAV_ICONS = {
  first: '<path d="M6 5v14M18 6l-8 6 8 6z"/>',
  prev: '<path d="M15 6l-6 6 6 6"/>',
  play: '<path d="M8 5.5v13l10-6.5z"/>',
  pause: '<path d="M8 5.5v13M16 5.5v13"/>',
  next: '<path d="M9 6l6 6-6 6"/>',
  last: '<path d="M18 5v14M6 6l8 6-8 6z"/>',
};
const navIcon = name => `<svg viewBox="0 0 24 24" width="22" height="22" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${NAV_ICONS[name]}</svg>`;

class ReplayView {
  constructor(id) {
    this.id = id;
    this.ply = 0; // moves played on the board
    this.analysis = null;
    this.variation = null; // the best line being shown instead of the game
    this.marked = {};
    app.innerHTML = '<div class="loading"><span class="spinner"></span></div>';
    this.onKey = e => this.key(e);
    document.addEventListener('keydown', this.onKey);
    this.load();
  }

  destroy() {
    this.closed = true;
    this.stop();
    document.removeEventListener('keydown', this.onKey);
    if (this.resize) this.resize.disconnect();
    if (this.board) this.board.cancel();
  }

  async load() {
    let rec;
    try {
      const res = await fetch('/api/history/' + this.id);
      if (res.status === 404) return this.renderMissing();
      if (!res.ok) throw new Error(res.statusText);
      rec = await res.json();
    } catch (_) {
      if (!this.closed) this.renderMissing('Couldn’t load this game. Check your connection and try again.');
      return;
    }
    if (this.closed) return;
    this.rec = rec;
    Object.assign(this, timeline(rec));
    this.render();
    const start = parseInt(location.hash.slice(1), 10);
    this.go(Number.isFinite(start) ? start : this.moves.length, { animate: false });
    this.loadAnalysis();
  }

  async loadAnalysis() {
    try {
      const res = await fetch(`/api/history/${this.id}/analysis`);
      if (!res.ok) throw new Error(res.statusText);
      const a = await res.json();
      if (this.closed) return;
      this.analysis = a;
    } catch (_) {
      this.analysisFailed = true;
    }
    if (this.closed) return;
    setHTML(this.listEl, this.moveListHTML());
    this.drawChart();
    this.paint();
  }

  renderMissing(text = 'This game doesn’t exist, or it never finished.') {
    app.innerHTML = `
      <section class="center">
        <div class="card">
          <h1>Game not found</h1>
          <p class="muted">${text}</p>
          <a href="/history" data-link class="btn primary">Past games</a>
        </div>
      </section>`;
  }

  /* --- layout --- */

  render() {
    const rec = this.rec;
    const r = resultOf(rec);
    const player = c => `
      <div class="player" data-c="${c}">
        <span class="swatch"><span class="bac ${c}"><i></i></span></span>
        <span class="name">${playerName(rec, c)}</span>
        <span class="count">0</span>
      </div>`;
    const nav = (name, label) => `<button type="button" class="nav-btn" data-nav="${name}" aria-label="${label}" title="${label}">${navIcon(name)}</button>`;
    app.innerHTML = `
      <section class="replay">
        <div class="replay-main">
          <div class="replay-head">
            <a href="/history" data-link class="back">‹ Past games</a>
            <span class="muted">${MODE_NAME[rec.mode]} · ${rec.map} · ${ago(rec.ended_at)}</span>
          </div>
          <div class="players">${player('green')}${player('red')}</div>
          <div class="scorebar" aria-hidden="true"><i class="g"></i><i class="r"></i></div>
          <div class="board replay-board"></div>
          <div class="controls">
            ${nav('first', 'First position')}${nav('prev', 'Previous move')}
            <button type="button" class="nav-btn play" data-nav="play" aria-label="Play" title="Play (space)">${navIcon('play')}</button>
            ${nav('next', 'Next move')}${nav('last', 'Last move')}
          </div>
          <p class="ply-label muted"></p>
        </div>
        <div class="replay-side">
          <div class="insight" aria-live="polite"></div>
          <div class="chart"></div>
          <div class="summary"></div>
          <div class="movelist-wrap">
            <div class="movelist-head"><span></span><span class="tag green">Green</span><span class="tag red">Red</span></div>
            <ol class="movelist"></ol>
          </div>
          <p class="final-line">${r.title} <span class="g">${rec.scores.green}</span>–<span class="r">${rec.scores.red}</span>${r.how ? ` · ${r.how}` : ''}</p>
        </div>
      </section>`;
    const root = $('.replay');
    this.board = new BoardView($('.board', root));
    this.board.setup(rec.w, rec.h, rec.start, true);
    this.board.sync(rec.start);
    this.counts = { green: $('.player[data-c="green"] .count', root), red: $('.player[data-c="red"] .count', root) };
    this.scorebar = { g: $('.scorebar .g', root), r: $('.scorebar .r', root) };
    this.insightEl = $('.insight', root);
    this.chartEl = $('.chart', root);
    this.summaryEl = $('.summary', root);
    this.listEl = $('.movelist', root);
    this.plyEl = $('.ply-label', root);
    setHTML(this.listEl, this.moveListHTML());
    root.addEventListener('click', e => this.click(e));
    if (window.ResizeObserver) {
      let width = 0;
      this.resize = new ResizeObserver(() => {
        if (this.chartEl.clientWidth !== width) { width = this.chartEl.clientWidth; this.drawChart(); this.paintCursor(); }
        this.drawArrows(this.arrows || []);
      });
      this.resize.observe(this.chartEl);
      this.resize.observe(this.board.el);
    }
  }

  /* --- navigation --- */

  click(e) {
    const t = e.target.closest('[data-ply],[data-nav],[data-act],[data-find]');
    if (!t || t.disabled) return;
    if (t.dataset.ply) { this.stop(); this.go(+t.dataset.ply, { animate: +t.dataset.ply === this.ply + 1 }); }
    else if (t.dataset.nav) this.nav(t.dataset.nav);
    else if (t.dataset.find) this.find(...t.dataset.find.split(':'));
    else if (t.dataset.act === 'best') this.showBest();
    else if (t.dataset.act === 'back') this.go(this.ply, { animate: false });
  }

  nav(a) {
    if (a === 'play') return this.playing ? this.stop() : this.play();
    this.stop();
    if (a === 'first') this.go(0);
    else if (a === 'prev') this.go(this.ply - 1);
    else if (a === 'next') this.go(this.ply + 1);
    else if (a === 'last') this.go(this.moves.length);
  }

  key(e) {
    if (e.metaKey || e.ctrlKey || e.altKey || !this.board) return;
    if (e.target instanceof Element && e.target.closest('input, textarea, button, a')) {
      if (e.key === ' ' || e.key === 'Enter') return; // let buttons and links handle their own keys
    }
    const a = { ArrowLeft: 'prev', ArrowRight: 'next', Home: 'first', End: 'last', ' ': 'play' }[e.key];
    if (!a) return;
    e.preventDefault();
    this.nav(a);
  }

  play() {
    if (this.ply >= this.moves.length) this.go(0, { animate: false });
    this.playing = true;
    const step = () => {
      if (this.ply >= this.moves.length) return this.stop();
      this.go(this.ply + 1);
      this.timer = setTimeout(step, 1200);
    };
    this.timer = setTimeout(step, 350);
    this.paintControls();
  }

  stop() {
    clearTimeout(this.timer);
    this.playing = false;
    if (this.board) this.paintControls();
  }

  /** Shows the game after `p` moves. One step forward is animated. */
  go(p, { animate = true } = {}) {
    p = Math.max(0, Math.min(this.moves.length, p));
    const b = this.board;
    b.cancel();
    this.variation = null;
    if (animate && p === this.ply + 1) {
      const m = this.moves[this.ply];
      b.sync(this.positions[this.ply]);
      const wait = b.animate(m, m.by);
      b.later(() => b.sync(this.positions[p]), wait);
    } else {
      b.sync(this.positions[p]);
    }
    this.ply = p;
    const m = this.moves[p - 1];
    this.mark('last', m ? [m.from, m.to] : []);
    history.replaceState(null, '', p === this.moves.length ? location.pathname : `${location.pathname}#${p}`);
    this.paint();
  }

  /** Jumps to the next move of `color` with that grade, after the current one. */
  find(color, grade) {
    const n = this.moves.length;
    const a = this.analysis;
    if (!a) return;
    for (let i = 1; i <= n; i++) {
      const k = (this.ply - 1 + i + n) % n;
      if (this.moves[k].by === color && a.moves[k] && a.moves[k].class === grade) {
        this.stop();
        this.go(k, { animate: false });
        this.go(k + 1);
        return;
      }
    }
  }

  /** Plays the best move instead of the one played, then the opponent's best answer. */
  showBest() {
    const k = this.ply - 1;
    const a = this.analysis && this.analysis.moves[k];
    if (!a) return;
    this.stop();
    const { w, h } = this.rec;
    const by = this.moves[k].by;
    const best = a.top[0];
    const b = this.board;
    const pos = { w, h, cells: this.positions[k].split('') };
    b.cancel();
    b.sync(this.positions[k]);
    const first = applyMove(pos, by, best.from, best.to);
    const afterFirst = pos.cells.join('');
    let wait = b.animate(first, by);
    let reply = null;
    if (a.best_reply && !first.filled.length) {
      reply = applyMove(pos, other(by), a.best_reply.from, a.best_reply.to);
      const end = pos.cells.join('');
      b.later(() => {
        b.sync(afterFirst);
        b.animate(reply, reply.by);
      }, wait + 700);
      wait += 700 + 1400;
      b.later(() => b.sync(end), wait);
    } else {
      b.later(() => b.sync(afterFirst), wait);
    }
    this.variation = { k, first, reply, before: this.positions[k] };
    this.paintBoardMarks();
    this.mark('last', []);
    this.drawArrows(reply ? [best, { ...reply, reply: true }] : [best]);
    this.paintInsight();
  }

  mark(cls, cells) {
    const els = this.board.cellsEl;
    (this.marked[cls] || []).forEach(i => els[i] && els[i].classList.remove(cls));
    this.marked[cls] = cells;
    cells.forEach(i => els[i].classList.add(cls));
  }

  /* --- painting --- */

  paint() {
    const cells = this.positions[this.ply];
    const scores = { green: countOf(cells, 'G'), red: countOf(cells, 'R') };
    for (const c of COLORS) {
      const v = String(scores[c]);
      if (this.counts[c].textContent !== v) this.counts[c].textContent = v;
      const toMove = this.ply < this.moves.length && this.moves[this.ply].by === c;
      $(`.player[data-c="${c}"]`).classList.toggle('active', toMove);
    }
    this.scorebar.g.style.flexGrow = scores.green;
    this.scorebar.r.style.flexGrow = scores.red;
    setHTML(this.plyEl, this.ply ? `${this.ply} / ${this.moves.length}` : `Start · ${plural(this.moves.length, 'move')}`);
    this.paintControls();
    this.listEl.querySelectorAll('[aria-current]').forEach(el => el.removeAttribute('aria-current'));
    const cur = this.listEl.querySelector(`[data-ply="${this.ply}"]`);
    if (cur) {
      cur.setAttribute('aria-current', 'step');
      const box = this.listEl.parentElement;
      if (box.scrollHeight > box.clientHeight) {
        const top = cur.offsetTop;
        if (top < box.scrollTop + 30 || top > box.scrollTop + box.clientHeight - 60) box.scrollTop = top - box.clientHeight / 2;
      }
    }
    this.paintInsight();
    this.paintBoardMarks();
    this.paintCursor();
    setHTML(this.summaryEl, this.summaryHTML());
  }

  /** On the board: the grade of the move just played, and an arrow to the better move if there was one. */
  paintBoardMarks() {
    if (this.gradeEl) this.gradeEl.remove();
    this.gradeEl = null;
    const k = this.ply - 1;
    const a = this.analysis && this.analysis.moves[k];
    if (!a || this.variation) return this.drawArrows([]);
    const g = GRADES[a.class];
    const el = document.createElement('span');
    el.className = `grade-badge q-${a.class}`;
    el.textContent = g.glyph;
    el.title = g.label;
    this.board.cellsEl[this.moves[k].to].appendChild(el);
    this.gradeEl = el;
    const better = a.class !== 'best' && a.class !== 'forced';
    this.drawArrows(better ? [a.top[0]] : []);
  }

  /** Arrows from cell to cell, drawn over the board. */
  drawArrows(list) {
    this.arrows = list;
    const board = this.board.el;
    if (!this.arrowsEl) {
      this.arrowsEl = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      this.arrowsEl.setAttribute('class', 'arrows');
      this.arrowsEl.setAttribute('aria-hidden', 'true');
      board.appendChild(this.arrowsEl);
    }
    const cells = this.board.cellsEl;
    const size = cells[0].offsetWidth;
    this.arrowsEl.setAttribute('viewBox', `0 0 ${board.clientWidth} ${board.clientHeight}`);
    const centre = i => [cells[i].offsetLeft + cells[i].offsetWidth / 2, cells[i].offsetTop + cells[i].offsetHeight / 2];
    this.arrowsEl.innerHTML = list.map(m => {
      const [x1, y1] = centre(m.from);
      const [x2, y2] = centre(m.to);
      const len = Math.hypot(x2 - x1, y2 - y1);
      const [ux, uy] = [(x2 - x1) / len, (y2 - y1) / len];
      const [nx, ny] = [-uy, ux];
      const shaft = size * 0.07; // half widths
      const head = size * 0.2;
      const tip = [x2 - ux * size * 0.06, y2 - uy * size * 0.06];
      const base = [tip[0] - ux * size * 0.36, tip[1] - uy * size * 0.36];
      const start = [x1 + ux * size * 0.12, y1 + uy * size * 0.12];
      const pt = ([x, y], w) => `${(x + nx * w).toFixed(1)},${(y + ny * w).toFixed(1)}`;
      const points = [pt(start, shaft), pt(base, shaft), pt(base, head), pt(tip, 0), pt(base, -head), pt(base, -shaft), pt(start, -shaft)];
      return `<polygon class="arrow${m.reply ? ' reply' : ''}" points="${points.join(' ')}"/>`;
    }).join('');
  }

  paintControls() {
    const root = app;
    const at = this.ply;
    const set = (name, disabled) => { const el = $(`[data-nav="${name}"]`, root); if (el) el.disabled = disabled; };
    set('first', at === 0);
    set('prev', at === 0);
    set('next', at >= this.moves.length);
    set('last', at >= this.moves.length);
    const play = $('[data-nav="play"]', root);
    if (play) {
      setHTML(play, navIcon(this.playing ? 'pause' : 'play'));
      play.setAttribute('aria-label', this.playing ? 'Pause' : 'Play');
    }
  }

  moveListHTML() {
    const { w, h } = this.rec;
    const a = this.analysis;
    const btn = k => {
      const m = this.moves[k];
      const g = a && a.moves[k] && a.moves[k].class;
      const flag = FLAWS.includes(g) ? `<span class="q q-${g}" title="${GRADES[g].label}">${GRADES[g].glyph}</span>` : '';
      const conv = m.converted.length ? `<small>×${m.converted.length}</small>` : '';
      return `<button type="button" class="mv" data-ply="${k + 1}">${moveName(m, w, h)}${conv}${flag}</button>`;
    };
    let html = '';
    for (let k = 0; k < this.moves.length; k += 2) {
      html += `<li><span class="num">${k / 2 + 1}</span>${btn(k)}${k + 1 < this.moves.length ? btn(k + 1) : '<span></span>'}</li>`;
    }
    return html;
  }

  summaryHTML() {
    const a = this.analysis;
    if (!a) return '';
    const row = c => {
      const mine = a.moves.filter((m, k) => this.moves[k].by === c);
      const chips = ['best', ...FLAWS].map(g => {
        const n = mine.filter(m => m.class === g).length;
        const label = `${n} ${GRADES[g].label.toLowerCase()}${n === 1 ? '' : 's'}`;
        return `<button type="button" class="chip q-${g}" data-find="${c}:${g}" title="${label}${n && g !== 'best' ? ': jump to the next one' : ''}" aria-label="${label}"${n ? '' : ' disabled'}><b>${GRADES[g].glyph}</b>${n}</button>`;
      }).join('');
      return `<div class="sum-row"><span class="tag ${c}">${playerName(this.rec, c)}</span><span class="chips">${chips}</span></div>`;
    };
    return row('green') + row('red');
  }

  /* --- the explanation of a move --- */

  paintInsight() {
    setHTML(this.insightEl, this.variation ? this.variationHTML() : this.ply ? this.moveHTML(this.ply - 1) : this.startHTML());
  }

  startHTML() {
    const a = this.analysis;
    const depth = a ? a.depth : 6;
    return `
      <div class="insight-head"><b>Start</b></div>
      <p>Green moves first. Step through the game with <kbd>←</kbd> <kbd>→</kbd> or the buttons, or tap a move in the list.</p>
      <p class="muted">Each move is compared with the best one by looking ${depth} moves ahead (${depth / 2} each) and counting the lead: your cells minus theirs.</p>
      <p class="muted">
        <span class="q q-best">★</span> best, <span class="q q-good">✓</span> good, <span class="q q-inaccuracy">?!</span> inaccuracy,
        <span class="q q-mistake">?</span> mistake, <span class="q q-blunder">??</span> blunder.</p>
      ${this.analysisStatus()}`;
  }

  analysisStatus() {
    if (this.analysis) return '';
    if (this.analysisFailed) return '<p class="muted">The analysis isn’t available right now.</p>';
    return '<p class="analysing"><span class="spinner small"></span>Analysing the game…</p>';
  }

  /** "copies to d4 and converts 2" */
  describe(color, board, from, to) {
    const { w, h } = this.rec;
    const n = captures(board, color, to).length;
    const act = isJump(w, from, to) ? `jumps from <b>${sq(from, w, h)}</b> to <b>${sq(to, w, h)}</b>` : `copies to <b>${sq(to, w, h)}</b>`;
    return `${act} and ${n ? `converts <b>${n}</b>` : 'converts nothing'}`;
  }

  moveHTML(k) {
    const { w, h } = this.rec;
    const m = this.moves[k];
    const c = m.by;
    const o = other(c);
    const before = { w, h, cells: this.positions[k] };
    const after = { w, h, cells: this.positions[k + 1] };
    const gap = (m.jump ? 0 : 1) + 2 * m.converted.length;
    const a = this.analysis && this.analysis.moves[k];
    const grade = a ? GRADES[a.class] : null;

    let html = `
      <div class="insight-head">
        <span class="tag ${c}">${NAME[c]}</span><b>${turnOf(k)} ${moveName(m, w, h)}</b>
        ${grade ? `<span class="badge q-${a.class}"><b>${grade.glyph}</b>${grade.label}</span>` : ''}
      </div>
      <p>${NAME[c]} ${this.describe(c, before, m.from, m.to)}${m.jump ? `, leaving ${sq(m.from, w, h)} empty` : ''}.
        ${gap ? `${NAME[c]} gains ${gap} on ${NAME[o]}.` : 'The gap between the two doesn’t change.'}</p>`;
    if (m.filled.length) {
      html += `<p>${NAME[o]} has no move left, so the last ${plural(m.filled.length, 'empty cell')} go to ${NAME[c]}. Game over.</p>`;
    }
    if (!a) return html + this.analysisStatus();

    const best = a.top[0];
    const reply = a.reply;
    const replyConv = reply ? captures(after, o, reply.to).length : 0;
    if (reply) {
      const hole = m.jump && reply.to === m.from;
      html += `<p>${NAME[o]}’s best answer: <b>${moveName(reply, w, h)}</b>${replyConv ? `, converting ${replyConv}` : ''}${hole ? ', right into the hole the jump left' : ''}.</p>`;
    }
    if (a.class === 'forced') return html + '<p class="verdict">It was the only move.</p>';
    if (a.class === 'best') return html + `<p class="verdict">Nothing better: ${a.choices > 1 ? `the best of ${a.choices} possible moves` : 'the best move'}.</p>`;

    html += `
      <p class="verdict"><span class="arrow-key" aria-hidden="true"></span>Better: <button type="button" class="mvlink" data-act="best">${moveName(best, w, h)}</button>, which ${this.describe(c, before, best.from, best.to)}.</p>
      <table class="compare">
        <tr><th></th><th>${NAME[c]}’s lead ${this.analysis.depth} moves later</th></tr>
        <tr><td>Played ${moveName(m, w, h)}</td><td>${this.outlook(a.score)}</td></tr>
        <tr><td>Best ${moveName(best, w, h)}</td><td>${this.outlook(best.score)}</td></tr>
      </table>
      ${FLAWS.includes(a.class) ? `<p class="tip">${this.tip(k, a, before, replyConv)}</p>` : ''}
      <button type="button" class="btn ghost small" data-act="best">Show the best move</button>`;
    return html;
  }

  outlook(score) {
    const win = this.analysis.win;
    if (score >= win) return '<b>wins by force</b>';
    if (score <= -win) return '<b>loses by force</b>';
    return `<b>${signed(score)}</b>`;
  }

  /** One lesson that fits what went wrong. */
  tip(k, a, before, replyConv) {
    const { w, h } = this.rec;
    const win = this.analysis.win;
    const m = this.moves[k];
    const c = m.by;
    const o = other(c);
    const best = a.top[0];
    const bestJump = isJump(w, best.from, best.to);
    const bestConv = captures(before, c, best.to).length;
    let bestReplyConv = 0;
    if (a.best_reply) {
      const after = { w, h, cells: before.cells.split('') };
      applyMove(after, c, best.from, best.to);
      bestReplyConv = captures(after, o, a.best_reply.to).length;
    }
    if (best.score >= win) {
      return `There was a forced win: after ${moveName(best, w, h)}, ${NAME[o]} soon runs out of moves and every empty cell goes to ${NAME[c]}.`;
    }
    if (a.score <= -win) {
      return `This lets ${NAME[o]} force a win: ${NAME[c]} soon has no move left, and when a player can’t move, every empty cell goes to the other one.`;
    }
    if (m.jump && a.reply && a.reply.to === m.from && replyConv >= 2) {
      return 'Jumping out of a group leaves a hole beside your bacteria. The opponent lands in it and converts them.';
    }
    if (m.jump && !bestJump && m.converted.length <= bestConv) {
      return 'A jump costs a cell: it leaves a hole behind. Jump only when it converts more than any copy can.';
    }
    if (replyConv >= 3 && bestReplyConv < replyConv) {
      return 'Before moving, check what the opponent takes back. An empty cell touching several of your bacteria is a target.';
    }
    if (bestConv > m.converted.length) return 'Look at every capture first: the best move converted more right away.';
    if (bestConv < m.converted.length && bestReplyConv < replyConv) {
      return 'Taking the most right now isn’t always best: the best move converts less but leaves the opponent less to take back.';
    }
    return 'The difference shows a few moves later. Tap “Show the best move” to see the line.';
  }

  variationHTML() {
    const { w, h } = this.rec;
    const v = this.variation;
    const a = this.analysis.moves[v.k];
    const c = v.first.by;
    const r = v.reply;
    return `
      <div class="insight-head"><span class="badge q-best"><b>★</b>Best line</span><span class="muted">instead of ${turnOf(v.k)} ${moveName(this.moves[v.k], w, h)}</span></div>
      <p><span class="arrow-key" aria-hidden="true"></span>${NAME[c]} ${this.describe(c, { w, h, cells: v.before }, v.first.from, v.first.to)}.</p>
      ${r ? `<p><span class="arrow-key reply" aria-hidden="true"></span>${NAME[r.by]}’s best answer, <b>${moveName(r, w, h)}</b>, converts ${r.converted.length ? `<b>${r.converted.length}</b>` : 'nothing'}.</p>` : ''}
      ${v.first.filled.length ? `<p>${NAME[other(c)]} is stuck: ${NAME[c]} takes every empty cell and wins.</p>` : ''}
      <p class="muted">${NAME[c]}’s lead ${this.analysis.depth} moves later: ${this.outlook(a.top[0].score)} instead of ${this.outlook(a.score)}.</p>
      <div class="row">
        <button type="button" class="btn ghost small" data-act="best">Replay</button>
        <button type="button" class="btn small" data-act="back">Back to the game</button>
      </div>`;
  }

  /* --- advantage chart --- */

  /**
   * Green's lead after each move, as the analysis sees it. Index 0 is the start.
   * Each score ends on the opponent's answer, so on its own the curve zigzags:
   * averaging two moves in a row cancels that out.
   */
  evals() {
    const a = this.analysis;
    if (!a || !a.moves.length) return null;
    const win = a.win;
    const raw = [a.moves[0].top[0].score];
    a.moves.forEach((m, k) => raw.push(this.moves[k].by === 'green' ? m.score : -m.score));
    const plain = raw.filter(v => Math.abs(v) < win).map(Math.abs);
    const cap = Math.max(6, ...plain);
    // A decided game counts as a big lead, so it doesn't swamp the average.
    const v = raw.map(x => (Math.abs(x) >= win ? Math.sign(x) * (cap + 2) : x));
    return raw.map((x, p) => ({
      value: p ? (v[p - 1] + v[p]) / 2 : v[0],
      decided: Math.abs(x) >= win ? Math.sign(x) : 0,
    }));
  }

  drawChart() {
    const evals = this.evals();
    const el = this.chartEl;
    if (!evals || !el) { if (el) el.innerHTML = ''; return; }
    const vals = evals.map(e => e.value);
    const n = vals.length - 1;
    const max = Math.max(6, ...vals.map(Math.abs));
    const W = Math.max(200, el.clientWidth);
    const H = 84;
    const pad = 7;
    const mid = H / 2;
    const x = p => pad + (n ? p / n : 0) * (W - 2 * pad);
    const y = v => mid - (Math.max(-max, Math.min(max, v)) / max) * (mid - pad);
    this.chartX = x;
    this.chartN = n;
    this.chartPad = pad;
    const pts = vals.map((v, p) => `${x(p).toFixed(1)},${y(v).toFixed(1)}`);
    const line = 'M' + pts.join('L');
    const area = `${line}L${x(n).toFixed(1)},${mid}L${x(0).toFixed(1)},${mid}Z`;
    const dots = this.analysis.moves.map((m, k) => (m.class === 'mistake' || m.class === 'blunder')
      ? `<circle class="dot q-${m.class}" cx="${x(k + 1).toFixed(1)}" cy="${y(vals[k + 1]).toFixed(1)}" r="4.5"/>` : '').join('');
    el.innerHTML = `
      <div class="chart-head"><b>Advantage</b><span class="muted"><span class="key g"></span>Green ahead <span class="key r"></span>Red ahead</span></div>
      <div class="chart-plot">
        <svg width="${W}" height="${H}" viewBox="0 0 ${W} ${H}" role="img" aria-label="Score gap after each move. Mistakes and blunders are marked.">
          <defs>
            <clipPath id="clip-up"><rect x="0" y="0" width="${W}" height="${mid}"/></clipPath>
            <clipPath id="clip-down"><rect x="0" y="${mid}" width="${W}" height="${mid}"/></clipPath>
          </defs>
          <path class="area g" d="${area}" clip-path="url(#clip-up)"/>
          <path class="area r" d="${area}" clip-path="url(#clip-down)"/>
          <line class="zero" x1="${pad}" x2="${W - pad}" y1="${mid}" y2="${mid}"/>
          <line class="cursor" y1="2" y2="${H - 2}"/>
          <line class="hover" y1="2" y2="${H - 2}" visibility="hidden"/>
          <path class="line" d="${line}"/>
          ${dots}
        </svg>
        <div class="chart-tip" hidden></div>
      </div>`;
    const svg = $('svg', el);
    const tipEl = $('.chart-tip', el);
    const hover = $('.hover', el);
    const at = e => {
      const r = svg.getBoundingClientRect();
      return Math.max(0, Math.min(n, Math.round(((e.clientX - r.left - pad) / (r.width - 2 * pad)) * n)));
    };
    svg.addEventListener('pointermove', e => {
      const p = at(e);
      const px = x(p);
      hover.setAttribute('x1', px);
      hover.setAttribute('x2', px);
      hover.setAttribute('visibility', 'visible');
      tipEl.hidden = false;
      tipEl.innerHTML = this.chartTip(p, evals[p]);
      const half = tipEl.offsetWidth / 2;
      tipEl.style.left = Math.max(half, Math.min(W - half, px)) + 'px';
    });
    svg.addEventListener('pointerleave', () => { hover.setAttribute('visibility', 'hidden'); tipEl.hidden = true; });
    svg.addEventListener('click', e => { this.stop(); this.go(at(e), { animate: false }); });
  }

  chartTip(p, e) {
    const v = Math.round(e.value);
    const lead = e.decided ? `${e.decided > 0 ? 'Green' : 'Red'} wins by force` : v === 0 ? 'Even' : `${v > 0 ? 'Green' : 'Red'} ${signed(Math.abs(v))}`;
    if (!p) return `<b>Start</b> · ${lead}`;
    const m = this.moves[p - 1];
    const g = GRADES[this.analysis.moves[p - 1].class];
    return `<b>${turnOf(p - 1)} ${moveName(m, this.rec.w, this.rec.h)}</b> <span class="tag ${m.by}">${NAME[m.by]}</span> · ${g.label} · ${lead}`;
  }

  paintCursor() {
    const cur = this.chartEl && $('.cursor', this.chartEl);
    if (!cur || !this.chartX) return;
    const px = this.chartX(Math.min(this.ply, this.chartN));
    cur.setAttribute('x1', px);
    cur.setAttribute('x2', px);
  }
}

async function copyText(text, btn) {
  let ok = false;
  try {
    await navigator.clipboard.writeText(text);
    ok = true;
  } catch (_) {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.style.cssText = 'position:fixed;opacity:0';
    document.body.appendChild(ta);
    ta.select();
    try { ok = document.execCommand('copy'); } catch (_) { /* nothing else to try */ }
    ta.remove();
  }
  if (!ok) return;
  btn.textContent = 'Copied ✓';
  btn.classList.add('ok');
  buzz(10);
  setTimeout(() => { btn.textContent = 'Copy'; btn.classList.remove('ok'); }, 1600);
}

route();
