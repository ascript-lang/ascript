//! `std/caps` — the opt-out **capability** model (FFI campaign §4).
//!
//! Capabilities are **default-all-granted**: a fresh [`CapSet`] grants every
//! capability, so the central dispatch gate ([`crate::stdlib::required_cap`] +
//! `Interp::require_cap`) is a no-op short-circuit until something is *dropped*.
//! That keeps every existing program byte-identical. Authority is then narrowed
//! — never widened — at three scopes (CLI `--deny`/`--sandbox`, the
//! `ascript.toml` `[capabilities]` table, and the in-code, **irreversible**
//! `caps.drop`).
//!
//! This module is **CORE** (no Cargo feature gate): capabilities exist even in a
//! bare `--no-default-features` build — you can still deny `fs`/`net`/`process`/
//! `ffi`/`env`. The `Cap`/`CapSet`/`FsScope`/`NetScope` types defined here are the
//! security substrate; the `std/caps` script-facing module routing
//! (`has`/`list`/`drop`/`dropAll`) is wired in [`exports`]/[`call`] below.

use crate::value::Value;

/// The five dangerous resource classes. A small, fixed, **closed** set — one per
/// resource class — so the gate's enumeration is total (a new `std/*` module is
/// forced to declare which of these, if any, it requires).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cap {
    /// Filesystem read/write/metadata/listing (`fs`; `io` stdin reads; `os` file ops).
    Fs,
    /// Sockets, HTTP, **DNS**, WebSocket, UDP, servers, net-topology (`os.networkInterfaces`…).
    Net,
    /// Spawning subprocesses (`process`).
    Process,
    /// `ffi.open` and therefore all native calls (`ffi`).
    Ffi,
    /// Reading/writing environment variables (`env`).
    Env,
}

impl Cap {
    /// The single-bit mask of this capability within a [`CapSet`]'s bitset.
    const fn bit(self) -> u8 {
        match self {
            Cap::Fs => 1 << 0,
            Cap::Net => 1 << 1,
            Cap::Process => 1 << 2,
            Cap::Ffi => 1 << 3,
            Cap::Env => 1 << 4,
        }
    }

    /// The lowercase wire name (`"fs"`, `"net"`, …). The inverse of [`cap_name`].
    pub fn name(self) -> &'static str {
        match self {
            Cap::Fs => "fs",
            Cap::Net => "net",
            Cap::Process => "process",
            Cap::Ffi => "ffi",
            Cap::Env => "env",
        }
    }

    /// All five capabilities, in a stable order (used by `caps.list`/`dropAll`).
    pub const ALL: [Cap; 5] = [Cap::Fs, Cap::Net, Cap::Process, Cap::Ffi, Cap::Env];
}

/// The bitmask with every one of the five capability bits set (`0b0001_1111`).
const ALL_BITS: u8 = Cap::Fs.bit()
    | Cap::Net.bit()
    | Cap::Process.bit()
    | Cap::Ffi.bit()
    | Cap::Env.bit();

/// Parse a wire capability name into a [`Cap`], or `None` for an unknown name.
/// Used by CLI `--deny`, the manifest table, and `caps.drop`/`caps.has`.
pub fn cap_name(name: &str) -> Option<Cap> {
    match name {
        "fs" => Some(Cap::Fs),
        "net" => Some(Cap::Net),
        "process" => Some(Cap::Process),
        "ffi" => Some(Cap::Ffi),
        "env" => Some(Cap::Env),
        _ => None,
    }
}

/// The "deny mode" of an `fs` carve-out: deny *writes* only (reads still allowed),
/// or deny *all* fs access except an allow-list of path prefixes (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsDeny {
    /// `deny = "write"` — reads allowed everywhere, writes only under `allow`.
    Write,
    /// `deny = "all"` — neither read nor write except under `allow`.
    All,
}

/// The "deny mode" of a `net` carve-out: deny *external* hosts (loopback/private
/// still allowed), or deny *all* hosts except an allow-list (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetDeny {
    /// `deny = "external"` — loopback/private addresses allowed, public denied except `allow`.
    External,
    /// `deny = "all"` — every host denied except `allow`.
    All,
}

/// Granular `fs` carve-out: deny the class, allow back specific path prefixes
/// (§4.4). Present ONLY when a carve-out is configured — its `Option` on the
/// `CapSet` is the Gate-12 fast-path discriminator (when `None`, the second-stage
/// path check short-circuits with no canonicalization, §4.4).
///
/// Heap-allocated (`Vec`), so a [`CapSet`] holding one is **not** `Copy`; the
/// common case carries `None` and stays `Copy` (see [`CapSet::bits_snapshot`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsScope {
    pub deny: FsDeny,
    /// Allowed path prefixes (canonicalized at check time, prefix-matched).
    pub allow: Vec<String>,
}

impl FsScope {
    /// Stage-2 decision: is operating on `path` (a write iff `is_write`) allowed
    /// under this carve-out? `path` should already be the resolved/canonicalized
    /// path; the allow-list entries are matched as canonicalized prefixes.
    ///
    /// - `deny = "write"`: a READ is always allowed; a WRITE is allowed only under
    ///   an `allow` prefix.
    /// - `deny = "all"`: any access (read or write) is allowed only under an
    ///   `allow` prefix.
    pub fn allows_path(&self, path: &std::path::Path, is_write: bool) -> bool {
        match self.deny {
            FsDeny::Write if !is_write => true, // reads always allowed in write-deny mode
            _ => self.allow.iter().any(|pfx| path_has_prefix(path, pfx)),
        }
    }
}

/// Does `path` lie under the allow-prefix `prefix`? Both are canonicalized as far
/// as possible (a non-existent target canonicalizes by joining onto the nearest
/// existing ancestor) before a component-wise prefix comparison, so `./cache/../x`
/// can't escape an allowed `./cache`.
fn path_has_prefix(path: &std::path::Path, prefix: &str) -> bool {
    let cpath = canonical_lossy(path);
    let cpfx = canonical_lossy(std::path::Path::new(prefix));
    cpath.starts_with(&cpfx)
}

/// Best-effort canonicalization: canonicalize the longest existing ancestor and
/// re-append the rest, so a not-yet-created file still resolves `..`/symlinks in
/// its existing parents. Falls back to the input if nothing canonicalizes.
fn canonical_lossy(p: &std::path::Path) -> std::path::PathBuf {
    if let Ok(c) = p.canonicalize() {
        return c;
    }
    // Walk up to the nearest existing ancestor, canonicalize it, re-join the tail.
    let mut existing = p;
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while let Some(parent) = existing.parent() {
        if let Some(name) = existing.file_name() {
            tail.push(name.to_os_string());
        }
        if parent.exists() {
            if let Ok(c) = parent.canonicalize() {
                let mut out = c;
                for seg in tail.iter().rev() {
                    out.push(seg);
                }
                return out;
            }
        }
        existing = parent;
    }
    p.to_path_buf()
}

/// Granular `net` carve-out: deny the class, allow back specific hosts (§4.4).
/// See [`FsScope`] for the Gate-12 short-circuit rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetScope {
    pub deny: NetDeny,
    /// Allowed host strings (exact host match at connect/bind time).
    pub allow: Vec<String>,
}

impl NetScope {
    /// Stage-2 decision: is connecting to / binding `host` allowed under this
    /// carve-out? `host` is the bare host (no port).
    ///
    /// - `deny = "external"`: loopback / private-range addresses are allowed; a
    ///   public address is allowed only if it is on the `allow` list.
    /// - `deny = "all"`: only `allow`-listed hosts are reachable.
    pub fn allows_host(&self, host: &str) -> bool {
        if self.allow.iter().any(|h| h == host) {
            return true;
        }
        match self.deny {
            NetDeny::External => host_is_loopback_or_private(host),
            NetDeny::All => false,
        }
    }
}

