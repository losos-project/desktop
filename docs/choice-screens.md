# Active users and the choice screens

Under the EU's Digital Markets Act, an operating system with 45 million
monthly active end users in the EU is a gatekeeper, by Art. 3(2)(b). Art.
6(3) says a gatekeeper has to show users in the EEA a choice screen for the
browser and the search engine. derisk has both screens and keeps them hidden
until a count of active users turns them on, so the OS keeps that count.

![losos-ping posts a monthly id and the architecture to the proxy's /ping, which adds the id to HyperLogLogs in Redis and answers which choice screens are on; the answer goes to policy.conf, and derisk opens Settings, Default apps at login](images/choice-screens.svg)

**What is sent.** `nixos/modules/ping.nix` gives every signed-in user a
systemd user timer, `losos-ping`, that runs a few minutes into each session
and then daily. It POSTs two lines to `$LOSOS_PROXY_URL/ping`. One is `id`,
the SHA-256 of a random secret in `~/.local/state/losos/ping-secret` and the
current month. The other is `arch`. The id changes every month, so the proxy
can count a person once a month but cannot link one month to the next.
Nothing else is sent, so no account, no hardware id and no version.

> [!TIP]
> A user turns it off in Settings, Privacy ("Count me as an active user"),
> which writes `privacy.usage_ping = false`; a build without
> `LOSOS_PROXY_URL`, or with `losos.ping.url = null`, has no timer.

**What is counted.** `proxy/` adds the id to the month's HyperLogLogs in
Redis, `actives:<YYYY-MM>` and `actives:<YYYY-MM>:<arch>`. When Vercel's
`x-vercel-ip-country` header names an EU member state, it also adds the id
to `actives:<YYYY-MM>:eu` and to the day's `actives:<YYYY-MM-DD>:eu`. Month
keys are kept for 100 days and day keys for 3. The proxy reads only that
country code, to pick the keys. It never reads or stores the address, and a
HyperLogLog cannot give an id back. Each count it compares against a
threshold is the larger of the current period's EU count and the previous
one's, so a new month or day does not start from zero.

**What comes back.** The answer is
`{"region":"eu","choice_screens":{"browser":…,"search":…}}`. Both screens are
on for a request from the EEA, meaning the EU, Iceland, Liechtenstein and
Norway, once either EU count reaches its threshold. They are off everywhere
else. The script writes the answer to `~/.local/state/derisk/policy.conf`,
which derisk reads. If a screen is on and nothing has been chosen yet, derisk
opens Settings on its Default apps page at login, and the page stays in
Settings afterwards. Both screens list their options in a new random order
each time, with none preselected. The browser screen offers the installed
browsers and installs others from Flathub.

![Settings' Default apps page opened at login: a Browser list and a Search engine list, each in random order with nothing chosen](images/choice-screens-default-apps.png)

To set it up, add an Upstash Redis database to the Vercel project from the
Marketplace. That sets `KV_REST_API_URL` and `KV_REST_API_TOKEN`, and
`UPSTASH_REDIS_REST_URL` and `UPSTASH_REDIS_REST_TOKEN` work too. Without a
database, the proxy answers pings with both screens off and counts nothing.
`CHOICE_SCREENS_AT` sets the monthly threshold, 45000000 by default.
`CHOICE_SCREENS_DAILY_AT` sets a threshold in daily EU pings by distinct
user, with none by default. `CHOICE_SCREENS=on` or `off` forces both screens
either way, for testing or by choice. To read the counts, run
`PFCOUNT actives:<YYYY-MM>:eu` in the database's console, or leave off `:eu`
to count everyone. A person's location comes from their address at the time
of the ping, not from a setting on the machine, so a VPN changes it.
