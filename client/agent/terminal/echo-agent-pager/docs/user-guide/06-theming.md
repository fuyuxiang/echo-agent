# Theming and Appearance Customization

EchoAgent Build draws all TUI colors from a central theme. You can switch themes while EchoAgent is running, follow your operating system's light or dark appearance, and adjust scrollback layout, animations, and block styling through configuration files.

---

## Available Themes

EchoAgent includes five built-in themes, plus an `auto` option that follows your system appearance:

| Theme | Config Names | Description | Truecolor Required |
|-------|-------------|-------------|--------------------|
| **EchoAgentNight** | `echo-agent-night`, `echo-agent-night`, `dark` | Neutral dark base with a magenta accent. Default theme. Survives quantization cleanly on 256-color and 16-color terminals. | No |
| **EchoAgentDay** | `echo-agent-day`, `echo-agent-day`, `light`, `day` | Light theme for bright terminal backgrounds. | No |
| **TokyoNight** | `tokyonight`, `tokyo-night`, `tokyo` | Dark, blue-tinted backgrounds from the Tokyo Night palette. Loses its character when quantized. | Yes |
| **RosePineMoon** | `rosepine`, `rose-pine`, `rosepine-moon`, `rose-pine-moon` | Muted dark palette with mauve accents, from the Rosé Pine family. | Yes |
| **OscuraMidnight** | `oscura`, `oscura-midnight` | Deep dark base with purple accents. | Yes |

