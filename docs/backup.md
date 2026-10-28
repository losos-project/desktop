# Backups to a LosOS box

A [LosOS](https://losos.dasmat.us) box is the project's home server. It runs
LosOS cloud, which is Nextcloud at `/nextcloud`, and LosOS Git, which is
Forgejo at `/forgejo/`. Once a day, or as often as the person sets, LosOS
Desktop backs each person's home up to a box on the local network:

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
2. **LosOS cloud** and **LosOS Git.** Each takes the account's name and
   password once and trades them for a credential that belongs to this
   computer. LosOS cloud gives an app password, listed under the account's
   security settings. LosOS Git gives an access token named "LosOS Desktop
   backups (*computer*)" with the `write:repository` and `write:user`
   scopes, listed under the account's applications. The password is not
   kept. The two apps keep separate accounts on a box, and signing in to
   just one is enough to back up. Without LosOS Git, repositories and
   configs go to LosOS cloud as plain files with everything else.
3. **Back up automatically** turns the timer's backups on, and the choice
   beside it sets how often: every hour, every 6 or 12 hours, every day,
   which is the default, or every week. **Back up now** runs one at once.

The page shows when the last backup went through and what it sent. It also
lists any file or repository the box refused, with the box's reason.

The same steps on the command line:

```sh
losos-backup choose mattbox.local
printf '%s' "$password" | losos-backup sign-in cloud matus
printf '%s' "$password" | losos-backup sign-in git matus
losos-backup on
losos-backup every 6      # hours between backups, 1 to 168
losos-backup run          # now
losos-backup status       # what the page shows, as JSON
```

## What a backup does

`losos-backup.timer` fires every hour. The run goes ahead only when backups
are on and the last successful one is as old as the time set between them,
a day unless the person changed it. A computer that was off,
asleep or away from the box catches up within an hour of coming back. If the
box cannot be reached, the run is skipped and tried again the next hour,
and that does not count as a failure. The runs belong to the person and run
in their user manager, because systemd-homed unlocks a home only while its
owner is signed in. A backup runs with the lowest CPU and disk priority.

It walks the home without following symbolic links. LosOS cloud cannot
hold a link, so the run lists every link outside the configs, with its
target, in `.losos-backup-links.tsv` at the top of the computer's folder
there, and sends the list again whenever it changes. Its header has the
shell line that makes the links again in a restored home. A config that is
a link, as Home Manager writes them, goes to the configs' repository as a
link, which Git keeps as it is.

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
  the repository it belongs to. A repository that uses Git LFS has the
  objects in its `.git/lfs/objects` uploaded too, through LosOS Git's LFS
  batch API with the same token and certificate pin, so its large files are
  on the box and not only their pointers. The box answers for the objects
  it already has without an upload. A box with LFS turned off in Forgejo
  makes that repository's line in the report say so.
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
> the box; a LosOS Git token can only be deleted with the password, in the
> account's settings there.

## Tested

`losos-backup`'s own tests cover the walk, the names, the paths, the
relay's hosts files and a snapshot of uncommitted work.
`nixos/tests/backup.nix` boots a box with Nextcloud and Forgejo laid out the
way a LosOS box lays them out, behind a certificate made the way the box makes
its own. It also boots a desktop that chooses the box and signs in to both
apps. The desktop backs up a home with a large file, a cache, configs and a
repository with uncommitted work. It then backs up again after changes, and
the test checks what arrived and what was removed, including the configs'
second commit on top of their first. The test repeats this through a hosts
file written the way the relay writes it, and finally checks that a run is
skipped once no machine names the box.
