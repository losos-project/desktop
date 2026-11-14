//! The decisions behind the GHCR proxy, compiled to WebAssembly.
//!
//! The proxy (`api/proxy.ts`) serves these from one host, all out of this
//! project's GitHub Container Registry namespace:
//!
//! * **A Nix binary cache.** `nix copy --to file://...` writes a narinfo and a
//!   NAR per store path; `tools/nix-cache-push` puts each pair in GHCR as one
//!   OCI artifact, `nix-cache:<store hash>`. Nix asks a cache for
//!   `/<hash>.narinfo` and then for whatever its `URL:` names, and this maps
//!   both onto that artifact. It serves paths CI uploads, alongside
//!   cache.nixos.org for stock nixpkgs packages.
//! * **systemd-sysupdate's update source.** CI pushes each release as an OCI
//!   artifact already (`images:<channel>-<version>-<arch>`), and also under
//!   the moving tag `<channel>-<arch>`. `/updates/<channel>/<arch>/<file>`
//!   serves a file of the newest one, `SHA256SUMS` included, which is all a
//!   `url-file` transfer asks of its source. That is the install side.
//! * **The web flasher's downloads.** The flasher (`website/static/flasher/`)
//!   is a page on another origin, and a browser reads a cross-origin
//!   response only when it says `Access-Control-Allow-Origin`, which GHCR's
//!   storage does not. `/flasher/<channel>/<arch>/<file>` serves the GSI's
//!   files of the same release as `/updates/` does, streamed through the
//!   proxy one byte range at a time instead of redirected, and only those.
//! * **CI's build caches.** CI keeps its compiler caches in GHCR as
//!   artifacts split into parts, `losos-ccache:<arch>` for the overlay's
//!   patched GTK and Qt. `/build-cache/<name>/<arch>/<part>` serves one
//!   part, so CI rounds and any other builder fetch them here without a
//!   GHCR login, and with the same names CI pushes them under.
//! * **The active-user count.** `POST /ping` is what each signed-in user's
//!   `losos-ping` timer sends once a day (`nixos/modules/ping.nix`): a
//!   per-user id already hashed with the month, and the architecture. The
//!   proxy adds the id to that month's `HyperLogLog` in Redis and answers with
//!   the policy that turns the browser and search engine choice screens on
//!   once the count is large enough. [`ping`] and [`policy`] are that half.
//!
//! What lives here is every decision the proxy makes: which request is which,
//! what is a valid name, which layer of a manifest answers it, and how a
//! narinfo is rewritten. The JavaScript around it only performs I/O, because
//! `fetch` is the one thing a module with no imports cannot do. So everything
//! that could let a crafted path reach something it should not is in code
//! that `cargo test` runs.
//!
//! # The interface
//!
//! Strings in, strings out, over linear memory: the host calls `alloc`,
//! writes UTF-8, calls a function, and gets back `(ptr << 32) | len` of a
//! string it must hand to `free` after copying. `alloc`'s buffer is consumed
//! by the call it is passed to. No JSON crosses: the host
//! parses the registry's JSON natively and passes plain lines.

use std::fmt::Write;

/// Nix's base-32 alphabet: the digits and lowercase letters without e, o, t, u.
const NIX32: &str = "0123456789abcdfghijklmnpqrsvwxyz";

/// The architectures a release is built for.
const ARCHES: &[&str] = &["x86_64", "aarch64"];

/// What a request maps to.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// A fixed answer: content type and body.
    Static(&'static str, &'static str),
    /// One layer of one OCI artifact in the namespace.
    Layer {
        /// Repository under the namespace, e.g. `nix-cache`.
        repository: &'static str,
        /// Tag of the artifact.
        tag: String,
        /// `org.opencontainers.image.title` of the layer that answers.
        title: String,
        /// How the layer's bytes reach the client.
        kind: Kind,
    },
    /// Nothing here; the reason goes in the 404's body.
    Missing(&'static str),
}

/// The build caches `/build-cache/` serves: the name in the path and the
/// repository under the namespace CI pushes it to.
const BUILD_CACHES: &[(&str, &str)] = &[("losos", "losos-ccache")];

