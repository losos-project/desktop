# Backups to a LosOS box

A [LosOS](https://losos.dasmat.us) box is the project's home server. It runs
LosOS cloud, which answers Nextcloud's apps at `/nextcloud`, and LosOS Git,
its Git server at `/git/`. People sign in to both through LosOS Login, the
box's own sign-in. Once a day, LosOS Desktop backs each person's home up
to a box on the local network:

- every Git repository in the home goes to LosOS Git, as a private
  repository of the person's own;
- the configs, meaning everything in `~/.config` and the dotfiles in the
  home itself, go to LosOS Git too, as the history of one private
  repository, `<this computer's name>-config`;
- everything else goes to their LosOS cloud, under
  `Backups/<this computer's name>/`, and only what changed since the last
  time.

`losos-backup` does the work, from a timer in each person's session. Its code
is in `src/losos-backup` and its module in `nixos/modules/backup.nix`. The
Backups page in derisk Settings sets it up.

## Setting it up

The Backups page is under System in Settings. It has three steps.

1. **Box.** The page lists the boxes it can reach. Pick one, or type its
   name, such as `mattbox.local`. The page fetches the box's certificate
   from `/setup/losos-ca.crt` and shows its SHA-256 fingerprint, spelled the
   same way as in the box's own setup wizard, so you can compare the two.
   From then on, every connection to the box has to present exactly that
   certificate. The box's certificate is self-signed for its `.local` name,
   so there is no certificate authority to ask instead.
2. **Sign in with LosOS Login.** The page shows a link and a short code.
   Open the link in a browser, sign in to LosOS Login with your password
   and, if your account has one, your second factor, and approve "LosOS
   Desktop backups". Nothing is typed into Settings. The box then makes
   this computer a LosOS cloud app password, named "LosOS on *computer*"
   and listed under the account's security settings, and a LosOS Git token
   of the same name with the `write:repository` and `read:user` scopes. A
   second sign-in from the same computer replaces both. LosOS Login's own
   token lasts ten minutes, can only ask for those two credentials, and
   isn't kept. On a box without LosOS Git, repositories and configs go to
   LosOS cloud as plain files with everything else.

   A box older than this sign-in can still be signed in to LosOS cloud with
   a name and password, under **Sign in with a password instead**. That
   way doesn't work for an account with a second factor.
3. **Back up every day** turns the timer's backups on. **Back up now**
   runs one at once.

The page shows when the last backup went through and what it sent. It also
lists any file or repository the box refused, with the box's reason.

The same steps on the command line:

```sh
losos-backup choose mattbox.local
losos-backup sign-in box  # prints the link and code as JSON, then waits
# or, one app at a time:
printf '%s' "$password" | losos-backup sign-in cloud matus
printf '%s' "$token" | losos-backup sign-in git   # a token from LosOS Git's settings
losos-backup on
losos-backup run          # now
losos-backup status       # what the page shows, as JSON
```

## What a backup does

`losos-backup.timer` fires every hour. The run goes ahead only when backups
are on and the last successful one is a day old. A computer that was off,
asleep or away from the box catches up within an hour of coming back. If the
box cannot be reached, the run is skipped and tried again the next hour,
and that does not count as a failure. The runs belong to the person and run
in their user manager, because systemd-homed unlocks a home only while its
owner is signed in. A backup runs with the lowest CPU and disk priority.

It walks the home without following symbolic links.

- A directory with a `.git` directory, or a bare repository, is a
  repository. Its branches and tags are force-pushed to a repository named
  after its directory, or after its whole path when two directories share a
  name. A branch deleted at home is deleted on the box too, so the box's
  copy is a mirror. Uncommitted work goes as well. The work tree as it is,
  with untracked files but not ignored ones, becomes one commit on top of
  `HEAD` on the branch `losos-backup/uncommitted`. That commit is made with
  a separate index, so the repository's own index, branches and stash stay
  as they were, and the repository's hooks do not run. A worktree's
  checkout, which has a `.git` file, is skipped, because its commits are in
  the repository it belongs to.
- The configs are every file under `~/.config` and every file directly in
  the home whose name starts with a dot, such as `.bashrc` and
  `.gitconfig`. They are committed together to `main` of the private
  repository `<computer>-config`, one commit for each run that finds them
  changed, so the repository holds their history. Each run starts from
  `main` as the box has it. If this computer's own copy in
  `~/.local/state/losos-backup/config.git` is lost, the history still
  carries on, and an edit made on the box becomes a parent commit instead of
  a refused push. The commit holds exactly those files as they are, and no
  ignore file leaves any out. A repository inside `~/.config`, such as an
  editor's config kept in Git, goes as a repository of its own instead. Dot
  directories in the home, like `~/.ssh` or `~/.local`, are not configs and
  go to LosOS cloud. The configs go there too until they reach LosOS Git,
  so if a push fails they are still backed up with the rest.
- Every other file goes to LosOS cloud at its path under the home, with its
  modification time, whenever its size or time differs from what the last
  backup sent. The box's web server takes at most 512 MiB in one request, so
  a file larger than 128 MiB goes up in 64 MiB chunks with Nextcloud's
  chunked upload. A file or folder deleted from the home is deleted on the
  box as well and goes to LosOS cloud's trash. To get an older copy back,
  look there or in the old versions LosOS cloud keeps of every changed
  file.
- Some things are left out: `~/.cache`, the trash, the downloads and disks
  of Flatpak, containers and machines, cargo's and npm's caches, any
  `node_modules`, any directory with a `CACHEDIR.TAG`, systemd-homed's
  `~/.identity`, and the backups' own settings and state. cargo's `target`,
  ccache and most other caches write a `CACHEDIR.TAG`.
  `~/.config/losos-backup/exclude` adds more paths, one per line, relative
  to the home.

If the box refuses a file, for example a name Nextcloud forbids like
`.htaccess`, the report lists it and the rest carries on. When the box stops
answering, the run stops and keeps what it had already sent, and the next run
continues from there.

## How it reaches the box

On a PC, the [LosOS boxes](losos-boxes.md) machine is the one machine that
talks to LosOS boxes, and every other machine reaches a box through it. Its
`losos-boxes route` takes each connection on ports 80 and 443 and passes it
on unchanged to the box its `Host` header or TLS server name asks for, so the
box's own certificate answers. The host's `losos-boxes relay` writes the
boxes each such machine sees into `/run/losos-boxes/hosts/<machine>`, one
`<machine's address> <box>.local` line per box. The backups read those files
the same way libvirt's DNS does for the [machines](hypervisor.md). They
connect to that address with the box's name, and git gets the same pin
through `http.curloptResolve`.

That directory exists on every PC with machines, and it is empty while no
LosOS boxes machine is running. In that case a run starts the person's LosOS
boxes machine with `pm machine start`, waits up to five minutes for it to see
the box, and stops it again afterwards. A run never goes around the machine
to reach the box. Only an administrator may start a machine on
`qemu:///system`, so for anyone else the machine has to be running already. A
system without the directory, such as a phone, has no machines and reaches
the box by name through resolved's mDNS.

## Where things are kept

| Path | What |
| --- | --- |
| `~/.config/losos-backup/config.json` | The box, whether backups are on, the accounts |
| `~/.config/losos-backup/secrets.json` | The app password and the token, readable by the person alone |
| `~/.config/losos-backup/box.pem` | The box's pinned certificate, which git reads as its only CA |
| `~/.local/state/losos-backup/files.json` | What LosOS cloud has, with each file's size and time |
| `~/.local/state/losos-backup/repos.json` | The name each repository got in LosOS Git |
| `~/.local/state/losos-backup/config.git` | This computer's copy of the configs' repository |
| `~/.local/state/losos-backup/last.json` | The last run's report |

Unlike `losos`'s keys, the two secrets live in a file instead of the Secret
Service. The timer runs with nobody at the screen, and a locked keyring would
pop up a window asking for its password. systemd-homed encrypts the home
they live in whenever its owner is signed out.

> [!NOTE]
> Signing out of LosOS cloud on the Backups page revokes its app password on
> the box. Signing out of LosOS Git only forgets the token here: remove it
> in LosOS Git's settings, or sign in again from this computer to replace it.

## Tested

`losos-backup`'s own tests cover the walk, the names, the paths, the relay's
hosts files and a snapshot of uncommitted work. `nixos/tests/backup.nix` boots
a box with Nextcloud and Forgejo at the paths a LosOS box serves LosOS cloud
and LosOS Git from, behind a certificate made the way the box makes its own.
It also boots a desktop that chooses the box and signs in to both apps, with a
password and with a token made on the box. The desktop backs up a home with a
large file, a cache, configs and a repository with uncommitted work. It then
backs up again after changes, and the test checks what arrived and what was
removed, including the configs' second commit on top of their first. The test
repeats this through a hosts file written the way the relay writes it, and
finally checks that a run is skipped once no machine names the box.
`nixos/tests/losos-box.nix` does it against a real LosOS box: the desktop
signs in with a code that a browser approves on the box's LosOS Login, and
its home reaches LosOS Cloud and its repository LosOS Git.
