# The terminal

The terminal is derisk's own `derisk-terminal`, from `crates/derisk-terminal` in
derisk, and the image ships no other. It replaced foot, which the image carried
only because derisk had no terminal. The [virtual machine](hypervisor.md) guest
template ships it too.

Press Super+Enter to open one, as in i3 and sway. "Move to main pane", which
used that key, is now Super+Shift+Enter, and older settings files that still
hold the old keys move over automatically.

![derisk-terminal in a derisk session, showing Nerd Font icons, a powerline prompt, box drawing, the 256 colours and text styles](images/terminal.png)

## Fonts and icons

The terminal draws the theme's monospace font from its Nerd Font build when one
is installed. Missing glyphs come from Symbols Nerd Font, then the usual
monospace fonts. `nixos/modules/desktop.nix` installs Hack Nerd Font, because
Hack is the theme's default, and the symbols-only Nerd Font for themes whose
font has no Nerd Font build. An icon followed by a blank cell may span both
cells, as Nerd Font icons are designed to. Box drawing and block characters are
drawn to fit the cell rather than taken from the font, so lines meet in any
font.

## Rendering

The terminal draws each frame on the GPU through OpenGL ES 3 or OpenGL 3.3, in
one draw call, from an atlas of glyphs it has already drawn. Without a GPU, or
when OpenGL itself is llvmpipe, it draws the same frame on the CPU into shared
memory, which is faster than llvmpipe. `--renderer gpu` or
`--renderer software` forces one.

Output only changes the grid. A frame is built when the compositor asks for
one, so a command that prints a lot draws once per frame, not once per line.
Colours and font follow derisk's theme, and a change in Settings reaches a
running terminal the next time it gets focus.

It accepts foot's command line: a bare command or `-e`, `-a`/`--app-id`,
`-T`/`--title` and `-D`. A desktop entry written for foot works with only the
program name changed.

`checks.x86_64-linux.terminal` (`nixos/tests/terminal.nix`) starts it under
cage in a VM with no GPU, where it must pick software, then again with
`--renderer gpu`, where it must draw through llvmpipe, and reads its text off
the screen both times. The boot test checks that the terminal, Hack Nerd Font
and the symbols font are installed and foot is not.

## The tools in the shell

Commands you type are the Rust versions wherever the Rust version takes the GNU
tool's options, on the host and in the guest template.
`nixos/modules/userland.nix` sets this up:

| Command | From |
|---|---|
| `ls`, `cp`, `cat` and the rest of coreutils | [uutils coreutils](https://github.com/uutils/coreutils) |
| `find`, `xargs`, `locate`, `updatedb` | [uutils findutils](https://github.com/uutils/findutils) |
| `gpg`, `gpgv`, `gpgconf` | [Sequoia's GnuPG chameleon](https://gitlab.com/sequoia-pgp/sequoia-chameleon-gnupg), with `sq` beside it |

The chameleon reads and writes GnuPG's `~/.gnupg`, so git commit signing and
anything else that runs `gpg` keep working, and secret keys stay with GnuPG's
`gpg-agent`. The agent asks for passphrases through `pinentry-gnome3`: gcr's
dialog on the desktop, curses in a terminal without a display. The chameleon
won't create `~/.gnupg` itself, so each user manager creates it at start.
`chcon` and `runcon` still come from GNU coreutils, because uutils leaves out
those SELinux-only tools.

These stay GNU because the Rust versions don't take the same options yet:

| Tool | Why |
|---|---|
| `diff` | uutils' has no `-r`, `diff3` or `sdiff` |
| `sed` | uutils' ignores `-z`, `\U` and `\b`, and gets `sed 'n;d'` wrong |
| `ps` and the rest of procps | uutils' rejects `ps aux` |
| util-linux, tar | uutils has only work in progress |
| grep, awk, less, gzip | no Rust version takes their options |

Services and NixOS's own scripts use GNU throughout, because NixOS sets their
`PATH`, not the profile, and they were written for GNU. So does
`systemd-pull`'s update signature check, which passes gpg
`--import-options=merge-only`, an option the chameleon refuses.

> [!IMPORTANT]
> There is no `sudo`, `su` or `run0`, and nothing runs a command as root.
> `pm system` manages units and updates ([pm on LosOS](pm.md#administering-the-machine)).
> The guest template keeps sudo-rs's `su`, because a guest has no pm service.

The boot test, `nixos/tests/boot.nix`, checks which tool answers `ls`, `find`
and `gpg`. It creates a key with the chameleon, signs a file, and verifies the
signature afterwards. It doesn't pipe one gpg into another, because the
chameleon's `--verify` reports a BAD signature while a second gpg is still
writing, even though the finished bytes check out.