/// Is `host` a loopback or private-range address (allowed under `deny = "external"`)?
/// Recognizes the literal loopback names plus parsed IPv4/IPv6 loopback/private/
/// link-local addresses. A name that is not a recognized private literal is treated
/// as external (the conservative, secure default — an unknown name could resolve
/// anywhere).
fn host_is_loopback_or_private(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    // Strip IPv6 brackets if present.
    let h = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(host);
    if let Ok(ip) = h.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local()
            }
            std::net::IpAddr::V6(v6) => {
                // `is_unique_local`/`is_unicast_link_local` are unstable on stable
                // Rust, so test the well-known prefixes directly: fc00::/7 (ULA) and
                // fe80::/10 (link-local), plus loopback.
                let seg0 = v6.segments()[0];
                v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
            }
        };
    }
    false
}

/// Extract the bare host (no port, no IPv6 brackets) from a `"host:port"`-style
/// address string — the form an allow-list names (§4.4). Mirrors the DNS path's
/// stripping (`net_host.rs`): an IPv6 literal carries multiple colons and keeps its
/// brackets stripped; a single trailing `:port` is removed; a bare host is returned
/// whole. Used by the net stage-2 host check across HTTP/UDP/WS/server (BLOCKER 1).
pub fn host_of_addr(addr: &str) -> &str {
    if let Some(rest) = addr.strip_prefix('[') {
        // `[::1]:8080` → `::1`. Take up to the closing bracket.
        return rest.split(']').next().unwrap_or(rest);
    }
    if addr.chars().filter(|&c| c == ':').count() == 1 {
        // Exactly one colon → `host:port`; strip the port.
        return addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
    }
    // No port, or a bare IPv6 literal (multiple colons, no brackets) → whole.
    addr
}

/// Extract the bare host from a URL string (`http://host:port/path`,
/// `ws://host/...`). Returns `None` for a URL with no parseable authority (a
/// relative URL or a malformed string — the caller treats that as "no host to
/// check", letting the underlying connect surface its own Tier-1 error). Used by
/// the HTTP and WebSocket net stage-2 checks (BLOCKER 1).
pub fn host_of_url(url: &str) -> Option<String> {
    // Find the authority component after `scheme://`.
    let after_scheme = url.split_once("://").map(|(_, rest)| rest)?;
    // Authority ends at the first `/`, `?`, or `#`.
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // Strip any `userinfo@` prefix.
    let hostport = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    if hostport.is_empty() {
        return None;
    }
    Some(host_of_addr(hostport).to_string())
}

/// The strictness order of the two `fs` deny modes: `All` (deny read AND write) is
/// stricter than `Write` (deny writes only). Returns the stricter of `(a, b)`.
fn stricter_fs_deny(a: FsDeny, b: FsDeny) -> FsDeny {
    match (a, b) {
        (FsDeny::All, _) | (_, FsDeny::All) => FsDeny::All,
        _ => FsDeny::Write,
    }
}

/// The strictness order of the two `net` deny modes: `All` (only allow-listed hosts)
/// is stricter than `External` (loopback/private also reachable). Returns the
/// stricter of `(a, b)`.
fn stricter_net_deny(a: NetDeny, b: NetDeny) -> NetDeny {
    match (a, b) {
        (NetDeny::All, _) | (_, NetDeny::All) => NetDeny::All,
        _ => NetDeny::External,
    }
}

/// Intersect two allow-lists: an entry survives only if it appears in BOTH (so a
/// merged carve-out can never allow back something either side denied). Order follows
/// `a` and duplicates are dropped — the verdict is set-membership, so order/dup are
/// not observable.
fn intersect_allow(a: &[String], b: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in a {
        if b.contains(s) && !out.contains(s) {
            out.push(s.clone());
        }
    }
    out
}

/// The more-restrictive `fs` carve-out for `restrict_with`. `None` means the result
/// has NO `fs` carve-out — either because both sides grant `fs` fully (the bit stays
/// set via the bitset intersection) OR because some side denies `fs` outright (the
/// bit is already clear and a denied class carries no allow-list).
fn merge_fs(a: &CapSet, b: &CapSet) -> Option<FsScope> {
    let a_full = a.has(Cap::Fs);
    let b_full = b.has(Cap::Fs);
    match (a_full, &a.fs_scope, b_full, &b.fs_scope) {
        // Both grant fully → no carve-out (bit intersection leaves it granted).
        (true, _, true, _) => None,
        // One grants fully, the other carves out → use the carve-out.
        (true, _, false, Some(s)) | (false, Some(s), true, _) => Some(s.clone()),
        // Both carve out → stricter deny mode + intersected allow-list.
        (false, Some(sa), false, Some(sb)) => Some(FsScope {
            deny: stricter_fs_deny(sa.deny, sb.deny),
            allow: intersect_allow(&sa.allow, &sb.allow),
        }),
        // Some side denies fs outright (no scope) → whole cap denied, no carve-out.
        _ => None,
    }
}

/// The more-restrictive `net` carve-out for `restrict_with` — the `net` mirror of
/// [`merge_fs`].
fn merge_net(a: &CapSet, b: &CapSet) -> Option<NetScope> {
    let a_full = a.has(Cap::Net);
    let b_full = b.has(Cap::Net);
    match (a_full, &a.net_scope, b_full, &b.net_scope) {
        (true, _, true, _) => None,
        (true, _, false, Some(s)) | (false, Some(s), true, _) => Some(s.clone()),
        (false, Some(sa), false, Some(sb)) => Some(NetScope {
            deny: stricter_net_deny(sa.deny, sb.deny),
            allow: intersect_allow(&sa.allow, &sb.allow),
        }),
        _ => None,
    }
}

/// The per-`Interp` capability set: a five-bit grant bitset plus the two optional
/// granular carve-outs for `fs`/`net` (§4.3/§4.4).
///
/// **Default = all granted.** The only mutators **subtract** ([`deny`](CapSet::deny),
/// [`deny_all_dangerous`](CapSet::deny_all_dangerous)) — there is deliberately **no
/// `grant`**, which is the entire security argument: a dropped capability is gone
/// for the life of the (dedicated / top-level) isolate.
///
/// The bitset is a `u8` so a grant test is a `Copy` snapshot read (Gate-12: the
/// hot path never touches the heap `Vec`s — those are reached only when a
/// carve-out is actually configured).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapSet {
    bits: u8,
    /// `Some` only when an `fs` carve-out is configured (§4.4 Gate-12 discriminator).
    pub fs_scope: Option<FsScope>,
    /// `Some` only when a `net` carve-out is configured (§4.4 Gate-12 discriminator).
    pub net_scope: Option<NetScope>,
}

impl Default for CapSet {
    /// All five capabilities granted, no carve-outs — the byte-identical default
    /// every fresh `Interp` starts with.
    fn default() -> Self {
        CapSet::all_granted()
    }
}

impl CapSet {
    /// A set with **every** capability granted and no carve-outs (the default).
    pub const fn all_granted() -> Self {
        CapSet {
            bits: ALL_BITS,
            fs_scope: None,
            net_scope: None,
        }
    }

    /// Is `cap` granted (its bit still set)? A `Copy` bitset read — the Gate-12
    /// hot path. NOTE: a *granular-configured* capability whose bit is set is
    /// "granted-outright at the dispatch site, defer to the second stage"; the
    /// `Some(scope)` discriminator is what tells the gate to defer (§4.4).
    pub const fn has(&self, cap: Cap) -> bool {
        self.bits & cap.bit() != 0
    }

    /// **Subtract** `cap` from the set (clear its bit). Monotone and idempotent:
    /// denying an already-denied capability is a no-op. There is **no inverse**.
    /// Dropping a capability also clears any granular carve-out it carried (a
    /// fully-denied class has no allow-list to honor — the carve-out only made
    /// sense while the class was otherwise reachable).
    pub fn deny(&mut self, cap: Cap) {
        self.bits &= !cap.bit();
        match cap {
            Cap::Fs => self.fs_scope = None,
            Cap::Net => self.net_scope = None,
            _ => {}
        }
    }

