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

## Demo recordings for PRs

When you build a user-visible feature, prove it with a recording on the PR:

1. Get the feature into the VM (`exec` a build in `~/workspace/<repo>`, or `cp` the built app in) and launch it.
2. `shovel record start <name>`, then drive the feature with `click`/`type`/`key`, pausing ~1s between steps so viewers can follow. Clicks are drawn on the video.
3. `shovel --json record stop <name>` returns `mp4` and `gif` paths. Pull one frame (or screenshot) to check the recording shows what you intended before posting.
4. `shovel pr-attach <gif> <mp4> --pr <number|url> -m "<what the recording shows>"` posts a PR comment with the GIF inline and the MP4 linked. Files go to the repo's `shovel-media` branch, never the PR branch.

Keep recordings short (under ~60s); GitHub's upload limit is ~95 MB per file.

## Human takeover

The user can watch any VM (`shovel view`) and take control. While they hold it, GUI commands exit with **code 3** and `"human_control": true`. Then run `shovel wait-control <name>` and continue after it returns; `exec` keeps working throughout. Use `--force` only when the user asks.

When you need the user to look at or do something in the VM, run `shovel view <name>` and tell them.
