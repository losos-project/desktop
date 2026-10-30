# losos-eid-signin [--print] [URI]: sign in to this computer with the eID
# card from now on (eid-signin.nix, docs/tokens.md). Run by the person
# whose account it is, at the seat, with the card in a reader. --print
# only says which certificate it would enrol.
#
# The first time, the host downloads the eID klient for the eID machine,
# which an administrator allows, and the machine starts. Then homectl
# enrols the card's encryption certificate, the one whose key may decrypt:
# the card has a signing certificate too, and homed needs to know which.
# URI names a certificate when there is more than one such.

machine=losos-service-machine-losos-eid.service
print=""
if [ "${1:-}" = --print ]; then
  print=1
  shift
fi

# Not where nixos/tests/eid-signin.nix runs the machine's half itself.
if systemctl cat "$machine" >/dev/null 2>&1 && ! systemctl -q is-active "$machine"; then
  echo "Downloading the eID klient for the eID machine. An administrator is asked to allow it."
  systemctl start losos-eid-fetch.service
  for _ in $(seq 60); do
    systemctl -q is-active "$machine" && break
    sleep 1
  done
fi

# The card's certificates, through p11-kit's client module straight to the
# relay's socket (P11_KIT_SERVER_ADDRESS, eid-signin.nix), from the moment
# the eID machine has booted and the card is in. gnutls' p11tool rather
# than `p11-kit list-objects`, which garbles the labels and IDs of objects
# it reads over the RPC (p11-kit 0.26.5).
certificates() {
  p11tool --provider "$P11_KIT_CLIENT" --list-all-certs --only-urls 2>/dev/null
}

# A certificate's key may decrypt, and it is a person's, not an authority's.
encrypts() {
  p11tool --provider "$P11_KIT_CLIENT" --export "$1" 2>/dev/null |
    openssl x509 -noout -ext keyUsage,basicConstraints 2>/dev/null |
    awk '/Key Encipherment|Data Encipherment/ { key = 1 } /CA:TRUE/ { ca = 1 } END { exit !(key && !ca) }'
}

if [ $# -gt 0 ]; then
  uri=$1
else
  found=""
  for _ in $(seq 90); do
    found=$(certificates)
    [ -n "$found" ] && break
    sleep 1
  done
  if [ -z "$found" ]; then
    echo "losos-eid-signin: the eID machine shows no card. Is the card in the reader?" >&2
    exit 1
  fi

  candidates=()
  while read -r cert; do
    if encrypts "$cert"; then candidates+=("$cert"); fi
  done <<<"$found"

  case ${#candidates[@]} in
    0)
      echo "losos-eid-signin: the card has no certificate whose key may decrypt" >&2
      exit 1
      ;;
    1)
      uri=${candidates[0]}
      ;;
    *)
      echo "losos-eid-signin: the card has more than one certificate that could sign you in:" >&2
      for cert in "${candidates[@]}"; do
        subject=$(p11tool --provider "$P11_KIT_CLIENT" --export "$cert" |
          openssl x509 -noout -subject -nameopt utf8)
        echo "  $cert" >&2
        echo "    $subject" >&2
      done
      echo "Run losos-eid-signin again with the one to use." >&2
      exit 1
      ;;
  esac
fi

# homectl reads the public half from the objects the URI matches, and
# homed decrypts with the private key it matches, so the URI names neither
# kind: only the certificate's ID, which the key beside it shares. Its
# label may differ from the certificate's.
uri=$(printf '%s\n' "$uri" | sed -e 's/;object=[^;]*//' -e 's/;type=[^;]*//')

if [ -n "$print" ]; then
  echo "$uri"
  exit 0
fi

echo "Enrolling $uri"
echo "homectl asks for the card's PIN, to find its key, and your password, to change your account."
homectl update "$(id -un)" --pkcs11-token-uri="$uri"
echo "Done. At the login screen, put the card in and type its PIN when you are asked for it."