    /// Deny **all five** dangerous capabilities (`--sandbox` / `caps.dropAll`).
    /// Clears the carve-outs too (the class is gone). Monotone.
    pub fn deny_all_dangerous(&mut self) {
        for cap in Cap::ALL {
            self.deny(cap);
        }
    }

    /// Install an `fs` carve-out: deny the class but allow back the listed path
    /// prefixes. The bit is **cleared** (the class is denied-outright) while the
    /// scope provides the allow-list the second-stage check consults (§4.4).
    pub fn set_fs_scope(&mut self, scope: FsScope) {
        self.bits &= !Cap::Fs.bit();
        self.fs_scope = Some(scope);
    }

    /// Install a `net` carve-out (deny the class, allow back the listed hosts).
    pub fn set_net_scope(&mut self, scope: NetScope) {
        self.bits &= !Cap::Net.bit();
        self.net_scope = Some(scope);
    }

    /// The list of currently-granted capability names, in stable order — backs
    /// `caps.list()`. A capability with a carve-out is NOT "granted" here (its bit
    /// is cleared); only an outright-granted capability is listed.
    pub fn granted_names(&self) -> Vec<&'static str> {
        Cap::ALL
            .iter()
            .filter(|c| self.has(**c))
            .map(|c| c.name())
            .collect()
    }

    /// A cheap `Copy` snapshot of the grant bitset for the no-carve-out hot path.
    /// (The full `CapSet` is `!Copy` because of the heap `Vec`s, but the bitset —
    /// all the dispatch-site gate needs in the common case — is `Copy`.)
    pub const fn bits_snapshot(&self) -> CapBits {
        CapBits(self.bits)
    }

    /// The dispatch-site decision for `cap` (§4.3/§4.4): the gate consults this to
    /// decide allow / deny-now / defer-to-stage-2.
    ///
    /// - bit set → [`CapDecision::Allow`] (granted-outright).
    /// - bit cleared AND a granular carve-out IS configured for this cap →
    ///   [`CapDecision::Defer`] (the second stage at the connect/bind / fs-path
    ///   entry re-checks the resolved host/path).
    /// - bit cleared and NO carve-out → [`CapDecision::Deny`] (denied-outright).
    ///
    /// Gate-12: only `fs`/`net` can ever `Defer`; every other cap is bit-only, so
    /// its decision is the cheap `Allow`/`Deny`. And `Defer` requires a `Some`
    /// scope — when the scope is `None` (the default and the all-deny/all-grant
    /// cases) the decision is conclusive here with no canonicalization downstream.
    pub fn dispatch_decision(&self, cap: Cap) -> CapDecision {
        if self.has(cap) {
            return CapDecision::Allow;
        }
        match cap {
            Cap::Fs if self.fs_scope.is_some() => CapDecision::Defer,
            Cap::Net if self.net_scope.is_some() => CapDecision::Defer,
            _ => CapDecision::Deny,
        }
    }

    /// Compose this set with `other` by **monotone intersection** (BNDL N4): the
    /// result grants a capability ONLY IF BOTH sets grant it — neither side can
    /// re-grant what the other denied. This is the security property of an embedded
    /// capability floor: `archive.caps.restrict_with(&cli_caps)` enforces the
    /// build-time restriction AND any run-time `--deny`, and a run-time flag can only
    /// ever narrow further, never widen.
    ///
    /// Per whole-cap bit: granted in the result iff granted in BOTH (`bits & bits`).
    /// For the `fs`/`net` carve-outs the result is the **more restrictive** of the
    /// two sides (so a carve-out never widens access):
    /// - either side denies the whole cap (bit clear, no scope) → the result denies
    ///   the whole cap (no scope; over-restriction is sound);
    /// - one side grants the cap fully and the other carries a scope → the result
    ///   uses that scope;
    /// - both carry a scope → the **stricter** deny-mode and the **intersection** of
    ///   allow-lists; if the deny-modes are incomparable (they are totally ordered
    ///   here, so this never fires) the whole cap is denied.
    ///
    /// Monotone, commutative on the verdict, and idempotent (`x.restrict_with(&x) == x`).
    pub fn restrict_with(&self, other: &CapSet) -> CapSet {
        let mut out = CapSet {
            // Whole-cap intersection: granted iff granted in BOTH.
            bits: self.bits & other.bits,
            fs_scope: None,
            net_scope: None,
        };
        // fs carve-out: the more restrictive of the two sides.
        if let Some(scope) = merge_fs(self, other) {
            out.set_fs_scope(scope);
        }
        // net carve-out: the more restrictive of the two sides.
        if let Some(scope) = merge_net(self, other) {
            out.set_net_scope(scope);
        }
        out
    }

    /// Build a `CapSet` by denying every name in `names` (CLI `--deny a,b` /
    /// manifest `deny = [...]`). An unknown name is a clean `Err(name)` — never a
    /// panic — so a hostile manifest/CLI input is rejected, not crashed.
    pub fn from_deny_list<I, S>(names: I) -> Result<CapSet, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = CapSet::all_granted();
        for name in names {
            let name = name.as_ref();
            match cap_name(name) {
                Some(cap) => set.deny(cap),
                None => return Err(name.to_string()),
            }
        }
        Ok(set)
    }

    // ─────────────────── Module-archive serialization (BNDL §5) ───────────────
    // The build-time CapSet (its `bits` plus the variable-length `fs_scope`/
    // `net_scope` carve-outs) is embedded in a module-archive manifest — the
    // variable-length home the fixed 32-byte bundle footer cannot provide. The
    // decoder runs over UNTRUSTED bytes (a tampered archive), so it is fully
    // bounds-checked and allocation-bomb-capped: it returns a clean error and
    // NEVER panics or indexes out of range.

    /// Serialize this `CapSet` to a self-describing byte vector for embedding in a
    /// module-archive manifest (BNDL §5). Little-endian length prefixes.
    ///
    /// Layout:
    /// - `bits: u8`
    /// - `fs_scope`: presence `u8` (0 = `None`, 1 = `Some`); if present, a `deny`
    ///   mode byte then a `u16` count of allowed prefixes, each a `u32`-len-prefixed
    ///   UTF-8 string.
    /// - `net_scope`: same shape, with allowed hosts.
    ///
    /// Written by **destructuring** `self` so a future `CapSet` field is a compile
    /// error here until it is handled — the serializer can never silently drop a
    /// field.
    pub fn to_bytes(&self) -> Vec<u8> {
        // Exhaustiveness: destructure so a new field fails to compile here.
        let CapSet {
            bits,
            fs_scope,
            net_scope,
        } = self;

        let mut out = Vec::new();
        out.push(*bits);
        write_fs_scope(&mut out, fs_scope.as_ref());
        write_net_scope(&mut out, net_scope.as_ref());
        out
    }

    /// Parse a `CapSet` from the front of `b`, returning the decoded value and the
    /// number of bytes consumed (so trailing manifest data is allowed). Every read
    /// is bounds-checked and every count/length is capped — malformed or hostile
    /// input yields a [`CapsDecodeError`], never a panic.
    pub fn from_bytes(b: &[u8]) -> Result<(CapSet, usize), CapsDecodeError> {
        let mut cur = Cursor::new(b);
        let bits = cur.u8()?;
        let fs_scope = read_fs_scope(&mut cur)?;
        let net_scope = read_net_scope(&mut cur)?;
        Ok((
            CapSet {
                bits,
                fs_scope,
                net_scope,
            },
            cur.pos,
        ))
    }
}

