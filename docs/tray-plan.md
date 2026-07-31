# Plan: `jdrgb-tray` — a tray helper for picking preset colors

Status: proposed, not started.

## Goal

A small always-running tray icon that lets you pick a preset color from a menu
without opening a terminal. The icon itself shows the current color. It starts
at logon.

Explicitly *not* a general GUI for jdrgb — no tuning, no per-LED editing, no
settings window. If a thing needs more than one click, it stays in the CLI.

## Decision summary

| Question | Decision |
|---|---|
| Separate project, or part of jdrgb? | Part of jdrgb — a second binary in this crate |
| GUI toolkit | None. Hand-rolled Win32 via the existing `windows-sys` |
| How the tray applies a color | Spawns `jdrgb.exe` (v1); direct library call is a later option |
| How much of `main.rs` moves | Only the color tables and state I/O (~200 lines), not the device layer |
| Startup mechanism | A **separate per-user logon task** — not the existing SYSTEM task |

## Why part of jdrgb rather than a standalone helper

Three things a standalone helper would need already exist here:

- `PRESETS` / `GPU_PRESETS` (`src/main.rs:68`, `:147`) — the names and their
  per-device tuned values.
- `SWATCH` + `swatch_rgb()` (`src/main.rs:196`, `:218`) — the *on-screen
  appearance* of each preset, which is exactly what a menu swatch needs and is
  not derivable from the wire values. `coolwhite`'s `#FFB0D0` draws as pink.
- `load_last()` / `record()` (`src/main.rs:1666`, `:1655`) — the per-device
  state file that gives the menu its "current color" marking.

A separate C++ app in the style of `hdd-toggle` would have to duplicate all
three and would drift the next time a color is tuned by eye. Scraping
`jdrgb presets` doesn't help: that output carries wire values, not appearance
values.

### Why a second binary and not a `jdrgb tray` subcommand

`jdrgb.exe` is a console binary. A tray app wants
`#![windows_subsystem = "windows"]`, and one executable cannot be both
subsystems. Two `[[bin]]` targets over a shared module solves it cleanly and
keeps `jdrgb.exe` exactly what the README claims it is — one-shot, nothing
resident. The tray is opt-in and separately shippable.

```toml
[lib]
name = "jdrgb"
path = "src/lib.rs"

[[bin]]
name = "jdrgb"        # console subsystem, unchanged
path = "src/main.rs"

[[bin]]
name = "jdrgb-tray"   # windows subsystem
path = "src/tray/main.rs"
```

`windows-sys` features the tray adds: `Win32_UI_WindowsAndMessaging`,
`Win32_UI_Shell`, `Win32_UI_HiDpi`, `Win32_Graphics_Gdi`,
`Win32_System_Threading` (`CreateMutexW`), `Win32_Graphics_Dwm`
(`DwmSetWindowAttribute`, used by the dark-mode helper).

## Measured cost to the CLI

All measured on this machine, current source (see Appendix A for method):

- **Binary size: no change at all.** Adding all six `windows-sys` features
  above produced a byte-identical 246,272-byte `jdrgb.exe`. `windows-sys` is
  pure FFI declarations; unreferenced ones emit no code and create no imports.
- **Startup: no change.** ~4.5 ms warm, which is essentially all Windows
  process creation. No new DLL imports for the CLI.
- **Inlining: no change.** The release profile already has `lto = true` (fat)
  and `codegen-units = 1`, so a module boundary costs nothing, and LTO
  reachability drops tray-only code from `jdrgb.exe`.

Real costs: `cargo build --release` links two binaries instead of one, and
releases ship two exes.

## Architecture

The tray does not talk to hardware. It spawns `jdrgb.exe` for every apply:

```
jdrgb-tray.exe  ──spawn──>  jdrgb.exe warmwhite --all
      │                          │
      └── src/palette.rs ────────┘   (shared: names, swatch colors, state file)
```

Rationale for spawning rather than linking the device layer in v1:

- The extraction shrinks from ~600 lines (device layer, retry logic, reporting)
  to ~200 lines of pure data, lookups, and state I/O. Nothing to untangle.
- The tray's message loop can never be blocked by a wedged `hidapi`
  enumeration. (This bounds the *UI*, not the work queue — see Phase 4.)
- Errors come back as the CLI's own stderr text, already written for humans.

Cost is one process spawn per click (~5 ms plus device work). Irrelevant at
human click rates.

Linking the library directly stays open as a later change. It is not needed to
start, and the usual objection to it — that `println!` aborts a
windows-subsystem binary — is false (Appendix A).

## Work

### Phase 1 — extract the palette (`src/palette.rs`)