/// One part of a split cache, as CI's `split -d -a 2` names it.
fn is_cache_part(s: &str) -> bool {
    s.strip_prefix("ccache.tar.zst.part-")
        .is_some_and(|n| n.len() == 2 && n.chars().all(|c| c.is_ascii_digit()))
}

/// A content digest: the only algorithm GHCR serves, and a string that goes
/// into a URL path next.
fn is_digest(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

/// How a layer is served.
#[derive(Debug, PartialEq)]
pub enum Kind {
    /// Fetched and rewritten by [`narinfo`], because its `URL:` must point back
    /// here. A narinfo is a few hundred bytes.
    Narinfo,
    /// Redirected to GHCR's signed blob URL, so a NAR or a disk image is never
    /// streamed through the proxy.
    Redirect,
    /// Fetched and streamed through, honouring a byte range, for a browser
    /// that cannot follow a redirect to storage without CORS headers. Each
    /// request is one range the flasher asks for, so none outlasts the
    /// function's time limit.
    Stream,
    /// Fetched and passed through byte for byte, for a small file whose client
    /// may not follow a redirect off the host it was pointed at. Bytes rather
    /// than text because `SHA256SUMS.gpg` is a binary `OpenPGP` signature, which
    /// a decode to UTF-8 and back would corrupt.
    Inline,
}

/// What `/nix-cache-info` says. Priority 30 puts it ahead of a default
/// cache.nixos.org (40) on a machine that has both configured.
const CACHE_INFO: &str = "StoreDir: /nix/store\nWantMassQuery: 1\nPriority: 30\n";

fn is_nix32(s: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&s.len()) && s.chars().all(|c| NIX32.contains(c))
}

/// A store path's hash part: exactly 32 characters of base 32.
fn is_store_hash(s: &str) -> bool {
    is_nix32(s, 32..=32)
}

/// A NAR's file name as `nix copy` writes it: its file hash in base 32 and a
/// compression suffix.
fn is_nar_file(s: &str) -> bool {
    let Some((hash, suffix)) = s.split_once('.') else {
        return false;
    };
    is_nix32(hash, 1..=64) && ["nar", "nar.xz", "nar.zst", "nar.bz2"].contains(&suffix)
}

/// A release channel, such as `nightly` (`losos.channel`), and short
/// enough that `<channel>-<arch>` stays a valid OCI tag.
fn is_channel(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
}

/// A release file name. No slash, no leading dot, nothing a shell or a URL
/// would read twice.
fn is_release_file(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

/// Map a request path to what answers it.
///
/// The path is taken as the client sent it, after the proxy has stripped any
/// query. Percent-escapes are not decoded: no valid name contains one, so a
/// path that has any is simply not found.
#[must_use]
pub fn route(path: &str) -> Route {
    let path = path.strip_prefix('/').unwrap_or(path);
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        ["nix-cache-info"] => Route::Static("text/x-nix-cache-info", CACHE_INFO),
        [file] if file.ends_with(".narinfo") => {
            let hash = file.trim_end_matches(".narinfo");
            if !is_store_hash(hash) {
                return Route::Missing("not a store path hash");
            }
            Route::Layer {
                repository: "nix-cache",
                tag: hash.into(),
                title: (*file).into(),
                kind: Kind::Narinfo,
            }
        }
        ["nar", hash, file] => {
            if !is_store_hash(hash) || !is_nar_file(file) {
                return Route::Missing("not a NAR this cache serves");
            }
            Route::Layer {
                repository: "nix-cache",
                tag: (*hash).into(),
                title: (*file).into(),
                kind: Kind::Redirect,
            }
        }
        ["updates", channel, arch, file] => {
            if !is_channel(channel) || !ARCHES.contains(arch) || !is_release_file(file) {
                return Route::Missing("not a release file");
            }
            // sysupdate reads SHA256SUMS (and its signature) itself and follows
            // no redirect it did not expect; the images are what is large.
            let kind = if file.starts_with("SHA256SUMS") {
                Kind::Inline
            } else {
                Kind::Redirect
            };
            Route::Layer {
                repository: "images",
                tag: format!("{channel}-{arch}"),
                title: (*file).into(),
                kind,
            }
        }
        ["flasher", channel, arch, file] => {
            // The GSI's files and the manifest that hashes them; the rest of
            // a release is for sysupdate and stays a redirect under /updates/.
            if !is_channel(channel)
                || !ARCHES.contains(arch)
                || !is_release_file(file)
                || !(*file == "SHA256SUMS" || file.contains("_gsi-"))
            {
                return Route::Missing("not a file the flasher writes");
            }
            Route::Layer {
                repository: "images",
                tag: format!("{channel}-{arch}"),
                title: (*file).into(),
                kind: if *file == "SHA256SUMS" {
                    Kind::Inline
                } else {
                    Kind::Stream
                },
            }
        }
        ["build-cache", name, arch, file] => {
            let Some((_, repository)) = BUILD_CACHES.iter().find(|(n, _)| n == name) else {
                return Route::Missing("not a build cache");
            };
            if !ARCHES.contains(arch) || !is_cache_part(file) {
                return Route::Missing("not a part of a build cache");
            }
            // Parts are up to 4 GB, so they never pass through the proxy.
            Route::Layer {
                repository,
                tag: (*arch).into(),
                title: (*file).into(),
                kind: Kind::Redirect,
            }
        }
        // `/flatpak/` was a Flatpak remote serving the Uranium Flatpak. It
        // went with Uranium when Danube replaced it (docs/danube.md), and is
        // not coming back.
        _ => Route::Missing("unknown path"),
    }
}

