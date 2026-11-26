# Terminal Support and Troubleshooting

EchoAgent Build runs as a full-screen TUI. It relies on terminal support for color,
clipboard, keyboard input, mouse input, and full-screen display. Terminals,
multiplexers, containers, and SSH sessions can handle these features differently.

## Diagnose and Fix Terminal Problems

Run `/doctor` in EchoAgent to check the current session and see available fixes. If
EchoAgent cannot start, run `echoagent doctor` in your shell. Use `echoagent doctor --json`
for a machine-readable report.

Doctor checks the terminal, multiplexer, color support, keyboard and newline
behavior, clipboard routes, and microphone availability when audio capture is
included. The in-app command can also check live session details such as
notification focus tracking and sandbox profile conflicts.

A report can contain issues or recommendations and still exit successfully.
`echoagent doctor --json` reports the same color capability when piped. Microphone
checks do not start recording, so Doctor cannot detect macOS permission failures
that appear only as silence during capture.

`/terminal-setup`, `/terminal-check`, and `/terminal-info` remain aliases for
`/doctor`.

When Doctor finds an explicit unhealthy tmux setting, `/doctor fix` lists the
available automatic fixes. Apply one named fix at a time, for example
`/doctor fix tmux-clipboard` or `echoagent doctor fix dcs-passthrough --yes`.
Doctor can persist these four tmux options:

- `terminal.tmux-clipboard` — `set -g set-clipboard on`
- `terminal.dcs-passthrough` — `set -wg allow-passthrough on`
- `terminal.tmux-extended-keys` — `set -g extended-keys on`
- `terminal.tmux-truecolor` — `set -as terminal-features ",*:RGB"`

A tmux fix edits only the persistent config on the computer hosting the affected
tmux server, including remote sessions. Plain tmux uses the real
`$HOME/.tmux.conf`; Byobu-tmux uses its effective `BYOBU_CONFIG_DIR` and refuses
to guess if that directory is unavailable or unsafe. EchoAgent preserves the file's
line endings and mode, makes a backup when changing an existing file, and
refuses conflicting or ambiguous direct assignments.

EchoAgent deliberately does **not** run `tmux source-file` or change the live tmux
server. Reload with the exact command shown after apply, or detach and reattach,
then run `/doctor` again. Until reload, the live finding is expected to remain.
The conservative config scan checks direct global assignments only; review
sourced files, conditionals, plugins, and generated tmux setup yourself.

---

## Detected Terminals

EchoAgent detects these terminal emulators from environment variables:

- **Apple Terminal**
- **Ghostty**
- **iTerm2**
- **Warp**
- **WezTerm**
- **Kitty**
- **Alacritty**
- **Rio**
- **foot** (Wayland-native, Linux)
- **VS Code**, **Cursor**, **Windsurf**, and **Zed** integrated terminals
- **JetBrains** IDE terminals
- **EchoAgent Desktop**
- **VTE**-based terminals such as GNOME Terminal, GNOME Console, and Tilix
- **Windows Terminal**

Detection has these limitations:

- Inside tmux, variables that identify the outer terminal may not reach EchoAgent.
- Over SSH, many terminal variables are not forwarded.
- tmux's global environment reflects the first client attached to the server,
  not necessarily the current terminal.

---

## Common Problems and Fixes

### Colors look wrong or lack truecolor

Run `/doctor`. A fully supported setup shows `color truecolor` and `themes all`.
If it does not, Doctor shows the detected limitation and the relevant fix.

Inside tmux there are two separate questions: what color EchoAgent emits, and what
color survives the multiplexer. The `color` line answers the first. For the
second, when the attached client is not marked `RGB`, tmux rewrites every
24-bit color to the nearest color the outer terminal's terminfo advertises,
which can be as few as eight. Themes then look washed out even though `color`
reads `truecolor`. Doctor reports this as `terminal.tmux-truecolor`. Reload
your tmux config and then detach and reattach: the server reads the new option
only on reload, and a client fixes its color depth only at attach, so neither
step alone changes anything.

### Clipboard problems

EchoAgent writes through up to three routes, shown in `/doctor` under **Clipboard**:

