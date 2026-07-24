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

  const out = [];
  const nodes = document.querySelectorAll(INTERACTIVE_SELECTOR);
  let i = 0;
  for (const el of nodes) {
    if (isHidden(el)) continue;
    if (i >= 200) break; // cap for token budget
    const r = el.getBoundingClientRect();
    const role = roleOf(el);
    const name = accessibleName(el);
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
      // CSS selector hint for re-resolution (best-effort)
      selectorHint: (() => {
        if (el.id) return '#' + CSS.escape(el.id);
        return '';
      })(),
    });
    i++;
  }
  return out;
})();
