// Virtual cursor overlay for the live viewer.
//
// [Decision Log]
// - 목적과 의도: CDP Input.dispatchMouseEvent is the real input path, but it
//   does not render an OS-level cursor on a headless/Xvfb display. This overlay
//   is a pure visual layer that follows the same pointer events the page
//   already receives, so the viewer sees where the agent's mouse is.
// - 기존 구현 및 제약 조건: A previous per-fixture blue SVG cursor only worked
//   on one demo page. We need a universal overlay that works on any page.
// - 검토한 주요 대안: (a) sync cursor_pos via Runtime.evaluate after each
//   Input.* call — adds latency and couples the visual layer to the input
//   engine. (b) server-side frame compositing — costly and design-inflexible.
// - 선택한 방식: Inject a pointer-events:none overlay div that listens to the
//   same pointermove/pointerdown/pointerup events CDP already dispatches. No
//   input-engine changes needed; the overlay is purely a passive observer.
// - 장점: Zero coupling to input engine, works on every page automatically,
//   no per-move evaluate() round-trips. 단점: Relies on CDP dispatching DOM
//   pointer events (true for Input.dispatchMouseEvent).
// - 수정 시 주의: Never set pointer-events to anything but none — this div
//   must never intercept clicks intended for the page below.
//
// ## Why a solid pointer instead of the old neon arrow
// The overlay sits on top of the page under test, so it competes with the
// content the viewer is actually trying to read. The previous 48px cyan arrow
// with a blurred glow halo covered a meaningful chunk of small controls and
// read as decoration rather than as a cursor. This is a plain opaque pointer at
// roughly OS cursor size: white fill for contrast on dark pages, a near-black
// outline for contrast on light ones, and a soft drop shadow so it separates
// from either. It stays legible without a glow, which means no blur filter
// repainting on every move.
(function () {
  if (window.__hu_cursor_injected) return;
  window.__hu_cursor_injected = true;

  // Roughly the size of a real OS pointer. Large enough to read in a scaled
  // viewer tab, small enough not to hide the control being clicked.
  var SIZE = 28;

  // Arrow outline. The tip sits at (3, 2) in the 28-unit viewBox; the wrapper
  // is offset by the same amount so the drawn tip lands exactly on the pointer
  // coordinate rather than near it.
  var ARROW_PATH = 'M3 2.2 L3 19.6 Q3 21 4.2 20.2 L8.6 16.8 L11.4 23.4 ' +
    'Q11.8 24.4 12.8 24 L14.6 23.2 Q15.6 22.8 15.2 21.8 L12.4 15.4 ' +
    'L17.9 15.2 Q19.4 15.2 18.4 14 L4.6 1.8 Q3 0.6 3 2.2 Z';

  var cursor = document.createElement('div');
  cursor.id = 'hu-cursor';
  cursor.style.cssText =
    'position:fixed;left:0;top:0;width:' + SIZE + 'px;height:' + SIZE + 'px;' +
    'pointer-events:none;z-index:2147483647;' +
    'transform:translate(-3px,-2px);' +
    'filter:drop-shadow(0 2px 6px rgba(0,0,0,.45));' +
    'transition:transform .09s ease,opacity .12s ease;' +
    'opacity:0;will-change:left,top;';
  cursor.innerHTML =
    '<svg width="' + SIZE + '" height="' + SIZE + '" viewBox="0 0 28 28" ' +
    'xmlns="http://www.w3.org/2000/svg">' +
    '<path d="' + ARROW_PATH + '" fill="#ffffff" stroke="#111111" ' +
    'stroke-width="1.5" stroke-linejoin="round"/>' +
    '</svg>';

  // Click feedback: a single ripple that expands and fades. The old version
  // held a persistent cyan ring for the whole press, which read as a UI element
  // belonging to the page rather than as a click.
  var ripple = document.createElement('div');
  ripple.id = 'hu-cursor-ripple';
  ripple.style.cssText =
    'position:fixed;left:0;top:0;width:22px;height:22px;' +
    'pointer-events:none;z-index:2147483646;' +
    'border:2px solid rgba(255,255,255,.9);border-radius:50%;' +
    'box-shadow:0 0 0 1px rgba(0,0,0,.35);' +
    'transform:translate(-50%,-50%) scale(.2);opacity:0;';

  function attach() {
    document.documentElement.appendChild(cursor);
    document.documentElement.appendChild(ripple);
  }
  if (document.documentElement) attach();
  else document.addEventListener('DOMContentLoaded', attach);

  var x = -100, y = -100;
  function place() {
    cursor.style.left = x + 'px';
    cursor.style.top = y + 'px';
    ripple.style.left = x + 'px';
    ripple.style.top = y + 'px';
  }

  document.addEventListener('pointermove', function (e) {
    x = e.clientX; y = e.clientY;
    cursor.style.opacity = '1';
    place();
  }, true);

  document.addEventListener('pointerdown', function (e) {
    x = e.clientX; y = e.clientY;
    place();
    // Press: nudge the arrow a hair instead of scaling it up. A larger arrow
    // would hide more of the control being clicked at the exact moment the
    // viewer wants to see it.
    cursor.style.transform = 'translate(-1px,0px)';
    // Restart the ripple from zero: clear the transition, reset, flush, then
    // re-enable so the animation replays on every click rather than only the
    // first.
    ripple.style.transition = 'none';
    ripple.style.transform = 'translate(-50%,-50%) scale(.2)';
    ripple.style.opacity = '1';
    void ripple.offsetWidth;
    ripple.style.transition = 'transform .3s ease-out,opacity .3s ease-out';
    ripple.style.transform = 'translate(-50%,-50%) scale(1.6)';
    ripple.style.opacity = '0';
  }, true);

  document.addEventListener('pointerup', function () {
    cursor.style.transform = 'translate(-3px,-2px)';
  }, true);

  document.addEventListener('pointerleave', function () {
    cursor.style.opacity = '0.25';
  }, true);
  document.addEventListener('pointerenter', function () {
    cursor.style.opacity = '1';
  }, true);

  // Expose the last known pointer state for tests and diagnostics.
  window.__hu_cursor = { x: 0, y: 0, down: false };
  document.addEventListener('pointermove', function (e) {
    window.__hu_cursor.x = e.clientX; window.__hu_cursor.y = e.clientY;
  }, true);
  document.addEventListener('pointerdown', function () {
    window.__hu_cursor.down = true;
  }, true);
  document.addEventListener('pointerup', function () {
    window.__hu_cursor.down = false;
  }, true);
})();