/// These cap the decoder's allocation against a tampered archive. An `fs`/`net`
/// carve-out is human-authored config, so both limits sit far above any legitimate
/// value while staying small enough to bound a hostile decode.
///
/// `MAX_ENTRIES` — a real allow-list is O(10) entries; 4096 is generous yet stays
/// **below `u16::MAX` (65535)**, so the serialized `u16` count field can never
/// overflow for a legitimately-constructed carve-out.
const MAX_ENTRIES: usize = 4096;
/// `MAX_STRING_LEN` — a real path prefix / host is O(200) bytes; 64 KiB is far above
/// any legitimate entry while capping the per-string decode allocation.
const MAX_STRING_LEN: usize = 64 * 1024;

/// Mode wire bytes (kept local to the (de)serializer so the on-disk encoding is a
/// single source of truth, decoupled from any future enum reordering).
const FS_DENY_WRITE: u8 = 0;
const FS_DENY_ALL: u8 = 1;
const NET_DENY_EXTERNAL: u8 = 0;
const NET_DENY_ALL: u8 = 1;

/// An error decoding a serialized [`CapSet`] from (possibly hostile) archive bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapsDecodeError {
    /// The input ended before a required field could be read.
    Truncated,
    /// A presence byte was neither 0 (`None`) nor 1 (`Some`).
    InvalidPresence(u8),
    /// A deny-mode byte did not name a known mode.
    InvalidMode(u8),
    /// An allow-list entry count exceeded [`MAX_ENTRIES`].
    CountTooLarge(usize),
    /// A string length exceeded [`MAX_STRING_LEN`].
    StringTooLong(usize),
    /// An allow-list entry was not valid UTF-8.
    InvalidUtf8,
}

impl std::fmt::Display for CapsDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapsDecodeError::Truncated => write!(f, "truncated capability-set encoding"),
            CapsDecodeError::InvalidPresence(b) => {
                write!(f, "invalid carve-out presence byte {b} (expected 0 or 1)")
            }
            CapsDecodeError::InvalidMode(b) => write!(f, "invalid deny-mode byte {b}"),
            CapsDecodeError::CountTooLarge(n) => {
                write!(f, "allow-list entry count {n} exceeds the maximum {MAX_ENTRIES}")
            }
            CapsDecodeError::StringTooLong(n) => {
                write!(f, "allow-list string length {n} exceeds the maximum {MAX_STRING_LEN}")
            }
            CapsDecodeError::InvalidUtf8 => write!(f, "allow-list entry is not valid UTF-8"),
        }
    }
}

impl std::error::Error for CapsDecodeError {}

/// A bounds-checked forward reader over a byte slice. Every accessor advances `pos`
/// only after verifying the read fits, so an out-of-range read is a clean
/// [`CapsDecodeError::Truncated`], never a slice panic.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Cursor { buf, pos: 0 }
    }

    /// Borrow the next `n` bytes (checked), advancing past them.
    fn take(&mut self, n: usize) -> Result<&'a [u8], CapsDecodeError> {
        let end = self.pos.checked_add(n).ok_or(CapsDecodeError::Truncated)?;
        let slice = self.buf.get(self.pos..end).ok_or(CapsDecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, CapsDecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CapsDecodeError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, CapsDecodeError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Read a `u32`-length-prefixed UTF-8 string, capped at [`MAX_STRING_LEN`].
    fn string(&mut self) -> Result<String, CapsDecodeError> {
        let len = self.u32()? as usize;
        if len > MAX_STRING_LEN {
            return Err(CapsDecodeError::StringTooLong(len));
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(|s| s.to_string())
            .map_err(|_| CapsDecodeError::InvalidUtf8)
    }

    /// Read a `u16`-count then that many `u32`-length-prefixed strings. The count is
    /// capped at [`MAX_ENTRIES`] BEFORE any reservation, so a hostile count can't
    /// trigger an allocation bomb.
    fn string_list(&mut self) -> Result<Vec<String>, CapsDecodeError> {
        let count = self.u16()? as usize;
        if count > MAX_ENTRIES {
            return Err(CapsDecodeError::CountTooLarge(count));
        }
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.string()?);
        }
        Ok(out)
    }
}

/// Append a `u32`-length-prefixed UTF-8 string.
fn write_string(out: &mut Vec<u8>, s: &str) {
    // Lengths are capped on decode; on encode a legitimately-authored allow-list is
    // always well under the cap, so a plain truncating cast is safe in practice and
    // the decoder rejects anything pathological.
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// Append a `u16`-count then each string in `list`.
fn write_string_list(out: &mut Vec<u8>, list: &[String]) {
    // MAX_ENTRIES (4096) << u16::MAX (65535), so a legitimately-constructed allow-list
    // never overflows; the decoder rejects counts > MAX_ENTRIES anyway.
    debug_assert!(list.len() <= u16::MAX as usize, "allow-list too long for u16 count");
    out.extend_from_slice(&(list.len() as u16).to_le_bytes());
    for s in list {
        write_string(out, s);
    }
}

fn write_fs_scope(out: &mut Vec<u8>, scope: Option<&FsScope>) {
    match scope {
        None => out.push(0),
        Some(s) => {
            // Destructure for exhaustiveness — a new FsScope field breaks here.
            let FsScope { deny, allow } = s;
            out.push(1);
            out.push(match deny {
                FsDeny::Write => FS_DENY_WRITE,
                FsDeny::All => FS_DENY_ALL,
            });
            write_string_list(out, allow);
        }
    }
}

fn write_net_scope(out: &mut Vec<u8>, scope: Option<&NetScope>) {
    match scope {
        None => out.push(0),
        Some(s) => {
            let NetScope { deny, allow } = s;
            out.push(1);
            out.push(match deny {
                NetDeny::External => NET_DENY_EXTERNAL,
                NetDeny::All => NET_DENY_ALL,
            });
            write_string_list(out, allow);
        }
    }
}

fn read_fs_scope(cur: &mut Cursor) -> Result<Option<FsScope>, CapsDecodeError> {
    match cur.u8()? {
        0 => Ok(None),
        1 => {
            let deny = match cur.u8()? {
                FS_DENY_WRITE => FsDeny::Write,
                FS_DENY_ALL => FsDeny::All,
                other => return Err(CapsDecodeError::InvalidMode(other)),
            };
            let allow = cur.string_list()?;
            Ok(Some(FsScope { deny, allow }))
        }
        other => Err(CapsDecodeError::InvalidPresence(other)),
    }
}

fn read_net_scope(cur: &mut Cursor) -> Result<Option<NetScope>, CapsDecodeError> {
    match cur.u8()? {
        0 => Ok(None),
        1 => {
            let deny = match cur.u8()? {
                NET_DENY_EXTERNAL => NetDeny::External,
                NET_DENY_ALL => NetDeny::All,
                other => return Err(CapsDecodeError::InvalidMode(other)),
            };
            let allow = cur.string_list()?;
            Ok(Some(NetScope { deny, allow }))
        }
        other => Err(CapsDecodeError::InvalidPresence(other)),
    }
}

/// The dispatch-site verdict for a capability (§4.4). `Defer` is only ever
/// produced for `fs`/`net` with a configured carve-out — see
/// [`CapSet::dispatch_decision`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapDecision {
    /// Granted outright — proceed (the common case).
    Allow,
    /// Denied outright — raise the recoverable denial panic now.
    Deny,
    /// A granular carve-out is configured — pass the dispatch gate and let the
    /// connect/bind / fs-path stage-2 check enforce the allow-list.
    Defer,
}

/// A `Copy` snapshot of a [`CapSet`]'s grant bitset — what the dispatch-site gate
/// reads (never holding the `caps` `RefCell` borrow across an `.await`). Carries no
/// carve-out info (that lives in the `!Copy` `CapSet`); a granular-configured
/// capability reads here as **denied** at the bit level, which is exactly the
/// "denied-outright OR defer-to-second-stage" trigger the gate wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapBits(u8);

