# losos-guest-flatpak: what the host's Settings asks of the Flatpak machine
# through the QEMU guest agent (derisk-machines' flatpak module holds the
# other half). The agent runs this as root.
#
#   losos-guest-flatpak apps
#       `global <TAB> <overrides>`, then a line per installed app: `app`,
#       its ID, `user` or `system`, its name, and its metadata and the
#       user's overrides of it, both base64, separated by tabs
#   losos-guest-flatpak overrides <app>
#       standard input becomes the user's overrides of <app>; empty input
#       removes them
#   losos-guest-flatpak flatpak <permission-show|permission-set|permission-remove> ...
#       that `flatpak` command, as the user, in its session, where the
#       portals' permission store is
#
# and what the host's Store asks of it (derisk-store's flatpak module):
#
#   losos-guest-flatpak store run <list|remote-ls> ...
#       that `flatpak` command, as the user, printing what it printed once
#       it has ended
#   losos-guest-flatpak store start <install|uninstall|update|remote-add> ...
#       starts that `flatpak` command as the user, in a unit of the user's
#       manager, and returns at once; one at a time
#   losos-guest-flatpak store put <name>
#       standard input becomes a file of the user's named <name>, and its
#       path is printed: a .flatpakref or .flatpakrepo opened on the host,
#       for `start install --from` or `start remote-add --from`
#   losos-guest-flatpak store progress
#       `running`, or `done <exit status>` once it has ended, then all the
#       last job has printed, its carriage returns made line ends

user=@user@
flatpak=@flatpak@
home=/home/$user
userdir=$home/.local/share/flatpak
sysdir=/var/lib/flatpak

usage() {
  echo "usage: losos-guest-flatpak apps | overrides <app> | flatpak permission-show|permission-set|permission-remove ... | store run|start|put|progress ..." >&2
  exit 2
}

# An app ID as flatpak takes one, and so a file name that stays in the
# overrides directory.
valid() {
  [[ $1 =~ ^[A-Za-z0-9_][A-Za-z0-9_.-]*$ ]] && [ "${#1}" -le 255 ]
}

# A file, base64-encoded on one line; nothing for a file that is not there.
encoded() {
  if [ -f "$1" ]; then
    base64 -w0 <"$1"
  fi
}

