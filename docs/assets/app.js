/* =========================================================================
   AScript docs — client app
   - hash router that fetches Markdown fragments from content/
   - compact Markdown renderer (headings, tables, lists, code, callouts)
   - AScript syntax highlighter
   - cmd-K search across all pages
   - "on this page" TOC + scroll-spy
   ========================================================================= */

// ---- site map (drives sidebar + search) ---------------------------------
const NAV = [
  { title: 'Introduction', items: [
    ['introduction', 'Overview'],
    ['getting-started', 'Getting started'],
    ['cli', 'The ascript CLI'],
    ['runtime', 'Compilation & runtime'],
  ]},
  { title: 'Language', items: [
    ['language/syntax', 'Syntax & control flow'],
    ['language/values-types', 'Values & types'],
    ['language/type-contracts', 'Type contracts'],
    ['language/errors', 'Errors & results'],
    ['language/classes-enums', 'Classes, enums, match'],
    ['language/modules-async', 'Modules & async'],
  ]},
  { title: 'Standard library', items: [
    ['stdlib/overview', 'Overview'],
    ['stdlib/collections', 'Core & collections'],
    ['stdlib/utilities', 'Utilities (LRU, events, templates)'],
    ['stdlib/data', 'Data & serialization'],
    ['stdlib/system', 'System & files'],
    ['stdlib/db', 'Databases (Postgres & Redis)'],
    ['stdlib/time', 'Time & locale'],
    ['stdlib/net', 'Networking & HTTP'],
    ['stdlib/log', 'Logging'],
    ['stdlib/telemetry', 'Telemetry & observability'],
    ['stdlib/tui', 'Terminal UI'],
  ]},
  { title: 'Resources', items: [
    ['examples', 'Examples'],
  ]},
];

const PAGE_TITLES = {};
const PAGE_ORDER = [];
NAV.forEach(s => s.items.forEach(([slug, title]) => { PAGE_TITLES[slug] = title; PAGE_ORDER.push(slug); }));

// ======================= Markdown renderer ===============================
function escapeHtml(s) {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

function renderInline(s) {
  // inline code first (protect its contents)
  const codes = [];
  s = s.replace(/`([^`]+)`/g, (_, c) => { codes.push(c); return `\uE000${codes.length - 1}\uE000`; });
  s = escapeHtml(s);
  // links [text](url)
  s = s.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_, t, u) => {
    const internal = !/^https?:|^#|^mailto:/.test(u);
    const href = internal ? `#/${u.replace(/^\.?\//, '')}` : u;
    const ext = /^https?:/.test(u) ? ' target="_blank" rel="noopener"' : '';
    return `<a href="${href}"${ext}>${t}</a>`;
  });
  // bold + italic
  s = s.replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>');
  s = s.replace(/(^|[^*])\*([^*]+)\*/g, '$1<em>$2</em>');
  // restore inline code
  s = s.replace(/\uE000(\d+)\uE000/g, (_, i) => `<code>${escapeHtml(codes[+i])}</code>`);
  return s;
}

function slugify(s) {
  return s.toLowerCase().replace(/[^\w\s-]/g, '').trim().replace(/\s+/g, '-');
}

