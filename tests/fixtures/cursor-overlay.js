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

  var cursor = document.createElement('div');
  cursor.id = 'hu-cursor';
  cursor.style.cssText =
    'position:fixed;left:0;top:0;width:28px;height:28px;' +
    'pointer-events:none;z-index:2147483647;' +
    'transform:translate(-2px,-2px);' +
    'transition:width .08s ease,height .08s ease,opacity .12s ease;' +
    'opacity:0;will-change:left,top,transform;';
  cursor.innerHTML =
    '<svg width="28" height="28" viewBox="0 0 28 28" xmlns="http://www.w3.org/2000/svg">' +
    '<path d="M6 3l0 18 4.5-4.5 3.2 7 2.2-1-3.2-7 6 0z" ' +
    'fill="#5ce1ff" opacity="0.55" filter="url(#huGlow)"/>' +
    '<path d="M6 3l0 18 4.5-4.5 3.2 7 2.2-1-3.2-7 6 0z" ' +
    'fill="#a0f0ff" stroke="#ffffff" stroke-width="1.4" stroke-linejoin="round"/>' +
    '<defs><filter id="huGlow" x="-60%" y="-60%" width="220%" height="220%">' +
    '<feGaussianBlur stdDeviation="2.2"/></filter></defs>' +
    '</svg>';

  var ring = document.createElement('div');
  ring.id = 'hu-cursor-ring';
  ring.style.cssText =
    'position:fixed;left:0;top:0;width:20px;height:20px;' +
    'pointer-events:none;z-index:2147483646;' +
    'border:2px solid #5ce1ff;border-radius:50%;' +
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
    cursor.style.width = '32px'; cursor.style.height = '32px';
  }, true);
  document.addEventListener('pointerup', function () {
    ring.style.opacity = '0';
    ring.style.transform = 'translate(-50%,-50%) scale(0)';
    cursor.style.width = '28px'; cursor.style.height = '28px';
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