Move, unchanged: `PRESETS`, `GPU_PRESETS`, `SWATCH`, `DEFAULT_COLOR`,
`DEFAULT_PRESET`, `swatch_rgb`, `lookup_preset`, `parse_hex`, and the **whole**
state module (`state_path`, `Last`, `parse_state`, `load_state`, `encode_state`,
`record`, `load_last`).

Move `record()` too, even though the tray never calls it — it depends on
`load_state`/`encode_state`, and splitting the group would leave `main.rs`
reaching back across the crate boundary for its own state writes. Only
`jdrgb.exe` writes state in v1.

A binary consumes its package's library as a *separate crate*, so everything
above needs `pub`, including the `Last` variants. Nothing here is a behavior
change; the existing unit tests (`src/main.rs:1762`) that cover these move with
them.

Verify: `cargo test` passes and `jdrgb.exe` is still 246,272 bytes.

### Phase 2 — tray shell

A normal top-level window that is never shown (no `WS_VISIBLE`). **Not**
`HWND_MESSAGE`: message-only windows do not receive broadcast messages, and the
tray must receive `TaskbarCreated`.

- `Shell_NotifyIconW(NIM_ADD)` with the retry loop pattern from
  `hdd-control-gui.cpp:404` — Explorer's notification area may not be ready at
  logon.
- Immediately after **every** successful `NIM_ADD`, set
  `uVersion = NOTIFYICON_VERSION_4` and call `NIM_SETVERSION`. This must be
  repeated on every add, including Explorer-restart recovery, and it changes the
  callback parameter packing — handle `WM_CONTEXTMENU`, `NIN_SELECT`, and
  `NIN_KEYSELECT` rather than raw `WM_RBUTTONUP`.
- Handle `RegisterWindowMessageW("TaskbarCreated")` and re-add. Without this the
  icon vanishes permanently if Explorer restarts.
- `NIM_DELETE` on orderly shutdown (`WM_DESTROY`/`WM_ENDSESSION`).
- A named mutex (`CreateMutexW` + `ERROR_ALREADY_EXISTS`) so a second launch
  exits instead of stacking a duplicate icon.
- DPI awareness set as the very first thing in `main`, before any window or
  DPI-dependent call — or via the manifest, which is more robust.
- Dark-mode menus: lift `InitDarkMode` / `ApplyDarkModeToWindow` from
  `hdd-toggle/src/hdd-control-gui.cpp:108-135`. Note this depends on
  **undocumented `uxtheme.dll` ordinals** (`hdd-control-gui.cpp:111`). Guard
  every `GetProcAddress` and degrade to a light menu rather than failing. Test
  light, dark, and high-contrast early.

A stable `guidItem` + `NIF_GUID` would let Explorer remember the icon's
placement across reinstalls. Optional, and it has a sharp edge: the GUID is
bound to the exe path, so moving the binary silently breaks the icon. Skip in
v1.

### Phase 3 — swatches

`MENUITEMINFOW` with `MIIM_BITMAP` and an `hbmpItem` pointing at a 32bpp
premultiplied-BGRA `CreateDIBSection`, filled by hand. A circle is a distance
test per pixel — no GDI+, no Direct2D, no image assets:

```rust
let c = (size as f32 - 1.0) / 2.0;
let rad = c - 0.5;
for y in 0..size {
    for x in 0..size {
        let d = ((x as f32 - c).hypot(y as f32 - c) - rad + 0.5).clamp(0.0, 1.0);
        let a = 1.0 - d;                       // 1 inside, 0 out, fractional edge
        let p = |v: u8| (v as f32 * a) as u32; // premultiply
        px[(y * size + x) as usize] =
            ((a * 255.0) as u32) << 24 | p(r) << 16 | p(g) << 8 | p(b);
    }
}
```

**A bare filled circle is not enough.** `white` and `coolwhite` vanish against a
light menu; `black` disappears against a dark one; and `black` and `off` are
indistinguishable. Draw a ring: composite the fill over a 1px border at ~50%
grey, or ring it in the swatch color's own luminance-inverted tone. Still no
image assets — it is one more distance comparison in the same loop. Give `off`
a distinct treatment (hollow ring, or a diagonal slash) so it never reads as
`black`.

Other notes:

- Use `MIIM_BITMAP`, **not** owner-draw. Owner-draw would forfeit the
  Windows 11 rounded/acrylic menu styling that `hdd-toggle` preserves.
- Premultiply the edge pixels or antialiased edges get halos.
- Alpha outside the circle means one bitmap composites correctly in both light
  and dark menus — composites correctly, which is not the same as *visible*.
  Hence the ring.
- Size from the DPI of the monitor the menu will actually appear on
  (`MonitorFromPoint(cursor)` → `GetDpiForMonitor`), resolved at menu-open time.
  Do **not** derive it from the hidden window: `GetDpiForWindow` reports the
  monitor containing that never-shown HWND, which may not be where the tray is.
  For the tray icon itself, `Shell_NotifyIconGetRect` locates the real icon.
  Cache the bitmap set keyed by DPI.
