# claude-tabs

A tab manager for the [Claude Code CLI](https://claude.com/claude-code). Each tab is its own Claude conversation, and your tabs come back when you rerun the program.

## Shortcuts

| Keys | Action |
| --- | --- |
| Ctrl+T | New tab |
| Ctrl+W | Close tab |
| Ctrl+Shift+T | Reopen last closed tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Cycle tabs in most-recently-used order |
| Ctrl+PgDn / Ctrl+PgUp | Next / previous tab |
| Ctrl+1 – Ctrl+8 | Go to tab 1–8 |
| Ctrl+9 | Go to last tab |

## Requirements

- `claude` on your `PATH`.
- A terminal that supports the kitty keyboard protocol (

## Install

On Linux and macOS, run:

```sh
curl -fsSL https://raw.githubusercontent.com/donnybeelo/claude-tabs/main/install.sh | sh
```

On Windows, run this in PowerShell:

```powershell
irm https://raw.githubusercontent.com/donnybeelo/claude-tabs/main/install.ps1 | iex
```

Both install `claude-tabs` for your user, and update it when ran again.

## How it works

- Each tab runs Claude inside an embedded terminal emulator (`alacritty_terminal`). The tab bar is drawn on the top row, and only cells that changed are redrawn.
- Saved windows live in `~/.local/state/claude-tabs/` (`%LOCALAPPDATA%\claude-tabs\` on Windows), one file per window.
- To follow `/resume` and `/clear`, and to know when Claude is busy, it reads files Claude Code writes under `~/.claude/sessions/` and `~/.claude/projects/`. These aren't a documented interface, so a Claude Code update could change them.
- Programs running inside a tab can open a new tab:
  - on Linux and macOS, by sending `SIGUSR1` to the process ID in `$CLAUDE_TABS_PID`;
  - on Windows, by connecting to the named pipe `\\.\pipe\claude-tabs-<pid>`, using that same process ID.
