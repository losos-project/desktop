# The Agent

The Agent is derisk's window for building software and changing the desktop
with an AI agent. Each chat runs [Claude Code](https://claude.com/claude-code)
in the project's folder, and the window shows what it does as it goes. It is
`derisk-agent`, from `crates/derisk-agent` in derisk, and
`nixos/modules/agent.nix` installs it on the PC image.

## The window

The left column holds projects, chats, routines and artifacts. A project has
instructions, folders and MCP servers, like a Claude project. A routine is a
prompt that runs on a schedule as one of your own systemd timers. Artifacts are
the apps and files the agent made.

The right column shows live previews. Ask for an app and the agent writes a
Rust app that draws with egui, builds it, and runs it next to the chat. The
preview sandboxes it as WebAssembly (`wasm32-wasip2` with stdio and nothing
else), or runs it natively when the app needs files, network or processes.

Every chat gets two MCP servers. `derisk-agent mcp` has the previews,
`nix_build`, `pm_build` and the forge's pull requests. `derisk mcp` reads and
arranges the desktop. Built-in skills customize the desktop from a prompt,
write new skills, and delete unused online accounts. The Agent passes them to
Claude Code as a plugin with `--plugin-dir` on every turn, so they never land
in `~/.claude`, and your own skills work alongside them.

The command palette can send it requests. A typed request that matches nothing
offers "Ask the agent: …", which opens the Agent on a new chat with the request
filled in, or hands it to the open window. Nothing is sent until you press
**Send**. The palette opens `derisk-agent:ask?prompt=…` links, which the
Agent's desktop entry claims.

## Its own machine

Claude Code runs whatever it decides to, and in your session that would reach
everything you have. So the Agent runs in a machine of its own, one per person,
called `agent-<user>`, which has only these:

| What | How it gets there |
| ---- | ----------------- |
| Its window | waypipe over vsock to derisk's gate, like any [machine](hypervisor.md)'s window. derisk frames it in black. |
| The person's files | Only what they pick in the [portal machine](portal-machine.md)'s dialog, such as a project's folder. The machine gets a copy, and what the Agent saves goes back. Drives come through the [files machine](files-machine.md) and network shares through the shares machine, as they do for every machine. Nothing of the host's is shared with it. |
| The desktop's tools | The tools channel, which passes on only the requests `derisk mcp` offers. |
| Web links | The tools channel, which opens `http` and `https` links in the person's browser on the desktop. Files are never opened there. |
| The network | One card on libvirt's `default` network, behind the host's NAT, and so behind the network machine when there is one. |

It is a service machine: the host boots it from its own store, so it is part
of the image, updates with it, and doesn't show in the Machines app. Projects,
chats, routines and Claude Code's sign-in live on its one disk, its home, at
`/var/lib/losos-agent-machine/<uid>/home.qcow2`. The disk is LUKS-encrypted
with the key `~/.local/share/losos/agent-machine.key`, inside your encrypted
home, so it opens only while you are signed in. The host reads the key as you
and hands it to libvirt as an ephemeral secret, forgotten once QEMU has opened
the disk.

> [!WARNING]
> Deleting the key deletes the Agent's disk. The next open creates a new key
> and an empty disk, because the old one can no longer be read.

Like every NixOS service machine, it has Kicksecure's hardening
([Service machines](schizo-mode.md#service-machines)). No program can trace
another or read its memory, and io_uring is off. It keeps one SUID program,
PAM's `unix_chkpwd`, without which the user session doesn't start. Claude
Code's sandbox still works, because bubblewrap uses user namespaces and needs
no SUID. A debugger the Agent starts on a program it built is refused.

### Files

**Choose folder** opens the portal machine's dialog through the FileChooser
portal, like any app in a machine. The host's relay copies the folder in and
records it as picked for `agent-<user>`, and the Agent works on the copy. Each
file it saves is written back to your folder, the only place the relay lets it
write. The relay serves the machine only while its owner, named in its
`<pm:person>` metadata, is the person in front.

### Starting and stopping

The Agent's desktop entry runs `losos-agent-machine open`, which asks a root
service on the host, through `/run/losos-agent-machine/open.sock`, to start the
machine of whoever the kernel says connected. The service then passes the link,
such as the palette's "Ask the agent", to the machine's launch port, which
accepts it only from the host's root. The first open creates the disk and key.

The machine powers off after ten minutes with no Agent window, no routine
running and none scheduled. Routine timers live in the machine, so someone with
routines keeps it running, and the balloon returns freed memory to the host. It
also stops when you sign out, before homed locks your home.

### The tools channel

In the machine, `losos-agent-machine relay` serves derisk's agent socket,
`$XDG_RUNTIME_DIR/derisk/agent.sock`, where `derisk mcp` and the Agent expect
it. Each connection goes to the host over vsock, to a port numbered after the
machine's address. There, `losos-agent-machine tools` runs in your session and
accepts connections from your machine alone, by the kernel-stamped address. It
passes `state`, `dispatch`, `ask`, `tree`, `find`, `act`, `screenshot` and
`input` to derisk, which still asks you on screen before anything destructive.
It refuses menus, widgets, palette data and `choose`, which would let the
machine put things on the desktop that look like the system's own.

`src/losos-agent-machine` is both ends of all three channels.
`nixos/guest/agent.nix` is the machine and `nixos/modules/agent.nix` the
host's half.

## Where it ships

The Agent is on the PC image, the same image for desktops, laptops and tablets.
The [phone](halium.md) image leaves it out: its three columns need at least 900
logical pixels, which a screen under 600 on its short side can't fit.

Claude Code is not in the image, because nixpkgs marks it unfree. The Agent
runs `claude` from `PATH`, or the program `DERISK_AGENT_CLAUDE` names, and says
so in the chat when there is neither. Anthropic's installer, run in the Agent's
machine, puts it in `~/.local/bin`, which is on the machine's `PATH` and disk,
so it survives the machine stopping. Its sign-in page opens in your browser on
the desktop ([What is not done](not-done.md)).

## How it is built

`nixos/pkgs/derisk.nix` builds it as `derisk.agent`, a separate cargo build
from the same derisk source and `cargoHash`. Its previews link wasmtime with
WASI. derisk's own wasmtime, which runs palette plugins, is built without WASI
on purpose, and cargo would unify both into one feature set in a single build.

## Sonne, and why it left

The Agent came from Sonne, the project's fork of [Zed](https://zed.dev), which
the PC image used to ship as its code editor. Sonne added about half a gigabyte
to `/usr`, whose update slot is a fixed 8G, for work the Text Editor and the
Agent's code view already cover. Its agent window, skills, project setup and
previews moved into derisk, and Sonne left the image. derisk-gpui and mcsapi
took GPUI from Sonne's repository until it stopped being public. They now take
it from upstream Zed, at the commit Sonne was forked from.
