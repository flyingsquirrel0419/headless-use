// Functional harness for src/browser/stealth.js, driven by tests/it_stealth.rs.
//
// The pre-load script is plain JS injected into a page, so nothing in the Rust
// test suite can execute it. This builds a worst-case fake DOM (a headless
// shell: webdriver true, zero plugins, SwiftShader WebGL, no window.chrome, no
// browser chrome around the window), runs the real script against it, and
// asserts the resulting view — including that every replacement function still
// reports native source, which is the property a detector checks.
//
// Usage: node stealth_dom.mjs <path-to-stealth.js>
import fs from 'node:fs';
import assert from 'node:assert';

const scriptPath = process.argv[2];
if (!scriptPath) {
  process.stderr.write('usage: node stealth_dom.mjs <path-to-stealth.js>\n');
  process.exit(2);
}
const src = fs.readFileSync(scriptPath, 'utf8');

// ---- fake DOM -------------------------------------------------------------
class Navigator {}
class Plugin {}
class MimeType {}
class PluginArray {}
class MimeTypeArray {}
class PermissionStatus {
  constructor(state) { this._state = state; }
  get state() { return this._state; }
}
class Permissions {
  query() { return Promise.resolve(new PermissionStatus('denied')); }
}
class WebGLRenderingContext {
  getParameter(p) { return p === 0x9245 ? 'Google Inc. (Google)' : p === 0x9246 ? 'ANGLE (Google, Vulkan 1.3.0 (SwiftShader Device (Subzero)))' : 'x'; }
}
class WebGL2RenderingContext extends WebGLRenderingContext {}

const nav = Object.create(Navigator.prototype);
Object.defineProperty(Navigator.prototype, 'webdriver', { get() { return true; }, configurable: true });
const emptyPlugins = Object.create(PluginArray.prototype);
Object.defineProperty(PluginArray.prototype, 'length', { get() { return 0; }, configurable: true });
Object.defineProperty(Navigator.prototype, 'plugins', { get() { return emptyPlugins; }, configurable: true });
Object.defineProperty(Navigator.prototype, 'mimeTypes', { get() { return Object.create(MimeTypeArray.prototype); }, configurable: true });
Object.defineProperty(Navigator.prototype, 'pdfViewerEnabled', { get() { return false; }, configurable: true });

const win = globalThis;
win.window = win;
Object.defineProperty(win, 'navigator', { value: nav, writable: true, enumerable: true, configurable: true });
win.Navigator = Navigator;
win.Plugin = Plugin;
win.MimeType = MimeType;
win.PluginArray = PluginArray;
win.MimeTypeArray = MimeTypeArray;
win.Permissions = Permissions;
win.PermissionStatus = PermissionStatus;
win.WebGLRenderingContext = WebGLRenderingContext;
win.WebGL2RenderingContext = WebGL2RenderingContext;
win.Notification = class Notification {};
Object.defineProperty(win.Notification, 'permission', { get() { return 'denied'; }, configurable: true });
win.innerWidth = 1280;
win.innerHeight = 720;
win.outerWidth = 1280;
win.outerHeight = 720; // headless: no browser chrome
win.screen = { width: 1280, height: 720, availWidth: 1280, availHeight: 720, colorDepth: 24 };
delete win.chrome;
// collectors.js-style instrumentation, present before stealth runs
win.__hu_installed__ = true;
win.__hu_console__ = [];
win.__hu_net_inflight__ = 0;
const nativeFetchSource = 'function fetch() { [native code] }';
win.fetch = function () { return Promise.resolve(); }; // wrapper, non-native source
win.XMLHttpRequest = class XMLHttpRequest {};
win.XMLHttpRequest.prototype.open = function () {};
win.XMLHttpRequest.prototype.send = function () {};
for (const lvl of ['log', 'info', 'warn', 'error', 'debug']) {
  const orig = console[lvl];
  console[lvl] = function () { return orig.apply(console, arguments); };
}

// ---- run ------------------------------------------------------------------
// eslint-disable-next-line no-new-func -- the "untrusted" input is our own file
new Function(src)();

// ---- assert ---------------------------------------------------------------
const checks = [];
const check = (name, fn) => { try { fn(); checks.push(['ok', name]); } catch (e) { checks.push(['FAIL', name + ' :: ' + e.message]); } };

