//! MCP tool definitions with proper JSON Schema input shapes.
//!
//! Each tool exposes a typed `inputSchema` so MCP clients can validate calls
//! and render structured input forms. Schemas describe only what the agent
//! supplies; the server resolves the rest (references, coordinates) internally.

use serde_json::{json, Value};

/// One tool definition: (name, description, inputSchema).
struct ToolDef {
    name: &'static str,
    description: &'static str,
    schema: Value,
}

/// All tools this server advertises via `tools/list`.
fn tools() -> Vec<ToolDef> {
    vec![
    ToolDef {
        name: "browser_open",
        description: "Open a URL in the browser. Returns the final URL after navigation.",
        schema: json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to navigate to." }
            },
            "required": ["url"]
        }),
    },
    ToolDef {
        name: "browser_observe",
        description: "Return the page's interactive elements with @eN references and the current generation. Prefer this over screenshots to save tokens. Set listeners=true to also detect programmatically-attached click listeners (slower; up to two CDP round-trips per candidate element). Elements flagged opaqueInteractive are clickable surfaces whose interior targets cannot be enumerated — pick coordinates from a screenshot instead of expecting child refs.",
        schema: json!({
            "type": "object",
            "properties": {
                "mode": { "type": "string", "enum": ["interactive-only"], "description": "interactive-only excludes visual widgets (canvas, svg, cursor:pointer divs)." },
                "listeners": { "type": "boolean", "default": false, "description": "Also scan for programmatically-attached click listeners via CDP. Slower; off by default." }
            }
        }),
    },
    ToolDef {
       name: "browser_screenshot",
       description: "Capture a PNG screenshot and return it as a base64 image. By default captures the viewport; set fullPage for the whole page, or element to a @g<gen>:e<num> reference to capture only that element's region.",
       schema: json!({
           "type": "object",
           "properties": {
               "fullPage": { "type": "boolean", "default": false, "description": "Capture the entire scrollable page." },
               "element": { "type": "string", "description": "Capture only this element's region (e.g. @g1:e3)." }
           }
       }),
   },
    ToolDef {
        name: "browser_dewiggle",
        description: "Capture several frames of an animated/wiggling text region and reverse the per-glyph vertical wobble using PIXELS ONLY. Use this for wiggle CAPTCHAs (e.g. neal.fun Level 3). This is an honest pixel-only approach: it never reads answer arrays, component props, or DOM text — only the captured image intensities. Returns a realigned+averaged base64 PNG plus optional per-glyph crops when chars is set.",
        schema: json!({
            "type": "object",
            "properties": {
                "region": {
                    "type": "array",
                    "items": { "type": "number" },
                    "description": "Capture region [x,y,w,h] in viewport CSS px. Omit to auto-detect the canvas element's bounding box."
                },
                "frames": { "type": "integer", "default": 12, "description": "Number of frames to capture (2..=60)." },
                "intervalMs": { "type": "integer", "default": 80, "description": "Milliseconds between frames." },
                "chars": { "type": "integer", "description": "Segment into N equal-width glyph bands and return per-glyph crops." }
            }
        }),
    },
    ToolDef {
        name: "browser_click",
        description: "Click an element. Prefer a generation-bound reference (@g<gen>:e<num>) from observe; coordinates {x,y} also accepted. On STALE_REFERENCE error, call browser_observe again. Uses real mouse events, not JS click. Always returns a pre-click hit report; set effects=true to also sample post-click effects (adds ~300 ms): zero effects (no mutations/network/navigation/focus change) means a dead click.",
        schema: json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string", "description": "Semantic reference from observe, e.g. @e3." },
                "x": { "type": "number", "description": "Viewport X coordinate (CSS px)." },
                "y": { "type": "number", "description": "Viewport Y coordinate (CSS px)." },
                "button": { "type": "string", "enum": ["left","right","middle","back","forward"], "default": "left" },
                "count": { "type": "integer", "default": 1, "description": "Click count (1=single, 2=double, 3=triple)." },
                "modifiers": { "type": "string", "description": "Comma-separated: ctrl,shift,alt,meta." },
                "hold": { "type": "integer", "description": "Hold duration in ms before release." },
                "effects": { "type": "boolean", "default": false, "description": "Sample post-click effects (DOM mutations, network, navigation, focus) for 300 ms. Off by default; when off, effects is null." }
            }
        }),
    },
    ToolDef {
        name: "browser_hover",
        description: "Hover an element by @g<gen>:e<num> reference or {x,y} coordinate.",
        schema: json!({
            "type": "object",
            "properties": {
                "ref": { "type": "string" },
                "x": { "type": "number" },
                "y": { "type": "number" }
            }
        }),
    },
    ToolDef {
        name: "browser_mouse_move",
        description: "Move the cursor to {x,y}.",
        schema: json!({
            "type": "object",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" }
            },
            "required": ["x", "y"]
        }),
    },
    ToolDef {
        name: "browser_mouse_down",
        description: "Press a mouse button (does not release).",
        schema: json!({
            "type": "object",
            "properties": {
                "button": { "type": "string", "enum": ["left","right","middle","back","forward"], "default": "left" }
            }
        }),
    },
    ToolDef {
        name: "browser_mouse_up",
        description: "Release a mouse button.",
        schema: json!({
            "type": "object",
            "properties": {
                "button": { "type": "string", "enum": ["left","right","middle","back","forward"], "default": "left" }
            }
        }),
    },
    ToolDef {
        name: "browser_scroll",
        description: "Scroll by {dx,dy} at an optional {at} point. Uses real wheel events.",
        schema: json!({
            "type": "object",
            "properties": {
                "dx": { "type": "number", "default": 0 },
                "dy": { "type": "number", "default": 0 },
                "at": { "type": "string", "description": "Scroll at this coordinate, e.g. 400,300." },
                "steps": { "type": "integer", "default": 10 },
                "duration": { "type": "integer", "default": 300, "description": "Total duration in ms." }
            }
        }),
    },
    ToolDef {
        name: "browser_drag",
        description: "Drag from [x,y] to [x,y]. Emits intermediate mouseMoved events so sliders/canvases work.",
        schema: json!({
            "type": "object",
            "properties": {
                "from": { "type": "array", "items": { "type": "number" }, "description": "[x,y] start." },
                "to": { "type": "array", "items": { "type": "number" }, "description": "[x,y] end." },
                "steps": { "type": "integer", "default": 25 },
                "duration": { "type": "integer", "default": 500 }
            },
            "required": ["from", "to"]
        }),
    },
    ToolDef {
        name: "browser_drag_path",
        description: "Drag along a free path of [[x,y],...] points.",
        schema: json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "JSON array of [x,y] points, e.g. [[100,200],[400,350]]." },
                "duration": { "type": "integer", "default": 800 }
            },
            "required": ["path"]
        }),
    },
    ToolDef {
        name: "browser_type",
        description: "Type ASCII text as key events. For CJK/emoji use browser_insert_text.",
        schema: json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "delay": { "type": "integer", "default": 0, "description": "Inter-key delay in ms." },
                "sensitive": { "type": "boolean", "default": false, "description": "If true, redacts in traces." }
            },
            "required": ["text"]
        }),
    },
    ToolDef {
        name: "browser_insert_text",
        description: "Insert text verbatim via Input.insertText (Korean/CJK/emoji safe).",
        schema: json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" },
                "sensitive": { "type": "boolean", "default": false }
            },
            "required": ["text"]
        }),
    },
    ToolDef {
        name: "browser_key_press",
        description: "Press a key chord, e.g. Control+L, Enter, ArrowDown, Backspace, F5.",
        schema: json!({
            "type": "object",
            "properties": {
                "chord": { "type": "string", "description": "Key chord, e.g. Control+Shift+P." }
            },
            "required": ["chord"]
        }),
    },
    ToolDef {
        name: "browser_wait",
        description: "Wait until the page is stable (network idle + DOM quiet).",
        schema: json!({
            "type": "object",
            "properties": {
                "timeout": { "type": "integer", "default": 10000, "description": "Max wait in ms." }
            }
        }),
    },
    ToolDef {
        name: "browser_console",
        description: "Return console entries. Filter by level.",
        schema: json!({
            "type": "object",
            "properties": {
                "level": { "type": "string", "enum": ["error", "warn"], "description": "Filter to this level or above." }
            }
        }),
    },
    ToolDef {
        name: "browser_network",
        description: "Return network entries. Set failed=true for only failures.",
        schema: json!({
            "type": "object",
            "properties": {
                "failed": { "type": "boolean", "default": false }
            }
        }),
    },
    ToolDef {
        name: "browser_close",
        description: "Close the browser session.",
        schema: json!({ "type": "object", "properties": {} }),
    },
    ToolDef {
        name: "trace_start",
        description: "Start recording a trace. Actions are written to actions.jsonl + report.html under a run directory. Returns the run directory path.",
        schema: json!({
            "type": "object",
            "properties": {
                "base": { "type": "string", "description": "Base directory for the trace run (defaults to cwd)." }
            }
        }),
    },
    ToolDef {
        name: "trace_stop",
        description: "Stop tracing, flush the trace, and write report.html. Returns the run directory path.",
        schema: json!({ "type": "object", "properties": {} }),
    },
    ToolDef {
        name: "replay",
        description: "Replay a recorded action sequence from a run directory against the current session. Re-observes as needed. Sensitive (redacted) values are skipped.",
        schema: json!({
            "type": "object",
            "properties": {
                "runDir": { "type": "string", "description": "Path to the run directory containing actions.jsonl." }
            },
            "required": ["runDir"]
        }),
    },
]
}
/// Return the full tool list as a JSON array (for `tools/list` result).
pub fn tool_definitions() -> Vec<Value> {
    tools()
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.schema,
            })
        })
        .collect()
}

