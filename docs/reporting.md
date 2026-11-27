# Reporting a problem

When something on the desktop stops responding, derisk offers to file it as a
GitHub issue in the project's repository. The installer does the same for a
failed install ([Reporting a failed install](installer.md#reporting-a-failed-install)).
Both use the same code, `src/losos-report`.

## What can be reported

| Problem | What derisk saw | What the report carries |
| --- | --- | --- |
| A window stopped responding | The app gave no answer to the compositor's ping for a few seconds | The app ID, its unit's state and journal, and each thread with the kernel function it waits in |
| A service failed | A unit is in the failed state | The unit's state, how it ended, and its journal |
| An action never finished | An app such as Settings was still running an action long after it should have finished | The app, the action's label, and the process's journal |

Every report also carries the System table from installer reports: image
version and architecture, firmware type, make and model, CPU, memory and
kernel. Journal lines are from this boot, 200 at most.

Window titles are never sent, only the app ID. A title is often the name of
your document or page, which has nothing to do with the bug.

## Where derisk offers it

- A window that stopped responding gets a card over the desktop with
  **Report**, **Force Quit** and **Wait**. The compositor pings the app on each
  click or key press, and shows the card when the answer is five seconds late.
  **Wait** is the default, because busy apps often recover.
- A failed service gets a **Report** row in the command palette, next to
  **Restart** and **Dismiss**.
- A Settings action that runs for 30 seconds shows "This is taking much longer
  than it should." under its spinner, with a **Report it** button.

All three open the same report dialog. None of them appear on an image that
does not report problems ([Where reports go](#where-reports-go)).

## Sending one

derisk runs `losos-report serve` for each report and talks to it in JSON lines
over stdin and stdout, the same protocol as `losos-installer serve`. derisk
draws both with the same page.

1. `losos-report` collects what the problem needs and writes the report.
2. derisk shows the full text and a field for your own note, which goes on top.
3. **Send** asks GitHub for an eight-letter code. derisk shows it next to a QR
   code for `github.com/login/device`, and you approve it on a phone or another
   computer.
4. `losos-report` files the issue with the token GitHub returns, and derisk
   shows the issue's address.

The issue is public, so MAC and IP addresses are replaced before you see the
text. The token belongs to a GitHub App whose only permission is issues. It
stays in `losos-report`'s memory for that one report and is never written to
disk.

The same problem on many machines should land in one issue. Each title ends
with a fingerprint, with numbers removed:

- A window that stopped responding: its app ID.
- A failed service: the unit without its instance, so
  `app-derisk-org.foo@1.service` and `app-derisk-org.foo@7.service` match, and
  how it ended.
- An action that never finished: the app and the action's label.

If an open issue already has that fingerprint, the report becomes a comment on
it.

## Where reports go

`losos-report` reads `/etc/losos/report.json`, which `nixos/modules/report.nix`
writes from `losos.report.repository` and `losos.report.clientId`:

```json
{ "repository": "losos-project/desktop", "client_id": "Iv23li…" }
```

The repository defaults to the one releases come from. The client ID comes
from the `LOSOS_REPORT_CLIENT_ID` repository variable in CI, shared with
installer reports ([setup](installer.md#reporting-a-failed-install)). An image
built without it has no `report.json`, `losos-report` answers that the system
does not report problems, and derisk offers no report.

For tests, `--repo`, `--client-id`, `--login` and `--api` override the file and
point sign-in and the API at a stand-in for GitHub.