impl CapBits {
    /// Are ALL five capabilities granted? The Gate-12 single-flag short-circuit:
    /// when true the gate is provably a no-op and returns without any per-cap
    /// lookup. This is the zero-cost default path.
    pub const fn all_granted(self) -> bool {
        self.0 == ALL_BITS
    }

    /// Is `cap`'s bit set in this snapshot?
    pub const fn has(self, cap: Cap) -> bool {
        self.0 & cap.bit() != 0
    }
}

// ─────────────────────────── `std/caps` module routing ───────────────────────
// CORE (no feature gate): the capability query/drop surface exists in every build.
// `import * as caps from "std/caps"` then `caps.has(...)` / `caps.drop(...)`.

use crate::error::AsError;
use crate::interp::{Control, Interp};
use crate::span::Span;

/// `std/caps` exports — flat names so `import * as caps` binds `caps.has` etc.
/// All four register in `std_arity.rs` (the drift-guard cross-checks exports).
pub fn exports() -> Vec<(&'static str, Value)> {
    vec![
        ("has", super::bi("caps.has")),
        ("list", super::bi("caps.list")),
        ("drop", super::bi("caps.drop")),
        ("dropAll", super::bi("caps.dropAll")),
    ]
}

impl Interp {
    /// `std/caps` dispatch (`&self` — `drop`/`dropAll` mutate `Interp.caps`).
    /// Note: the `caps` module is NOT gated by `required_cap` (querying/dropping
    /// authority is always permitted — you can only ever NARROW).
    pub(crate) async fn call_caps(
        &self,
        func: &str,
        args: &[Value],
        span: Span,
    ) -> Result<Value, Control> {
        match func {
            // caps.has(name) -> bool. Unknown name → Tier-2 panic (programmer error).
            "has" => {
                let name = super::want_string(&super::arg(args, 0), span, "caps.has")?;
                let cap = parse_cap_name(&name, span)?;
                // Granted iff the bit is set OR a carve-out is configured (a
                // carve-out means "partially granted", so `has` reports true).
                let caps = self.caps();
                let granted = caps.has(cap)
                    || matches!(cap, Cap::Fs if caps.fs_scope.is_some())
                    || matches!(cap, Cap::Net if caps.net_scope.is_some());
                Ok(Value::Bool(granted))
            }
            // caps.list() -> array<string> — currently outright-granted caps.
            "list" => {
                let names = self.caps().granted_names();
                let arr: Vec<Value> = names.into_iter().map(|n| Value::Str(n.into())).collect();
                Ok(Value::Array(crate::value::ArrayCell::new(arr)))
            }
            // caps.drop(name) -> nil — IRREVERSIBLE subtraction. Refused in a pooled
            // worker fn (§4.5a). Unknown name → Tier-2 panic.
            "drop" => {
                let name = super::want_string(&super::arg(args, 0), span, "caps.drop")?;
                let cap = parse_cap_name(&name, span)?;
                self.guard_drop_allowed("caps.drop", span)?;
                self.caps_deny(cap);
                Ok(Value::Nil)
            }
            // caps.dropAll() -> nil — drop all five. Same refusal rule.
            "dropAll" => {
                self.guard_drop_allowed("caps.dropAll", span)?;
                self.caps_deny_all();
                Ok(Value::Nil)
            }
            _ => Err(AsError::at(format!("std/caps has no function '{func}'"), span).into()),
        }
    }

    /// §4.5a: a `caps.drop`/`dropAll` is REFUSED (loud recoverable Tier-2 panic)
    /// when the isolate forbids dropping — i.e. a POOLED `worker fn`, whose shared
    /// `Interp` is reused across requests, so a drop would leak forward / require a
    /// re-grant. Durable only on the top-level program and a dedicated isolate.
    fn guard_drop_allowed(&self, op: &str, span: Span) -> Result<(), Control> {
        if self.caps_drop_allowed() {
            Ok(())
        } else {
            Err(Control::Panic(AsError::at(
                format!(
                    "{op} is not allowed inside a pooled worker fn (a shared isolate \
                     is reused across requests; drop capabilities in a dedicated \
                     isolate via run_in_worker or at the top level instead)"
                ),
                span,
            )))
        }
    }
}

