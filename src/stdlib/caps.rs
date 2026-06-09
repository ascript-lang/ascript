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

/// Granular `net` carve-out: deny the class, allow back specific hosts (§4.4).
/// See [`FsScope`] for the Gate-12 short-circuit rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetScope {
    pub deny: NetDeny,
    /// Allowed host strings (exact host match at connect/bind time).
    pub allow: Vec<String>,
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

// The `std/caps` module routing (exports / call) is added in Task 4 below.
// A placeholder keeps the module a clean unit until then.
#[allow(dead_code)]
pub(crate) fn _placeholder(_: &Value) {}

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
