# Shovel

Disposable macOS VMs for agents. Agents spin up a Mac, test apps and OS features in it, and throw it away — without touching your machine. You can watch any VM live and take over at any time.

- **CLI first** — `shovel` (Rust) is the interface for agents and humans; every command supports `--json`.
- **Host app** — `Shovel.app` (Swift, menu bar) runs the VMs with Apple's Virtualization.framework and provides the live view / takeover window.
- **No guest agent** — commands run over SSH; screenshots and input go through the VZ framebuffer (VNC), so they work even before login.
- **Your files, safely** — each VM gets an APFS copy-on-write clone of `~/workspace` (seconds, near-zero disk), mounted at the same path inside the guest.

Requires Apple Silicon and macOS 14+. Apple limits each Mac to 2 concurrent macOS VMs.

## Install

Download `Shovel-<version>.dmg` from [Releases](https://github.com/backyardbit/shovel/releases), drag `Shovel.app` to Applications, then:

```sh
xattr -dr com.apple.quarantine /Applications/Shovel.app   # the app is ad-hoc signed, not notarized
sudo mkdir -p /usr/local/bin && sudo ln -sf /Applications/Shovel.app/Contents/Helpers/shovel /usr/local/bin/shovel
cp -R /Volumes/Shovel*/skills/shovel ~/.claude/skills/    # teach Claude Code to use it
```

From source:

```sh
make install        # builds Shovel.app -> ~/Applications, shovel -> ~/.cargo/bin
make dmg            # builds build/Shovel-<version>.dmg
cp -R skills/shovel ~/.claude/skills/   # teach Claude Code to use it
```

## One-time: build the golden image

```sh
shovel image create          # downloads the latest macOS IPSW (~17 GB) and installs it
shovel image setup           # boots it in a window; follow the printed checklist
shovel password set          # save the guest account password in your login keychain
shovel image stop
```

The checklist: create an admin account with **your Mac username** (skip Apple Account and FileVault), then run `bash "/Volumes/My Shared Files/shovel/setup.sh"` in the guest Terminal. It installs the SSH key, passwordless sudo, Remote Login, automatic login, Xcode Command Line Tools, your time zone, and the `~/workspace` link, and turns off sleep and auto-updates.

## Use

```sh
shovel up t1                          # clone image + workspace, boot, wait for SSH
shovel exec t1 -- 'cd ~/workspace/myapp && xcodebuild'
shovel cp ./MyApp.app t1:/Applications/
shovel screenshot t1                  # prints the PNG path
shovel click t1 720 450
shovel type t1 'hello'
shovel key t1 cmd+space
shovel password type t1 --enter       # types the guest password into the focused field
shovel view t1                        # live window; "Take Over" pauses agent input
shovel takeover t1 / shovel release t1
shovel down t1 --rm
```

## Computer use over MCP (any agent)

`shovel mcp` is a stdio [MCP](https://modelcontextprotocol.io) server, so any MCP client (Claude Code, Codex, Cursor, Claude Desktop, or your own agent on an MCP SDK) sees the VM as a screen it can screenshot, click and type into, with the same kind of tools it uses for computer use on a real desktop. Bind a VM with `--vm` or `SHOVEL_VM`, or let the agent call `vm_up` to create one.

Claude Code:

```sh
claude mcp add shovel -- shovel mcp                    # agent creates a VM with vm_up
claude mcp add shovel -e SHOVEL_VM=t1 -- shovel mcp    # bound to VM t1
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.shovel]
command = "shovel"
args = ["mcp"]
tool_timeout_sec = 300
default_tools_approval_mode = "approve"   # otherwise every click needs approval (and fails in `codex exec`)
```

Claude Desktop, Cursor (`.cursor/mcp.json`) and SDK clients use the usual `mcpServers` block:

```json
{"mcpServers": {"shovel": {"command": "/Users/you/.cargo/bin/shovel", "args": ["mcp"], "env": {"SHOVEL_VM": "t1"}}}}
```

GUI apps don't inherit your shell `PATH`, so use the absolute path from `which shovel`.

Tools: `screenshot`, `zoom`, `left_click`, `right_click`, `middle_click`, `double_click`, `triple_click`, `mouse_move`, `left_click_drag`, `left_mouse_down`, `left_mouse_up`, `scroll`, `type`, `key`, `hold_key`, `cursor_position`, `wait`, `exec`, `open_application`, `type_password`, `wait_for_control`, `vm_view`, `vm_list`, `vm_up`, `vm_down`. Screenshots come back inline as PNG images of at most 1280x800 (`--max-width`/`--max-height`), and the coordinates the agent reads off a screenshot are the ones it clicks; the server maps them to the VM display. `vm_down` only deletes VMs that the same server created.

Codex: some Codex builds list MCP tools through a tool search instead of the model's tool list, so a model can miss them. Naming them in the prompt ("using the shovel MCP tools, ...") makes it look them up.

Known limits: the pointer is not drawn in screenshots, and non-ASCII text is typed by pasting it through the VM clipboard (so `type` overwrites the guest clipboard when the text has characters like é or ✓).

## Recording demos for PRs

```sh
shovel record start t1                # background recorder; agent clicks are highlighted
shovel click t1 400 300 ...           # drive the feature
shovel record stop t1                 # prints .mp4 and .gif paths
shovel pr-attach demo.gif demo.mp4 --pr 42 -m "New onboarding flow"
```

Encoding uses AVFoundation inside Shovel.app (no ffmpeg). GitHub has no API for PR comment attachments, so `pr-attach` commits the files to an orphan `shovel-media` branch via `gh` and posts a comment embedding the GIF and linking the MP4.

Workspace modes for `up`: `--share-mode clone` (default, private CoW snapshot), `rw` / `ro` (live mount of the host directory), `none`. Use `--share <dir>` to expose a different directory.

While a human has control, GUI commands exit with code 3; agents wait with `shovel wait-control <name>`.

## Layout

```
cli/    Rust CLI: storage, cloning, SSH, VNC client
host/   Swift host app: VM lifecycle, macOS install, VNC server, viewer
skills/ Claude Code skill for agents
```

State lives in `~/.shovel` (`images/`, `vms/`, `ipsw/`, `keys/`, `host.sock`); override with `SHOVEL_HOME`.

## License

MIT. See [LICENSE](LICENSE).
