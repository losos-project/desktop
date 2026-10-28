# The host's half of the files machine (files-machine.nix): mounts each
# volume @machine@ lists under /run/media/<user>/<name> for the person
# whose session is in front, with sshfs reading SFTP from the machine over
# losos-vm-call, and lets go of each one that goes. Runs while the machine
# does; its unit stops with it, and everything it mounted goes then.

state=/run/losos-files
mkdir -p "$state/mounts"

# The user of the session in front, if there is one.
active_user() {
  local session
  session=$(loginctl show-seat seat0 -p ActiveSession --value 2>/dev/null) || return 1
  [ -n "$session" ] || return 1
  loginctl show-session "$session" -p Name --value 2>/dev/null
}

# A mount is an sshfs whose "ssh" is losos-vm-call (@sftp@ runs it with
# the volume's name), kept by its pid in $state/mounts/<name>, which also
# holds where it is mounted.
mount_one() {
  local name=$1 user=$2 uid gid dir
  uid=$(id -u "$user")
  gid=$(id -g "$user")
  dir=/run/media/$user/$name
  install -d -m 0700 -o "$uid" -g "$gid" "/run/media/$user"
  mkdir -p "$dir"
  # The files are the person's: sshfs shows them as theirs and lets the
  # kernel check access as it would for their own.
  LOSOS_FILES_NAME=$name setsid sshfs -f \
    -o ssh_command=@sftp@,allow_other,default_permissions \
    -o "uid=$uid,gid=$gid,fsname=@machine@,subtype=losos-files" \
    "files:/media/$name" "$dir" &
  printf '%s\n%s\n' "$!" "$dir" > "$state/mounts/$name"
  echo "$name: mounted for $user at $dir"
}

unmount_one() {
  local name=$1 pid dir
  { read -r pid; read -r dir; } < "$state/mounts/$name" || true
  [ -n "${dir:-}" ] && fusermount3 -uz "$dir" 2>/dev/null || true
  [ -n "${pid:-}" ] && kill "$pid" 2>/dev/null || true
  [ -n "${dir:-}" ] && rmdir "$dir" 2>/dev/null || true
  rm -f "$state/mounts/$name"
  echo "$name: let go"
}

cleanup() {
  for file in "$state"/mounts/*; do
    [ -e "$file" ] && unmount_one "$(basename "$file")"
  done
}
trap cleanup EXIT
trap 'exit 0' TERM INT

user=
while :; do
  now=$(active_user || true)
  # Another person in front: theirs, not the last one's.
  if [ "$now" != "$user" ]; then
    cleanup
    user=$now
  fi
  if [ -n "$user" ] && list=$(losos-vm-call call files-list 2>/dev/null); then
    names=$(printf '%s\n' "$list" | cut -f1)
    for file in "$state"/mounts/*; do
      [ -e "$file" ] || continue
      name=$(basename "$file")
      pid=$(head -n1 "$file")
      # Gone from the machine, or its connection ended (ejected, or the
      # sftp-server died): let go, and mount it again if it is still there.
      if ! printf '%s\n' "$names" | grep -qxF -- "$name" || ! kill -0 "$pid" 2>/dev/null; then
        unmount_one "$name"
      fi
    done
    while IFS= read -r name; do
      [ -n "$name" ] || continue
      [ -e "$state/mounts/$name" ] || mount_one "$name" "$user"
    done <<< "$names"
  fi
  sleep 2
done
