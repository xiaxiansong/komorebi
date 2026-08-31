# AutoHotkey for the window model

This page is the shortcut half of the window / container / workspace model. Every command it binds
is a `komorebic` subcommand, and every decision about where a window lives, which container owns it,
which slot that container has and what happens when it goes away is made by komorebi itself. The
script is a launcher, not a second window manager.

It does not use `whkd`, and it does not need a `whkdrc`.

## Requirements

- AutoHotkey v2 (the script declares `#Requires AutoHotkey v2.0` and will refuse to run on v1).
- `komorebic.exe` on `PATH`, or its full path written into the variable at the top of the script.

Save the script as `komorebi.ahk` in `$Env:KOMOREBI_CONFIG_HOME`, or in `$Env:USERPROFILE` when
that variable is not set, and run it. `Alt+Ctrl+S` then starts komorebi with the static
configuration next to it.

## What to change at the top

| Variable | Meaning | Default |
| --- | --- | --- |
| `KomorebicPath` | Where `komorebic.exe` is | `"komorebic.exe"` (found on `PATH`) |
| `KomorebiConfigHome` | Config directory | `%KOMOREBI_CONFIG_HOME%`, else `%USERPROFILE%` |
| `StaticConfig` | Static JSON configuration | `<config home>\komorebi.json` |
| `FloatingMoveDelta` | Step for moving a floating window | `40` |
| `FloatingResizeDelta` | Step for resizing a floating window | `40` |

The two deltas are separate on purpose, and so are the commands they feed: moving a floating window
and resizing one are distinct from the container commands which look similar. Passing no delta at
all makes komorebi use its configured `floating_move_delta` / `floating_resize_delta` instead.

## Reading the result of a command

komorebi answers a mutating command with an outcome, and `komorebic` exits with a code for it, so a
script can tell "you asked to move a tiled window" apart from "komorebi is not running". The
`Komorebic` helper turns a non-zero code into a tooltip:

| Exit code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | komorebic could not reach komorebi |
| `10` | No-op: valid command, nothing to change |
| `11` | The target window is not floating |
| `12` | The target window is minimized |
| `13` | There is no valid target |
| `14` | The target window is ignored by configuration |
| `15` | The target window is temporarily unmanaged |
| `16` | The command would have broken a model invariant and was refused |

## Things worth knowing about the bindings

- **Suspending is not ignoring.** `Alt+U` (`suspend-window`) takes the focused window out of
  management and leaves it exactly where it is; ordinary Win32 events will not take it back.
  `Alt+Shift+U` (`resume-window`) hands it back, and it is then processed as if it had just opened -
  it does not return to its old container, workspace or stack position. A window excluded by an
  ignore rule never enters the model at all and neither command applies to it.
- **Floating is not unmanaged.** `Alt+T` toggles a window between `Stored` and `Floating` placement.
  A floating window still belongs to its container, still holds its place in the stack and still
  travels with the container; komorebi simply stops positioning it. `Win+arrow` moves it and
  `Win+Shift/Ctrl+arrow` resizes one of its edges, and neither touches a container or a slot.
- **Adding and removing containers are two keys each, and none of them moves the focus.** `Alt+C`
  adds one by dividing the largest slot, `Alt+Shift+C` and `Alt+Ctrl+C` force the dividing line.
  The window the created container gets is the second most recent one in the workspace's focus
  history which is not the window its own container is showing, so no container changes what it is
  drawing and the operation refuses when there are as many containers as there are windows. `Alt+D`
  removes the container created most recently, which is exactly the inverse of `Alt+C`, and
  `Alt+Shift+D` removes the focused one instead. Both deal their windows out to the containers
  which remain.
- **Containers, not windows, are what the direction keys move.** `Alt+Shift+H/J/K/L` swaps whole
  containers, `Alt+arrow` merges the focused window into the container in that direction, and
  `Alt+N` raises the next window in the current stack to the top.
- **Single windows move by stable ID.** `move-to-workspace` and `send-to-workspace` take an index
  and move the whole container. To send one window on its own, use `Alt+G` / `Alt+B`, which ask for
  a workspace or container ID - `komorebic state` reports them - and the `Alt+Shift` variants add
  `--follow` so focus travels with the window. `Alt+Shift+F1..F3` does the same across monitors
  with `move-window-to-monitor`.
- **`Alt+I` opens the script's own panel**, and `Alt+/` does the same. `komorebic
  toggle-shortcuts` shows the bindings from a `whkdrc`, which this configuration does not have, so
  the script carries a small AutoHotkey v2 panel listing what it actually binds. The tray icon
  carries the same entry, for when a hotkey is swallowed by an elevated foreground window. The
  panel is an ordinary captioned window, so `window_is_eligible` would hand it a slot; give it an
  `ignore_rules` entry so it floats over the tiling instead:

    ```json
    { "kind": "Title", "id": "komorebi 快捷键", "matching_strategy": "StartsWith" }
    ```

## The script

```autohotkey
{% include "./common-workflows/komorebi-model.ahk" %}
```
