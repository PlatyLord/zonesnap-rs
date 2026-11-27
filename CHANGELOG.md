# Changelog

All notable changes to ZoneSnap are documented in this file.

## [v1.3.0] - 2026-11-27

**Goal:** Eliminate pixel-hunting on thin Windows 11 DWM borders by adding opt-in outward-expanded resize hit-boxes and optional floating circular grab handles around the active foreground window—with zero idle CPU or memory overhead when disabled.

### Added
- **Expanded Resize Borders (Phase 1):** Opt-in outward resize hit-ring around the active foreground window (`edge_resize.enabled`, default `false`), supporting configurable outward padding (`grab_padding_px`, `4`–`32` px), a corners-only mode (`corners_only`), native directional resize cursors, and smooth resize reuse via ZoneSnap's existing `AltResize` session (no extra window-moving path).
- **Optional Visual Grab Handles (Phase 2):** Floating circular layered GDI grab handle (`edge_resize.show_handles`) that appears when hovering an eligible outer edge or corner, with configurable diameter (`handle_size_px`, `12`–`36` px) and customizable RGB/hex color (`handle_color`).
- **Low-Overhead Architecture:** Zero WinEvent hooks, zero GDI handle allocation, and an immediate lock-free atomic bailout in the low-level mouse hook when disabled; when enabled, uses `SetWinEventHook` + integer bounding-ring math (`O(1)` per `WM_MOUSEMOVE`) plus UIPI/elevation checks and occlusion guards so neighboring windows stay interactive.
- **Settings Editor Integration:** Added the **Expanded Resize Borders & Grab Handles** section to the **Appearance** tab (including a hex color picker and **Match Overlay Colors** sync).

### Documentation
- Updated [README.md](README.md) (`Why ZoneSnap?`, `How ZoneSnap Compares`, `Highlights & Workflow Notes`, `Configuration (zones.json)`, and release badge) to highlight the opt-in Expanded Resize Borders & Grab Handles feature.

## [v1.2.1] - 2026-11-27

**Goal:** Make active window snapping, dragging, and resizing noticeably snappier and smoother—matching or beating AltSnap under load—while keeping idle memory under ~2 MB and eliminating resize artifacts.

### Performance
- **SIMD-Optimized Release Profile:** Switched `[profile.release]` to `opt-level = 3` (enabling LLVM SIMD auto-vectorization for faster 1440p/4K overlay blending while keeping the single-file `.exe` around ~4 MB and ~1.5 MB idle RAM).
- **Zero-Stall Mouse Hook:** Raised the `zonesnap-hook` thread priority to `THREAD_PRIORITY_HIGHEST` and removed `OpenProcess` / `QueryFullProcessImageNameW` from normal Windows left- and right-clicks in `WH_MOUSE_LL`.
- **Cached Daemon State:** Cached the active `MonitorInfo` list, window `can_maximize` state, and `zones.json` path in `DaemonState`, eliminating per-move syscalls and synchronous disk `stat` calls when opening the overlay.
- **Reusable Overlay Text Scratch:** Cached the GDI scratch DC, 32-bit DIB section, and `HFONT`s (`TextScratch`) across overlay badge draws (released cleanly on the 10s idle eviction timer).

### Fixed
- **Smoother, Artifact-Free Resizing:** Queried each window's real `WM_GETMINMAXINFO` minimum track size (`30 ms` non-blocking timeout) to stop Left/Top-edge jitter when shrinking apps past their minimum dimensions, added `IsHungAppWindow` safety guards, and added an `8 ms` (~120 Hz) resize ceiling with a 1-shot flush timer (`TIMER_RESIZE_FLUSH`) plus `RightUp` final settle while keeping synchronous back-pressure so slower apps like File Explorer never build up laggy queues.

### Added
- **About & Credits Avatar:** Embedded a compact `10 KB` circular author avatar (`assets/avatar.bin`) loaded lazily in the **About & Credits** tab.

---

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