function renderMarkdown(md) {
  const lines = md.replace(/\r\n/g, '\n').split('\n');
  let html = '', i = 0;
  const headings = [];

  while (i < lines.length) {
    let line = lines[i];

    // fenced code
    const fence = line.match(/^```(\w+)?\s*$/);
    if (fence) {
      const lang = fence[1] || 'text';
      const buf = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) { buf.push(lines[i]); i++; }
      i++; // closing fence
      const code = lang === 'ascript' ? highlightAScript(buf.join('\n')) : escapeHtml(buf.join('\n'));
      html += `<pre><button class="copybtn">copy</button><span class="codeblock-lang">${lang}</span><code>${code}</code></pre>`;
      continue;
    }

    // callout:  > [!NOTE] text...   (consumes following > lines)
    const callout = line.match(/^>\s*\[!(NOTE|TIER1|TIER2|WARN)\]\s*(.*)$/i);
    if (callout) {
      const kind = callout[1].toLowerCase();
      const labels = { note: 'NOTE', tier1: 'TIER 1', tier2: 'TIER 2', warn: 'CAUTION' };
      const buf = [callout[2]];
      i++;
      while (i < lines.length && /^>\s?/.test(lines[i])) { buf.push(lines[i].replace(/^>\s?/, '')); i++; }
      html += `<div class="callout ${kind}"><div class="cico">${labels[kind]}</div><div>${renderInline(buf.join(' ').trim())}</div></div>`;
      continue;
    }

    // table (header row + |---| separator)
    if (/^\|.*\|/.test(line) && i + 1 < lines.length && /^\|[\s:|-]+\|/.test(lines[i + 1])) {
      const parseRow = r => r.replace(/^\||\|$/g, '').split('|').map(c => c.trim());
      const head = parseRow(line);
      i += 2;
      const rows = [];
      while (i < lines.length && /^\|.*\|/.test(lines[i])) { rows.push(parseRow(lines[i])); i++; }
      let t = '<table><thead><tr>' + head.map(h => `<th>${renderInline(h)}</th>`).join('') + '</tr></thead><tbody>';
      t += rows.map(r => '<tr>' + r.map(c => `<td>${renderInline(c)}</td>`).join('') + '</tr>').join('');
      t += '</tbody></table>';
      html += t;
      continue;
    }

    // headings
    const h = line.match(/^(#{1,4})\s+(.*)$/);
    if (h) {
      const lvl = h[1].length;
      const text = h[2].trim();
      const id = slugify(text);
      if (lvl === 2 || lvl === 3) headings.push({ lvl, text, id });
      html += `<h${lvl} id="${id}">${renderInline(text)}</h${lvl}>`;
      i++; continue;
    }

    // hr
    if (/^---+\s*$/.test(line)) { html += '<hr>'; i++; continue; }

    // lists
    if (/^\s*([-*]|\d+\.)\s+/.test(line)) {
      const ordered = /^\s*\d+\.\s+/.test(line);
      const tag = ordered ? 'ol' : 'ul';
      let list = `<${tag}>`;
      while (i < lines.length && /^\s*([-*]|\d+\.)\s+/.test(lines[i])) {
        const item = lines[i].replace(/^\s*([-*]|\d+\.)\s+/, '');
        list += `<li>${renderInline(item)}</li>`;
        i++;
      }
      list += `</${tag}>`;
      html += list;
      continue;
    }

    // blank
    if (/^\s*$/.test(line)) { i++; continue; }

    // eyebrow marker  :::eyebrow text
    const eb = line.match(/^:::eyebrow\s+(.*)$/);
    if (eb) { html += `<div class="eyebrow">${renderInline(eb[1])}</div>`; i++; continue; }

    // paragraph (gather until blank/special)
    const para = [line]; i++;
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !/^(#{1,4}\s|```|\||>\s*\[!|\s*([-*]|\d+\.)\s|---+\s*$|:::)/.test(lines[i])) {
      para.push(lines[i]); i++;
    }
    html += `<p>${renderInline(para.join(' '))}</p>`;
  }

  return { html, headings };
}

// ======================= AScript syntax highlighter ======================
const AS_KEYWORDS = new Set(('let const fn return if else while for of in match async await ' +
  'class extends super self enum import export from').split(' '));
const AS_LITERALS = new Set('nil true false'.split(' '));
const AS_BUILTINS = new Set('print len type assert range recover Ok Err test'.split(' '));

