# Input model

## Coordinate system

All coordinates are **viewport CSS pixels**, origin at the top-left of the
viewport. `Input.dispatchMouseEvent` `x`/`y` use this same space. Device scale
factor is 1 by default so CSS pixels == device pixels.

## Mouse buttons

| Button  | CDP string | Bitmask |
| ------- | ---------- | ------- |
| left    | left       | 1       |
| right   | right      | 2       |
| middle  | middle     | 4       |
| back    | back       | 8       |
| forward | forward    | 16      |

`buttons` reports the cumulative bitmask of currently-held buttons, tracked
per-page session.

## Modifiers

Bitmask: `alt=1, ctrl=2, meta=4, shift=8`. Parsed from comma-separated strings
(`ctrl,shift`) or JSON arrays.

## Drag

- `mouse.drag`: linear interpolation from→to with N steps; emits `mouseMoved`
  between `mousePressed` and `mouseReleased`.
- `mouse.drag_path`: free path of points; interpolates 8+ steps between each pair.

Intermediate moves are required because sliders, canvases, and sortable lists
listen for `pointermove` and break on teleport.

## Keyboard events

| CDP type | When                                |
| -------- | ----------------------------------- |
| keyDown  | key pressed                         |
| char     | generated for printable keys (text)|
| keyUp    | key released                        |

`type` synthesizes keyDown+char per ASCII char. `insert-text` uses
`Input.insertText` for non-ASCII (한글/CJK/emoji), which is reliable and avoids
IME state machines.

## Unicode & IME

OS-level IME composition is not emulated. For CJK/emoji, always use
`insert-text` / `Input.insertText`. The chord parser preserves case for the
final key (`P` stays `P`), while modifier names are case-insensitive.
