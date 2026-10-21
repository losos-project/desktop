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

## Where it ships

The Agent is on the PC image, which is the same image for desktops, laptops
and tablets. The Halium image for [phones](halium.md) leaves it out. Its
window is three columns at least 900 logical pixels wide, which does not fit
a screen under 600 on its short side.

Claude Code itself is not in the image, because nixpkgs marks it unfree. The
Agent runs `claude` from `PATH`, or the program `DERISK_AGENT_CLAUDE` names.
When there is neither, it says so in the chat. [What is not done](not-done.md)
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
