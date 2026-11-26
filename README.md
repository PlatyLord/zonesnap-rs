# ZoneSnap

A lightweight, portable Windows window-snapping and window-management utility written in Rust.

## Features

- **True Portability & Zero Bloat:** Single standalone `zonesnap.exe` with zero external runtime DLLs. Stores all configuration in `zones.json` directly alongside the executable (never writes to `%APPDATA%` or the Windows Registry). Generates a sensible default `zones.json` automatically on first launch.
- **PerMonitorV2 DPI Awareness:** Accurate physical-pixel coordinates across mixed-resolution and mixed-DPI multi-monitor setups using each monitor's `rcWork` work area (excluding taskbars).
- **Dual Snapping Modes (Multiple Saved Layouts Per Monitor):**
  - **Dynamic Grid (WindowGrid style):** Configurable `cols x rows` grid (default `12x6`). Anchor cell starts under the cursor; drag across cells to span any rectangle.
  - **Custom Zones (FancyZones style):** Normalized `[x1, y1, x2, y2]` zone rectangles. Supports holding `Shift` or hovering within `16px` of a shared border to merge multiple zones into their combined bounding box.
- **Alt-Window Manipulation:** Move windows from anywhere inside the window (`Alt + Left-Drag`) and resize from the nearest edge or corner (`Alt + Right-Drag`) using a 3x3 grid division.
- **Ctrl Monitor-Lock:** Hold `Ctrl` during any window drag or zone snap to clamp the cursor (`ClipCursor`) to the active monitor's bounds, preventing overshoot onto adjacent screens.
- **Flush DWM Snapping & Unsnap Restore:** Measures `DWMWA_EXTENDED_FRAME_BOUNDS` to compensate for Windows 10/11 invisible drop-shadow borders so windows sit 100% flush. Automatically restores a snapped window's original pre-snap dimensions when dragged away without re-snapping.
- **Per-App Blacklist (`excluded_apps`):** Passes mouse hooks through untouched for configured executables (defaults: `mstsc.exe`, `vmconnect.exe`).
- **System Tray & Visual Zone Editor:** Native Windows system tray menu plus an on-demand `egui` settings & visual zone editor window with live config hot-reloading.

## Controls & Shortcuts

| Action | Default Trigger | Notes |
| --- | --- | --- |
| **Snap via Standard Drag** | Drag title bar + tap `Right-Click` | Suppresses context menu and opens zone overlay on cursor's monitor; release `Left-Click` to snap |
| **Move Window Anywhere** | `Alt + Left-Drag` | Moves window without needing the title bar |
| **Hand Off Alt-Move to Zones** | Tap `Right-Click` during `Alt + Left-Drag` | Opens zone overlay seamlessly mid-drag |
| **Resize from Nearest Edge/Corner** | `Alt + Right-Drag` | Resizes smoothly using a 3x3 grid of the window bounds |
| **Monitor-Lock Cursor** | Hold `Ctrl` while dragging | Clamps cursor inside active monitor bounds (`ClipCursor`) |
| **Cycle Monitor Layouts** | `Mouse Wheel` while overlay is visible | Switches between saved layouts for that monitor and shows a toast badge |
| **Merge Custom Zones** | Hold `Shift` or hover shared border (`±16px`) | Merges hovered custom zones into their combined bounding box |
| **Cancel Overlay** | `Esc` or second `Right-Click` | Dismisses overlay without snapping |
| **Activate Zone Overlay (Keyboard)** | `Win + Alt + Z` | Opens overlay for the focused window; `Left-Click` snaps, `Right-Click`/`Esc` cancels |
| **Open Settings & Zone Editor** | `Win + Alt + E` or Tray Menu | Launches visual layout editor and settings GUI |

## Portable Deployment

Copy `zonesnap.exe` to any folder (including a USB drive) and run it:
- On first launch, `zones.json` is automatically created in the same folder as `zonesnap.exe`.
- Right-click the ZoneSnap icon in the Windows system tray to **Configure Zones & Settings**, **Reload zones.json**, or **Exit**.

## `zones.json` Schema

```json
{
  "version": 1,
  "appearance": {
    "highlight_color": [56, 189, 248],
    "highlight_opacity": 0.35,
    "border_color": [56, 189, 248],
    "zone_bg_opacity": 0.14
  },
  "behavior": {
    "restore_on_unsnap": true,
    "excluded_apps": ["mstsc.exe", "vmconnect.exe"]
  },
  "hotkeys": {
    "activate_overlay": { "modifiers": ["Win", "Alt"], "key": "Z" },
    "open_settings": { "modifiers": ["Win", "Alt"], "key": "E" },
    "move_resize_modifier": "Alt",
    "monitor_lock_modifier": "Ctrl"
  },
  "monitors": {
    "\\\\.\\DISPLAY1_2560x1440": {
      "device_name": "\\\\.\\DISPLAY1",
      "width": 2560,
      "height": 1440,
      "active_layout_index": 0,
      "layouts": [
        {
          "name": "Dynamic Grid (12x6)",
          "mode": { "type": "dynamic_grid", "cols": 12, "rows": 6 }
        },
        {
          "name": "Columns (3-Up)",
          "mode": {
            "type": "custom_zones",
            "zones": [
              [0.0, 0.0, 0.33333334, 1.0],
              [0.33333334, 0.0, 0.6666667, 1.0],
              [0.6666667, 0.0, 1.0, 1.0]
            ]
          }
        }
      ]
    }
  }
}
```

## Building from Source

Requires the Rust toolchain (`x86_64-pc-windows-msvc`) and Visual Studio C++ Build Tools:

```powershell
cargo build --release
cargo test
```

The standalone executable is output to `target/release/zonesnap.exe`.
