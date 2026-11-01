# Focus mode

Focus mode is for games and for heavy creative work: 3D, CAD, video and
music. While a program like that is open, the background work the desktop
does waits. File indexing, backups, updates, the ad-block list fetch and
the like stop using the processor and the disk. Ollama, the language model
behind Ask derisk, stops and gives its memory back. When the last of those
programs closes, everything carries on where it stopped.

`nixos/modules/focus.nix` sets it up, and `losos-focus`
(`src/losos-focus`) does the work.

## What turns it on

[GameMode](https://github.com/FeralInteractive/gamemode) is the switch.
A program asks GameMode for focus while it runs, and GameMode turns focus
mode on for the first program and off when the last one exits. A program
asks in one of three ways:

- derisk asks for every window whose program's desktop entry is in the
  category `Game`, `3DGraphics`, `Engineering`, `AudioVideoEditing`,
  `Sequencer` or `Midi`. That covers games, Blender, FreeCAD, Kdenlive,
  Ardour and LMMS, for example.
- Many games ask by themselves. In Steam, set a game's launch options to
  `gamemoderun %command%`. Lutris and Heroic have a GameMode switch.
- Anything else runs under `gamemoderun`:

  ```console
  $ gamemoderun krita
  ```

`losos-focus start` and `losos-focus stop` turn it on and off by hand, and
`losos-focus status` says whether it is on.

While GameMode is on it also sets the processor to its fastest governor,
as it does on any Linux system. The screen still blanks and locks when
nobody touches the computer, unless the program itself asks it not to, as
video players and most games do.

## What waits

| Waits | How |
| --- | --- |
| Your file index, backups, the LosOS boxes browser, the active-user count, Danube's captive portal watch, the Mac downloads watch | frozen in your `background.slice` |
| Updates, the ad-block lists, firmware metadata, log rotation, SSD trimming, Podman's cleanup | frozen in the system's `losos-background.slice` |
| The timers that start that work | stopped, and started again afterwards |
| Ollama | stopped, and started again afterwards |

Frozen work is held at the instruction it had reached, by the kernel's
cgroup freezer. It keeps its memory and resumes as if nothing happened.
An update download frozen for long enough that the server hangs up is
cleared away and tried again at the next update.

systemd refuses to start anything in a frozen slice, so the timers stop
too, rather than fire and fail. Once started again, the update, backup
and ad-block timers run straight away if their time came meanwhile.
Starting one of those services by hand while focus mode is on fails
with "Cannot start frozen unit".

The desktop itself does not wait, since it draws the program's window.
It already costs next to nothing while nothing else on the screen
changes: mcsapi's compositor redraws only what changed.

## Logging out

If you log out while focus mode is on, it turns off. GameMode goes with
your session, so the system unit that holds focus,
`losos-focus@<you>.service`, also watches for your session to end. Updates
never stay frozen until the next boot because of a session that closed.

With two people signed in, the system's work waits while either of them
is in focus mode.

## Who may turn it on

polkit lets the person signed in at the computer start and stop their own
`losos-focus@<name>.service` and no other unit. pm's rule refuses every
other systemd action to anyone but pm. GameMode's helpers that set the
processor governor and the split-lock slowdown are allowed to the same
person. Its GPU overclocking helper is not.
