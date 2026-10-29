# Shovel

Disposable macOS VMs for agents. Agents spin up a Mac, test apps and OS features in it, and throw it away — without touching your machine. You can watch any VM live and take over at any time.

- **CLI first** — `shovel` (Rust) is the interface for agents and humans; every command supports `--json`.
- **Host app** — `Shovel.app` (Swift, menu bar) runs the VMs with Apple's Virtualization.framework and provides the live view / takeover window.
- **No guest agent** — commands run over SSH; screenshots and input go through the VZ framebuffer (VNC), so they work even before login.
- **Your files, safely** — each VM gets an APFS copy-on-write clone of `~/workspace` (seconds, near-zero disk), mounted at the same path inside the guest.

Requires Apple Silicon and macOS 14+. Apple limits each Mac to 2 concurrent macOS VMs.

## Install

```sh
make install        # builds Shovel.app -> ~/Applications, shovel -> ~/.cargo/bin
cp -R skills/shovel ~/.claude/skills/   # teach Claude Code to use it
```

## One-time: build the golden image

```sh
shovel image create          # downloads the latest macOS IPSW (~17 GB) and installs it
shovel image setup           # boots it in a window; follow the printed checklist
shovel image stop
```

The checklist: create an admin account with **your Mac username**, run `bash "/Volumes/My Shared Files/shovel/setup.sh"` in the guest Terminal (installs the SSH key, passwordless sudo, Remote Login, no sleep, `~/workspace` link), enable automatic login, shut down.

## Use

```sh
shovel up t1                          # clone image + workspace, boot, wait for SSH
shovel exec t1 -- 'cd ~/workspace/myapp && xcodebuild'
shovel cp ./MyApp.app t1:/Applications/
shovel screenshot t1                  # prints the PNG path
shovel click t1 720 450
shovel type t1 'hello'
shovel key t1 cmd+space
shovel view t1                        # live window; "Take Over" pauses agent input
shovel takeover t1 / shovel release t1
shovel down t1 --rm
```

Workspace modes for `up`: `--share-mode clone` (default, private CoW snapshot), `rw` / `ro` (live mount of the host directory), `none`. Use `--share <dir>` to expose a different directory.

While a human has control, GUI commands exit with code 3; agents wait with `shovel wait-control <name>`.

## Layout

```
cli/    Rust CLI: storage, cloning, SSH, VNC client
host/   Swift host app: VM lifecycle, macOS install, VNC server, viewer
skills/ Claude Code skill for agents
```

State lives in `~/.shovel` (`images/`, `vms/`, `ipsw/`, `keys/`, `host.sock`); override with `SHOVEL_HOME`.
