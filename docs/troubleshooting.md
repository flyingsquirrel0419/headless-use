# Troubleshooting

Run `headless-use doctor` first — it checks OS, browser, CDP, screenshot, fonts,
`/dev/shm`, and temp-dir writability.

## Browser not found

```
error[BROWSER_NOT_FOUND]: no browser found in PATH
```
Install Chromium, or set `HEADLESS_USE_BROWSER_PATH=/path/to/chrome`.

## "Multiple targets are not supported in headless mode" (exit 13)

This happens when a value that looks like a URL is passed as a separate argv
token. `headless-use` joins `--remote-debugging-port=N`, `--user-data-dir=...`
with `=` to avoid this.

## Shared library errors

Install Chromium's runtime libs: `apt-get install chromium` or the `chromium`
package. `doctor` reports missing libraries indirectly via a launch test.

## Docker crash / blank screenshots

- Set `--shm-size=1g` (Chromium uses `/dev/shm`; 64MB default crashes on big pages).
- Use `--no-sandbox` only as root in trusted CI; prefer the non-root image.

## Korean/CJK shows boxes

Install `fonts-noto-cjk`. `doctor` checks for Korean-capable fonts.

## Screenshot is blank

Wait for the page to load. `run` already waits until the page is stable after
opening the URL; in a `serve`/`view` session, send the `wait` JSON-RPC method
(`{"method":"wait","params":{"timeout":10000}}`) — or call the `browser_wait`
MCP tool — before screenshotting. It returns once the network has been idle and
the DOM quiet for a moment, or reports `stable: false` on timeout. Also ensure
the viewport matches (`--viewport 1280x720`).

## Click does nothing

- Use `observe` and click by `@eN` reference rather than guessing coordinates.
- If `ELEMENT_NOT_INTERACTABLE`, the element may be covered — close the dialog
  or scroll into view, then re-observe.

## Stale reference

```
error[STALE_REFERENCE]: stale reference @e7
```
Run `observe` again; the page changed since the last observation.

## A bot check / CAPTCHA appears only in headless

The page hands headless Chrome a challenge that headful Chrome passes silently.
Add `--stealth`, which removes the headless signals (`navigator.webdriver`,
`HeadlessChrome` in the UA and `Sec-CH-UA`, SwiftShader WebGL strings, empty
plugins, window-sized screen) while staying headless. See
[Stealth mode](../README.md#stealth-mode).

If it still challenges you:

- **Check the browser.** `--stealth` warns if it could only find
  `chrome-headless-shell`; that build is missing APIs bot checks read. Install
  full Chrome/Chromium, or point at it with `--browser-path`.
- **Confirm the signals are actually gone** — `page.evaluate` on the loaded page:
  ```
  JSON.stringify({wd: navigator.webdriver, ua: navigator.userAgent,
                  brands: navigator.userAgentData.brands})
  ```
  None of these may contain `Headless`, and `wd` must be `false`.
- **An interactive checkbox still has to be clicked.** Stealth only removes the
  browser's fingerprint; a widget that asks for a click needs one. The widget
  lives in a cross-origin iframe, where element references are limited — click by
  coordinate from a screenshot.
- **Fall back to a real display**: `--compat xvfb` runs a headful browser under
  Xvfb (needs `Xvfb` installed, roughly double the memory).