/// Map an MCP tool name to the internal JSON-RPC method used by `rpc::dispatch`.
pub fn tool_for_method(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "browser_open" => "browser.open",
        "browser_observe" => "observe",
        "browser_screenshot" => "screenshot",
        "browser_dewiggle" => "dewiggle",
        "browser_click" => "click",
        "browser_hover" => "hover",
        "browser_mouse_move" => "mouse.move",
        "browser_mouse_down" => "mouse.down",
        "browser_mouse_up" => "mouse.up",
        "browser_scroll" => "scroll",
        "browser_drag" => "mouse.drag",
        "browser_drag_path" => "mouse.drag_path",
        "browser_type" => "type",
        "browser_insert_text" => "insert-text",
        "browser_key_press" => "key.press",
        "browser_wait" => "wait",
        "browser_console" => "console",
        "browser_network" => "network",
        "browser_close" => "browser.close",
        "trace_start" => "trace.start",
        "trace_stop" => "trace.stop",
        "replay" => "replay",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tools_have_unique_names() {
        let mut names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate tool names");
    }

    #[test]
    fn all_tools_have_schemas() {
        for t in tools() {
            assert_eq!(
                t.schema["type"], "object",
                "{} missing object schema",
                t.name
            );
        }
    }

    #[test]
    fn tool_for_method_covers_all_tools() {
        for t in tools() {
            assert!(
                tool_for_method(t.name).is_some(),
                "{} has no method mapping",
                t.name
            );
        }
    }
}
