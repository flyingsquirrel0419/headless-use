(() => {
  // Returns a compact list of interactive elements with role/name/bounds/state.
  // We use the composed tree + accessibility semantics rather than full AX tree
  // round-trips, which is faster and avoids huge payloads.
  const INTERACTIVE_SELECTOR = [
    'a[href]','button','input','select','textarea',
    '[role="button"]','[role="link"]','[role="checkbox"]','[role="radio"]',
    '[role="tab"]','[role="menuitem"]','[role="menuitemcheckbox"]','[role="menuitemradio"]',
    '[role="switch"]','[role="textbox"]','[role="searchbox"]','[role="combobox"]',
    '[role="option"]','[role="treeitem"]','[contenteditable=""]','[contenteditable="true"]',
    'summary','details'
  ].join(',');

  const isHidden = (el) => {
    if (!el.isConnected) return true;
    const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden' || cs.visibility === 'collapse') return true;
    if (parseFloat(cs.opacity) === 0) return true;
    const r = el.getBoundingClientRect();
    if (r.width === 0 && r.height === 0) return true;
    return false;
  };

  // [Decision Log] — visual widgets (canvas / svg / non-standard clickable)
  // - 목적과 의도: 표준 interactive selector에 잡히지 않는 캔버스 게임,
  //   SVG UI, role 속성 없는 div 기반 위젯(가짜 reCAPTCHA 등)까지 ref로
  //   잡아 에이전트가 정확한 클릭 좌표를 얻을 수 있게 한다. 이전에는 이런
  //   페이지에서 observe가 빈 배열을 반환해 에이전트가 좌표를 추측으로
  //   클릭해야 했다.
  // - 검토한 주요 대안: 모든 div 수집(노이즈 폭발), full AX tree 병합.
  // - 선택한 방식: 두 번째 패스로 "시각적으로 클릭 가능해 보이는" 요소만
  //   추가 수집. cursor:pointer computed style, onclick 속성, 또는
  //   canvas/svg 태그인 가시적 요소를 잡는다.
  // - 장점: 비표준 위젯까지 ref 확보. 단점: 휴리스틱이라 일부 누락/과잉.
  // - 수정 시 주의: interactive-only 모드에서는 visual widget을 제외해야
  //   토큰 폭발을 막는다. visual 플래그로 구분한다.
  const VISUAL_SELECTOR = [
    'canvas', 'svg', 'video', 'embed',
    '[onclick]', '[tabindex]',
    '[role="img"]', '[role="application"]', '[role="dialog"]', '[role="gridcell"]'
  ].join(',');

  // Heuristic: is this element plausibly clickable even without a role?
  // cursor:pointer is the strongest signal; onclick attribute is another.
  // canvas/svg are treated as interactive surfaces themselves.
  const looksClickable = (el) => {
    if (el.hasAttribute('onclick')) return true;
    const cs = getComputedStyle(el);
    if (cs.cursor === 'pointer' || cs.cursor === 'hand') return true;
    const tag = el.tagName.toLowerCase();
    if (tag === 'canvas' || tag === 'svg' || tag === 'video') return true;
    return false;
  };

  // Name for a visual widget: aria-label, title, alt, or nearby text.
  const visualName = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const title = el.getAttribute('title');
    if (title && title.trim()) return title.trim();
    const alt = el.getAttribute('alt');
    if (alt && alt.trim()) return alt.trim();
    // canvas may have aria-label on a parent or sibling text.
    const text = (el.innerText || el.textContent || '').trim();
    if (text) return text.slice(0, 80);
    return '';
  };

  const accessibleName = (el) => {
    const aria = el.getAttribute('aria-label');
    if (aria && aria.trim()) return aria.trim();
    const labelled = el.getAttribute('aria-labelledby');
    if (labelled) {
      const ref = document.getElementById(labelled);
      if (ref) return (ref.textContent || '').trim();
    }
    if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA' || el.tagName === 'SELECT') {
      if (el.id) {
        const lbl = document.querySelector(`label[for="${CSS.escape(el.id)}"]`);
        if (lbl) return (lbl.textContent || '').trim();
      }
      const ph = el.getAttribute('placeholder');
      if (ph && ph.trim()) return ph.trim();
      const title = el.getAttribute('title');
      if (title && title.trim()) return title.trim();
    }
    const text = (el.innerText || el.textContent || '').trim();
    return text.slice(0, 120);
  };

  const roleOf = (el) => {
    const explicit = el.getAttribute('role');
    if (explicit) return explicit;
    const t = el.tagName.toLowerCase();
    if (t === 'a' && el.hasAttribute('href')) return 'link';
    if (t === 'button') return 'button';
    if (t === 'input') {
      const type = (el.getAttribute('type') || 'text').toLowerCase();
      if (type === 'checkbox') return 'checkbox';
      if (type === 'radio') return 'radio';
      if (type === 'submit' || type === 'button' || type === 'reset') return 'button';
      return 'textbox';
    }
    if (t === 'textarea' || el.getAttribute('contenteditable') === 'true' || el.getAttribute('contenteditable') === '') return 'textbox';
    if (t === 'select') return 'combobox';
    if (t === 'summary') return 'button';
    return t;
  };

  const pushElement = (el, opts) => {
    if (isHidden(el)) return false;
    if (out.length >= 200) return false; // cap for token budget
    const r = el.getBoundingClientRect();
    const role = opts.role;
    const name = opts.name;
    const disabled = el.disabled === true || el.getAttribute('aria-disabled') === 'true';
    const focused = document.activeElement === el;
    let checked = null;
    if (role === 'checkbox' || role === 'radio' || role === 'switch' || role === 'menuitemcheckbox' || role === 'menuitemradio') {
      checked = el.checked === true || el.getAttribute('aria-checked') === 'true';
    }
    // Never expose password/secret input values — they would leak to the model/MCP/stdout.
    const isSensitive = (el.type === 'password') || (el.getAttribute('autocomplete') || '').includes('password');
    const value = (!isSensitive && el.value != null && el.value !== '') ? String(el.value).slice(0,80) : null;
    // backend node id via experimental is unreliable here; use a synthetic key
    // combining tag path + name so references are reasonably stable.
    const backendId = el.dataset.headlessUseRef
      ? parseInt(el.dataset.headlessUseRef, 10)
      : 0;
    out.push({
      role,
      name,
      tagName: el.tagName.toLowerCase(),
      x: Math.round(r.left),
      y: Math.round(r.top),
      width: Math.round(r.width),
      height: Math.round(r.height),
      visible: true,
      enabled: !disabled,
      focused,
      checked,
      value,
      visual: !!opts.visual,
      // CSS selector hint for re-resolution (best-effort)
      selectorHint: (() => {
        if (el.id) return '#' + CSS.escape(el.id);
        return '';
      })(),
      opaqueInteractive: !!opts.opaque,
    });
    pushedEls.add(el);
    return true;
  };

  const out = [];
  const pushedEls = new Set();
  const nodes = document.querySelectorAll(INTERACTIVE_SELECTOR);
 let i = 0;
  for (const el of nodes) {
    pushElement(el, { role: roleOf(el), name: accessibleName(el), visual: false });
  }

  // Second pass: visual widgets (canvas, svg, cursor:pointer divs, etc.).
  // Skip any element already captured above (by identity) to avoid duplicates.
  const seen = new Set(out.map(e => e.selectorHint).filter(Boolean));
  const visualNodes = document.querySelectorAll(VISUAL_SELECTOR);
  for (const el of visualNodes) {
    // De-dup by id when present; otherwise we can't cheaply dedup by identity
    // in the JS array, so rely on the selectorHint check for id'd elements.
    const hint = el.id ? '#' + CSS.escape(el.id) : '';
    if (hint && seen.has(hint)) continue;
    if (!looksClickable(el)) continue;
    // For div/span with cursor:pointer, require a reasonable size to avoid
    // noise (e.g. tiny decorative divs).
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) continue;
    const role = el.getAttribute('role') || el.tagName.toLowerCase();
    const isOpaqueSurface = el.tagName.toLowerCase() === 'canvas';
    if (pushElement(el, { role, name: visualName(el), visual: true, opaque: isOpaqueSurface })) {
      if (hint) seen.add(hint);
    }
  }

  // Third pass: listener candidates. Plain elements that MIGHT have
  // programmatically-attached click listeners. We cannot see listeners from
  // page JS; the Rust side asks CDP DOMDebugger.getEventListeners per
  // candidate. We only nominate viewport-visible, reasonably-sized elements
  // not already captured, capped to keep the CDP round-trips bounded.
  const CANDIDATE_CAP = 150;
  const vw = window.innerWidth, vh = window.innerHeight;
  const cands = [];
  window.__hu_cand__ = [];
  let truncated = false;
  const all = document.body ? document.body.querySelectorAll('*') : [];
  for (const el of all) {
    if (cands.length >= CANDIDATE_CAP) { truncated = true; break; }
    if (pushedEls.has(el)) continue;
    if (el === document.documentElement || el === document.body) continue;
    const tag = el.tagName.toLowerCase();
    if (tag === 'script' || tag === 'style' || tag === 'link' || tag === 'meta') continue;
    if (el.matches(INTERACTIVE_SELECTOR)) continue; // captured or hidden duplicate
    if (el.closest(INTERACTIVE_SELECTOR)) continue; // inside a captured element
    if (isHidden(el)) continue;
    const r = el.getBoundingClientRect();
    if (r.width < 8 || r.height < 8) continue;
    if (r.right < 0 || r.bottom < 0 || r.left > vw || r.top > vh) continue; // outside viewport
    window.__hu_cand__.push(el);
    cands.push({
      index: cands.length,
      role: el.getAttribute('role') || tag,
      name: visualName(el),
      tagName: tag,
      x: Math.round(r.left),
      y: Math.round(r.top),
      width: Math.round(r.width),
      height: Math.round(r.height),
      childCount: el.childElementCount,
      selectorHint: el.id ? '#' + CSS.escape(el.id) : '',
    });
  }
  return { elements: out, candidates: cands, truncated };
})();
