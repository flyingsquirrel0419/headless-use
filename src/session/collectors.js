// Pre-load collector script injected via Page.addScriptToEvaluateOnNewDocument.
// Runs before any page JS, so it captures console output from the first line on.
//
// Console only. Network used to be collected here too, by wrapping fetch and
// XMLHttpRequest — that code was replaced by the CDP Network.* tracker (see
// src/session/network_tracker.rs) and then left behind, injected into every
// page, feeding a buffer nothing read. It was not harmless: the array had no
// length cap, so a chatty single-page app grew page memory without limit, and a
// wrapped `fetch.toString()` is not native source, which is exactly the signal
// stealth mode then had to paper over. Do not reintroduce it: CDP sees every
// request, including ones JS never touches.
(function () {
  if (window.__hu_installed__) return;
  window.__hu_installed__ = true;
  window.__hu_console__ = [];

  var pushConsole = function (level, args, meta) {
    try {
      var text = args.map(function (a) {
        if (typeof a === 'string') return a;
        if (a instanceof Error) return a.stack || a.message;
        try { return JSON.stringify(a); } catch (_) { return String(a); }
      }).join(' ');
      window.__hu_console__.push({ level: level, text: text, url: meta && meta.url, line: meta && meta.line });
      if (window.__hu_console__.length > 500) window.__hu_console__.shift();
    } catch (_) {}
  };
  ['log', 'info', 'warn', 'error', 'debug'].forEach(function (lvl) {
    var orig = console[lvl];
    console[lvl] = function () {
      var args = Array.prototype.slice.call(arguments);
      pushConsole(lvl === 'log' ? 'info' : lvl, args);
      orig.apply(console, args);
    };
  });
  window.addEventListener('error', function (ev) {
    pushConsole('error', [ev.message + (ev.error && ev.error.stack ? '\n' + ev.error.stack : '')], { url: ev.filename, line: ev.lineno });
  });
  window.addEventListener('unhandledrejection', function (ev) {
    var r = ev.reason;
    pushConsole('error', ['Unhandled promise rejection: ' + (r && r.stack ? r.stack : String(r))]);
  });

})();
