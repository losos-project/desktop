# Reporting a problem

When something on the desktop stops responding, derisk offers to report it
as an issue in the project's repository on GitHub. The installer does the
same for a failed install, as [the installer's page](installer.md#reporting-a-failed-install)
describes. Both send their reports with the same code, `src/losos-report`.

## What can be reported

A report is about one of three things:

| Problem | What derisk saw | What the report carries |
| --- | --- | --- |
| A window stopped responding | The app's window gave no answer to the compositor's ping for a few seconds | The app's ID, its unit's state and journal, and each of its threads with the kernel function it waits in |
| A service failed | A unit is in the failed state | The unit's state, how it ended, and its journal |
| An action never finished | An app such as Settings was still running an action long after it should have finished | The app, the action's own label, and the process's journal |

Every report also has the System table that an installer report has: the
image's version and architecture, the firmware type, the machine's make and
model, its CPU, memory and kernel. The journal lines are from this boot, at
most 200 of them.

A window's title is never sent, only its app's ID. A title is often the name
of a document or a page, which belongs to the person, not to the bug.

## Sending one

derisk runs `losos-report serve` for each report and talks to it in JSON
lines on stdin and stdout. It is the same protocol `losos-installer serve`
uses for a failed install, so derisk draws both with the same page:

1. `losos-report` reads what the problem needs from this machine and writes
   the report.
2. derisk shows the whole text and a field for a note of the person's own,
   which goes above it.
3. "Send" asks GitHub for an eight-letter code. derisk shows it beside a QR
   code for `github.com/login/device`, and the person approves it on a phone
   or another computer.
4. `losos-report` files the issue with the token GitHub hands back, and
   derisk shows the issue's address.

The report is public, so MAC and IP addresses are replaced before it is
shown. The token belongs to a GitHub App whose only permission is issues. It
stays in `losos-report`'s memory for that one report and is never written to
disk.

The same problem on many machines should be one issue. A report's title ends
with a fingerprint of what went wrong, with numbers taken out:

- a window that stopped responding is fingerprinted by its app's ID;
- a failed service by its unit with the instance taken out, so
  `app-derisk-org.foo@1.service` and `app-derisk-org.foo@7.service` match,
  and by how it ended;
- an action that never finished by its app and the action's label.

When an open issue already carries that fingerprint, the report is added to
it as a comment instead.

## Where reports go

`losos-report` reads `/etc/losos/report.json`, which `nixos/modules/report.nix`
writes from `losos.report.repository` and `losos.report.clientId`:

```json
{ "repository": "losos-project/desktop", "client_id": "Iv23li…" }
```

The repository is the one the releases come from unless it is set. The
client ID comes from the `LOSOS_REPORT_CLIENT_ID` repository variable in CI,
the same one the installer's reports use, and setting it up is described on
[the installer's page](installer.md#reporting-a-failed-install). An image
built without it has no such file. `losos-report` then answers that this
system does not report problems, and derisk offers no report.

For tests, `--repo`, `--client-id`, `--login` and `--api` override the file
and point the sign-in and the API at a stand-in for GitHub.
