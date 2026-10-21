# The terminal

The desktop's terminal is derisk's own, `derisk-terminal`, from
`crates/derisk-terminal` in derisk, and the image ships no other. It replaced
foot, which the image carried only because derisk had no terminal. The
[virtual machine](hypervisor.md) template's guest ships it too. Super+Enter
opens one, as in i3 and sway. "Move to main pane", which had that key before,
is now Super+Shift+Enter. Settings files saved before the change move over to
the new keys if they still hold the old ones.

![derisk-terminal in a derisk session, showing Nerd Font icons, a powerline prompt, box drawing, the 256 colours and text styles](images/terminal.png)

- **Nerd Font icons.** It draws the theme's monospace font from that font's
  Nerd Font build when one is installed. Glyphs a font lacks come from
  Symbols Nerd Font, then from the usual monospace fonts. The image installs
  Hack Nerd Font, since Hack is the theme's default, and the symbols-only
  Nerd Font, which supplies the icons when a theme names a font with no Nerd
  Font build. `nixos/modules/desktop.nix` installs both. An icon followed by
  a blank cell may span both cells, which is how Nerd Font icons are
  designed. Box drawing and block characters are drawn to fit the cell
  instead of taken from the font, so lines meet across cells in any font.
- **On the GPU, or in software.** It draws each frame on the GPU through
  OpenGL ES 3, or OpenGL 3.3, in one draw call, from an atlas of the glyphs it
  has drawn before. Where there is no GPU, or where OpenGL itself runs in
  software through llvmpipe, it draws the same frame on the CPU into shared
  memory instead, which is faster than llvmpipe. `--renderer gpu` or
  `--renderer software` picks one by hand.
- **Deferred.** What the shell prints only changes the grid. A frame is built
  from the grid when the compositor asks for one, so a command that prints a
  lot draws once per frame, not once per line.
- **The desktop's theme.** Colours and the font follow derisk's theme, and a
  change in Settings reaches a running terminal the next time it is focused.

It takes foot's command line, meaning a bare command or `-e`,
`-a`/`--app-id`, `-T`/`--title` and `-D`. So a desktop entry written for foot
runs in it with only the program's name changed.

`checks.x86_64-linux.terminal`, in `nixos/tests/terminal.nix`, starts it
under cage in a VM with no GPU, where it has to choose software. It starts it
again with `--renderer gpu`, where it has to draw through llvmpipe, and reads
its text off the screen both times. The image's own boot test checks that the
terminal, Hack Nerd Font and the symbols font are installed and foot is
not.

## The tools in the shell

The commands a person types are the Rust versions wherever a Rust version
takes the GNU tool's options, on the host and in the guest template alike.
`nixos/modules/userland.nix` sets this up:

| Command | From |
|---|---|
| `ls`, `cp`, `cat` and the rest of coreutils | [uutils coreutils](https://github.com/uutils/coreutils) |
| `find`, `xargs`, `locate`, `updatedb` | [uutils findutils](https://github.com/uutils/findutils) |
| `gpg`, `gpgv`, `gpgconf` | [Sequoia's GnuPG chameleon](https://gitlab.com/sequoia-pgp/sequoia-chameleon-gnupg), with `sq` beside it |

The chameleon reads and writes GnuPG's own `~/.gnupg`, so git's commit
signing and anything else that runs `gpg` keep working, and secret keys stay
with GnuPG's `gpg-agent`. The agent asks for a passphrase through
`pinentry-gnome3`, which is gcr's dialog on the desktop and curses in a
terminal with no display. Unlike GnuPG, the chameleon will not create
`~/.gnupg` itself, so each user manager makes it when it starts. Only `chcon`
and `runcon` still come from GNU coreutils, because uutils leaves out those
two, which only mean something under SELinux.

Some tools are still GNU, because the Rust version does not take the same
options yet:

- `diff`, since uutils' has no `-r`, `diff3` or `sdiff`;
- `sed`, since uutils' ignores `-z`, `\U` and `\b`, and gets `sed 'n;d'`
  wrong;
- `ps` and the rest of procps, since uutils' rejects `ps aux`;
- util-linux and tar, where uutils has only work in progress;
- grep, awk, less and gzip, which have no Rust version that takes their
  options.

Services and NixOS's own scripts run GNU throughout. NixOS sets their `PATH`,
not the profile, and they were written against GNU. So does `systemd-pull`'s
check of an update's signature, which passes gpg
`--import-options=merge-only`, an option the chameleon refuses.

> [!IMPORTANT]
> There is no `sudo`, `su` or `run0`, and nothing that runs a command as
> root: `pm system` manages units and updates ([pm on LosOS](pm.md#administering-the-machine)).
> The guest template keeps sudo-rs's `su`, since a guest has no pm service.

The image's boot test, `nixos/tests/boot.nix`, checks which of these
answers `ls`, `find` and `gpg`. It makes a key with the chameleon, signs a
file with it and verifies the signature. It verifies after signing instead of
piping one gpg into another, because the chameleon's `--verify` reports a BAD
signature when it reads one that a second gpg is still writing, even though
the same bytes check out once written.