- The `HBITMAP`s must outlive the menu; `DeleteObject` them when it is
  destroyed. Rebuild for a new DPI only after the menu has stopped referencing
  the old set. Build the 29 preset bitmaps once per DPI — those colors never
  change.

The tray icon reuses the same drawing code via `CreateIconIndirect`
(`ICONINFO { fIcon: TRUE, hbmColor: <ARGB DIB>, hbmMask: <1bpp AND mask> }`),
sized from the icon's own monitor DPI. So the icon *is* the current color, and
the project needs no icon artwork at all — no `assets/` tree, no ImageMagick
step.

Two things to get right here:

- **The mask must be a real AND mask** (1 = transparent outside the circle,
  0 = opaque inside), not all zeros. An all-zero mask appears to work until
  something consults the mask separately, then the icon renders as an opaque
  square.
- `CreateIconIndirect` **copies** its bitmaps, so `DeleteObject` both sources
  right after the call. `DestroyIcon` the previous `HICON` when replacing it and
  at shutdown.
- *Uncertain:* `CreateIconIndirect` does not document the premultiplication
  contract that GDI alpha-blending requires. The same buffer may or may not be
  reusable verbatim between menu and icon. Test the fractional edge pixels
  early; if edges render too dark, keep separate non-premultiplied buffers for
  the icon.

### Phase 4 — applying a color

Menu click spawns `jdrgb.exe <preset> [--gpu|--all]` on a **worker thread**, not
the message loop.

- One worker thread with an `mpsc` channel, so rapid clicks serialise rather
  than racing two writers onto the same controller.