function highlightAScript(src) {
  let out = '';
  let i = 0;
  const n = src.length;
  const isIdStart = c => /[A-Za-z_]/.test(c);
  const isId = c => /[A-Za-z0-9_]/.test(c);

  while (i < n) {
    const c = src[i];

    // line comment
    if (c === '/' && src[i + 1] === '/') {
      let j = i; while (j < n && src[j] !== '\n') j++;
      out += `<span class="tok-com">${escapeHtml(src.slice(i, j))}</span>`; i = j; continue;
    }
    // block comment
    if (c === '/' && src[i + 1] === '*') {
      let j = i + 2; while (j < n && !(src[j] === '*' && src[j + 1] === '/')) j++;
      j = Math.min(n, j + 2);
      out += `<span class="tok-com">${escapeHtml(src.slice(i, j))}</span>`; i = j; continue;
    }
    // template string with ${...}
    if (c === '`') {
      let j = i + 1; let seg = '`'; let res = '';
      while (j < n && src[j] !== '`') {
        if (src[j] === '$' && src[j + 1] === '{') {
          res += `<span class="tok-str">${escapeHtml(seg)}</span>`; seg = '';
          let k = j + 2, depth = 1;
          while (k < n && depth > 0) { if (src[k] === '{') depth++; if (src[k] === '}') depth--; if (depth === 0) break; k++; }
          const inner = src.slice(j + 2, k);
          res += `<span class="tok-tmpl">\${</span>${highlightAScript(inner)}<span class="tok-tmpl">}</span>`;
          j = k + 1;
        } else { seg += src[j]; j++; }
      }
      seg += '`';
      res += `<span class="tok-str">${escapeHtml(seg)}</span>`;
      out += res; i = j + 1; continue;
    }
    // plain strings
    if (c === '"' || c === "'") {
      const q = c; let j = i + 1;
      while (j < n && src[j] !== q) { if (src[j] === '\\') j++; j++; }
      j = Math.min(n, j + 1);
      out += `<span class="tok-str">${escapeHtml(src.slice(i, j))}</span>`; i = j; continue;
    }
    // numbers (incl 0x 0b 1e _ )
    if (/[0-9]/.test(c) || (c === '.' && /[0-9]/.test(src[i + 1] || ''))) {
      let j = i; while (j < n && /[0-9a-fA-FxXbBoO._eE+-]/.test(src[j])) {
        // stop a trailing +/- that isn't part of an exponent
        if ((src[j] === '+' || src[j] === '-') && !/[eE]/.test(src[j - 1])) break;
        j++;
      }
      out += `<span class="tok-num">${escapeHtml(src.slice(i, j))}</span>`; i = j; continue;
    }
    // identifiers / keywords
    if (isIdStart(c)) {
      let j = i; while (j < n && isId(src[j])) j++;
      const word = src.slice(i, j);
      // look ahead for "(" => function call
      let k = j; while (k < n && /\s/.test(src[k])) k++;
      const isCall = src[k] === '(';
      let cls = null;
      if (AS_KEYWORDS.has(word)) cls = 'tok-key';
      else if (AS_LITERALS.has(word)) cls = 'tok-bool';
      else if (AS_BUILTINS.has(word)) cls = 'tok-fn';
      else if (isCall) cls = 'tok-fn';
      else if (/^[A-Z]/.test(word)) cls = 'tok-type';
      out += cls ? `<span class="${cls}">${word}</span>` : word;
      i = j; continue;
    }
    // punctuation/operators
    if (/[{}()[\].,;]/.test(c)) { out += `<span class="tok-punc">${escapeHtml(c)}</span>`; i++; continue; }
    if (/[+\-*/%=<>!&|?:^~]/.test(c)) {
      let j = i; while (j < n && /[+\-*/%=<>!&|?:^~.]/.test(src[j])) j++;
      out += `<span class="tok-op">${escapeHtml(src.slice(i, j))}</span>`; i = j; continue;
    }
    out += escapeHtml(c); i++;
  }
  return out;
}

// ======================= sidebar =========================================
function buildSidebar() {
  const el = document.getElementById('sidebar');
  if (!el) return;
  el.innerHTML = NAV.map(sec => `
    <div class="nav-sec">
      <p class="nav-title">${sec.title}</p>
      ${sec.items.map(([slug, title]) => `<a href="#/${slug}" data-slug="${slug}">${title}</a>`).join('')}
    </div>`).join('');
}

function setActiveNav(slug) {
  document.querySelectorAll('#sidebar a').forEach(a => {
    a.classList.toggle('active', a.dataset.slug === slug);
  });
}

// ======================= page nav (prev / next) ==========================
function pageNavHtml(slug) {
  const idx = PAGE_ORDER.indexOf(slug);
  if (idx < 0) return '';
  const prev = PAGE_ORDER[idx - 1], next = PAGE_ORDER[idx + 1];
  let h = '<nav class="page-nav">';
  h += prev ? `<a class="prev" href="#/${prev}"><span class="pn-l">← Previous</span><span class="pn-t">${PAGE_TITLES[prev]}</span></a>` : '<span></span>';
  h += next ? `<a class="next" href="#/${next}"><span class="pn-l">Next →</span><span class="pn-t">${PAGE_TITLES[next]}</span></a>` : '<span></span>';
  h += '</nav>';
  return h;
}

