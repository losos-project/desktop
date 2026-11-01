# The Agent

The Agent is derisk's window for building software and changing the
desktop with an agent. It is `derisk-agent`, from `crates/derisk-agent` in
derisk, and `nixos/modules/agent.nix` installs it on the PC image. Each chat
runs [Claude Code](https://claude.com/claude-code) in the project's folder,
and the window shows what it does as it goes.

- **Projects, chats, routines and artifacts** are in the left column. A
  project holds instructions, folders and MCP servers, like a Claude
  project. A routine is a prompt that runs on a schedule, as a systemd
  timer of the person's own. Artifacts are the apps and files the agent
  made.
- **Live previews** are on the right. Asked for an app, the agent writes a
  Rust app that draws with egui and builds it, and the preview runs it
  beside the chat. The preview sandboxes it as WebAssembly, `wasm32-wasip2`
  with stdio and nothing else, or runs it natively when the app needs files,
  network or processes.
- **The desktop's own tools.** Every chat gets two MCP servers. The Agent's
  own, `derisk-agent mcp`, has the previews, `nix_build`, `pm_build` and the
  forge's pull requests. derisk's, `derisk mcp`, reads and arranges the
  desktop.
- **Built-in skills** customize the desktop from a prompt, write new skills,
  and delete unused online accounts. The Agent hands them to Claude Code as a
  plugin with `--plugin-dir` on every turn, so they never land in
  `~/.claude`, and a person's own skills work beside them.

derisk's command palette can send it requests. A typed request that matches
nothing offers "Ask the agent: …". That opens the Agent on a new chat with
the request typed in, or hands it to the window already open. Nothing is
sent until the person presses Send. The palette opens
`derisk-agent:ask?prompt=…` links, which the Agent's desktop entry claims.

## Its own machine

The Agent runs in a machine of its own, one for each person, called
`agent-<user>`. Claude Code runs whatever it decides to, and in the
person's session that would be everything they have. In the machine it has
only these:

| What | How it gets there |
| ---- | ----------------- |
| Its window | waypipe over vsock to derisk's gate, like any [machine](hypervisor.md)'s window. derisk frames it in black. |
| The person's files | Only what they pick in the [portal machine](portal-machine.md)'s dialog, such as a project's folder. The machine gets a copy, and what the Agent saves goes back. Drives come through the [files machine](files-machine.md) and network shares through the shares machine, as they do for every machine. Nothing of the host's is shared with it. |
| The desktop's tools | The tools channel, which passes on only the requests `derisk mcp` offers. |
| Web links | The tools channel, which opens `http` and `https` links in the person's browser on the desktop. Files are never opened there. |
| The network | One card on libvirt's `default` network, behind the host's NAT, and so behind the network machine when there is one. |

The machine is a service machine. The host boots it from its own store, so
it is part of the image and an update brings a new one. The Machines app
does not list it. Its projects, chats, routines and Claude Code's sign-in
live on its one disk, `/var/lib/losos-agent-machine/<uid>/home.qcow2`,
which is its home. The disk is encrypted with LUKS. Its key is
`~/.local/share/losos/agent-machine.key`, inside the person's encrypted
home, so the disk opens only while they are signed in. The host's root end
reads the key as the person and hands it to libvirt as an ephemeral
secret, which it forgets once QEMU has opened the disk.

Like every service machine that runs NixOS, it has Kicksecure's hardening
([schizo mode](schizo-mode.md) says what that is). No program in it can
trace another or read its memory, and io_uring is off. It keeps one SUID
program, PAM's `unix_chkpwd`, which Kicksecure keeps too: without it the
machine's user session does not start. Claude Code's sandbox still works,
because bubblewrap runs in user namespaces and needs no SUID. A debugger
the Agent starts on a program it built is refused.

> [!WARNING]
> Deleting the key deletes the Agent's disk with it. The next open makes a
> new key and an empty disk, because the old one can no longer be read.

**Its files.** The Agent's "Choose folder" opens the portal machine's
dialog through the FileChooser portal, like any app in a machine. The
host's relay copies the folder in and records it as picked for
`agent-<user>`, and the Agent works on the copy. Each file it saves is
written back to the person's folder, which is the only place the relay
lets it write. The relay serves the machine only while the person it
belongs to, named in its `<pm:person>` metadata, is the one in front.

**It starts when it is needed.** The Agent's desktop entry runs
`losos-agent-machine open`. That asks a root service on the host, through
`/run/losos-agent-machine/open.sock`, to start the machine of whoever the
kernel says connected. The service then passes the link, such as the
palette's "Ask the agent", to the machine's launch port. The machine
accepts it only from the host's root. The first open makes the disk and
its key.

**It stops when it is not.** The machine powers itself off after ten
minutes with no Agent window, no routine running and no routine
scheduled. A routine's timer is the machine's own, so someone with
routines keeps it running, and the balloon hands the memory it frees back
to the host. The machine also stops when the person signs out, before
homed locks their home again.

**The tools channel.** In the machine, `losos-agent-machine relay` serves
derisk's agent socket, `$XDG_RUNTIME_DIR/derisk/agent.sock`, where
`derisk mcp` and the Agent look for it. Each connection goes to the host
over vsock, to a port that is the machine's own address. There,
`losos-agent-machine tools` runs in the person's session. It takes
connections from their machine alone, by the address the kernel stamps on
them. It passes `state`, `dispatch`, `ask`, `tree`, `find`, `act`,
`screenshot` and `input` to derisk, which still has the person confirm
anything destructive on screen. It refuses menus, widgets, palette data
and `choose`, which would let the machine put things on the desktop that
look like the system's own.

`src/losos-agent-machine` is both ends of all three channels.
`nixos/guest/agent.nix` is the machine and `nixos/modules/agent.nix` the
host's half.

## Where it ships

The Agent is on the PC image, which is the same image for desktops, laptops
and tablets. The Halium image for [phones](halium.md) leaves it out. Its
window is three columns at least 900 logical pixels wide, which does not fit
a screen under 600 on its short side.

Claude Code itself is not in the image, because nixpkgs marks it unfree. The
Agent runs `claude` from `PATH`, or the program `DERISK_AGENT_CLAUDE` names.
When there is neither, it says so in the chat. Anthropic's installer, run in
the Agent's machine, puts it in `~/.local/bin`, which is on the machine's
`PATH` and on the machine's disk, so it stays when the machine stops. Its
sign-in page opens in the person's browser on the desktop. [What is not done](not-done.md)
lists this.

## How it is built

`nixos/pkgs/derisk.nix` builds it as `derisk.agent`, a separate cargo build
from the same `components/derisk` checkout and `cargoHash` as derisk. Its
previews link wasmtime with WASI. derisk's own wasmtime, which runs the
palette's plugins, is built without WASI on purpose, and cargo would give
both the same features in one build.

## Sonne, and why it left

The Agent came from Sonne, the project's fork of [Zed](https://zed.dev). The
PC image shipped Sonne as its code editor and opened text and source files in
it. Sonne added about half a gigabyte to `/usr`, whose update slot is a fixed
8G, for an editor whose work the Text Editor and the Agent's own code view
already cover. Its agent window, skills, project setup and previews moved
into derisk, and Sonne left the image for good. derisk-gpui and mcsapi took
GPUI from Sonne's repository too, until that repository stopped being
public. They now take it from upstream Zed, at the commit Sonne was forked
from.
