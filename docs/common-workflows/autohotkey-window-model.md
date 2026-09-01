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

## The shape of the keymap

Four rules cover almost all of it: `Alt` is the model itself, `Shift` is the reverse or the "whole
container" version, `Ctrl` means the floating window, and `H J K L` are left, down, up, right. The
arrow keys are never combined with `Win`, because `Win+arrow` belongs to the Windows snap
shortcuts and a binding which fights the shell for a key is a binding which sometimes does nothing.

| Key | Action |
| --- | --- |
| `Alt+H J K L` | Focus the container to the left / below / above / right |
| `Alt+Shift+H J K L` | Swap the focused container with that neighbour |
| `Alt+arrow` | Move the focused window into the container in that direction |
| `Alt+Shift+arrow` | Merge the container in that direction into the focused one |
| `Alt+[` `Alt+]` | Walk the focused container's window history |
| `Alt+Shift+[` `Alt+Shift+]` | Walk the workspace's container history |
| `Alt+N` | Raise the window under the top of the stack |
| `Alt+X` / `Alt+Shift+X` / `Alt+Ctrl+X` | Add a container: automatic / left-right / top-bottom |
| `Alt+Z` / `Alt+Shift+Z` | Remove the newest container / the focused one |
| `Alt+F` / `Alt+Shift+F` / `Alt+Ctrl+F` | Floating / maximized / fullscreen |
| `Alt+W` / `Alt+M` / `Alt+Shift+M` | Close / minimize / restore the last minimized window |
| `Alt+Ctrl+H J K L` | Move the floating window |
| `Alt+Ctrl+Shift+H J K L` | Grow the floating window's left / bottom / top / right edge |
| `Alt+Ctrl+Shift+Y U I O` | Shrink the same four edges (the row above `H J K L`) |
| `Alt+Ctrl+arrow` / `Alt+Ctrl+Shift+arrow` | Push a container boundary outwards / inwards |
| `Alt+1..8` | Focus a workspace |
| `Alt+A` / `Alt+Shift+A` | New workspace / delete this workspace and merge it |
| `Alt+Ctrl+[` `Alt+Ctrl+]` | Move the workspace one position left / right |
| `Alt+I` | The shortcut panel |

## Things worth knowing about the bindings

- **Suspending is not ignoring.** `Alt+U` (`suspend-window`) takes the focused window out of
  management and leaves it exactly where it is; ordinary Win32 events will not take it back.
  `Alt+Shift+U` (`resume-window`) hands it back, and it is then processed as if it had just opened -
  it does not return to its old container, workspace or stack position. A window excluded by an
  ignore rule never enters the model at all and neither command applies to it.
- **Floating is not unmanaged.** `Alt+F` toggles a window between `Stored` and `Floating` placement.
  A floating window still belongs to its container, still holds its place in the stack and still
  travels with the container; komorebi simply stops positioning it. `Alt+Ctrl+H/J/K/L` moves it and
  `Alt+Ctrl+Shift+H/J/K/L` and `Alt+Ctrl+Shift+Y/U/I/O` resize one of its edges, and none of them
  touches a container or a slot. The mouse moves it too: dragging a floating window records where
  it was dropped, and a drag which carries it off the screen is pulled back only as far as leaving
  a grabbable strip inside the work area.
- **`Alt+arrow` moves one window, `Alt+Shift+arrow` moves a whole container.** The first is
  `stack`: the focused window joins the container in that direction and both containers survive.
  The second is `merge-container`: every window of that neighbour joins the focused container, the
  neighbour is destroyed and its slot goes back to the arrangement through the same expansion the
  ordinary deletion path uses. Neither moves the focus off the window the user is looking at.
- **Adding and removing containers are two keys each, and none of them moves the focus.** `Alt+X`
  adds one by dividing the largest slot, `Alt+Shift+X` and `Alt+Ctrl+X` force the dividing line.
  The window the created container gets is the second most recent one in the workspace's focus
  history which is not the window its own container is showing, so no container changes what it is
  drawing and the operation refuses when there are as many containers as there are windows. `Alt+Z`
  removes the container created most recently, which is exactly the inverse of `Alt+X`, and
  `Alt+Shift+Z` removes the focused one instead. Both deal their windows out to the containers
  which remain.
- **Walking a history does not rewrite it.** `Alt+[` and `Alt+]` walk the focused container's
  window history and raise the window they land on to the top of the container; `Alt+Shift+[` and
  `Alt+Shift+]` walk the workspace's container history and move the focus to the container they
  land on. Neither reorders the history it is reading, because a history whose head is rewritten on
  every step can only ever reach its two most recent entries - the same reason `Alt+Tab` holds its
  order until the key is released. The walk ends at the next focus which does not come from these
  keys, and the ordinary recording resumes there.
- **Single windows move by stable ID.** `move-to-workspace` and `send-to-workspace` take an index
  and move the whole container. To send one window on its own, use `Alt+G` / `Alt+B`, which ask for
  a workspace or container ID - `komorebic state` reports them - and the `Alt+Shift` variants add
  `--follow` so focus travels with the window. `Alt+Shift+F1..F3` does the same across monitors
  with `move-window-to-monitor`.
- **`Alt+I` opens the script's own panel.** `komorebic toggle-shortcuts` shows the bindings from a
  `whkdrc`, which this configuration does not have, so the script carries a small AutoHotkey v2
  panel listing what it actually binds. The tray icon carries the same entry, for when a hotkey is
  swallowed by an elevated foreground window. The panel is an ordinary captioned window, so
  `window_is_eligible` would hand it a slot; give it an `ignore_rules` entry so it floats over the
  tiling instead:

    ```json
    { "kind": "Title", "id": "komorebi 快捷键", "matching_strategy": "StartsWith" }
    ```

## The script

```autohotkey
{% include "./common-workflows/komorebi-model.ahk" %}
```