/// Parse a capability name arg, raising a Tier-2 panic on an unknown name
/// (programmer error — not Tier-1 data). Shared by `has`/`drop`.
fn parse_cap_name(name: &str, span: Span) -> Result<Cap, Control> {
    cap_name(name).ok_or_else(|| {
        Control::Panic(AsError::at(
            format!(
                "unknown capability '{name}' (expected one of: fs, net, process, ffi, env)"
            ),
            span,
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_all_granted() {
        let cs = CapSet::all_granted();
        for cap in Cap::ALL {
            assert!(cs.has(cap), "{} should be granted by default", cap.name());
        }
        assert_eq!(CapSet::default(), CapSet::all_granted());
        assert!(cs.bits_snapshot().all_granted());
    }

    #[test]
    fn deny_clears_only_that_cap() {
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Ffi);
        assert!(!cs.has(Cap::Ffi), "ffi denied");
        for cap in [Cap::Fs, Cap::Net, Cap::Process, Cap::Env] {
            assert!(cs.has(cap), "{} still granted", cap.name());
        }
        assert!(!cs.bits_snapshot().all_granted(), "snapshot reflects the drop");
        assert!(!cs.bits_snapshot().has(Cap::Ffi));
        assert!(cs.bits_snapshot().has(Cap::Net));
    }

    #[test]
    fn deny_is_monotone_and_idempotent() {
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Net);
        cs.deny(Cap::Net); // again — no-op
        assert!(!cs.has(Cap::Net));
        // No widening API exists: the only mutators are `deny`/`deny_all_dangerous`/
        // `set_*_scope` (all subtractive). This is asserted structurally — there is
        // no `grant` method to call. (A compile-time guarantee, restated for intent.)
    }

    #[test]
    fn deny_all_dangerous_clears_all_five() {
        let mut cs = CapSet::all_granted();
        cs.deny_all_dangerous();
        for cap in Cap::ALL {
            assert!(!cs.has(cap), "{} should be denied", cap.name());
        }
        assert!(cs.granted_names().is_empty());
    }

    #[test]
    fn carve_out_fields_default_to_none() {
        let cs = CapSet::all_granted();
        assert!(cs.fs_scope.is_none());
        assert!(cs.net_scope.is_none());
    }

    #[test]
    fn from_deny_list_parses_and_rejects_unknown() {
        let cs = CapSet::from_deny_list(["ffi", "process"]).unwrap();
        assert!(!cs.has(Cap::Ffi));
        assert!(!cs.has(Cap::Process));
        assert!(cs.has(Cap::Fs) && cs.has(Cap::Net) && cs.has(Cap::Env));

        let err = CapSet::from_deny_list(["ffi", "bogus"]).unwrap_err();
        assert_eq!(err, "bogus");
    }

    #[test]
    fn granted_names_reflects_drops() {
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Process);
        cs.deny(Cap::Env);
        let names = cs.granted_names();
        assert_eq!(names, vec!["fs", "net", "ffi"]);
    }

    #[test]
    fn capset_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<CapSet>();
        assert_send::<CapBits>();
    }

    #[test]
    fn capbits_is_copy() {
        // CapBits is the Copy snapshot the gate reads across an await boundary.
        fn assert_copy<T: Copy>() {}
        assert_copy::<CapBits>();
        assert_copy::<Cap>();
    }

    #[test]
    fn net_scope_external_allows_loopback_blocks_public() {
        let scope = NetScope {
            deny: NetDeny::External,
            allow: vec!["api.internal".into()],
        };
        // Loopback / private are allowed under deny="external".
        assert!(scope.allows_host("localhost"));
        assert!(scope.allows_host("127.0.0.1"));
        assert!(scope.allows_host("10.0.0.5"));
        assert!(scope.allows_host("192.168.1.1"));
        assert!(scope.allows_host("::1"));
        assert!(scope.allows_host("fc00::1")); // ULA
        assert!(scope.allows_host("fe80::1")); // link-local
        // An allow-listed host is carved back in.
        assert!(scope.allows_host("api.internal"));
        // A public address / unknown name is blocked.
        assert!(!scope.allows_host("8.8.8.8"));
        assert!(!scope.allows_host("example.com"));
        assert!(!scope.allows_host("evil.test"));
    }

    #[test]
    fn net_scope_all_only_allows_listed() {
        let scope = NetScope {
            deny: NetDeny::All,
            allow: vec!["127.0.0.1".into()],
        };
        assert!(scope.allows_host("127.0.0.1")); // explicitly allowed
        assert!(!scope.allows_host("localhost")); // loopback NOT auto-allowed under "all"
        assert!(!scope.allows_host("10.0.0.1"));
        assert!(!scope.allows_host("example.com"));
    }

    #[test]
    fn fs_scope_write_deny_allows_reads_blocks_writes_outside_allow() {
        let dir = std::env::temp_dir();
        let allowed = dir.join("ascript_caps_cache");
        std::fs::create_dir_all(&allowed).ok();
        let scope = FsScope {
            deny: FsDeny::Write,
            allow: vec![allowed.to_string_lossy().to_string()],
        };
        // A read anywhere is allowed in write-deny mode.
        assert!(scope.allows_path(std::path::Path::new("/etc/hosts"), false));
        // A write under the allowed subtree is allowed.
        assert!(scope.allows_path(&allowed.join("x.txt"), true));
        // A write OUTSIDE the allowed subtree is blocked.
        assert!(!scope.allows_path(&dir.join("other.txt"), true));
        std::fs::remove_dir_all(&allowed).ok();
    }

    #[test]
    fn fs_scope_all_deny_blocks_reads_and_writes_outside_allow() {
        let dir = std::env::temp_dir();
        let allowed = dir.join("ascript_caps_all");
        std::fs::create_dir_all(&allowed).ok();
        let scope = FsScope {
            deny: FsDeny::All,
            allow: vec![allowed.to_string_lossy().to_string()],
        };
        // Read under allow → ok; read outside → blocked.
        assert!(scope.allows_path(&allowed.join("a"), false));
        assert!(!scope.allows_path(&dir.join("nope"), false));
        // `..` escape from an allowed prefix is blocked (canonicalization).
        assert!(!scope.allows_path(&allowed.join("../escape"), false));
        std::fs::remove_dir_all(&allowed).ok();
    }

    #[test]
    fn dispatch_decision_allow_deny_defer() {
        // Granted outright → Allow.
        let cs = CapSet::all_granted();
        assert_eq!(cs.dispatch_decision(Cap::Net), CapDecision::Allow);
        // Denied outright (no carve-out) → Deny.
        let mut denied = CapSet::all_granted();
        denied.deny(Cap::Net);
        assert_eq!(denied.dispatch_decision(Cap::Net), CapDecision::Deny);
        // Carve-out configured → Defer (stage 2 enforces).
        let mut carved = CapSet::all_granted();
        carved.set_net_scope(NetScope {
            deny: NetDeny::External,
            allow: vec![],
        });
        assert_eq!(carved.dispatch_decision(Cap::Net), CapDecision::Defer);
        // fs mirror.
        let mut fscarved = CapSet::all_granted();
        fscarved.set_fs_scope(FsScope {
            deny: FsDeny::Write,
            allow: vec![],
        });
        assert_eq!(fscarved.dispatch_decision(Cap::Fs), CapDecision::Defer);
        // A non-granular cap with the bit cleared is always Deny (never Defer).
        let mut ffi = CapSet::all_granted();
        ffi.deny(Cap::Ffi);
        assert_eq!(ffi.dispatch_decision(Cap::Ffi), CapDecision::Deny);
    }

    // ─────────────────────── `std/caps` module routing (Task 4) ──────────────
    use crate::interp::{Control, Interp};
    use crate::span::Span;

    fn span() -> Span {
        Span::new(0, 0)
    }

    #[tokio::test]
    async fn caps_has_reports_grant_and_drop() {
        let interp = Interp::new();
        // Default: everything granted.
        let r = interp
            .call_caps("has", &[Value::Str("net".into())], span())
            .await
            .unwrap();
        assert_eq!(r, Value::Bool(true));
        // Drop net → has("net") becomes false.
        interp
            .call_caps("drop", &[Value::Str("net".into())], span())
            .await
            .unwrap();
        let r = interp
            .call_caps("has", &[Value::Str("net".into())], span())
            .await
            .unwrap();
        assert_eq!(r, Value::Bool(false));
        // Other caps untouched.
        let r = interp
            .call_caps("has", &[Value::Str("fs".into())], span())
            .await
            .unwrap();
        assert_eq!(r, Value::Bool(true));
    }

    #[tokio::test]
    async fn caps_list_reflects_grants() {
        let interp = Interp::new();
        interp
            .call_caps("drop", &[Value::Str("ffi".into())], span())
            .await
            .unwrap();
        let list = interp.call_caps("list", &[], span()).await.unwrap();
        if let Value::Array(a) = list {
            let names: Vec<String> = a
                .borrow()
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.to_string(),
                    _ => panic!("expected strings"),
                })
                .collect();
            assert_eq!(names, vec!["fs", "net", "process", "env"]); // ffi dropped
        } else {
            panic!("caps.list should return an array");
        }
    }

    #[tokio::test]
    async fn caps_drop_is_irreversible_no_regrant_api() {
        let interp = Interp::new();
        interp
            .call_caps("drop", &[Value::Str("process".into())], span())
            .await
            .unwrap();
        // Stays false — there is NO grant function to call (caps.call has no "grant"
        // arm; the only mutators are drop/dropAll). Dropping again is idempotent.
        interp
            .call_caps("drop", &[Value::Str("process".into())], span())
            .await
            .unwrap();
        let r = interp
            .call_caps("has", &[Value::Str("process".into())], span())
            .await
            .unwrap();
        assert_eq!(r, Value::Bool(false));
        // A bogus func name (e.g. "grant") errors — no re-grant path exists.
        let no_grant = interp.call_caps("grant", &[Value::Str("process".into())], span()).await;
        assert!(no_grant.is_err(), "there must be no caps.grant");
    }

    #[tokio::test]
    async fn caps_drop_all_clears_everything() {
        let interp = Interp::new();
        interp.call_caps("dropAll", &[], span()).await.unwrap();
        let list = interp.call_caps("list", &[], span()).await.unwrap();
        if let Value::Array(a) = list {
            assert!(a.borrow().is_empty(), "dropAll should leave no grants");
        } else {
            panic!("list should be an array");
        }
    }

    #[tokio::test]
    async fn caps_unknown_name_is_tier2_panic() {
        let interp = Interp::new();
        match interp.call_caps("has", &[Value::Str("bogus".into())], span()).await {
            Err(Control::Panic(e)) => assert!(e.message.contains("unknown capability"), "{}", e.message),
            other => panic!("expected unknown-cap panic, got {other:?}"),
        }
        match interp.call_caps("drop", &[Value::Str("nope".into())], span()).await {
            Err(Control::Panic(e)) => assert!(e.message.contains("unknown capability"), "{}", e.message),
            other => panic!("expected unknown-cap panic, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn caps_drop_refused_in_pooled_worker() {
        let interp = Interp::new();
        // Simulate a pooled worker isolate: drops are refused.
        interp.set_caps_drop_allowed(false);
        match interp.call_caps("drop", &[Value::Str("net".into())], span()).await {
            Err(Control::Panic(e)) => {
                assert!(e.message.contains("pooled worker"), "{}", e.message)
            }
            other => panic!("expected pooled-drop refusal, got {other:?}"),
        }
        // The refusal must NOT have mutated caps: net is still granted.
        let r = interp
            .call_caps("has", &[Value::Str("net".into())], span())
            .await
            .unwrap();
        assert_eq!(r, Value::Bool(true), "a refused drop must not mutate caps");
        // dropAll is likewise refused.
        assert!(interp.call_caps("dropAll", &[], span()).await.is_err());
    }

    #[tokio::test]
    async fn caps_drop_durable_on_top_level_isolate() {
        let interp = Interp::new();
        assert!(interp.caps_drop_allowed(), "top-level isolate allows drops");
        interp
            .call_caps("drop", &[Value::Str("env".into())], span())
            .await
            .unwrap();
        assert!(!interp.caps().has(Cap::Env), "drop mutated Interp.caps");
    }

    #[test]
    fn host_of_addr_strips_port_and_brackets() {
        assert_eq!(host_of_addr("example.com:8080"), "example.com");
        assert_eq!(host_of_addr("127.0.0.1:0"), "127.0.0.1");
        assert_eq!(host_of_addr("example.com"), "example.com");
        // IPv6 with brackets + port → bare address.
        assert_eq!(host_of_addr("[::1]:8080"), "::1");
        assert_eq!(host_of_addr("[fe80::1]:443"), "fe80::1");
        // Bare IPv6 (multiple colons, no brackets) → whole.
        assert_eq!(host_of_addr("::1"), "::1");
    }

    #[test]
    fn host_of_url_extracts_host() {
        assert_eq!(host_of_url("http://example.com/x").as_deref(), Some("example.com"));
        assert_eq!(host_of_url("https://8.8.8.8:443/").as_deref(), Some("8.8.8.8"));
        assert_eq!(host_of_url("ws://127.0.0.1:9000/sock").as_deref(), Some("127.0.0.1"));
        // userinfo stripped.
        assert_eq!(host_of_url("http://user:pass@host.test/").as_deref(), Some("host.test"));
        // IPv6 literal.
        assert_eq!(host_of_url("http://[::1]:8080/").as_deref(), Some("::1"));
        // query/fragment-only path still resolves the authority.
        assert_eq!(host_of_url("https://api.internal?q=1").as_deref(), Some("api.internal"));
        // No authority → None (caller lets the connect surface its own error).
        assert_eq!(host_of_url("not-a-url"), None);
        assert_eq!(host_of_url("/relative/path"), None);
    }

    // ─────────────────────── CapSet serialization (Task 1.1) ──────────────────
    // `to_bytes`/`from_bytes` for embedding a build-time CapSet into a module-archive
    // manifest (§5). Variable-length carve-outs don't fit the fixed 32-byte footer.

    #[test]
    fn capset_roundtrip_full() {
        // A non-trivial set: some bits dropped, an fs carve-out, a net carve-out.
        let mut cs = CapSet::all_granted();
        cs.deny(Cap::Process);
        cs.deny(Cap::Env);
        cs.set_fs_scope(FsScope {
            deny: FsDeny::All,
            allow: vec!["./cache".into(), "/var/tmp/app".into()],
        });
        cs.set_net_scope(NetScope {
            deny: NetDeny::External,
            allow: vec!["api.internal".into(), "10.0.0.5".into(), "héllo.example".into()],
        });

        let bytes = cs.to_bytes();
        let (decoded, consumed) = CapSet::from_bytes(&bytes).expect("round-trips");
        assert_eq!(decoded, cs, "round-trip yields an equal CapSet");
        assert_eq!(consumed, bytes.len(), "consumes exactly the serialized region");
    }

    #[test]
    fn capset_roundtrip_default_no_carveouts() {
        let cs = CapSet::all_granted();
        let bytes = cs.to_bytes();
        let (decoded, consumed) = CapSet::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, cs);
        assert_eq!(consumed, bytes.len());
        // No carve-outs → presence bytes are 0, so the encoding is compact.
        assert_eq!(bytes.len(), 3, "bits + two presence bytes when no carve-outs");
    }

    #[test]
    fn capset_roundtrip_fs_only() {
        let mut cs = CapSet::all_granted();
        cs.set_fs_scope(FsScope { deny: FsDeny::Write, allow: vec![] });
        let bytes = cs.to_bytes();
        let (decoded, consumed) = CapSet::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, cs);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn from_bytes_allows_trailing_data() {
        // The decoder reports bytes_consumed so a manifest can hold trailing fields.
        let cs = CapSet::all_granted();
        let mut bytes = cs.to_bytes();
        let n = bytes.len();
        bytes.extend_from_slice(b"TRAILING MANIFEST DATA");
        let (decoded, consumed) = CapSet::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, cs);
        assert_eq!(consumed, n, "stops after the CapSet region, leaving trailing bytes");
    }

    #[test]
    fn from_bytes_empty_is_err_not_panic() {
        assert!(matches!(
            CapSet::from_bytes(&[]),
            Err(CapsDecodeError::Truncated)
        ));
    }

    #[test]
    fn from_bytes_truncated_is_clean_err() {
        // Build a valid encoding, then truncate at every prefix — none may panic.
        let mut cs = CapSet::all_granted();
        cs.set_fs_scope(FsScope {
            deny: FsDeny::All,
            allow: vec!["abc".into(), "defg".into()],
        });
        cs.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec!["host.example".into()],
        });
        let full = cs.to_bytes();
        for cut in 0..full.len() {
            // Any strict prefix must be Err, never a panic and never a partial Ok.
            let r = CapSet::from_bytes(&full[..cut]);
            assert!(r.is_err(), "prefix of len {cut} must be Err");
        }
        // The full buffer is Ok.
        assert!(CapSet::from_bytes(&full).is_ok());
    }

    #[test]
    fn from_bytes_invalid_mode_byte_is_err() {
        // bits=0x1f (all granted), fs present, mode = 99 (invalid) → clean Err.
        let bytes = [ALL_BITS, 1u8, 99u8, 0u8, 0u8 /* net absent */];
        assert!(matches!(
            CapSet::from_bytes(&bytes),
            Err(CapsDecodeError::InvalidMode(99))
        ));
    }

    #[test]
    fn from_bytes_invalid_presence_byte_is_err() {
        let bytes = [ALL_BITS, 7u8 /* not 0 or 1 */];
        assert!(matches!(
            CapSet::from_bytes(&bytes),
            Err(CapsDecodeError::InvalidPresence(7))
        ));
    }

    #[test]
    fn from_bytes_over_large_count_is_err_not_alloc_bomb() {
        // bits, fs present, mode=All(1), count = u16::MAX (way over MAX_ENTRIES) →
        // clean Err BEFORE any allocation; the buffer is tiny so a naive
        // `Vec::with_capacity(count)` would also be caught, but the cap rejects first.
        let mut bytes = vec![ALL_BITS, 1u8, 1u8];
        bytes.extend_from_slice(&u16::MAX.to_le_bytes()); // count = 65535
        bytes.extend_from_slice(b"x"); // a few stray bytes
        assert!(matches!(
            CapSet::from_bytes(&bytes),
            Err(CapsDecodeError::CountTooLarge(_))
        ));
    }

    #[test]
    fn from_bytes_over_large_string_len_is_err() {
        // fs present, mode=All, count=1, then a string len of u32::MAX → clean Err,
        // never an allocation bomb or out-of-bounds read.
        let mut bytes = vec![ALL_BITS, 1u8, 1u8];
        bytes.extend_from_slice(&1u16.to_le_bytes()); // count = 1
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // string len = 4GB
        bytes.extend_from_slice(b"short");
        assert!(matches!(
            CapSet::from_bytes(&bytes),
            Err(CapsDecodeError::StringTooLong(_))
        ));
    }

    #[test]
    fn from_bytes_invalid_utf8_is_err() {
        // fs present, mode=All, count=1, len=2, bytes = invalid utf8 (0xff 0xff).
        let mut bytes = vec![ALL_BITS, 1u8, 1u8];
        bytes.extend_from_slice(&1u16.to_le_bytes()); // count = 1
        bytes.extend_from_slice(&2u32.to_le_bytes()); // len = 2
        bytes.extend_from_slice(&[0xff, 0xff]); // invalid utf8
        bytes.push(0u8); // net absent
        assert!(matches!(
            CapSet::from_bytes(&bytes),
            Err(CapsDecodeError::InvalidUtf8)
        ));
    }

    // ─────────────────────── restrict_with — monotone intersection (N4) ───────
    // The embedded-caps floor: archive.caps.restrict_with(&cli_caps). A capability is
    // granted in the result iff BOTH sides grant it; a scope merges to the stricter.

    #[test]
    fn restrict_all_granted_is_identity() {
        // all_granted ∩ X == X and X ∩ all_granted == X for any X.
        let mut x = CapSet::all_granted();
        x.deny(Cap::Process);
        x.set_net_scope(NetScope { deny: NetDeny::External, allow: vec!["api.internal".into()] });
        assert_eq!(CapSet::all_granted().restrict_with(&x), x);
        assert_eq!(x.restrict_with(&CapSet::all_granted()), x);
    }

    #[test]
    fn restrict_is_idempotent() {
        let mut x = CapSet::all_granted();
        x.deny(Cap::Env);
        x.set_fs_scope(FsScope { deny: FsDeny::Write, allow: vec!["./cache".into()] });
        assert_eq!(x.restrict_with(&x), x, "x ∩ x == x");
    }

    #[test]
    fn restrict_deny_net_and_deny_fs_denies_both() {
        let deny_net = CapSet::from_deny_list(["net"]).unwrap();
        let deny_fs = CapSet::from_deny_list(["fs"]).unwrap();
        let r = deny_net.restrict_with(&deny_fs);
        assert!(!r.has(Cap::Net), "net denied (from the net side)");
        assert!(!r.has(Cap::Fs), "fs denied (from the fs side)");
        // Everything else stays granted (neither side denied it).
        assert!(r.has(Cap::Process) && r.has(Cap::Ffi) && r.has(Cap::Env));
        // A whole-deny carries no carve-out.
        assert!(r.net_scope.is_none() && r.fs_scope.is_none());
        // Commutative verdict.
        assert_eq!(deny_fs.restrict_with(&deny_net), r);
    }

    #[test]
    fn restrict_never_regrants_embedded_floor() {
        // The N4 property: an embedded floor that denies net can NOT be re-granted by
        // a run-time set that grants net (here: all_granted).
        let floor = CapSet::from_deny_list(["net"]).unwrap();
        let runtime = CapSet::all_granted();
        let eff = floor.restrict_with(&runtime);
        assert!(!eff.has(Cap::Net), "embedded net-deny survives an all-granted runtime");
    }

    #[test]
    fn restrict_scope_intersect_whole_deny_is_whole_deny() {
        // A net carve-out ∩ a whole net-deny → the whole cap is denied (the carve-out's
        // allow-list is dropped; over-restriction is sound).
        let mut carved = CapSet::all_granted();
        carved.set_net_scope(NetScope { deny: NetDeny::External, allow: vec!["api.internal".into()] });
        let whole_deny = CapSet::from_deny_list(["net"]).unwrap();
        let r = carved.restrict_with(&whole_deny);
        assert!(!r.has(Cap::Net));
        assert!(r.net_scope.is_none(), "no carve-out survives a whole-cap deny");
        // fs mirror.
        let mut fscarved = CapSet::all_granted();
        fscarved.set_fs_scope(FsScope { deny: FsDeny::Write, allow: vec!["./cache".into()] });
        let rf = fscarved.restrict_with(&CapSet::from_deny_list(["fs"]).unwrap());
        assert!(!rf.has(Cap::Fs));
        assert!(rf.fs_scope.is_none());
    }

    #[test]
    fn restrict_full_grant_with_scope_uses_the_scope() {
        // all_granted ∩ (net carve-out) → the carve-out (the grant side imposes nothing).
        let mut carved = CapSet::all_granted();
        carved.set_net_scope(NetScope { deny: NetDeny::All, allow: vec!["10.0.0.5".into()] });
        let r = CapSet::all_granted().restrict_with(&carved);
        assert!(!r.has(Cap::Net), "carve-out clears the bit");
        let s = r.net_scope.as_ref().expect("carve-out carried through");
        assert_eq!(s.deny, NetDeny::All);
        assert_eq!(s.allow, vec!["10.0.0.5".to_string()]);
    }

    #[test]
    fn restrict_scope_intersect_scope_takes_stricter_and_intersects_allow() {
        // net: External (on a) ∩ All (on b) → All (stricter); allow = intersection.
        let mut a = CapSet::all_granted();
        a.set_net_scope(NetScope {
            deny: NetDeny::External,
            allow: vec!["api.internal".into(), "10.0.0.5".into()],
        });
        let mut b = CapSet::all_granted();
        b.set_net_scope(NetScope {
            deny: NetDeny::All,
            allow: vec!["10.0.0.5".into(), "other.host".into()],
        });
        let r = a.restrict_with(&b);
        let s = r.net_scope.as_ref().expect("merged carve-out");
        assert_eq!(s.deny, NetDeny::All, "All is stricter than External");
        assert_eq!(s.allow, vec!["10.0.0.5".to_string()], "allow = intersection of both");
        // Commutative.
        assert_eq!(b.restrict_with(&a).net_scope, r.net_scope);

        // fs: Write (on a) ∩ All (on b) → All (stricter); allow = intersection.
        let mut fa = CapSet::all_granted();
        fa.set_fs_scope(FsScope { deny: FsDeny::Write, allow: vec!["./cache".into(), "/tmp/x".into()] });
        let mut fb = CapSet::all_granted();
        fb.set_fs_scope(FsScope { deny: FsDeny::All, allow: vec!["/tmp/x".into()] });
        let rf = fa.restrict_with(&fb);
        let sf = rf.fs_scope.as_ref().expect("merged fs carve-out");
        assert_eq!(sf.deny, FsDeny::All);
        assert_eq!(sf.allow, vec!["/tmp/x".to_string()]);
    }

    #[test]
    fn restrict_is_monotone_no_bit_appears_that_was_absent() {
        // Exhaustive over all 32 bit combinations: a bit is set in the result ONLY IF
        // it was set in BOTH inputs (the core monotone property over whole caps).
        for ab in 0u8..32 {
            for bb in 0u8..32 {
                let a = CapSet { bits: ab, fs_scope: None, net_scope: None };
                let b = CapSet { bits: bb, fs_scope: None, net_scope: None };
                let r = a.restrict_with(&b);
                for cap in Cap::ALL {
                    if r.has(cap) {
                        assert!(a.has(cap) && b.has(cap),
                            "{} granted in result but not in both inputs", cap.name());
                    }
                }
            }
        }
    }

    #[test]
    fn set_fs_scope_clears_bit_and_records_allow() {
        let mut cs = CapSet::all_granted();
        cs.set_fs_scope(FsScope {
            deny: FsDeny::Write,
            allow: vec!["./cache".into()],
        });
        // Bit cleared (denied-outright at the dispatch site → defer to stage 2).
        assert!(!cs.has(Cap::Fs));
        // Carve-out recorded (the Gate-12 discriminator is now `Some`).
        assert!(cs.fs_scope.is_some());
        let scope = cs.fs_scope.as_ref().unwrap();
        assert_eq!(scope.deny, FsDeny::Write);
        assert_eq!(scope.allow, vec!["./cache".to_string()]);
    }
}