/// Render a [`Route`] as the one-line plan the host reads.
///
/// `static <content-type>\n<body>`, `layer <repository> <tag> <title> <kind>`
/// or `missing <reason>`. Every field of a plan was validated above and
/// contains no space.
#[must_use]
pub fn plan(path: &str) -> String {
    match route(path) {
        Route::Static(content_type, body) => format!("static {content_type}\n{body}"),
        Route::Layer {
            repository,
            tag,
            title,
            kind,
        } => {
            let kind = match kind {
                Kind::Narinfo => "narinfo",
                Kind::Redirect => "redirect",
                Kind::Stream => "stream",
                Kind::Inline => "inline",
            };
            format!("layer {repository} {tag} {title} {kind}")
        }
        Route::Missing(reason) => format!("missing {reason}"),
    }
}

/// Pick the layer titled `title` from a manifest's layers.
///
/// Input: the title on the first line, then one `<digest>\t<title>` per
/// layer. Output: the digest, or the empty string when no layer has that
/// title or its digest is not a sha256 one -- the only algorithm GHCR serves,
/// and a string that goes into a URL path next.
#[must_use]
pub fn pick(input: &str) -> String {
    let mut lines = input.lines();
    let Some(title) = lines.next() else {
        return String::new();
    };
    lines
        .filter_map(|line| line.split_once('\t'))
        .find(|(_, t)| *t == title)
        .map(|(digest, _)| digest)
        .filter(|digest| is_digest(digest))
        .map(String::from)
        .unwrap_or_default()
}

