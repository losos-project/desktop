# Signing in

You prove who you are to PAM and only to PAM. derisk's login screen relays
whatever the PAM stack asks, and its lock screen asks the same stack through a
`derisk auth` process. The **Sign-in** page in Settings sets up a security key,
fingerprints, verification codes, or a new password. `nixos/modules/sign-in.nix`
writes the stacks.

Each account is a systemd-homed record whose home is a LUKS volume on the
`home` partition ([Where the OS lives](layout.md)). Only a method that can
produce the volume's key can log you in.

![derisk's login screen asking for a password, and the same screen after a wrong one, relaying PAM's "Sorry, try again"](images/sign-in-login-screen.png)

| Method | Login screen | Lock screen | polkit, `pm system` |
| --- | --- | --- | --- |
| Password | yes | yes | yes |
| Security key (FIDO2) | yes, instead of the password | yes | yes |
| Slovak eID card | yes, instead of the password | yes | yes |
| Fingerprint | no | yes, beside the field | yes |
| Verification code | after the password, when turned on | no | no |

## Security keys

Settings enrolls a FIDO2 key, the kind that holds passkeys, into your homed
record with `homectl update --fido2-device=auto`, passing your password and the
key's PIN in the environment. homed wraps the home's key with the security
key's hmac-secret, so the key alone opens your home. pam_systemd_home asks for
the "Security token PIN" and a touch, and derisk shows both prompts. A new key
replaces the previous one, and your password keeps working. Passkeys on a phone
or in a laptop's TPM aren't offered, because homed can't get a key out of them.

## The Slovak eID card

The eID card works the same way through homed's PKCS#11 support: homed encrypts
a secret to the card's encryption certificate, and the card decrypts it after
its PIN. The Ministry's closed module that reads the card runs in its own
machine. `losos-eid-signin` sets it up
([Signing in with the eID](tokens.md#signing-in-with-the-eid)).

## Fingerprints

fprintd drives the reader. NixOS would put pam_fprintd first in every PAM
service, letting a finger "log in" without opening the home. So `login`,
`derisk-login`, `derisk-setup`, `derisk-greeter` and the lock screen's field
(`derisk`) turn it off. The lock screen listens to the reader through its own
service, `derisk-fingerprint`, with pam_fprintd alone, in a second conversation
next to the field. Touch the reader or type, and whichever PAM accepts first
unlocks. derisk offers the reader only when that service file exists.

polkit keeps NixOS's default, so a finger can answer an administrator prompt.
Settings enrolls fingerprints with `fprintd-enroll`. fprintd allows that only
after you authenticate again, and derisk's polkit agent asks, so no polkit rule
skips it.

## polkit and pm

`derisk session` is polkit's authentication agent for its logind session.
Actions such as `pm system`, installing an app for every user, or enrolling a
fingerprint need someone to authenticate. For those, the desktop dims under a
dialog that says what is asked and who may answer: you, if polkit accepts you,
or otherwise a choice of administrators.

The dialog relays PAM's `polkit-1` stack through `polkit-agent-helper-1`, which
NixOS runs from `/run/polkit/agent-helper.socket`, so it asks in that stack's
order: the fingerprint reader first if you enrolled a finger, until it gives
up, then the password or the security key's PIN. Every key press goes to the
dialog while it is open. Agent input can cancel it but not authenticate.
`pm system` in a terminal uses the dialog too, and falls back to `pkttyagent`
on the terminal only when the session has no agent
([pm on LosOS](pm.md#administering-the-machine)).

## Verification codes

To turn codes on, scan the QR code in Settings into an authenticator app and
type the first code back. Settings checks it before saving anything. The secret
and five one-time recovery codes go in `~/.google_authenticator`, read-only to
you, in pam_google_authenticator's format. `derisk-login` runs the module with
`nullok`, so only people with the file are asked, after the password or
security key. NixOS runs pam_systemd_home early in that stack, so the home
holding the file is open when the module reads it. pam_unix is left out of that
service, because it would ask a homed user for the password a second time.

## Dimming and the privacy display

The login screen, lock screen and polkit dialog dim the backlight from the
first character of a password, and restore it once the field has been empty for
a moment or the prompt is gone. They use logind's `SetBrightness` on their own
session, so only displays in `/sys/class/backlight`, as on laptops and phones,
dim. A desktop monitor stays as it is.

The privacy display is a software take on Samsung's feature of the same name,
off until you turn it on in **Settings › Privacy**. A PC panel can't narrow its
viewing angle as Samsung's does, so the same triggers start the camera instead:
typing a password, opening apps you chose, or notification pop-ups. While a
second face is looking, the protected part of the screen is covered: the whole
screen for a password or chosen app, only the pop-ups for a notification. The
cover is opaque, says why it is there, and has a **Show anyway** button that
holds until the trigger ends.

`derisk-faces`, built next to derisk in `nixos/pkgs/derisk.nix`, counts faces.
It takes a few small frames a second from the first V4L2 camera and runs them
through [libfacedetection](https://github.com/ShiqiYu/libfacedetection)'s CNN,
which derisk vendors and binds with bindgen. derisk runs it only while a
trigger holds, so the camera light is on only then, and frames never leave that
process's memory. With no camera it prints `camera none` and covers nothing,
which the boot VM test checks.

## Password age

A password lasts 365 days, with warnings in the last 14, as
`/etc/derisk/sign-in.conf` from `sign-in.nix` sets. derisk writes these limits
into homed records as `passwordChangeMaxUSec` and `passwordChangeWarnUSec`.
Setup creates the first account with them, and the display manager updates any
record that differs when its owner logs in, so changing the option reaches
every account. Only an administrator may change those fields, so you can't opt
out by editing your own record.

During the warning period, pam_systemd_home warns "Password will expire soon"
at each login. Once the password expires, the login screen asks for a new one
through `pam_chauthtok` before the session starts. The lock screen still
unlocks a running session with an expired password and shows a reminder, so
nobody is locked out of their own session over a date. Settings shows the
expiry date and changes the password with `homectl passwd`.
