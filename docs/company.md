# Company accounts

A company that runs [LosOS](https://losos.dasmat.us) keeps its people in
LosOS Login, the sign-in of one of its boxes. That box sits behind the
company's edge gateway, the LosOS edge that relays the boxes on the office
network to the internet. A computer joined to that box lets everyone with a
LosOS Login account there log in with the same name and password. Each person
still gets an encrypted home of their own on the computer, made the first
time they log in.

`losos-login-bridge` does the work. It is the bridge between LosOS Login and
systemd-homed. Its code is in `src/losos-login-bridge` and its module in
`nixos/modules/company.nix`.

## Joining a computer

An administrator of the computer joins it once, with the name of the box and
the name of one of the box's administrators:

```sh
losos-login-bridge fingerprint mattbox.local
losos-login-bridge join mattbox.local <fingerprint> matus
```

`fingerprint` fetches the box's certificate from `/setup/losos-ca.crt` and
prints its SHA-256 fingerprint. Compare it with the one in the box's setup
wizard before you go on, as with [backups](backup.md#setting-it-up). The
certificate is self-signed for the box's `.local` name, so the fingerprint
is the only proof that the computer is talking to the right box.

`join` asks for the box administrator's LosOS Login password, and for a
verification code when their account has an authenticator app. Then the
join service checks four things:

1. polkit lets the person running `join` administer this computer. derisk
   shows its usual dialog for an administrator's password.
2. The box still presents the certificate with that fingerprint.
3. LosOS Login accepts the name and password, and the person is in
   `losos-admins` on the box.
4. The box goes through an edge gateway on the company's network. The box's
   `GET /api/edge` names that gateway, which is a secondary link in
   [the LosOS handbook's edge federation](https://github.com/dasmatus/losos/blob/main/handbook/docs/in-depth/edge-federation.md).
   A box that reaches an official edge directly, or none at all, is refused.

The service writes `/var/lib/losos-login-bridge/joined.json`. That file
holds the box's name, its certificate, the gateway, and the realm that
company accounts get. The realm is the gateway's cluster name, such as
`office`, or the box's own name when the gateway has no cluster.

`join` takes two options:

| Option | What it does |
| --- | --- |
| `--public-name NAME` | The name the box's edge routes to it from the internet, such as `mattbox.acme.losos.cfd`. Logins away from the office ask LosOS Login there, after the office network. |
| `--admin-group GROUP` | A LosOS Login group whose people administer this computer. It can be given more than once. Without it, the group is `losos-admins`. |

`losos-login-bridge status` prints the joined box as JSON, and
`losos-login-bridge leave` takes the computer out again. Leaving keeps every
company account. homed then logs each of them in with the password LosOS
Login last accepted.

## Logging in

Someone logging in here for the first time types their LosOS Login name in
the login screen's name field, under the people it lists. When the computer
has only one account, the screen goes straight to that account's password,
and Escape goes back to the name field.

The bridge's PAM module, `pam_losos_login.so`, sits before
`pam_systemd_home` in two stacks: derisk's login screen (`derisk-login`) and
the console's (`login`). It reads the password the same way the modules
after it do, so the person types it once. What happens next depends on who
is logging in and on whether LosOS Login answers:

| Who | LosOS Login says yes | LosOS Login says no | LosOS Login is out of reach |
| --- | --- | --- | --- |
| Someone new to this computer | homed makes their account, and the login goes on | Refused | Refused, since nothing here can check them |
| A company account | Their record is brought up to date, and the login goes on | Refused | homed decides with the password LosOS Login last accepted |
| A local account | The bridge stays out of it | The bridge stays out of it | The bridge stays out of it |

The bridge first tries the box on the office network, at its `.local` name,
with the pinned certificate. It tries the public name next, when the join
gave one, which has a certificate from a public authority. It waits four
seconds for each before it gives up, so a laptop with no network still
reaches homed quickly.

LosOS Login is Authelia. The bridge asks it the same questions its sign-in
page does, under `/auth/`:

1. `POST /auth/api/firstfactor` with the name and password.
2. `GET /auth/api/authz/auth-request` for the box's own address. That is the
   question the box's nginx asks before every admin page, and its answer
   names the person, with their groups in `Remote-Groups`.
3. If the person's account asks for a second factor, the bridge asks them
   for the code from their authenticator app, sends it to
   `POST /auth/api/secondfactor/totp`, and repeats step 2.
4. `POST /auth/api/logout`, so the box keeps no session for a login that
   has already happened.

PAM has no way to ask for a passkey. An account whose only second factor is
a passkey is told to add an authenticator app.

### The account it makes

A company account is an ordinary homed record with an encrypted home area,
like the ones in [Users](users.md). It is made with:

```sh
homectl create <name> --realm=<realm> --real-name=<LosOS Login's name> \
  --enforce-password-policy=no --member-of=<groups> --email-address=<e-mail>
```

The realm marks it as a company account. A record with any other realm, or
none, is a local account that the bridge never touches. Password rules are
LosOS Login's to set, so this computer's own policy is off for the account.
The groups are `wheel` for someone in an administrator group, and the
groups Setup gives every account, such as `networkmanager`.

At each later login that LosOS Login accepts, the bridge updates three
things through homed's own copy of the record (`homectl update --offline`):
the real name and e-mail, `wheel`, and the lock. Any other group someone
added on this computer stays.

### A password changed at the box

The home area is a LUKS volume whose key is the password. When someone
changes their password in LosOS Login, the computer still holds the old one.
At their next login LosOS Login accepts the new password and homed does not.
The login screen then asks for the previous password, and the bridge
re-keys the home with `homectl passwd`. Only the person knows that previous
password, so nothing else can do this step for them.

> [!IMPORTANT]
> While the computer is offline, the password that logs in is the one LosOS
> Login last accepted on this computer. A password changed at the box takes
> effect here at the next login that reaches the box, and an account removed
> or disabled there once the computer next sees the office network.

### Someone the company let go

Joining also gets the computer a token of its own for the box's people
list. Every five minutes, while the computer is on the office network,
`losos-login-bridge sync` reads that list. It locks the homed account of
anyone the box no longer has or has disabled, and takes `wheel` from anyone
no longer in an administrator group. It never unlocks anyone or grants
anything: that waits for the person's own login, which LosOS Login answers.
A box without the list (an older LosOS) joins as before, and the lock then
waits for the next login that reaches it.

## Managing the computer through Mesh CI

The company manages its computers from a repository in the box's LosOS
Git, with Mesh CI. A job whose `runs-on` is `[losos-desktop, <computer>]`
waits on the box for that joined computer, and
`losos-login-bridge-manage.service` asks the box for it with the same token,
through lososd, on the office network. The LosOS handbook's Mesh CI page
says how to write one.

Such a job runs no shell and no other action. Its steps are only these:

| Step | `with:` | What the computer does |
|---|---|---|
| `losos/desktop-status` | | Says its version, update channel and company accounts |
| `losos/desktop-update` | | Runs `systemd-sysupdate` now |
| `losos/desktop-reboot` | | Restarts a minute after the job ends |
| `losos/desktop-channel` | `channel` | Follows another update channel (`losos-update-channel@`) |
| `losos/desktop-lock` | `user` | Locks a company account |
| `losos/desktop-unlock` | `user` | Unlocks a company account |

The box refuses anything else when it plans the job, and the computer
checks every step again before it runs any, so a job with one step it does
not know runs nothing. Lock and unlock touch company accounts only. The
service runs as root, which these steps need, and only while the computer
has a token: a path unit starts it when a join writes one, and it ends on
leave.

## Limits

[What is not done](not-done.md) lists them too. In short:

- A computer away from the office network is locked out of a removed
  person's account only when it next sees that network, and takes Mesh CI
  jobs only there.
- There is no page in Settings for joining yet.
- An account whose only second factor is a passkey cannot log in here.
