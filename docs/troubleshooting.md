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
with `=` to avoid this. If you pass `--extra-args`, avoid bare URL tokens.

## Shared library errors

Install Chromium's runtime libs: `apt-get install chromium` or the `chromium`
package. `doctor` reports missing libraries indirectly via a launch test.

## Docker crash / blank screenshots

- Set `--shm-size=1g` (Chromium uses `/dev/shm`; 64MB default crashes on big pages).
- Use `--no-sandbox` only as root in trusted CI; prefer the non-root image.

## Korean/CJK shows boxes

Install `fonts-noto-cjk`. `doctor` checks for Korean-capable fonts.

## Screenshot is blank

Wait for the page to load: `headless-use wait` before screenshotting. Ensure
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