// ======================= TOC + scroll spy ================================
let spyHandler = null;
function buildToc(headings) {
  const el = document.getElementById('toc');
  if (!el) return;
  const hs = headings.filter(h => h.lvl === 2 || h.lvl === 3);
  if (hs.length < 2) { el.innerHTML = ''; return; }
  el.innerHTML = `<div class="toc-title">On this page</div>` +
    hs.map(h => `<a href="#/${currentSlug}#${h.id}" class="${h.lvl === 3 ? 'lvl3' : ''}" data-id="${h.id}">${escapeHtml(h.text)}</a>`).join('');

  const targets = hs.map(h => document.getElementById(h.id)).filter(Boolean);
  if (spyHandler) document.removeEventListener('scroll', spyHandler);
  spyHandler = () => {
    let cur = targets[0]?.id;
    for (const t of targets) { if (t.getBoundingClientRect().top <= 120) cur = t.id; }
    el.querySelectorAll('a').forEach(a => a.classList.toggle('active', a.dataset.id === cur));
  };
  document.addEventListener('scroll', spyHandler, { passive: true });
  spyHandler();
}

// ======================= router ==========================================
// The slug of the page currently rendered — lets the TOC build same-page anchor
// links and lets the router scroll in place instead of reloading.
let currentSlug = null;

function scrollToAnchor(id) {
  if (!id) { window.scrollTo(0, 0); return; }
  const t = document.getElementById(id);
  if (t) t.scrollIntoView();
}

async function loadPage(slug) {
  const content = document.getElementById('content');
  if (!content) return;
  if (!PAGE_TITLES[slug]) slug = 'introduction';

  try {
    const res = await fetch(`content/${slug}.md`, { cache: 'no-cache' });
    if (!res.ok) throw new Error(res.status);
    const md = await res.text();
    const { html, headings } = renderMarkdown(md);
    content.innerHTML = `<div class="content-inner prose">${html}${pageNavHtml(slug)}</div>`;
    currentSlug = slug;
    setActiveNav(slug);
    buildToc(headings);
    wireCopyButtons();
    document.title = `${PAGE_TITLES[slug]} · AScript`;
    // jump to in-page anchor if present, else scroll to the top
    scrollToAnchor(location.hash.split('#')[2]);
  } catch (e) {
    content.innerHTML = `<div class="content-inner prose"><h1>Page not found</h1><p>Could not load <code>${slug}</code>. If you opened this file directly, serve the folder first — see the note in the README.</p></div>`;
  }
}

function wireCopyButtons() {
  document.querySelectorAll('.copybtn').forEach(btn => {
    btn.addEventListener('click', () => {
      const code = btn.parentElement.querySelector('code');
      navigator.clipboard?.writeText(code.innerText).then(() => {
        btn.textContent = 'copied'; btn.classList.add('done');
        setTimeout(() => { btn.textContent = 'copy'; btn.classList.remove('done'); }, 1400);
      });
    });
  });
}

function route() {
  // close mobile sidebar on navigation
  document.getElementById('sidebar')?.classList.remove('open');

  const raw = location.hash.slice(1); // drop the leading '#'
  if (raw.startsWith('/')) {
    // "#/slug" or "#/slug#anchor"
    const [slugPart, anchor] = raw.slice(1).split('#');
    const slug = slugPart || 'introduction';
    if (slug === currentSlug) {
      scrollToAnchor(anchor); // already on this page — just move to the heading
    } else {
      loadPage(slug); // loadPage scrolls to the anchor after rendering
    }
  } else if (raw) {
    // a bare "#anchor" (no page) — treat as an anchor within the current page
    scrollToAnchor(raw);
  } else {
    loadPage('introduction');
  }
}

