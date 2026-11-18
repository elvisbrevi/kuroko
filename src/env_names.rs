//! The product's own environment variables. Each one is named `KUROKO_<NAME>`; the former
//! `PTA_<NAME>` is still read when the new name is not set, so launchers, service units and scripts
//! written for earlier versions keep working (docs/rename-kuroko.md).
use std::ffi::OsString;

/// The prefix of every variable the product reads.
pub const PREFIX: &str = "KUROKO_";
/// The prefix earlier versions read; still accepted when the current name is not set.
pub const LEGACY_PREFIX: &str = "PTA_";

/// An isolated profile directory (absolute) instead of the standard one.
pub const PROFILE_DIR: &str = "PROFILE_DIR";
/// Accepted and ignored: the host never opens a window.
pub const HEADLESS: &str = "HEADLESS";

/// The current name of the variable `name` (`PROFILE_DIR` → `KUROKO_PROFILE_DIR`).
pub fn current(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// The former name of the variable `name` (`PROFILE_DIR` → `PTA_PROFILE_DIR`).
pub fn legacy(name: &str) -> String {
    format!("{LEGACY_PREFIX}{name}")
}

/// The value of `name` in `lookup`: the current name wins over the former one.
fn lookup(name: &str, lookup: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    lookup(&current(name)).or_else(|| lookup(&legacy(name)))
}

/// The value of the variable `name` in this process's environment.
pub fn var_os(name: &str) -> Option<OsString> {
    lookup(name, |key| std::env::var_os(key))
}

/// Both names of `name` with `value`, for a child process that may read either (a host of the
/// same build reads the current one; a script written for earlier versions, the former one).
pub fn pairs(name: &str, value: impl Into<OsString>) -> [(String, OsString); 2] {
    let value = value.into();
    [(current(name), value.clone()), (legacy(name), value)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: BTreeMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn the_current_name_wins_and_the_former_one_is_still_read() {
        assert_eq!(current(PROFILE_DIR), "KUROKO_PROFILE_DIR");
        assert_eq!(legacy(PROFILE_DIR), "PTA_PROFILE_DIR");
        let both = env(&[("KUROKO_PROFILE_DIR", "/new"), ("PTA_PROFILE_DIR", "/old")]);
        assert_eq!(lookup(PROFILE_DIR, both), Some("/new".into()));
        let old = env(&[("PTA_PROFILE_DIR", "/old")]);
        assert_eq!(lookup(PROFILE_DIR, old), Some("/old".into()));
        assert_eq!(lookup(PROFILE_DIR, env(&[])), None);
        // Another variable's names are never taken for this one.
        assert_eq!(lookup(PROFILE_DIR, env(&[("KUROKO_HEADLESS", "1")])), None);
    }

    #[test]
    fn a_child_gets_both_names() {
        let [current, legacy] = pairs(PROFILE_DIR, "/p");
        assert_eq!(current, ("KUROKO_PROFILE_DIR".into(), "/p".into()));
        assert_eq!(legacy, ("PTA_PROFILE_DIR".into(), "/p".into()));
    }
}