case "${1-}" in
apps)
  [ $# -eq 1 ] || usage
  printf 'global\t%s\n' "$(encoded "$userdir/overrides/global")"
  # The user's installation first, as flatpak runs the user's copy of an
  # app installed both ways.
  declare -A seen
  for kind in user system; do
    root=$sysdir
    [ "$kind" = system ] || root=$userdir
    for app in "$root"/app/*; do
      id=$(basename "$app")
      active=$app/current/active
      if ! valid "$id" || [ -n "${seen[$id]-}" ] || [ ! -f "$active/metadata" ]; then
        continue
      fi
      seen[$id]=1
      # A tab in the name would end its field early.
      name=$(sed -n 's/^Name=//p' "$active/export/share/applications/$id.desktop" 2>/dev/null |
        head -n 1 | tr -d '\t')
      printf 'app\t%s\t%s\t%s\t%s\t%s\n' "$id" "$kind" "$name" \
        "$(encoded "$active/metadata")" "$(encoded "$userdir/overrides/$id")"
    done
  done
  ;;
overrides)
  if [ $# -ne 2 ] || ! valid "$2"; then
    usage
  fi
  text=$(cat)
  # As the user, in the user's own directory: root writing there could be
  # led through a link the user's apps left.
  if [ -z "${text//[[:space:]]/}" ]; then
    exec runuser -u "$user" -- rm -f "$userdir/overrides/$2"
  fi
  runuser -u "$user" -- mkdir -p "$userdir/overrides"
  printf '%s\n' "$text" | runuser -u "$user" -- tee "$userdir/overrides/$2.new" >/dev/null
  exec runuser -u "$user" -- mv -f "$userdir/overrides/$2.new" "$userdir/overrides/$2"
  ;;
flatpak)
  shift
  case "${1-}" in
  permission-show | permission-set | permission-remove) ;;
  *) usage ;;
  esac
  # The permission store is a service of the user's session bus, which the
  # user's manager (lingering, common.nix) has from boot.
  exec systemd-run --quiet --user --machine="$user@.host" --pipe --wait --collect \
    --setenv=DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$(id -u "$user")/bus" \
    -- "$flatpak" "$@"
  ;;
store)
  shift
  uid=$(id -u "$user")
  bus="unix:path=/run/user/$uid/bus"
  # The last job's output and exit status, in the user's runtime directory,
  # which the user's manager (lingering, common.nix) has from boot.
  log=/run/user/$uid/losos-store.log
  status=/run/user/$uid/losos-store.status
  files=/run/user/$uid/losos-store-files
  # Installs, removals and updates run as the user, whom flatpak.nix's
  # polkit rule lets install for the whole machine, in the user's session
  # like the permission commands above, so `flatpak list` shows the user's
  # own installation beside the system's.
  case "${1-}" in
  run)
    shift
    case "${1-}" in
    list | remote-ls) ;;
    *) usage ;;
    esac
    exec systemd-run --quiet --user --machine="$user@.host" --pipe --wait --collect \
      --setenv=DBUS_SESSION_BUS_ADDRESS="$bus" -- "$flatpak" "$@"
    ;;
  put)
    if [ $# -ne 2 ] || ! valid "$2"; then
      usage
    fi
    # As the user, as `overrides` writes: in the user's runtime directory,
    # where flatpak reads it as the user. A flatpakref is a few kilobytes.
    runuser -u "$user" -- mkdir -p "$files"
    head -c 1048576 | runuser -u "$user" -- tee "$files/$2" >/dev/null
    echo "$files/$2"
    ;;
  start)
    shift
    case "${1-}" in
    install | uninstall | update | remote-add) ;;
    *) usage ;;
    esac
    # The agent hands a program's output over only once it has exited, so
    # a job that takes minutes runs in a unit of its own and the host reads
    # its log with `progress` meanwhile.
    if systemctl --user --machine="$user@.host" --quiet is-active losos-store.service; then
      echo "the Store is already installing or updating something" >&2
      exit 1
    fi
    runuser -u "$user" -- rm -f "$log" "$status"
    exec systemd-run --quiet --user --machine="$user@.host" --unit=losos-store --collect \
      --description="Store: flatpak $1" --setenv=DBUS_SESSION_BUS_ADDRESS="$bus" \
      -- "$(readlink -f "$0")" store job "$@"
    ;;
  job)
    # The unit `start` made, as the user: flatpak, then its exit status,
    # written whole so `progress` never reads half of it.
    shift
    case "${1-}" in
    install | uninstall | update | remote-add) ;;
    *) usage ;;
    esac
    if "$flatpak" "$@" >"$log" 2>&1; then code=0; else code=$?; fi
    echo "$code" >"$status.new"
    exec mv -f "$status.new" "$status"
    ;;
  progress)
    [ $# -eq 1 ] || usage
    # Running is asked first: a job that is not running has written its
    # status, unless it was killed before it could.
    if systemctl --user --machine="$user@.host" --quiet is-active losos-store.service; then
      echo running
    elif [ -f "$status" ]; then
      printf 'done %s\n' "$(runuser -u "$user" -- cat "$status")"
    else
      echo "done 1"
    fi
    # As the user, as `overrides` writes: the file is in the user's
    # directory, where a link could stand in for it.
    if [ -f "$log" ]; then
      runuser -u "$user" -- cat "$log" | tr '\r' '\n'
    fi
    ;;
  *) usage ;;
  esac
  ;;
*)
  usage
  ;;
esac
