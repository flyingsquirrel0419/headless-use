// Pre-load collector script injected via Page.addScriptToEvaluateOnNewDocument.
// Runs before any page JS, so it captures all console + network activity.
(function () {
  if (window.__hu_installed__) return;
  window.__hu_installed__ = true;
  window.__hu_console__ = [];
  window.__hu_network__ = [];

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

  // Network: wrap fetch + XHR.
  var origFetch = window.fetch;
  if (origFetch) {
    window.fetch = function () {
      var args = arguments;
      var url = (typeof args[0] === 'string') ? args[0] : (args[0] && args[0].url) || '';
      var method = (args[1] && args[1].method) || (args[0] && args[0].method) || 'GET';
      var start = performance.now();
      return origFetch.apply(this, args).then(function (resp) {
        window.__hu_network__.push({ method: method, url: url, status: resp.status, durationMs: Math.round(performance.now() - start) });
        return resp;
      }, function (err) {
        window.__hu_network__.push({ method: method, url: url, failed: String(err) });
        throw err;
      });
    };
  }
  var origXhrOpen = XMLHttpRequest.prototype.open;
  var origXhrSend = XMLHttpRequest.prototype.send;
  XMLHttpRequest.prototype.open = function (method, url) {
    this.__hu_method = method || 'GET';
    this.__hu_url = url || '';
    return origXhrOpen.apply(this, arguments);
  };
  XMLHttpRequest.prototype.send = function () {
    var self = this;
    var start = performance.now();
    this.addEventListener('loadend', function () {
      window.__hu_network__.push({ method: self.__hu_method, url: self.__hu_url, status: self.status, durationMs: Math.round(performance.now() - start) });
    });
    this.addEventListener('error', function () {
      window.__hu_network__.push({ method: self.__hu_method, url: self.__hu_url, failed: 'network error' });
    });
    return origXhrSend.apply(this, arguments);
  };
})();