check('webdriver is false', () => assert.strictEqual(navigator.webdriver, false));
check('webdriver getter reports native source', () => {
  const g = Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver').get;
  assert.strictEqual(g.toString(), 'function get webdriver() { [native code] }');
});
check('toString still throws for non-functions', () => {
  assert.throws(() => Function.prototype.toString.call({}), TypeError);
});
check('plain functions keep real source', () => {
  const f = function hello() { return 1; };
  assert.ok(f.toString().includes('return 1'));
});
check('toString masks itself', () => {
  assert.strictEqual(Function.prototype.toString.toString(), 'function toString() { [native code] }');
});
check('window.chrome shimmed with app/csi/loadTimes and no runtime', () => {
  assert.ok(window.chrome && window.chrome.app && typeof window.chrome.csi === 'function');
  assert.ok(typeof window.chrome.loadTimes === 'function');
  assert.strictEqual(window.chrome.runtime, undefined);
  assert.strictEqual(window.chrome.csi.toString(), 'function csi() { [native code] }');
  assert.ok(window.chrome.loadTimes().connectionInfo === 'h2');
});
check('plugins rebuilt with real prototypes', () => {
  assert.strictEqual(navigator.plugins.length, 5);
  assert.ok(navigator.plugins instanceof PluginArray);
  assert.ok(navigator.plugins[0] instanceof Plugin);
  assert.strictEqual(navigator.plugins[0].name, 'PDF Viewer');
  assert.strictEqual(navigator.plugins.item(1).name, 'Chrome PDF Viewer');
  assert.strictEqual(navigator.plugins.namedItem('WebKit built-in PDF').filename, 'internal-pdf-viewer');
  assert.strictEqual([...navigator.plugins].length, 5);
  assert.strictEqual(navigator.mimeTypes.length, 2);
  assert.ok(navigator.mimeTypes[0] instanceof MimeType);
  assert.strictEqual(navigator.mimeTypes['application/pdf'].suffixes, 'pdf');
  assert.ok(navigator.mimeTypes[0].enabledPlugin instanceof Plugin);
  assert.strictEqual(navigator.pdfViewerEnabled, true);
});
check('notification permission looks unanswered', () => {
  assert.strictEqual(Notification.permission, 'default');
});
check('webgl driver strings no longer say SwiftShader', () => {
  const gl = new WebGLRenderingContext();
  assert.strictEqual(gl.getParameter(0x9245), 'Google Inc. (Intel)');
  assert.ok(gl.getParameter(0x9246).includes('Mesa Intel'));
  assert.ok(!gl.getParameter(0x9246).includes('SwiftShader'));
  assert.strictEqual(gl.getParameter(0x1234), 'x');
  assert.strictEqual(WebGLRenderingContext.prototype.getParameter.toString(), 'function getParameter() { [native code] }');
  const gl2 = new WebGL2RenderingContext();
  assert.ok(gl2.getParameter(0x9246).includes('Mesa Intel'));
});
check('window has browser chrome height', () => {
  assert.strictEqual(window.outerHeight, 808);
  assert.strictEqual(window.outerWidth, 1280);
});
check('screen looks like a desktop', () => {
  assert.strictEqual(screen.width, 1920);
  assert.strictEqual(screen.height, 1080);
  assert.strictEqual(screen.availHeight, 1053);
});
check('tool instrumentation hidden from enumeration', () => {
  assert.ok(!Object.keys(window).includes('__hu_console__'));
  assert.ok(!Object.keys(window).includes('__hu_installed__'));
  assert.ok(Array.isArray(window.__hu_console__));
  window.__hu_net_inflight__++;
  assert.strictEqual(window.__hu_net_inflight__, 1);
});
check('collector wrappers report native source', () => {
  assert.strictEqual(window.fetch.toString(), nativeFetchSource);
  assert.strictEqual(XMLHttpRequest.prototype.open.toString(), 'function open() { [native code] }');
  assert.strictEqual(console.log.toString(), 'function log() { [native code] }');
});
const permCheck = (async () => {
  const st = await navigator_permissions_query();
  assert.strictEqual(st.state, 'prompt');
  assert.ok(st instanceof PermissionStatus);
  const other = await new Permissions().query({ name: 'geolocation' });
  assert.strictEqual(other.state, 'denied');
  assert.strictEqual(Permissions.prototype.query.toString(), 'function query() { [native code] }');
})();
function navigator_permissions_query() {
  return new Permissions().query({ name: 'notifications' });
}

permCheck.then(
  () => checks.push(['ok', 'permissions.query patched (real PermissionStatus kept)']),
  (e) => checks.push(['FAIL', 'permissions.query :: ' + e.message])
).finally(() => {
  let failed = 0;
  for (const [status, name] of checks) {
    if (status === 'FAIL') failed++;
    process.stdout.write(`${status === 'ok' ? '  ok' : 'FAIL'}  ${name}\n`);
  }
  process.stdout.write(failed ? `\n${failed} check(s) failed\n` : `\nall ${checks.length} checks passed\n`);
  process.exit(failed ? 1 : 0);
});
