# Ask derisk

derisk's command palette, on Super+Space, takes requests in plain words as
well as searches. Its own rules handle short ones such as "open files and
snap it left, then go to workspace 2", at once and offline. Anything else
goes to a small language model running on the computer. The model carries
the request out through the desktop's tools and answers in a sentence. It
handles requests like "put Firefox on the left and Files on the right",
"switch to the light theme" and "what do I have open on workspace 2?".

The palette's row says which will answer: "1 step" for the rules, "Press
Enter to ask qwen3:4b-instruct" for the model. The request then runs in the
palette's conversation, which typing `?` opens directly. It shows a line for
each step the model takes and a Stop button while it works.

## The model

LosOS runs [Ollama](https://ollama.com) as a system service, set up in
`nixos/modules/assistant.nix` and listening on `127.0.0.1:11434` only. The
image carries no model. The first request that needs one downloads it into
`/var/lib/ollama` on the root partition and shows the progress in the
conversation. After that it works without a network. On a PC, Ollama's Vulkan
build runs it on the GPU when there is one and on the CPU otherwise.

The default is Qwen3 4B Instruct, `qwen3:4b-instruct`, 2.5 GB and Apache-2.0.
Ollama's small models were compared on the same requests, and this one used
the desktop's tools best. It looks up a window's ID before snapping it, and it
finishes requests that take several steps. `granite4:micro`, at 2.1 GB, is
smaller and faster but made up window IDs. `qwen3.5:2b`, at 2.7 GB, repeated
steps it had already done.

## Choosing another model or server

Settings › Assistant has the server's address and the model's name. Any
Ollama-compatible server works, on this computer or another one, and any
model on it that supports tool calling. A server elsewhere sees every
request and whatever the model reads from the screen to carry it out, and
the page warns about that. The same page turns the model off, and then the
palette answers only what its rules cover. In `settings.conf` these are
`assistant.use_model`, `assistant.url` and `assistant.model`.

## What it may do

The model's tools come from the agent protocol in [derisk's
`ipc.rs`](https://github.com/losos-project/derisk/blob/main/crates/derisk/src/ipc.rs),
cut down for a small model. Each action is one flat tool, and results come
back as short text. The model runs them over an agent connection, so it can do
nothing an agent over `derisk mcp` couldn't. It can:

- read the open windows, the installed apps and the settings, and find
  elements on screen by role and name;
- open apps, web pages and web searches, and focus, snap, minimize, close
  and move windows between workspaces;
- with the person's say-so, click and type in apps, press keys, change a
  setting, open a file, and lock, suspend or end the session.

The steps in the last group wait in the palette with Allow and Don't
allow, and Enter allows. Only real input answers them. A click an agent
injected does not count, so the model cannot approve itself. Logging out,
rebooting and powering off also ask on screen afterwards, as they do for
every agent. The model cannot change the `assistant.*` settings that say
where requests go, and it cannot rebind shortcuts. One request stops after
12 tool calls.

What apps and web pages put on screen reaches the model as data. Its
instructions say never to follow it, and the steps that could act on it
ask first.

## Files

| Path | What |
| --- | --- |
| `~/.config/derisk/settings.conf` | `assistant.use_model`, `assistant.url`, `assistant.model` |
| `/var/lib/ollama/models` | Downloaded models |
| `ollama.service` | The server |