- **native** — the local operating-system clipboard.
- **tmux** — the tmux paste buffer when EchoAgent runs inside tmux.
- **OSC 52** — an escape sequence that can cross tmux, containers, or SSH.

#### Wayland

Modern Wayland compositors can update the clipboard without keeping the
terminal focused. Older compositors may require EchoAgent to remain focused until
the copy message appears. EchoAgent shows a startup warning when this applies; run
`/doctor` for the detected status and steps.

`ECHO_AGENT_CLIPBOARD_NO_DATA_CONTROL=1` is an advanced fallback that disables the
data-control route. Copies then use command-line clipboard tools.

#### OSC 52 kill switch

EchoAgent emits OSC 52 on Linux and across tmux, SSH, or displayless containers when
that route is enabled. A terminal that does not implement OSC 52 may display the
encoded payload as text. Set `ECHO_AGENT_CLIPBOARD_NO_OSC52=1` before starting EchoAgent to
disable that route. `/doctor` then shows `osc 52 off`; native and tmux routes are
unchanged.

#### Linux X11 selections

X11 **PRIMARY** and **CLIPBOARD** are separate:

- An unmodified middle click reads PRIMARY only when `DISPLAY` is set. Under
  XWayland, `xclip` or `xsel` must be on `PATH`.
- `Ctrl+V` reads CLIPBOARD and never falls back to PRIMARY.
- `Shift+Insert` remains the terminal's selected-text paste.

#### SSH and selected text

A remote EchoAgent process normally cannot read the local terminal's selection. Use
terminal-native `Shift+Insert`, or hold `Shift` while middle-clicking when the
terminal uses that gesture to bypass mouse reporting.

When EchoAgent cannot identify the outer terminal over SSH, it predicts that OSC 52
will be sent but marks the route as not verified. The copy toast then names the
backup file so you can retrieve the text. Run `/doctor` for other copy options.

#### Apple Terminal over SSH

Apple Terminal does not support OSC 52, so a remote copy cannot reach the local
clipboard. Each copy is still saved to a backup file (`~/.echo-agent/last-copy.txt` by
default; override with `ECHO_AGENT_COPY_FILE`); the toast names that path when delivery
is unverified or the clipboard is unreachable. You can also use `/copy <file>` or
`/minimal`.

For direct clipboard forwarding, run the SSH command from the local computer
through `echoagent wrap`, for example `echoagent wrap ssh user@host`. The same command can
wrap container and pod shells. It also restores terminal modes after a dropped
connection.

When an SSH session is not using `echoagent wrap`, EchoAgent shows the one-time tip
“Run `/doctor` for details and fixes.” The tip stops appearing after the session
is launched through wrap. Turn it off with `/settings` → **Show contextual
hints** → **SSH wrap**, or set `ssh_wrap = false` under
`[ui.contextual_hints]` in `$ECHO_AGENT_HOME/config.toml`. This setting does not hide
the Doctor recommendation.

For repeated SSH use, Doctor offers `echoagent doctor fix ssh-wrap`. It also shows
the one-off command, the file that would change, and the cases where the alias
should be bypassed. The ID `terminal.ssh-wrap` remains accepted and appears in
JSON.

> **Warning**: `echoagent wrap` is experimental and may not work in every setup.

#### iTerm2

iTerm2 can require permission for OSC 52 clipboard access. Run `/doctor`; the
`terminal.iterm2-clipboard-permission` recommendation shows the setting to
check.

### Fullscreen or alternate screen does not activate

Zellij and tmux control mode can limit the alternate screen. EchoAgent normally uses
inline mode in those environments. Run `/doctor` to see the detected condition.
You can configure `[terminal] alt_screen` in `~/.echo-agent/pager.toml`, or run
`echoagent --no-alt-screen` to confirm inline mode works.

### Zellij keybindings interfere with EchoAgent

Zellij can intercept Ctrl/Alt keys before they reach EchoAgent. On Zellij 0.41 or
later, use the **Unlock-First (non-colliding)** preset:

1. Press `Ctrl+o`, then `c`.
2. Open **Change Mode Behavior**.
3. Select **Unlock-First (non-colliding)**.
4. Press `Enter` to apply it.

