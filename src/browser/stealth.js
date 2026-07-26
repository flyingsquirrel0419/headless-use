// Stealth pre-load script, injected via Page.addScriptToEvaluateOnNewDocument
// when a session runs with `--stealth`. It runs in every frame (including
// cross-origin challenge iframes) before that frame's own JS, so a bot check
// that reads these properties on first paint sees the patched values.
//
// Rules this file follows, because they are what makes the difference between
// "hides headless" and "advertises a patched browser":
//
//  1. Patch only what is actually wrong. Every override is guarded by a check
//     of the real value first. On full Chrome + --headless=new most of these
//     are already correct, and an unnecessary override is one more accessor a
//     detector can catch lying.
//  2. Every replacement function reports native source. `Function.prototype
//     .toString` is wrapped once and answers from a WeakMap of patched
//     functions, so `navigator.plugins`'s getter, `fetch`, etc. all stringify
//     as `function ... () { [native code] }` like the originals.
//  3. Keep real platform identity (Linux). Claiming Windows while the fonts,
//     WebGL driver strings and Accept-Language all say Linux is a stronger
//     signal than being an honest Linux desktop.
(function () {
  'use strict';

  // ---- native-source masking ------------------------------------------------
  var nativeToString = Function.prototype.toString;
  var masked = new WeakMap();

  function mask(fn, name) {
    try {
      masked.set(fn, 'function ' + name + '() { [native code] }');
    } catch (e) {}
    return fn;
  }

  var stealthToString = function toString() {
    var src = masked.get(this);
    // Non-functions (and unpatched functions) fall through to the real
    // implementation, so `Function.prototype.toString.call({})` still throws
    // the TypeError a detector expects.
    return src !== undefined ? src : nativeToString.call(this);
  };
  mask(stealthToString, 'toString');
  try {
    Function.prototype.toString = stealthToString;
  } catch (e) {}

  // Define an accessor whose getter stringifies like a native one.
  function def(obj, prop, getter, name) {
    try {
      var prev = Object.getOwnPropertyDescriptor(obj, prop);
      Object.defineProperty(obj, prop, {
        get: mask(getter, name || 'get ' + prop),
        set: prev && prev.set,
        enumerable: prev ? prev.enumerable : true,
        configurable: true
      });
    } catch (e) {}
  }

  // ---- 1. navigator.webdriver ----------------------------------------------
  // --disable-blink-features=AutomationControlled already makes this false at
  // the source, which leaves no JS-visible trace. The patch is the fallback
  // for browsers/builds where the flag did not take.
  try {
    if (navigator.webdriver !== false) {
      def(Navigator.prototype, 'webdriver', function () {
        return false;
      }, 'get webdriver');
    }
  } catch (e) {}

  // ---- 2. window.chrome ----------------------------------------------------
  // Present on real Chrome and on --headless=new; absent on
  // chrome-headless-shell. Note `chrome.runtime` is deliberately NOT faked:
  // on a normal page real Chrome leaves it undefined, so adding it is itself
  // a tell.
  try {
    if (!window.chrome) {
      var startE = Date.now();
      var chromeShim = {
        app: {
          isInstalled: false,
          InstallState: {
            DISABLED: 'disabled',
            INSTALLED: 'installed',
            NOT_INSTALLED: 'not_installed'
          },
          RunningState: {
            CANNOT_RUN: 'cannot_run',
            READY_TO_RUN: 'ready_to_run',
            RUNNING: 'running'
          },
          getDetails: mask(function getDetails() {
            return null;
          }, 'getDetails'),
          getIsInstalled: mask(function getIsInstalled() {
            return false;
          }, 'getIsInstalled')
        },
        csi: mask(function csi() {
          return {
            onloadT: startE,
            startE: startE,
            pageT: performance.now(),
            tran: 15
          };
        }, 'csi'),
        loadTimes: mask(function loadTimes() {
          var t = performance.timing || {};
          var nav = (t.navigationStart || startE) / 1000;
          return {
            requestTime: nav,
            startLoadTime: nav,
            commitLoadTime: nav + 0.05,
            finishDocumentLoadTime: nav + 0.2,
            finishLoadTime: nav + 0.3,
            firstPaintTime: nav + 0.25,
            firstPaintAfterLoadTime: 0,
            navigationType: 'Other',
            wasFetchedViaSpdy: false,
            wasNpnNegotiated: true,
            npnNegotiatedProtocol: 'h2',
            wasAlternateProtocolAvailable: false,
            connectionInfo: 'h2'
          };
        }, 'loadTimes')
      };
      Object.defineProperty(window, 'chrome', {
        value: chromeShim,
        writable: true,
        enumerable: true,
        configurable: true
      });
    }
  } catch (e) {}

  // ---- 3. navigator.plugins / mimeTypes ------------------------------------
  // An empty PluginArray is a headless-shell signature. Rebuild the five PDF
  // entries real Chrome ships, with the real prototypes so `instanceof Plugin`
  // and `instanceof PluginArray` still hold.
  try {
    if (
      navigator.plugins &&
      navigator.plugins.length === 0 &&
      typeof Plugin !== 'undefined' &&
      typeof PluginArray !== 'undefined'
    ) {
      var PDF = 'Portable Document Format';
      var pluginNames = [
        'PDF Viewer',
        'Chrome PDF Viewer',
        'Chromium PDF Viewer',
        'Microsoft Edge PDF Viewer',
        'WebKit built-in PDF'
      ];
      var mimeSpecs = [
        { type: 'application/pdf', suffixes: 'pdf' },
        { type: 'text/pdf', suffixes: 'pdf' }
      ];

      var makeEntry = function (proto, fields) {
        var obj = Object.create(proto);
        Object.keys(fields).forEach(function (k) {
          var v = fields[k];
          def(obj, k, function () {
            return v;
          }, 'get ' + k);
        });
        return obj;
      };
      // A live NodeList-ish container: indexed access, length, item, namedInner,
      // and iteration, all reporting native source.
      var makeList = function (proto, entries, keyOf) {
        var list = Object.create(proto);
        entries.forEach(function (entry, i) {
          Object.defineProperty(list, i, {
            value: entry,
            enumerable: true,
            configurable: true
          });
          Object.defineProperty(list, keyOf(entry), {
            value: entry,
            enumerable: false,
            configurable: true
          });
        });
        def(list, 'length', function () {
          return entries.length;
        }, 'get length');
        Object.defineProperty(list, 'item', {
          value: mask(function item(i) {
            return entries[i] || null;
          }, 'item'),
          enumerable: false,
          configurable: true
        });
        Object.defineProperty(list, 'namedItem', {
          value: mask(function namedItem(name) {
            for (var i = 0; i < entries.length; i++) {
              if (keyOf(entries[i]) === name) return entries[i];
            }
            return null;
          }, 'namedItem'),
          enumerable: false,
          configurable: true
        });
        Object.defineProperty(list, Symbol.iterator, {
          value: mask(function values() {
            return entries[Symbol.iterator]();
          }, 'values'),
          enumerable: false,
          configurable: true
        });
        return list;
      };

      var mimes = mimeSpecs.map(function (m) {
        return makeEntry(MimeType.prototype, {
          type: m.type,
          suffixes: m.suffixes,
          description: PDF
        });
      });
      var plugins = pluginNames.map(function (name) {
        var p = makeEntry(Plugin.prototype, {
          name: name,
          filename: 'internal-pdf-viewer',
          description: PDF,
          length: mimes.length
        });
        mimes.forEach(function (m, i) {
          Object.defineProperty(p, i, {
            value: m,
            enumerable: true,
            configurable: true
          });
        });
        return p;
      });
      mimes.forEach(function (m) {
        def(m, 'enabledPlugin', function () {
          return plugins[0];
        }, 'get enabledPlugin');
      });

      var pluginArray = makeList(PluginArray.prototype, plugins, function (p) {
        return p.name;
      });
      var mimeArray = makeList(MimeTypeArray.prototype, mimes, function (m) {
        return m.type;
      });
      def(Navigator.prototype, 'plugins', function () {
        return pluginArray;
      }, 'get plugins');
      def(Navigator.prototype, 'mimeTypes', function () {
        return mimeArray;
      }, 'get mimeTypes');
    }
  } catch (e) {}

  // pdfViewerEnabled is false only where the PDF plugin is missing.
  try {
    if (navigator.pdfViewerEnabled === false) {
      def(Navigator.prototype, 'pdfViewerEnabled', function () {
        return true;
      }, 'get pdfViewerEnabled');
    }
  } catch (e) {}

  // ---- 4. notification permission ------------------------------------------
  // Headless has no permission UI, so notifications resolve to 'denied' where
  // a real profile reports the un-answered 'prompt'/'default'.
  try {
    if (typeof Notification !== 'undefined' && Notification.permission === 'denied') {
      def(Notification, 'permission', function () {
        return 'default';
      }, 'get permission');
    }
  } catch (e) {}
  try {
    if (typeof Permissions !== 'undefined' && Permissions.prototype.query) {
      var origQuery = Permissions.prototype.query;
      Permissions.prototype.query = mask(function query(descriptor) {
        var p = origQuery.call(this, descriptor);
        if (!descriptor || descriptor.name !== 'notifications') return p;
        // Patch the real PermissionStatus rather than returning a literal, so
        // `instanceof PermissionStatus` and `onchange` keep working.
        return p.then(function (status) {
          if (status && status.state === 'denied') {
            def(status, 'state', function () {
              return 'prompt';
            }, 'get state');
          }
          return status;
        });
      }, 'query');
    }
  } catch (e) {}

  // ---- 5. WebGL driver strings ---------------------------------------------
  // Software rendering answers UNMASKED_RENDERER with a SwiftShader string,
  // which no desktop GPU ever reports. Swap in an ANGLE/Mesa string of the
  // shape real Linux Chrome produces. Everything else falls through, so the
  // context stays functionally identical.
  try {
    var VENDOR = 0x9245;
    var RENDERER = 0x9246;
    var vendorStr = 'Google Inc. (Intel)';
    var rendererStr =
      'ANGLE (Intel, Mesa Intel(R) UHD Graphics 620 (KBL GT2), OpenGL 4.6 (Core Profile) Mesa 22.0.5)';
    var patchGetParameter = function (proto) {
      if (!proto || !proto.getParameter) return;
      var orig = proto.getParameter;
      proto.getParameter = mask(function getParameter(param) {
        if (param === VENDOR) return vendorStr;
        if (param === RENDERER) return rendererStr;
        return orig.call(this, param);
      }, 'getParameter');
    };
    if (typeof WebGLRenderingContext !== 'undefined') {
      patchGetParameter(WebGLRenderingContext.prototype);
    }
    if (typeof WebGL2RenderingContext !== 'undefined') {
      patchGetParameter(WebGL2RenderingContext.prototype);
    }
  } catch (e) {}

  // ---- 6. window / screen geometry -----------------------------------------
  // Headless has no browser chrome and no desktop: outerHeight equals
  // innerHeight and screen equals the window. Both are cheap checks that a
  // real desktop never satisfies.
  try {
    if (window.outerHeight === window.innerHeight || window.outerHeight === 0) {
      var CHROME_UI = 88; // tab strip + omnibox on Linux Chrome at dsf 1
      def(window, 'outerHeight', function () {
        return window.innerHeight + CHROME_UI;
      }, 'get outerHeight');
    }
    if (window.outerWidth === 0) {
      def(window, 'outerWidth', function () {
        return window.innerWidth;
      }, 'get outerWidth');
    }
  } catch (e) {}
  try {
    var scr = window.screen;
    if (scr && scr.width <= window.innerWidth && scr.height <= window.innerHeight) {
      var sw = Math.max(1920, window.innerWidth);
      var sh = Math.max(1080, window.innerHeight + 88);
      def(scr, 'width', function () {
        return sw;
      }, 'get width');
      def(scr, 'height', function () {
        return sh;
      }, 'get height');
      def(scr, 'availWidth', function () {
        return sw;
      }, 'get availWidth');
      def(scr, 'availHeight', function () {
        return sh - 27; // Linux panels/docks leave a strip unavailable
      }, 'get availHeight');
    }
  } catch (e) {}

  // ---- 7. hide this tool's own instrumentation ------------------------------
  // collectors.js (console + network capture) wraps `fetch`, XHR and the
  // console methods and parks its buffers on `window`. Both are visible: a
  // wrapped `fetch.toString()` is not native source, and the buffers show up in
  // `Object.keys(window)`. Report native source for the wrappers and make the
  // buffers non-enumerable; the collectors keep working untouched.
  try {
    ['__hu_installed__', '__hu_console__', '__hu_network__', '__hu_net_inflight__'].forEach(
      function (key) {
        var d = Object.getOwnPropertyDescriptor(window, key);
        if (d && d.enumerable && 'value' in d) {
          Object.defineProperty(window, key, {
            value: d.value,
            writable: true,
            enumerable: false,
            configurable: true
          });
        }
      }
    );
  } catch (e) {}
  try {
    var wrapped = [
      [window, 'fetch', 'fetch'],
      [window.XMLHttpRequest && XMLHttpRequest.prototype, 'open', 'open'],
      [window.XMLHttpRequest && XMLHttpRequest.prototype, 'send', 'send'],
      [window.console, 'log', 'log'],
      [window.console, 'info', 'info'],
      [window.console, 'warn', 'warn'],
      [window.console, 'error', 'error'],
      [window.console, 'debug', 'debug']
    ];
    wrapped.forEach(function (entry) {
      var owner = entry[0];
      if (!owner) return;
      var fn = owner[entry[1]];
      if (typeof fn === 'function' && nativeToString.call(fn).indexOf('[native code]') === -1) {
        mask(fn, entry[2]);
      }
    });
  } catch (e) {}
})();
