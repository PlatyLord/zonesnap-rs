# Changelog

All notable changes to ZoneSnap are documented in this file.

## [v1.2.0] - 2026-11-26

### Added
- **Configurable Trigger Pad Colors & Opacity:** Added `trigger_color` and `trigger_opacity` to `OverlayStyle` (`zones.json` and **Appearance & Behavior → Overlay & Editor Colors**) so decoupled trigger/launchpad pads can be themed in both the Zone Editor and live overlay.
- **Full Theme Color Matching:** **Match Overlay Colors** now updates Zone Fill, Zone Border, and Trigger Pad colors together across all 10 built-in themes.
- **Live Color Swatch Icons:** Replaced hardcoded "(Blue)" / "(Amber)" labels in the Zone Editor with live color swatch icons beside **Snap Output**, **Trigger Pad**, and the coordinate controls.

### Changed
- **Zone Editor Readability Overhaul:** Rebuilt the Custom Zones and Dynamic Grid canvas rendering for crisp contrast across all light and dark themes—added subtle 4×4 alignment guides, distinct selected vs. unselected fills, high-contrast rounded label pills with anti-collision placement, luminance-aware `Trigger N` badges, and topmost resize handles.
- **Documentation:** Updated `docs/` screenshots to match the refreshed editor theming and documented `trigger_color` / `trigger_opacity` in `README.md`.

---

## [v1.1.0] - 2026-11-26

### Added
- **Mouse-Button Move & Resize Modifier:** `move_resize_modifier` can now be mapped to `Mouse Back`, `Mouse Forward`, or `Middle Click` in **Shortcuts & Modifiers** (and `zones.json`) for one-handed mouse-only Move (`+ Left-Drag`), Resize (`+ Right-Drag`), Drag-to-Top Maximize, and Zone Overlay hand-off (`+ Left-Drag + Right-Click`). Quick taps without dragging pass through normally on release.

### Changed
- **Documentation:** Revamped and streamlined `README.md` with app icon hero banner, screenshots, visual feature comparison table, and hero screenshot wallpaper attribution.

---

## [v1.0.0] - 2026-11-26

### Added
- **10 Built-In GUI Themes:** Added `Dark`, `Light`, `Catppuccin Mocha`, `Catppuccin Macchiato`, `Catppuccin Frappé`, `Catppuccin Latte`, `Nord`, `Tokyo Night`, `Gruvbox Dark`, and `Solarized Light`, plus a **Match Overlay Colors** action.
- **Idle Overlay Memory Eviction:** Automatically frees full-screen overlay surfaces after 10 seconds of idle time to keep idle memory footprint around ~1–3 MB.
- **Public Release Metadata:** Added GPL-3.0 `LICENSE` and repository metadata.

### Fixed
- Fixed GUI text scale slider/numeric input behavior and centered the **About & Credits** tab layout.

---

## [v0.1.4] - 2026-11-26

### Added
- **Custom Trigger Areas:** Decouple a custom zone's drop target (`trigger`, amber) from its final snap rectangle (`bounds`, blue) so overlapping center/side layouts remain easy to target.
- **Center Focus + Sides Preset:** Added preset in the Visual Zone Editor showcasing decoupled trigger pads.
- **DPI-Friendly GUI Scaling:** Added Settings Window Text Scale (`85%–175%` and `Ctrl +`/`-`/`0`).

---

## [v0.1.3] - 2026-11-26

### Added
- **Expanded Shortcuts Reference:** Added a live 14-row **Complete Controls & Shortcuts Reference** table in **Shortcuts & Modifiers**.
- **About & Credits Tab:** Added **About & Credits** tab and license documentation.

---

## [v0.1.2] - 2026-11-26

### Added
- **Embedded `.exe` Icon Resource:** Generated and embedded multi-resolution Windows icon resources via `build.rs`.
- **Drag-to-Top Maximize:** Dragging a window to the top edge of any monitor during Move shows a Maximize preview and maximizes on release (`drag_top_maximize`).

---

## [v0.1.1] - 2026-11-26

### Added
- **Nearest-Corner Resize Modifier:** Added `corner_resize_modifier` (default `Shift`, i.e. `Alt + Shift + Right-Drag`) to force 2×2 quadrant corner resizing regardless of cursor start position.

### Fixed
- Resolved system tray context menu crash and optimized overlay rendering.

---

## [v0.1.0] - 2026-11-26

### Added
- **Initial Portable Release:** Single-binary Rust Windows 10/11 window manager (`zonesnap.exe`) with `PerMonitorV2` DPI awareness and portable `zones.json` storage beside the executable.
- **Two Snapping Modes:** **Dynamic Grid** (`2×2`–`24×16` anchor-to-cursor spanning) and **Custom Zones** (normalized rectangles with `Shift` multi-zone merge and `±16px` shared-border merge), triggered via Title Bar Drag + `Right-Click` or `Win + Alt + Z`.
- **Alt Move/Resize & Monitor-Lock:** `Alt + Left-Drag` to move from anywhere, `Alt + Right-Drag` for 3×3 edge/corner resize, `Ctrl` cursor clamping (`ClipCursor`), scroll-wheel layout cycling with toast indicator, and `excluded_apps` process blacklist.
- **DWM Flush Snapping & Unsnap Restore:** `DWMWA_EXTENDED_FRAME_BOUNDS` border compensation (including mixed-DPI second-pass verification) and automatic pre-snap window size restoration on unsnap.
- **Visual Zone Editor & System Tray:** `egui` layout/settings editor (`Win + Alt + E`) with multi-monitor layout copy/duplicate, tray layout submenu, and automated GitHub Release workflow.
