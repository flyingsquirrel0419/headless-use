// Neon-arrow virtual cursor overlay.
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
(function () {
  if (window.__hu_cursor_injected) return;
  window.__hu_cursor_injected = true;

  // Cursor size is intentionally large (48px) so it stays visible on
  // high-DPI viewer tabs and at small thumbnail sizes in recordings.
  var SIZE = 48;

  var cursor = document.createElement('div');
  cursor.id = 'hu-cursor';
  cursor.style.cssText =
    'position:fixed;left:0;top:0;width:' + SIZE + 'px;height:' + SIZE + 'px;' +
    'pointer-events:none;z-index:2147483647;' +
    'transform:translate(-3px,-3px);' +
    'transition:width .08s ease,height .08s ease,opacity .12s ease;' +
    'opacity:0;will-change:left,top,transform;';
  // Rounded soft pointer: a teardrop/arrow with curved edges and a rounded
  // tip, instead of the sharp angular arrow. The path uses quadratic curves
  // so the outline has no hard corners, which reads as more "alive" and less
  // robotic in the live viewer.
  cursor.innerHTML =
    '<svg width="' + SIZE + '" height="' + SIZE + '" viewBox="0 0 28 28" xmlns="http://www.w3.org/2000/svg">' +
    // Glow halo (blurred rounded pointer behind).
    '<path d="M5 3 Q4 3 4 4 L4 20 Q4 21.8 5.6 21 L9.5 18.6 L12.4 25.2 Q12.9 26.4 14 25.9 L15.6 25.1 Q16.7 24.5 16.2 23.3 L13.3 16.7 L18 16.7 Q19.8 16.7 19 15 Z" ' +
    'fill="#5ce1ff" opacity="0.6" filter="url(#huGlow)"/>' +
    // Core rounded pointer.
    '<path d="M5 3 Q4 3 4 4 L4 20 Q4 21.8 5.6 21 L9.5 18.6 L12.4 25.2 Q12.9 26.4 14 25.9 L15.6 25.1 Q16.7 24.5 16.2 23.3 L13.3 16.7 L18 16.7 Q19.8 16.7 19 15 Z" ' +
    'fill="#a0f0ff" stroke="#ffffff" stroke-width="1.1" stroke-linejoin="round"/>' +
    '<defs><filter id="huGlow" x="-80%" y="-80%" width="260%" height="260%">' +
    '<feGaussianBlur stdDeviation="3.5"/></filter></defs>' +
    '</svg>';

  // A ring shown on press for click feedback, scaled to the cursor size.
  var ring = document.createElement('div');
  ring.id = 'hu-cursor-ring';
  ring.style.cssText =
    'position:fixed;left:0;top:0;width:' + (SIZE * 0.6) + 'px;height:' + (SIZE * 0.6) + 'px;' +
    'pointer-events:none;z-index:2147483646;' +
    'border:3px solid #5ce1ff;border-radius:50%;' +
    'box-shadow:0 0 12px #5ce1ff;' +
    'transform:translate(-50%,-50%) scale(0);opacity:0;' +
    'transition:transform .12s ease,opacity .2s ease;';

  function attach() {
    document.documentElement.appendChild(cursor);
    document.documentElement.appendChild(ring);
  }
  if (document.documentElement) attach();
  else document.addEventListener('DOMContentLoaded', attach);

  var x = -100, y = -100;
  function moveRing() { ring.style.left = x + 'px'; ring.style.top = y + 'px'; }
  function moveCursor() { cursor.style.left = x + 'px'; cursor.style.top = y + 'px'; }

  document.addEventListener('pointermove', function (e) {
    x = e.clientX; y = e.clientY;
    cursor.style.opacity = '1';
    moveCursor(); moveRing();
  }, true);
  document.addEventListener('pointerdown', function (e) {
    x = e.clientX; y = e.clientY;
    moveCursor(); moveRing();
    ring.style.opacity = '1';
    ring.style.transform = 'translate(-50%,-50%) scale(1)';
    // Enlarge the arrow ~20% on press for tactile feedback.
    cursor.style.width = (SIZE * 1.2) + 'px';
    cursor.style.height = (SIZE * 1.2) + 'px';
  }, true);
  document.addEventListener('pointerup', function () {
    ring.style.opacity = '0';
    ring.style.transform = 'translate(-50%,-50%) scale(0)';
    cursor.style.width = SIZE + 'px';
    cursor.style.height = SIZE + 'px';
  }, true);
  document.addEventListener('pointerleave', function () { cursor.style.opacity = '0.25'; }, true);
  document.addEventListener('pointerenter', function () { cursor.style.opacity = '1'; }, true);

  window.__hu_cursor = { x: 0, y: 0, down: false };
  document.addEventListener('pointermove', function (e) {
    window.__hu_cursor.x = e.clientX; window.__hu_cursor.y = e.clientY;
  }, true);
  document.addEventListener('pointerdown', function () { window.__hu_cursor.down = true; }, true);
  document.addEventListener('pointerup', function () { window.__hu_cursor.down = false; }, true);
})();