- **Time-bound the child** (~90 s, above the CLI's own 60 s `--wait` ceiling) and
  kill it on expiry. Spawning protects the message loop but not the queue: one
  hung child on a synchronous HID (`main.rs:700`) or NVAPI (`gpu.rs:335`) call
  would otherwise block every later command forever, leaving a responsive tray
  that does nothing. Kill any in-flight child on exit.
- `CREATE_NO_WINDOW`, `.stdout(Stdio::null())`, `.stderr(Stdio::piped())`.
- On non-zero exit, show captured stderr in a balloon
  (`Shell_NotifyIconW(NIM_MODIFY)` with `NIF_INFO`). Shell balloons need no
  AUMID and no Start Menu shortcut, unlike `hdd-toggle`'s WinRT toasts.
  `szInfo` caps at 256 UTF-16 units including the terminator (200 recommended)
  and `szInfoTitle` at 64 — truncate on a char boundary rather than assuming it
  fits. Balloons can also be suppressed by Focus Assist, so never rely on one
  as the only signal; the icon must independently reflect reality.
- **A partial `--all` failure is a real state, not an error.** The CLI applies
  and records each device independently and only then reports failure if either
  leg failed (`main.rs:522`). So a non-zero exit can still mean the strip
  changed. Re-read state after every child regardless of exit code rather than
  treating non-zero as "nothing happened".
- Post completion back to the UI thread with a `WM_APP` message; never touch UI
  state from the worker.

Locating `jdrgb.exe`: same directory as the tray exe first, then `PATH`.

### Phase 5 — current color

Read `load_last(gpu)` and reverse-lookup the RGB against the preset tables to
find a name. Exact match only — these are values the tables themselves wrote.

- **Re-read the state file every time the menu opens**, not once at startup. A
  terminal `jdrgb red` changes the hardware without telling the tray, and
  `record()` is a best-effort whole-file replace (`main.rs:1655`) that the tray
  does not participate in. Re-reading on open is the cheap way to stay honest.
- For the GPU, check `GPU_PRESETS` first, then fall back to `PRESETS`.
- A hand-tuned hex or a `Last::Multi` matches nothing; mark nothing and show the
  hex in the status line.
- With target `All`, mark only if both devices resolve to the *same* name.

For marking the current item, prefer `SetMenuDefaultItem` (renders bold, leaves
the bitmap gutter alone) over `MFS_CHECKED`. *Correction to an earlier draft:*
check marks and `hbmpItem` are forced to share one gutter only under
`MNS_CHECKORBMP`; without that style they may occupy separate columns. So this
is a look-and-feel preference, not a hard constraint — but confirm the rendering
early in Phase 3, since it is cheap to test and annoying to discover late.

### Phase 6 — startup

**The existing scheduled task cannot be reused.** `install.ps1:111` registers it
as `NT AUTHORITY\SYSTEM` with `RunLevel Highest`. A SYSTEM task runs in session
0 and cannot show a tray icon in the user's session.

So the tray needs its own registration, in `install.ps1` behind a `-Tray`
switch:

- A second scheduled task, `-AtLogOn -User <invoking user>`, `RunLevel Limited`,
  running `jdrgb-tray.exe`.
- **Add `-Tray` to the self-elevation arg list at `install.ps1:41`.** That block
  reconstructs the argument list by hand; a switch not named there is silently
  dropped when the script relaunches under UAC.
- Capture the invoking user's SID *before* elevating and register the task for
  that SID. After `Start-Process -Verb RunAs`, `$env:USERNAME` is whoever
  answered the UAC prompt, which on a machine where the desktop user is not an
  admin is the wrong account. (Low-probability on a single-admin box, but it
  costs one line to be right.)
- The tray itself needs no elevation — `is_elevated()` (`src/main.rs:1510`) is
  used only to annotate `probe` output. Installing does, because it writes to
  Program Files.
- Start the task at install time so the icon appears immediately, rather than
  requiring a logoff.

`uninstall.ps1` needs matching work: add `jdrgb-tray.exe` to `$Artifacts`
(`uninstall.ps1:37`), unregister the user task, and stop the running tray. Stop
it by **full image path**, not `Stop-Process -Name jdrgb-tray` — a broad name
match would kill a development copy running from `target\release`.

The existing SYSTEM task stays exactly as it is. It fires at startup before any
user logs in, which the tray cannot do.

## Known issue: the state file is per-user, and the boot task is SYSTEM

`state_path()` (`src/main.rs:1579`) resolves `%LOCALAPPDATA%\jdrgb\last`. The
boot task runs as SYSTEM, so it writes
`C:\Windows\System32\config\systemprofile\AppData\Local\jdrgb\last`. The tray,
running as the logged-in user, reads a **different file**.

Consequence: at logon the tray cannot know what the boot task just applied. It
will agree with any `jdrgb` you run yourself, and with its own actions, but not
with the boot apply.

This is pre-existing — `jdrgb tune` already has it — but the tray is what makes
it visible. Options, in order of preference:

1. **Accept it in v1.** The tray marks nothing until its first action. Honest,
   zero code, and correct for everything the user does interactively.
2. Have the tray apply its own last-known color at logon. Rejected: it would
   race the SYSTEM task's own `AtLogOn` trigger (`install.ps1:119`), so two
   writers would hit the controller seconds apart with no defined winner.
3. Move state to a machine-wide location (`%ProgramData%\jdrgb\last`). Correct,
   and it makes the tray authoritative at logon, but it changes CLI behavior and
   needs its ACLs thought through — SYSTEM and the user both write it.

Recommend (1) for v1, with (3) as the principled fix if it grates.

## Menu shape

29 presets flat is ~810 px and would get scroll arrows on a 1080p display.

```
  ● warmwhite            <- disabled status line, swatch + current color name
  ─────────────
  Color            >     <- submenu, all 29 with swatches
  Target           >     <- Strip / GPU / Both, radio-marked
  Off
  ─────────────
  Reapply
  Exit
```

`Reapply` re-sends the last known color — useful after a controller reset. When
state is unknown, `Multi`, or mixed across devices, it falls back to the default
preset and the status line says so rather than showing a stale color.

Target selection is tray-local UI state, not a property of jdrgb. Persist it in
`%LOCALAPPDATA%\jdrgb\` or HKCU — **not** beside the executable, which lives in
an administrator-only-writable directory by deliberate design
(`install.ps1:73`).

## Non-goals for v1

Tuning UI, per-LED editing, rainbow/load/template, a settings window, custom
icon artwork, WinRT toasts, a recent-colors list, autostart-toggle-from-menu.
Each is easy to add later and none is needed to make the thing useful.

## Appendix A — verified findings

Measurements taken while writing this plan, on the current source:

- **Feature cost is zero.** Baseline `jdrgb.exe` is 246,272 bytes. With all six
  `windows-sys` features the tray needs added: 246,272 bytes, unchanged.
- **The build is not bit-reproducible.** Three builds of unchanged source gave
  three distinct SHA-256 hashes at identical size (PE timestamp / debug GUID).
  Compare sizes, not hashes, when checking for bloat.
- **CLI startup is ~4.5 ms warm** (~40 ms cold), over 8 runs of
  `jdrgb --version`.
- **`println!` does *not* kill a `windows_subsystem = "windows"` binary.**
  Tested directly, including with `SetStdHandle(STD_OUTPUT_HANDLE, NULL)` forced
  before the call: clean exit 0 either way. This is common folklore and it is
  wrong on current Rust; do not design around it. Redirecting the child's stdout
  to `Stdio::null()` in Phase 4 is belt-and-braces, not a fix.
- **Nothing at runtime needs elevation.** `is_elevated()` is cosmetic, used only
  in `probe` output.
- **`PRESETS` has 29 entries** (`src/main.rs:68`), not 30.
- **README drift, unrelated:** the README says the release binary is ~160 KB.
  It is 246 KB. Worth fixing.