Theme names are case-insensitive. The `auto` option (alias `system`) is documented under [Auto Theme (System Appearance)](#auto-theme-system-appearance).

### Minimal Mode Has No Theming

**Minimal mode** (`--minimal`) always renders with a single fixed terminal-native palette and ignores the `theme` settings entirely (they still apply to the full TUI). Minimal draws directly on your terminal's own background, so it uses your terminal's default foreground/background plus its 16-color ANSI palette — the same colors `git` or `ls` use — which stays readable on any light or dark terminal profile without detection or configuration. `/theme` and the theme rows in `/settings` are unavailable in minimal mode.

Syntax highlighting in minimal mode does **not** switch between light and dark theme files (polarity detection is intentionally avoided). Near-gray tokens inherit the terminal default foreground; chromatic tokens use base ANSI accents (red/green/yellow/blue/magenta/cyan) so read-file output and fenced code stay legible on both light and dark profiles.

---

## Switching Themes

### In the TUI

Run the `/theme` slash command (alias `/t`) to open the theme picker. As you move through the list with the arrow keys, EchoAgent previews each theme in real time. Press Enter to apply and save your choice, or press Escape to revert.

To switch without the picker, pass a name directly:

```
/theme tokyonight
```

Submitting `/theme` on its own -- without choosing from the picker -- cycles to the next theme.

### Via Config File

Set the theme in `~/.echo-agent/config.toml`:

```toml
[ui]
theme = "tokyonight"
```

---

## Auto Theme (System Appearance)

Set `theme = "auto"` to have EchoAgent follow your operating system's light/dark appearance and switch themes automatically:

```toml
[ui]
theme = "auto"
```

By default, dark mode maps to **EchoAgentNight** and light mode maps to **EchoAgentDay**. Override either mapping with `auto_dark_theme` and `auto_light_theme`:

```toml
[ui]
theme = "auto"
auto_dark_theme = "tokyonight"
auto_light_theme = "echo-agent-day"
```

`theme = "system"` is an alias for `theme = "auto"`.

### How Detection Works

| Platform | Method |
|----------|--------|
| **macOS** | Reads `AppleInterfaceStyle` system preference |
| **Linux** | Queries XDG Desktop Portal (`org.freedesktop.appearance.color-scheme`) |
| **Windows** | Reads the system personalization registry |
| **SSH / tmux / headless** | `ECHO_AGENT_APPEARANCE` or `LC_ECHO_AGENT_APPEARANCE` (`dark`/`light`), then `COLORFGBG`, then a startup OSC 11 background query. `echoagent wrap ssh …` stamps `LC_ECHO_AGENT_APPEARANCE` from the local OS theme so it survives SSH into the login shell. New tmux sessions inherit it only if the tmux server/session was created with that env (or `update-environment` includes it). OSC 11 is DCS-wrapped for tmux ≥ 3.3 when tmux is the immediate terminal (not an editor `:terminal`); reaching the outer emulator also needs `allow-passthrough`, and replies are best-effort. |

Once running, EchoAgent polls desktop APIs and env hints every 5 seconds. Toggling your OS between light and dark mode on a local desktop takes effect within seconds without restarting. Over SSH the wrap-stamped env is fixed for that hop.

You can also set `ECHO_AGENT_THEME` (or `LC_ECHO_AGENT_THEME`) to force a theme or `auto` without editing `config.toml`.

### Via the Settings Pane

Run `/settings` (alias `/config`) and open the **Appearance** category to set the **Auto dark theme** and **Auto light theme** interactively. Selecting `auto` in the `/theme` picker enables auto mode using these mappings.

---

## Color Support Detection

On startup, EchoAgent detects your terminal's color capability level:

| Level | Description | Detection |
|-------|-------------|-----------|
| **Truecolor** (24-bit) | Full RGB color. All themes render as designed. | `COLORTERM=truecolor` or equivalent terminal capability |
| **256-color** | Indexed palette. RGB values are mapped to the nearest palette entry. | Standard xterm-256color |
| **16-color** | ANSI names only. Colors are mapped to the closest ANSI color. | Basic terminal support |

When you set `NO_COLOR`, EchoAgent emits no color and renders in monochrome.

Run `/doctor` to see the detected color level and the themes available on this terminal. If truecolor is unavailable, Doctor shows the relevant setup steps or explains the terminal limitation.

### Automatic Quantization

Every theme is defined using full RGB values. At startup, EchoAgent quantizes all colors to match the detected capability level. This means:

- On **truecolor** terminals, colors pass through unchanged.
- On **256-color** terminals, each RGB value is mapped to the nearest indexed palette entry.
- On **16-color** terminals, colors map to ANSI names.

EchoAgentNight and EchoAgentDay use neutral grays that quantize cleanly. TokyoNight, RosePineMoon, and OscuraMidnight use distinctive tinted backgrounds that lose their character when quantized, which is why the theme picker hides them on non-truecolor terminals.

### Runtime-Generated Colors

Colors generated at runtime (syntax highlighting, background blending) are also quantized through the same pipeline, ensuring consistent appearance across all terminal types.

---

## Cursor Color

EchoAgent sets your terminal cursor to the current theme's `accent_user` color using the OSC 12 escape sequence, to indicate an active EchoAgent session. The cursor color is:

- Applied on startup and on theme switch.
- Reset to the terminal's default on exit via OSC 112.

This works in terminals that support OSC 12 (most modern terminals).

---

## Compact Mode

Toggle compact mode with the `/compact-mode` slash command. Compact mode:

- Removes outer vertical padding (top/bottom margins become 0).
- Reduces horizontal padding to the minimum (1 column).
- Reduces top padding in the prompt area and info blocks.

The setting is persisted in `~/.echo-agent/config.toml` under `[ui].compact_mode` and survives restarts.

Use compact mode on small screens to maximize content area.

---

## Syntax Highlighting

EchoAgent bundles three `.tmTheme` files for code-block syntax highlighting and selects one based on the active theme:

- `echo-agent-night.tmTheme` -- EchoAgentNight, RosePineMoon, and OscuraMidnight
- `echo-agent-day.tmTheme` -- EchoAgentDay
- `tokyo-night.tmTheme` -- TokyoNight

EchoAgent selects the matching file automatically when you switch themes. The `.tmTheme` files are built into the binary, so you cannot replace them with your own.

---

## Deep Customization with pager.toml

For fine-grained control over the TUI appearance, create `~/.echo-agent/pager.toml`. This file controls scrollback layout, block styling, animations, and more. All settings have defaults; specify only the values you override. (Dev builds generate this file as a template with every default commented out — uncomment a line to override it; commented values keep tracking future defaults.)

### Layout

Control viewport padding and block spacing:

```toml
[scrollback.layout]
outer_vpad = 1          # Vertical padding (top/bottom) for the viewport
outer_hpad_left = 2     # Left margin (minimum: 1)
outer_hpad_right = 2    # Right margin (minimum: 1)
block_pad_left = 2      # Padding between accent line and content
block_pad_right = 2     # Padding after content at right edge
```

### Scrollbar

```toml
[scrollback.scrollbar]
enabled = true          # Show/hide the scrollbar
gap_left = 0            # Gap between content and scrollbar (0 = adjacent)
gap_right = 0           # Gap between scrollbar and screen edge (0 = at edge)
# scrollbar_bg = "none" # Override background color (or "none" for theme default)
# scrollbar_fg = "none" # Override thumb color (or "none" for theme default)
```

### Scroll Behavior

```toml
[scrollback.scroll]
margin = 0                  # Context lines above/below selected entry (0 = edge)
min_page_fraction = 0       # Minimum scroll as % of viewport (0-100)
follow_indicator = "center" # "center" = show the ▼/▲ scroll arrows, "none" = hidden
follow_auto_select = true   # Auto-select latest entry when following
follow_by_overscroll = true # Scrolling past bottom engages follow mode
anchor_on_fold = true       # Keep block header at same screen position when folding
```

### Display Options

```toml
[scrollback.display]
sticky_headers = true              # Pin user prompts as headers when scrolled past
tab_width = 4                      # Spaces per tab character (0 = pass through)
expandable_indicator = true        # Show "›" on foldable collapsed entries
expandable_indicator_char = "›"    # Character to use (default: "›")
collapsed_accent_char = "❙"        # Accent for collapsed groupable blocks (falls back to "|" on the legacy Windows console)
dim_accent = 0.5                   # Blend factor for dimmed accents (0.0-1.0)
line_under_last_entry = false      # Horizontal line below last entry
selection_buttons = false          # Show copy/view buttons on selection box
```

### Animation

```toml
[animation]
fps = 30           # Frame rate (1-60). Higher = smoother, more CPU
wave_rows = 32     # Rows per wave cycle for accent animation
```

### Block Styling: Edit Diffs

```toml
[scrollback.blocks.edit]
indent = true                   # Indent diff content
vpad = false                    # Vertical padding around diffs
# expanded_by_default = true    # Unset: follows [ui] collapsed_edit_blocks in config.toml
                                # (flag on = collapsed one-liner); uncomment to pin either shape
hunk_separator = "…"            # Separator between hunks ("…", "───", "⋯", or "" for none)
dual_line_numbers = false       # Two-column line numbers (old + new, like GitHub)
# line_summary = false          # Show +N/-M in the collapsed header; unset follows the same flag
# bg = "none"                   # Block background ("none", "light", "dark")
```

### Block Styling: Thinking/Reasoning

```toml
[scrollback.blocks.thinking]
accent_enabled = true       # Show accent line for thinking blocks
animate = true              # Animate accent line while thinking
truncated_lines = 3         # Lines to show in truncated mode
bg_blend = 70               # Markdown-color blend with background (0-100)
header = true               # Show "Thinking..." header
header_bright = false       # Bright header style (vs dim/muted)
```

### Block Styling: Tool Calls

```toml
[scrollback.blocks.tool]
muted_collapsed = true     # Gray out collapsed tool calls
dim_details = true          # Dim parenthetical details (line counts, match counts)
bullet = "diamond"          # Bullet style before tool headers
```

Available bullet styles:

| Config Value | Character | Description |
|-------------|-----------|-------------|
| `none` | (none) | No bullet |
| `dot` | `·` | Middle dot (smallest) |
| `small-circle` | `•` | Bullet |
| `circle` | `●` | Filled circle |
| `small-triangle` | `▸` | Right-pointing small triangle |
| `triangle` | `▶` | Right-pointing triangle |
| `diamond` | `◆` | Filled diamond (default) |

### Block Styling: Execute (Shell Commands)

```toml
[scrollback.blocks.execute]
first_lines = 2                   # Output lines shown at start in truncated mode
last_lines = 3                    # Output lines shown at end in truncated mode
accent_enabled = true             # Show accent line (animated while running)
header_style = "label"            # "shell" ($ prefix) or "label" (Run prefix)
muted_command_collapsed = true    # Mute command text when collapsed
```

### Block Styling: User Prompts (Scrollback)

```toml
[scrollback.blocks.prompt]
vpad = true            # Vertical padding
bg = "light"           # Background ("none", "light", "dark")
show_prefix = true     # Show the prompt prefix character
min_lines = 2          # Minimum content lines in truncated/sticky mode
```

### Prompt Input Widget

```toml
[prompt]
collapse_unfocused = true    # Collapse when scrollback is focused
mouse_hover = true           # Show hover highlight on mouse over
show_prefix = true           # Show the prompt prefix character
```

### Terminal Behavior

```toml
[terminal]
alt_screen = "auto"    # "auto", "always", or "never"
```

Alt-screen policies:
- `auto` -- fullscreen in plain terminals and normal tmux; inline in tmux control mode and Zellij.
- `always` -- always enter fullscreen.
- `never` -- never enter fullscreen; run inline in the main scrollback.

### Plugins UI

```toml
disable_plugins = false   # Set to true to hide /hooks, /plugins commands and annotations
```

---

## Theme Color Slots

Each theme defines the following color slots that are used throughout the TUI:

**Backgrounds:** `bg_base`, `bg_light`, `bg_dark`, `bg_highlight`, `bg_hover`, `bg_terminal`, `bg_visual`

**Accents:** `accent_user`, `accent_assistant`, `accent_thinking`, `accent_tool`, `accent_system`, `accent_error`, `accent_success`, `accent_running`, `accent_skill`, `accent_plan`, `accent_verify`, `accent_remember`, `accent_model`

**Text:** `text_primary`, `text_secondary`

**Grays:** `gray_dim`, `gray`, `gray_bright`

**Semantic:** `command`, `path`, `running`, `warning`, `fuzzy_accent`

**Borders and scrollbar:** `selection_border`, `hover_border`, `prompt_border`, `prompt_border_active`, `scrollbar_bg`, `scrollbar_fg`

**Paste:** `paste_bg`, `paste_fg`, `paste_dim`

**Diff:** `diff_delete_bg`, `diff_delete_fg`, `diff_insert_bg`, `diff_insert_fg`, `diff_equal_fg`, `diff_gutter_fg`

**Markdown:** heading colors (`md_heading_h1`-`md_heading_h6`), `md_code`, `md_code_bg`, `md_text`, `md_muted`, `md_task_checked`, `md_task_unchecked`, `link_fg`

The theme system manages these slots internally and quantizes them automatically for your terminal.
