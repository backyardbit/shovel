---
name: shovel
description: Shovel macOS VMs — use when a task needs a throwaway Mac to install, run, click through, or test a macOS app or OS feature without touching the user's own machine.
---

# Shovel

`shovel` boots disposable macOS VMs cloned from a golden image. `shovel --help` and `shovel <command> --help` are the command reference; this file holds what `--help` does not say.

## Loop

1. `shovel --json up <name>` — boots and blocks until SSH answers (first boot ~1 min). Reuse a name to restart an existing VM.
2. Do the work: `exec` for anything scriptable, GUI commands only for what needs the screen.
3. `shovel down <name> --rm` when finished. The done state: `shovel ls` no longer lists your VM.

## The guest

- Full network access. Same username as the host, passwordless `sudo`.
- `~/workspace` in the guest is a private copy-on-write clone of the host's `~/workspace`, taken at `up`. Edits stay in the VM; bring results back with `shovel cp <name>:<path> <host-path>`.
- `shovel exec <name> -- '<shell>'` runs in a login zsh, so Homebrew and PATH behave as in Terminal. Its exit code is the command's.
- At most **2 macOS VMs run at once** (Apple limit, counting an image under setup). Stop yours before starting another.

## Seeing and driving the screen

- `shovel --json screenshot <name>` returns `{"path": ...}`; read that PNG to see the screen.
- `click`, `drag`, `scroll`, `move` take coordinates in the screenshot's pixel space (1440×900 by default).
- `type <name> '<text>'` types literally; `key <name> cmd+space return` presses combos in order.
- Take a fresh screenshot after each action you need to verify — the screen is the only ground truth for GUI state.

## Human takeover

The user can watch any VM (`shovel view`) and take control. While they hold it, GUI commands exit with **code 3** and `"human_control": true`. Then run `shovel wait-control <name>` and continue after it returns; `exec` keeps working throughout. Use `--force` only when the user asks.

When you need the user to look at or do something in the VM, run `shovel view <name>` and tell them.