Press `Ctrl+g` when you need Zellij's own pane or session controls. In minimal
mode, if `Ctrl+G` still does not reach EchoAgent, open the command palette and select
**Edit Prompt in External Editor**. This preserves the current draft; typing
`/edit-prompt` starts an empty editor draft because the command itself occupies
the composer.

### Ctrl+Enter does not interject in WezTerm

WezTerm ships with the Kitty keyboard protocol disabled. Run `/doctor` in EchoAgent.
The `terminal.wezterm-kitty` finding shows the setting and restart step. Over
SSH, Doctor shows only the workaround that can work in the current session.
Apple Terminal uses `Ctrl+O` for interjection because it cannot distinguish the
modified Enter chord.

### Shift+Enter does not insert a newline in VS Code

VS Code, Cursor, Windsurf, and Zed terminals use xterm.js, which only partially
implements the Kitty keyboard protocol and mis-encodes some shifted printable
keys. EchoAgent therefore does not negotiate the protocol there, and Shift+Enter can
arrive as the same `CR` as Enter. This also affects VS Code reached over SSH when
`TERM_PROGRAM` is not forwarded. Use `Alt+Enter` to insert a newline; `/doctor`
reports `terminal.newline-fallback` with the detected explanation and workaround.

### Mouse scrolling stops working

If EchoAgent stops receiving mouse input, re-enable mouse reporting in the terminal:

- **Apple Terminal**: **View → Allow Mouse Reporting** (`Cmd+R`).
- **iTerm2**: **Settings → Profiles → Terminal → Enable mouse reporting**.

### Voice dictation records nothing

After about 10 seconds without a transcript, EchoAgent stops capture and shows
**“No speech was detected. Voice stopped.”** with microphone fix steps. On macOS,
a denied microphone grant can look the same as silence because permission belongs
to the terminal hosting EchoAgent. Open **System Settings → Privacy & Security →
Microphone**, enable the terminal, and restart it. If access is already on, check
the input device and level under **System Settings → Sound → Input** and try
again.

Run `echoagent doctor`, or run `/doctor` while voice mode is on. The **Voice** section
shows the microphone EchoAgent would use. If no input device is available, Doctor
shows `voice.no-input-device` and the next steps. Doctor cannot detect denied
macOS microphone access passively when macOS supplies silence.

On macOS, each dictation uses a short-lived capture helper process so the audio
stack's memory is released when capture ends. If the helper itself may be the
problem, set `ECHO_AGENT_VOICE_CAPTURE=inprocess` to use the in-process fallback for
comparison.

### Byobu with GNU screen

Byobu on GNU screen has limited support. `/doctor` reports
`terminal.byobu-screen` and explains how to switch to Byobu's tmux backend.

### Arabic and Persian (RTL) text

Many terminals already reorder right-to-left text themselves (VTE-based
terminals, Terminal.app, Konsole, mlterm, and others). EchoAgent Build therefore
**does not** reorder RTL by default.

If Arabic or Persian in **scrollback** (or list content) reads backwards,
enable app-side reordering in `~/.echo-agent/pager.toml` (or project config):

```toml
[scrollback.display]
rtl_bidi = true
```

The setting reloads with appearance config (no full restart required). If text
looks correct with the default and becomes wrong after enabling this, turn it
back off — your terminal is already handling bidi.

When enabled:

- Reorders full content lines in scrollback, list content, and the fullscreen
  block viewer (plus the dashboard peek preview and hook popup, which mirror
  scrollback). Chrome, dropdowns, and modals stay logical so their hit-testing
  stays consistent.
- Leaves markdown table columns unchanged.
- Search highlights, selection/drag-copy, double-click word/URL selection, and
  link hit targets all map between the painted (visual) cells and the logical
  text of the same row, so on-screen highlights land on the right glyphs while
  clipboard paste stays in logical order.
- Base direction is resolved per painted row. A soft-wrapped continuation that
  starts with English can take a different base than the paragraph's first row.

This is not a full mirrored RTL UI.

---

## Still Stuck?

Run `/feedback` to report it.