/// Point a narinfo's `URL:` back at this proxy.
///
/// Input: the store hash on the first line, then the narinfo as `nix copy`
/// wrote it. `URL: nar/<file>` becomes `URL: nar/<hash>/<file>`, which is the
/// one thing [`route`] needs to find the artifact again. Nothing else changes:
/// the signature covers the store path, the NAR's hash and size and the
/// references, not the URL, so a rewritten narinfo verifies exactly as the
/// original. Anything unexpected -- no URL, a URL elsewhere, two of them --
/// returns the empty string, and the proxy answers 502 rather than serve it.
#[must_use]
pub fn narinfo(input: &str) -> String {
    let Some((hash, text)) = input.split_once('\n') else {
        return String::new();
    };
    if !is_store_hash(hash) {
        return String::new();
    }
    let mut out = String::with_capacity(text.len() + 40);
    let mut urls = 0;
    for line in text.lines() {
        if let Some(url) = line.strip_prefix("URL: ") {
            urls += 1;
            match url.strip_prefix("nar/") {
                Some(file) if is_nar_file(file) => {
                    let _ = writeln!(out, "URL: nar/{hash}/{file}");
                }
                _ => return String::new(),
            }
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if urls == 1 { out } else { String::new() }
}

/// Days a month's count is kept for: long enough that the previous month
/// can still be read all through the next one, and no longer.
pub const KEEP_MONTH_DAYS: u32 = 100;

/// Days a day's count is kept for: today's and yesterday's are read.
pub const KEEP_DAY_DAYS: u32 = 3;

/// Monthly active end users in the EU at which the choice screens turn on
/// by themselves: where the Digital Markets Act starts treating an
/// operating system as a gatekeeper (Art. 3(2)(b)), which then has to show
/// them (Art. 6(3)).
pub const DEFAULT_THRESHOLD: u64 = 45_000_000;

/// The EU's member states, by ISO 3166-1 alpha-2 code. The DMA counts users
/// "established or located in the Union".
const EU: [&str; 27] = [
    "AT", "BE", "BG", "CY", "CZ", "DE", "DK", "EE", "ES", "FI", "FR", "GR", "HR", "HU", "IE", "IT",
    "LT", "LU", "LV", "MT", "NL", "PL", "PT", "RO", "SE", "SI", "SK",
];

/// The rest of the EEA, which the DMA applies to as well: the choice
/// screens show here too, though the count is the EU's.
const EEA_ONLY: [&str; 3] = ["IS", "LI", "NO"];

/// Where a request comes from, as far as the DMA is concerned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Region {
    /// An EU member state: counted, and shown the screens.
    Eu,
    /// Iceland, Liechtenstein or Norway: shown the screens, not counted.
    Eea,
    /// Anywhere else, or unknown.
    Other,
}

impl Region {
    /// The region of a country code, as the edge network reports it from
    /// the request's address. Only the code is used; the address is not.
    #[must_use]
    pub fn of(country: &str) -> Self {
        let country = country.trim();
        if EU.contains(&country) {
            Self::Eu
        } else if EEA_ONLY.contains(&country) {
            Self::Eea
        } else {
            Self::Other
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Eu => "eu",
            Self::Eea => "eea",
            Self::Other => "other",
        }
    }
}

/// The days and months a ping is counted in, from the server's date.
#[derive(Debug, PartialEq)]
struct Dates {
    day: String,
    yesterday: String,
    month: String,
    last_month: String,
}

/// [`Dates`] for a `YYYY-MM-DD` date, or `None` for anything else.
fn dates(date: &str) -> Option<Dates> {
    let mut parts = date.split('-');
    let (year, mon, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || mon.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, mon, day): (u32, u32, u32) =
        (year.parse().ok()?, mon.parse().ok()?, day.parse().ok()?);
    let days_in = |y: u32, m: u32| match m {
        2 if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=12).contains(&mon) || !(1..=days_in(year, mon)).contains(&day) || year == 0 {
        return None;
    }
    let (last_year, last_mon) = if mon == 1 {
        (year - 1, 12)
    } else {
        (year, mon - 1)
    };
    let yesterday = if day == 1 {
        format!(
            "{last_year:04}-{last_mon:02}-{:02}",
            days_in(last_year, last_mon)
        )
    } else {
        format!("{year:04}-{mon:02}-{:02}", day - 1)
    };
    Some(Dates {
        day: format!("{year:04}-{mon:02}-{day:02}"),
        yesterday,
        month: format!("{year:04}-{mon:02}"),
        last_month: format!("{last_year:04}-{last_mon:02}"),
    })
}

/// Check a ping and say what to count it under.
///
/// Input: the server's date as `YYYY-MM-DD`, the country code the edge
/// network gives for the request (empty if none), then the request body,
/// which is `key=value` lines: `id` (64 lowercase hex digits, the client's
/// SHA-256 of its secret and the month) and `arch`. Unknown keys are
/// ignored, so a newer client can add one without older proxies refusing
/// it.
///
/// Output: `count <id> <region> <adds> <reads>`, or `bad <reason>`. `adds`
/// is the `HyperLogLogs` to add the id to, as `key=seconds to keep`, joined by
/// commas. Every user is counted in the month's total and its
/// architecture's; an EU user also in the month's and the day's EU counts.
/// `reads` is four keys, joined by commas, whose counts [`policy`] wants:
/// this and last month's EU count, today's and yesterday's. The id comes
/// back so that the host adds exactly the string checked here.
#[must_use]
pub fn ping(input: &str) -> String {
    let mut parts = input.splitn(3, '\n');
    let (Some(date), Some(country), Some(body)) = (parts.next(), parts.next(), parts.next()) else {
        return "bad no date".into();
    };
    let Some(Dates {
        day,
        yesterday,
        month,
        last_month,
    }) = dates(date)
    else {
        return "bad date".into();
    };
    let (mut id, mut arch) = (None, None);
    for line in body.lines() {
        match line.split_once('=') {
            Some(("id", v)) => id = Some(v.trim()),
            Some(("arch", v)) => arch = Some(v.trim()),
            _ => {}
        }
    }
    let is_id = |s: &str| s.len() == 64 && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
    let Some(id) = id.filter(|i| is_id(i)) else {
        return "bad id".into();
    };
    let Some(arch) = arch.filter(|a| ARCHES.contains(a)) else {
        return "bad arch".into();
    };
    let region = Region::of(country);
    let (keep_month, keep_day) = (KEEP_MONTH_DAYS * 86_400, KEEP_DAY_DAYS * 86_400);
    let mut adds = format!("actives:{month}={keep_month},actives:{month}:{arch}={keep_month}");
    if region == Region::Eu {
        let _ = write!(adds, ",actives:{month}:eu={keep_month},actives:{day}:eu={keep_day}");
    }
    format!(
        "count {id} {} {adds} actives:{month}:eu,actives:{last_month}:eu,\
         actives:{day}:eu,actives:{yesterday}:eu",
        region.as_str()
    )
}

/// A threshold from the environment: a number, or `default` when it is
/// empty or not one.
fn threshold(text: Option<&str>, default: Option<u64>) -> Option<u64> {
    match text.map(str::trim) {
        Some(t) if !t.is_empty() => t.parse().ok().or(default),
        _ => default,
    }
}

/// The policy a client gets back, as JSON.
///
/// Input: six lines. The mode, `on`, `off` or `auto` (anything else is
/// `auto`), so the screens can be forced either way for testing or by
/// choice; the client's region as [`ping`] named it; the monthly threshold
/// in active EU users ([`DEFAULT_THRESHOLD`] when empty); the daily
/// threshold in EU pings, by distinct user (none when empty); and the
/// monthly and daily EU counts, each empty when there is no store to count
/// in.
///
/// `auto` turns both screens on for a client in the EEA once either count
/// reaches its threshold, and keeps them off while nothing is counted. The
/// region goes back too, so the client knows why.
#[must_use]
pub fn policy(input: &str) -> String {
    let mut lines = input.split('\n');
    let mut next = || lines.next().map(str::trim).filter(|l| !l.is_empty());
    let mode = next().unwrap_or("auto");
    let region = match next() {
        Some("eu") => Region::Eu,
        Some("eea") => Region::Eea,
        _ => Region::Other,
    };
    let monthly_at = threshold(next(), Some(DEFAULT_THRESHOLD));
    let daily_at = threshold(next(), None);
    let monthly = next().and_then(|c| c.parse::<u64>().ok());
    let daily = next().and_then(|c| c.parse::<u64>().ok());
    let reached =
        |count: Option<u64>, at: Option<u64>| count.zip(at).is_some_and(|(count, at)| count >= at);
    let on = match mode {
        "on" => true,
        "off" => false,
        _ => region != Region::Other && (reached(monthly, monthly_at) || reached(daily, daily_at)),
    };
    format!(
        r#"{{"region":"{}","choice_screens":{{"browser":{on},"search":{on}}}}}"#,
        region.as_str()
    )
}

// The linear-memory interface. Only compiled for the target the host loads,
// so `cargo test` exercises the functions above directly.

#[cfg(target_arch = "wasm32")]
mod abi {
    use std::ptr::slice_from_raw_parts_mut;

    /// Every buffer that crosses is a boxed slice, whose allocation is exactly
    /// its length -- so `(ptr, len)` is all either side needs to free it.
    fn leak(bytes: Box<[u8]>) -> (*mut u8, usize) {
        let len = bytes.len();
        (Box::into_raw(bytes).cast::<u8>(), len)
    }

    /// Reserve `len` bytes for the host to write a string into.
    #[unsafe(no_mangle)]
    pub extern "C" fn alloc(len: usize) -> *mut u8 {
        leak(vec![0; len].into_boxed_slice()).0
    }

    /// Release a string a function below returned.
    ///
    /// # Safety
    ///
    /// `ptr` and `len` must be exactly what a function below returned, and
    /// must not be used afterwards.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn free(ptr: *mut u8, len: usize) {
        drop(unsafe { Box::from_raw(slice_from_raw_parts_mut(ptr, len)) });
    }

    /// Take the host's string, run `f`, and hand back the answer.
    ///
    /// Invalid UTF-8 is answered with the empty string, which every caller
    /// already treats as "no".
    unsafe fn call(ptr: *mut u8, len: usize, f: fn(&str) -> String) -> u64 {
        let input = unsafe { Box::from_raw(slice_from_raw_parts_mut(ptr, len)) };
        let output = std::str::from_utf8(&input).map(f).unwrap_or_default();
        let (out_ptr, out_len) = leak(output.into_bytes().into_boxed_slice());
        ((out_ptr as u64) << 32) | out_len as u64
    }

    /// # Safety
    ///
    /// `ptr` and `len` must come from [`alloc`]; ownership passes here.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn plan(ptr: *mut u8, len: usize) -> u64 {
        unsafe { call(ptr, len, super::plan) }
    }

    /// # Safety
    ///
    /// As for [`plan`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pick(ptr: *mut u8, len: usize) -> u64 {
        unsafe { call(ptr, len, super::pick) }
    }

    /// # Safety
    ///
    /// As for [`plan`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn narinfo(ptr: *mut u8, len: usize) -> u64 {
        unsafe { call(ptr, len, super::narinfo) }
    }

    /// # Safety
    ///
    /// As for [`plan`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ping(ptr: *mut u8, len: usize) -> u64 {
        unsafe { call(ptr, len, super::ping) }
    }

    /// # Safety
    ///
    /// As for [`plan`].
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn policy(ptr: *mut u8, len: usize) -> u64 {
        unsafe { call(ptr, len, super::policy) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "0c0x1c0lyb5dh3dkw4mv1bmp8vd6gn8f";
    const NAR: &str = "1w1fff338fvdw53sqgamddn1b2xgds473pv6y13gizdbqjv4i5p3.nar.xz";

    #[test]
    fn cache_info_is_static() {
        assert_eq!(
            plan("/nix-cache-info"),
            format!("static text/x-nix-cache-info\n{CACHE_INFO}")
        );
    }

    #[test]
    fn a_narinfo_and_its_nar_map_to_one_artifact() {
        assert_eq!(
            plan(&format!("/{HASH}.narinfo")),
            format!("layer nix-cache {HASH} {HASH}.narinfo narinfo")
        );
        assert_eq!(
            plan(&format!("/nar/{HASH}/{NAR}")),
            format!("layer nix-cache {HASH} {NAR} redirect")
        );
    }

    #[test]
    fn release_files_come_from_the_moving_tag() {
        assert_eq!(
            plan("/updates/nightly/x86_64/SHA256SUMS"),
            "layer images nightly-x86_64 SHA256SUMS inline"
        );
        assert_eq!(
            plan("/updates/stable/aarch64/losos-desktop_1.2_aarch64.efi"),
            "layer images stable-aarch64 losos-desktop_1.2_aarch64.efi redirect"
        );
    }

    #[test]
    fn the_flasher_streams_only_the_gsi_files() {
        assert_eq!(
            plan("/flasher/nightly/aarch64/SHA256SUMS"),
            "layer images nightly-aarch64 SHA256SUMS inline"
        );
        assert_eq!(
            plan("/flasher/nightly/aarch64/losos-desktop_1.2_gsi-userdata.simg.gz"),
            "layer images nightly-aarch64 losos-desktop_1.2_gsi-userdata.simg.gz stream"
        );
        for path in [
            "/flasher/nightly/aarch64/losos-desktop_1.2_aarch64.raw.xz",
            "/flasher/nightly/aarch64/SHA256SUMS.gpg",
            "/flasher/nightly/riscv64/losos-desktop_1.2_gsi-vbmeta.img",
            "/flasher/nightly/aarch64/../x_gsi-vbmeta.img",
        ] {
            assert!(plan(path).starts_with("missing "), "{path}");
        }
    }

    #[test]
    fn a_build_cache_part_is_a_layer_of_its_arch_tag() {
        assert_eq!(
            plan("/build-cache/losos/x86_64/ccache.tar.zst.part-00"),
            "layer losos-ccache x86_64 ccache.tar.zst.part-00 redirect"
        );
        assert_eq!(
            plan("/build-cache/losos/aarch64/ccache.tar.zst.part-07"),
            "layer losos-ccache aarch64 ccache.tar.zst.part-07 redirect"
        );
    }

    #[test]
    fn crafted_paths_find_nothing() {
        for path in [
            "/",
            "/v2/_catalog",
            "/../etc/passwd.narinfo",
            "/0c0x1c0lyb5dh3dkw4mv1bmp8vd6gn8e.narinfo", // 'e' is not base 32
            "/0c0x1c0lyb5dh3dkw4mv1bmp8vd6gn8.narinfo",  // 31 characters
            &format!("/nar/{HASH}/../x.nar"),
            &format!("/nar/{HASH}/{NAR}/more"),
            &format!("/nar/{HASH}/abc.tar"),
            "/updates/nightly/riscv64/SHA256SUMS",
            "/updates/Nightly/x86_64/SHA256SUMS",
            "/updates/nightly/x86_64/.hidden",
            "/updates/nightly/x86_64/a%2Fb",
            "/updates/nightly%20x/x86_64/SHA256SUMS",
            "/build-cache/other/x86_64/ccache.tar.zst.part-00",
            "/build-cache/losos/riscv64/ccache.tar.zst.part-00",
            "/build-cache/losos/x86_64/ccache.tar.zst.part-0",
            "/build-cache/losos/x86_64/ccache.tar.zst.part-000",
            "/build-cache/losos/x86_64/ccache.tar.zst.part-0a",
            "/build-cache/losos/x86_64/../x86_64/ccache.tar.zst.part-00",
            "/build-cache/losos/x86_64",
            "/build-cache/uranium/x86_64/ccache.tar.zst.part-00",
            "/flatpak/index/static",
            &format!("/flatpak/v2/uranium/blobs/sha256:{}", "a".repeat(64)),
        ] {
            assert!(
                plan(path).starts_with("missing "),
                "{path} -> {}",
                plan(path)
            );
        }
    }

    #[test]
    fn pick_finds_the_titled_sha256_layer() {
        let a = format!("sha256:{}", "a".repeat(64));
        let b = format!("sha256:{}", "b".repeat(64));
        let layers = format!("{NAR}\n{a}\t{HASH}.narinfo\n{b}\t{NAR}\n");
        assert_eq!(pick(&layers), b);
        assert_eq!(pick(&format!("other\n{a}\tx\n")), "");
        assert_eq!(pick("x\nsha512:abc\tx\n"), "");
        assert_eq!(pick(&format!("x\nsha256:{}/../\tx\n", "a".repeat(60))), "");
    }

    #[test]
    fn narinfo_url_points_back_here_and_nothing_else_moves() {
        let original = format!(
            "StorePath: /nix/store/{HASH}-hello-2.12\nURL: nar/{NAR}\nCompression: xz\n\
             NarHash: sha256:x\nNarSize: 1\nReferences: \nSig: k:abc\n"
        );
        let rewritten = narinfo(&format!("{HASH}\n{original}"));
        assert_eq!(
            rewritten,
            original.replace("URL: nar/", &format!("URL: nar/{HASH}/"))
        );
    }

    #[test]
    fn a_narinfo_pointing_elsewhere_is_refused() {
        for url in ["https://evil.example/x.nar", "nar/../x.nar", "nar/a/b.nar"] {
            assert_eq!(narinfo(&format!("{HASH}\nStorePath: x\nURL: {url}\n")), "");
        }
        assert_eq!(narinfo(&format!("{HASH}\nStorePath: x\n")), "");
        assert_eq!(
            narinfo(&format!("{HASH}\nURL: nar/{NAR}\nURL: nar/{NAR}\n")),
            ""
        );
        assert_eq!(narinfo(&format!("bad\nURL: nar/{NAR}\n")), "");
    }

    const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_ping_counts_under_its_day_month_arch_and_region() {
        assert_eq!(
            ping(&format!("2026-10-05\nSK\nid={ID}\narch=x86_64\nnew=1\n")),
            format!(
                "count {ID} eu actives:2026-10=8640000,actives:2026-10:x86_64=8640000,\
                 actives:2026-10:eu=8640000,actives:2026-10-05:eu=259200 \
                 actives:2026-10:eu,actives:2026-09:eu,actives:2026-10-05:eu,actives:2026-10-04:eu"
            )
        );
        assert_eq!(
            ping(&format!("2027-01-01\nNO\narch=aarch64\nid={ID}")),
            format!(
                "count {ID} eea actives:2027-01=8640000,actives:2027-01:aarch64=8640000 \
                 actives:2027-01:eu,actives:2026-12:eu,actives:2027-01-01:eu,actives:2026-12-31:eu"
            )
        );
        assert!(
            ping(&format!("2028-03-01\n\nid={ID}\narch=x86_64"))
                .starts_with(&format!("count {ID} other "))
        );
        assert!(
            ping(&format!("2028-03-01\n\nid={ID}\narch=x86_64")).ends_with("actives:2028-02-29:eu")
        );
        assert!(
            ping(&format!("2100-03-01\n\nid={ID}\narch=x86_64")).ends_with("actives:2100-02-28:eu")
        );
    }

    #[test]
    fn a_crafted_ping_is_refused() {
        for input in [
            format!("2026-10-05\nDE\nid={}\narch=x86_64", ID.to_uppercase()),
            format!("2026-10-05\nDE\nid={}\narch=x86_64", &ID[1..]),
            format!("2026-10-05\nDE\nid={ID} x\narch=x86_64"),
            format!("2026-10-05\nDE\nid={ID}\narch=riscv64"),
            format!("2026-10-05\nDE\nid={ID}"),
            format!("2026-13-01\nDE\nid={ID}\narch=x86_64"),
            format!("2026-02-30\nDE\nid={ID}\narch=x86_64"),
            format!("2026-10\nDE\nid={ID}\narch=x86_64"),
            format!("id={ID}"),
        ] {
            assert!(ping(&input).starts_with("bad "), "{input}");
        }
    }

    #[test]
    fn the_screens_follow_either_eu_count_in_the_eea_unless_forced() {
        let on = |r: &str| {
            format!(r#"{{"region":"{r}","choice_screens":{{"browser":true,"search":true}}}}"#)
        };
        let off = |r: &str| {
            format!(r#"{{"region":"{r}","choice_screens":{{"browser":false,"search":false}}}}"#)
        };
        assert_eq!(policy("auto\neu\n100\n\n99\n"), off("eu"));
        assert_eq!(policy("auto\neu\n100\n\n100\n"), on("eu"));
        assert_eq!(policy("auto\neea\n100\n\n100\n"), on("eea"));
        assert_eq!(
            policy("auto\nother\n100\n\n100\n"),
            off("other"),
            "the DMA is the EEA's"
        );
        assert_eq!(policy("\neu\n\n\n44999999\n"), off("eu"));
        assert_eq!(policy("\neu\n\n\n45000000\n"), on("eu"));
        assert_eq!(policy("auto\neu\n\n10\n5\n9"), off("eu"));
        assert_eq!(
            policy("auto\neu\n\n10\n5\n10"),
            on("eu"),
            "daily pings past theirs"
        );
        assert_eq!(
            policy("auto\neu\n\n\n5\n999999"),
            off("eu"),
            "no daily threshold unless set"
        );
        assert_eq!(
            policy("auto\neu\n0\n0\n\n"),
            off("eu"),
            "nothing counted is not big"
        );
        assert_eq!(policy("on\nother\n\n\n\n"), on("other"));
        assert_eq!(policy("off\neu\n1\n1\n5\n5"), off("eu"));
        assert_eq!(policy("bogus\nfr\nx\n\n46000000\n"), off("other"));
        assert_eq!(policy("bogus\neu\nx\n\n46000000\n"), on("eu"));
    }

    #[test]
    fn regions_come_from_the_country_code() {
        assert_eq!(Region::of("SK"), Region::Eu);
        assert_eq!(Region::of("LI"), Region::Eea);
        assert_eq!(Region::of("CH"), Region::Other);
        assert_eq!(Region::of("GB"), Region::Other);
        assert_eq!(Region::of(""), Region::Other);
        assert_eq!(Region::of("sk"), Region::Other);
    }
}