// ======================= search ==========================================
let SEARCH_INDEX = null;
async function buildSearchIndex() {
  if (SEARCH_INDEX) return SEARCH_INDEX;
  SEARCH_INDEX = [];
  await Promise.all(PAGE_ORDER.map(async slug => {
    try {
      const md = await (await fetch(`content/${slug}.md`, { cache: 'force-cache' })).text();
      // index each section
      const blocks = md.split(/\n(?=#{1,3}\s)/);
      blocks.forEach(b => {
        const m = b.match(/^#{1,3}\s+(.*)/);
        const heading = m ? m[1].trim() : PAGE_TITLES[slug];
        const text = b.replace(/[#`*>|]/g, ' ').replace(/\s+/g, ' ').trim();
        const id = m ? slugify(heading) : '';
        SEARCH_INDEX.push({ slug, heading, text, id });
      });
    } catch (e) {}
  }));
  return SEARCH_INDEX;
}

function runSearch(q) {
  const results = document.getElementById('search-results');
  q = q.trim().toLowerCase();
  if (!q) { results.innerHTML = `<div class="sr-empty">Type to search the docs…</div>`; return; }
  const terms = q.split(/\s+/);
  const scored = [];
  for (const item of (SEARCH_INDEX || [])) {
    const hay = (item.heading + ' ' + item.text).toLowerCase();
    let score = 0, ok = true;
    for (const t of terms) {
      if (!hay.includes(t)) { ok = false; break; }
      if (item.heading.toLowerCase().includes(t)) score += 10;
      score += 1;
    }
    if (ok) scored.push({ item, score });
  }
  scored.sort((a, b) => b.score - a.score);
  const top = scored.slice(0, 12);
  if (!top.length) { results.innerHTML = `<div class="sr-empty">No results for “${escapeHtml(q)}”.</div>`; return; }

  const mk = (s) => escapeHtml(s).replace(new RegExp(`(${terms.map(t=>t.replace(/[.*+?^${}()|[\]\\]/g,'\\$&')).join('|')})`, 'gi'), '<mark>$1</mark>');
  results.innerHTML = top.map(({ item }, idx) => {
    const href = `#/${item.slug}${item.id ? '#' + item.id : ''}`;
    const snippet = item.text.slice(0, 120);
    return `<a href="${href}" class="${idx === 0 ? 'sel' : ''}" data-href="${href}">
      <div class="sr-t">${mk(item.heading)}<span class="sr-page">${PAGE_TITLES[item.slug]}</span></div>
      <div class="sr-x">${mk(snippet)}…</div></a>`;
  }).join('');
}

function openSearch() {
  const ov = document.getElementById('search-overlay');
  if (!ov) return;
  ov.classList.add('open');
  const input = document.getElementById('search-input');
  input.value = ''; runSearch('');
  buildSearchIndex().then(() => runSearch(input.value));
  setTimeout(() => input.focus(), 30);
}
function closeSearch() { document.getElementById('search-overlay')?.classList.remove('open'); }

function wireSearch() {
  const ov = document.getElementById('search-overlay');
  if (!ov) return;
  const input = document.getElementById('search-input');
  document.querySelectorAll('[data-action="search"]').forEach(b => b.addEventListener('click', openSearch));
  input.addEventListener('input', () => runSearch(input.value));
  ov.addEventListener('click', e => { if (e.target === ov) closeSearch(); });

  document.addEventListener('keydown', e => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') { e.preventDefault(); ov.classList.contains('open') ? closeSearch() : openSearch(); }
    if (e.key === 'Escape') closeSearch();
    if (e.key === '/' && !/INPUT|TEXTAREA/.test(document.activeElement.tagName) && !ov.classList.contains('open')) { e.preventDefault(); openSearch(); }
    if (ov.classList.contains('open') && (e.key === 'Enter')) {
      const sel = ov.querySelector('.search-results a.sel');
      if (sel) { location.hash = sel.dataset.href.slice(1); closeSearch(); }
    }
    if (ov.classList.contains('open') && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      const items = [...ov.querySelectorAll('.search-results a')];
      const cur = ov.querySelector('.search-results a.sel');
      let idx = items.indexOf(cur);
      if (cur) cur.classList.remove('sel');
      idx = e.key === 'ArrowDown' ? Math.min(items.length - 1, idx + 1) : Math.max(0, idx - 1);
      if (items[idx]) { items[idx].classList.add('sel'); items[idx].scrollIntoView({ block: 'nearest' }); }
    }
  });
  document.getElementById('search-results').addEventListener('click', () => setTimeout(closeSearch, 0));
}

// ======================= boot ============================================
function boot() {
  buildSidebar();
  wireSearch();
  const mt = document.getElementById('menu-toggle');
  mt?.addEventListener('click', () => document.getElementById('sidebar').classList.toggle('open'));
  window.addEventListener('hashchange', route);
  route();
}

if (document.readyState !== 'loading') boot();
else document.addEventListener('DOMContentLoaded', boot);
