# echo-agent-pager

Terminal UI (TUI) for EchoAgent Build. Provides the interactive full-screen interface
including the scrollback view, prompt input, session management, and all modal
dialogs.

## Architecture

```
src/
├── app/                 # Application state and event handling
│   ├── app_view.rs      # Top-level state (welcome screen, agents, config)
│   ├── agent_view/      # Per-session agent view (struct in mod.rs + per-domain impl modules)
│   ├── dispatch/        # Action → Effect dispatcher (router + per-domain modules)
│   ├── effects.rs       # Async side effects (ACP calls, file I/O)
│   └── event_loop.rs    # Main event loop (input, ticks, ACP messages)
├── views/               # UI components
│   ├── prompt_widget.rs # Text editor with file search, slash, history
│   ├── welcome/         # Welcome screen (logo, menu, prompt)
│   ├── extensions_modal.rs   # Extensions modal (hooks, plugins, marketplace, skills, MCP servers)
│   ├── file_search/     # @-completion dropdown and line viewer
│   ├── slash_dropdown.rs# /command completion dropdown
│   └── ...              # Scrollback, status bar, panes, etc.
├── scrollback/          # Message history rendering
├── slash/               # Slash command registry and built-in commands
├── appearance/          # Theme and pager.toml config
├── acp/                 # Agent Communication Protocol client state
└── render/              # Low-level rendering helpers (color, wrapping, etc.)
```

## Key Concepts

- **AppView** — owns the welcome screen, agent sessions, and global config
- **AgentView** — one per session; owns the prompt, scrollback, tool panes, and modals
- **PromptWidget** — text editor component with file search (`@`), slash commands (`/`), history search, and paste elements
- **Action/Effect** — Elm-style architecture: input → Action → dispatch → Effect → state update

## Keyboard Shortcuts

| Key | Context | Action |
|-----|---------|--------|
| `Ctrl+P` or `?` | Agent screen | Open command palette |
| `Ctrl+L` | Any (non–VS Code family) | Open plugins/hooks modal; on VS Code / Cursor / Windsurf / Zed use `/plugins` or `/hooks` (`Ctrl+L` is mid-turn interject) |
| `Tab` | Prompt | Switch to scrollback |
| `Esc` | Turn running | Cancel — in minimal mode or with vim scrollback mode off (the default). Fullscreen vim mode: no-op (use `Ctrl+C`) |
| `Esc` `Esc` | Idle, non-empty prompt | Clear prompt (within 800ms; first press shows hint) |
| `Esc` `Esc` | Idle, empty prompt + messages | Open rewind picker (silent first press) |
| `Ctrl+M` | Prompt | Toggle multiline mode |
| `Shift+Enter` | Prompt | Insert newline |
| `/` | Prompt | Start slash command |
| `@` | Prompt | Start file search |
| `!` | Prompt (empty) | Enter bash mode |
| `Ctrl+C` | Prompt (with text) | Clear prompt (even while turn running) |
| `Ctrl+C` | Prompt (empty) + turn running | Cancel running turn |
| `Ctrl+B` | Agent screen + foreground command running | Send the command to the background |
| `Ctrl+G` | Agent screen (full TUI) | Toggle the tasks pane |
| `Ctrl+G` | Ordinary composer (minimal mode) | Edit the draft externally; use the command-palette entry if the chord is reserved |

## Docs

- [Terminal Support & Troubleshooting](docs/user-guide/21-terminal-support.md) — tmux/SSH truecolor, clipboard, mouse, diagnostics, `/doctor`
- [Hooks & Plugins Guide](docs/hooks-and-plugins.md) — managing hooks, plugins, and marketplace sources
- [Custom Hooks Guide](docs/custom-hooks.md) — creating, configuring, and writing your own hooks
- [Hook Examples](../../crates/echo-agent-hooks/examples/README.md) — sample hooks for common workflows
- [Hooks Crate (`echo-agent-hooks`)](../../crates/echo-agent-hooks/) — hook runtime, event types, and execution engine
- [Plugin Marketplace Crate (`echo-agent-plugin-marketplace`)](../../crates/echo-agent-plugin-marketplace/) — marketplace source loading, scanning, and install
