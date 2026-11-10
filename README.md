# Shovel

Disposable macOS VMs for agents. Agents spin up a Mac, test apps and OS features in it, and throw it away — without touching your machine. You can watch any VM live and take over at any time.

- **CLI first** — `shovel` (Rust) is the interface for agents and humans; every command supports `--json`.
- **Host app** — `Shovel.app` (Swift, menu bar) runs the VMs with Apple's Virtualization.framework and provides the live view / takeover window.
- **No guest agent** — commands run over SSH; screenshots and input go through the VZ framebuffer (VNC), so they work even before login.
- **Your files, safely** — each VM gets an APFS copy-on-write clone of `~/workspace` (seconds, near-zero disk), mounted at the same path inside the guest.

Requires Apple Silicon and macOS 14+. Apple limits each Mac to 2 concurrent macOS VMs.

## Install

With Homebrew:

```sh
brew install --cask backyardbit/tap/shovel   # Shovel.app -> /Applications, shovel on PATH
cp -R "$(brew --prefix)"/Caskroom/shovel/*/skills/shovel ~/.claude/skills/   # teach Claude Code to use it
```

Or download `Shovel-<version>.dmg` from [Releases](https://github.com/backyardbit/shovel/releases), drag `Shovel.app` to Applications, then:

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
shovel suspend t1                     # save state to disk, free the VM's RAM
shovel up t1                          # resume exactly where it was (--fresh: discard state, cold boot)
shovel down t1 --rm
```

### Suspend and resume

A running VM pins its full RAM (8 GB by default) on the host. `shovel suspend <name>` saves the VM's exact state (open apps, unsaved documents, running processes) to `~/.shovel/vms/<name>/state.vzvmsave` and stops it, which gives that RAM back; it takes about 4 s and writes a file roughly the size of the RAM the guest has touched (2.5–3 GB for a lightly used VM). `shovel up <name>` resumes it in about 4 s with the same IP, and any command that needs a running VM (`exec`, `ssh`, `cp`, `screenshot`, `click`/`type`/`key`/..., `record`, `view`, `password type`, every MCP tool) resumes it automatically first. `shovel ls` and `status` show `suspended` and the state file size.

The saved state belongs to the disk as it was at suspend time, so it is consumed on resume and never kept beside a running VM. `shovel up --fresh` discards it and cold boots; `shovel down` on a suspended VM discards it too (the VM becomes stopped, as if it had shut down); `down --rm` and `rm` delete everything. If a resume fails (for example after the VM's CPU or memory settings changed), the state file is kept and the error suggests `--fresh`.

`shovel up <name> --idle-suspend <minutes>` (saved in the VM's config; `0` turns it off) suspends the VM after it has been idle that long: no Shovel commands for it, no SSH session or VNC connection from the host (so a long `exec` or a recording keeps it awake), its viewer closed and not under human control, and the VM process below a quarter of a CPU core (an idle macOS guest uses about 5%). Quitting Shovel (`shovel host stop` or the menu) suspends running VMs instead of killing them.

macOS encrypts saved state with a Secure Enclave key it only creates while the Mac is unlocked, so suspending fails while the screen is locked; the VM keeps running and the error says so.

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

Tools: `screenshot`, `zoom`, `left_click`, `right_click`, `middle_click`, `double_click`, `triple_click`, `mouse_move`, `left_click_drag`, `left_mouse_down`, `left_mouse_up`, `scroll`, `type`, `key`, `hold_key`, `cursor_position`, `wait`, `exec`, `open_application`, `type_password`, `wait_for_control`, `vm_view`, `vm_list`, `vm_up`, `vm_suspend`, `vm_down`. Screenshots come back inline as PNG images of at most 1280x800 (`--max-width`/`--max-height`), and the coordinates the agent reads off a screenshot are the ones it clicks; the server maps them to the VM display. `vm_down` only deletes VMs that the same server created. `vm_suspend` frees a VM's RAM; the next tool call that needs it resumes it and says so in its output, and `vm_up` takes `idle_suspend_minutes`.

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

## Building Shovel with Shovel

```sh
scripts/selfbuild.sh            # build + test the committed sources in a clean VM
scripts/selfbuild.sh --ref main --keep
```

The VM compiles the CLI, runs the tests, bundles and signs Shovel.app, and smoke-tests the video encoder. It cannot boot VMs itself: Apple Silicon doesn't support nested virtualization for macOS guests.

## Layout

```
cli/    Rust CLI: storage, cloning, SSH, VNC client
host/   Swift host app: VM lifecycle, macOS install, VNC server, viewer
skills/ Claude Code skill for agents
```

State lives in `~/.shovel` (`images/`, `vms/`, `ipsw/`, `keys/`, `host.sock`); override with `SHOVEL_HOME`.

## License

MIT. See [LICENSE](LICENSE).
