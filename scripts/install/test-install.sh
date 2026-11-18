#!/bin/sh
# Runs install.sh against an archive this workflow just built, the way a person would, into a
# temporary installation root and a temporary profile (KUROKO_PROFILE_DIR): never a real profile,
# and never the network (the script's test source, enabled only with KUROKO_INSTALL_TEST=1).
#
#   scripts/install/test-install.sh DIST VERSION
#
# DIST holds kuroko-<target>-v<VERSION>.tgz and its .sha256. With KUROKO_INSTALL_TEST_NO_SERVICE=1
# the login service is neither registered nor checked (a computer without a per-user service
# manager, such as a container); KUROKO_INSTALL_TEST_RELEASE=X.Y.Z also installs that published
# release from GitHub through the real download path. Each variable is also read under its former
# PTA_ name.
# The sh -c checks take their values as arguments, so they are quoted on purpose.
# shellcheck disable=SC2016
set -eu

dist=$(cd "$1" && pwd)
version=$2
here=$(cd "$(dirname "$0")" && pwd)
script="$here/install.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/kuroko-install-test.XXXXXX")
root="$work/root"
# The upgrade from an installation made before the rename runs in a home of its own, so the
# default roots and startup files it reads and writes are never the real ones.
home="$work/home"
legacy="$home/.local/share/personal-teams-assistant"
export KUROKO_INSTALL_TEST=1
export KUROKO_PROFILE_DIR="$work/profile"
# Builds before the rename (the published release below) read only the former name.
export PTA_PROFILE_DIR="$KUROKO_PROFILE_DIR"
# A clean Cargo home: a Cargo installation is tested on purpose below.
export CARGO_HOME="$work/cargo"
service=yes
extra=
if [ "${KUROKO_INSTALL_TEST_NO_SERVICE:-${PTA_INSTALL_TEST_NO_SERVICE:-}}" = 1 ]; then
    service=no
    extra=--no-service
fi
release=${KUROKO_INSTALL_TEST_RELEASE:-${PTA_INSTALL_TEST_RELEASE:-}}
failures=0
new_mark="# >>> kuroko (install.sh) >>>"
old_mark="# >>> personal-teams-assistant (install.sh) >>>"

pass() { printf 'ok   - %s\n' "$1"; }
bad() {
    printf 'FAIL - %s\n' "$1"
    failures=$((failures + 1))
}
check() {
    description=$1
    shift
    if "$@"; then pass "$description"; else bad "$description"; fi
}
run() { sh "$script" --root "$root" ${extra:+"$extra"} "$@" >"$work/out" 2>&1; }
# install.sh with the separate home and no root given, as a person upgrading would run it.
run_home() {
    HOME="$home" XDG_CONFIG_HOME="$home/.config" SHELL=/bin/sh KUROKO_INSTALL_ROOT='' PTA_INSTALL_ROOT='' \
        sh "$script" "$@" >"$work/out" 2>&1
}
said() { grep -q "$1" "$work/out"; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi; }
kuroko() { "$root/bin/kuroko" "$@" </dev/null; }
marks() { grep -cxF "$1" "$2" || true; }

cleanup() {
    if [ -x "$root/bin/kuroko" ]; then
        sh "$script" --root "$root" --uninstall >/dev/null 2>&1 || true
    fi
    if [ -x "$legacy/bin/kuroko" ] || [ -x "$legacy/bin/pta" ]; then
        run_home --uninstall || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT

archive=$(cd "$dist" && ls kuroko-*-v"$version".tgz)
target=${archive#kuroko-}
target=${target%-v"$version".tgz}
mkdir -p "$work/source" "$work/tampered" "$work/empty" "$work/fake" "$KUROKO_PROFILE_DIR" "$CARGO_HOME"
cp "$dist/$archive" "$dist/$archive.sha256" "$work/source/"
cp "$dist/$archive" "$dist/$archive.sha256" "$work/tampered/"
printf 'X' | dd of="$work/tampered/$archive" bs=1 seek=4096 conv=notrunc 2>/dev/null
# An existing profile is adopted as it is.
printf 'synthetic profile marker\n' >"$KUROKO_PROFILE_DIR/adopted.txt"

echo "# Refusals that change nothing"
check "a digest mismatch is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/tampered" "$work/out"
check "  and says what happened" said "does not match the release's published SHA-256"
check "  and installs nothing" test ! -e "$root/bin"
check "the former PTA_INSTALL_TEST name still enables the test source" sh -c '! KUROKO_INSTALL_TEST= PTA_INSTALL_TEST=1 sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/tampered" "$work/out"
check "  (it got as far as the digest)" said "does not match the release's published SHA-256"
check "a missing archive is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" --version "$3" >"$4" 2>&1' "$script" "$root" "$work/empty" "$version" "$work/out"
check "  and says what to do" said "has no archive for"
printf '#!/bin/sh\ncase "$1" in -m) echo riscv64 ;; *) echo Linux ;; esac\n' >"$work/fake/uname"
chmod +x "$work/fake/uname"
check "an unsupported target is refused" sh -c '! PATH="$0:$PATH" KUROKO_INSTALL_TEST_TARGET= PTA_INSTALL_TEST_TARGET= sh "$1" --root "$2" --test-source "$3" >"$4" 2>&1' "$work/fake" "$script" "$root" "$work/source" "$work/out"
check "  and points to Cargo" said "There is no prebuilt archive"
printf '{"installs":{"personal-teams-assistant 0.11.2 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["personal-teams-assistant","pta"]}}}\n' >"$CARGO_HOME/.crates2.json"
check "a second copy next to a Cargo installation is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "  and says how to switch" said "cargo uninstall personal-teams-assistant"
check "  with that package's own CLI" said "pta service uninstall --confirmed"
printf '{"installs":{"kuroko 0.18.0 (registry+https://github.com/rust-lang/crates.io-index)":{"bins":["kuroko","kuroko-host","personal-teams-assistant","pta"]}}}\n' >"$CARGO_HOME/.crates2.json"
check "a second copy next to a Cargo installation of kuroko is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "  and says how to switch" said "cargo uninstall kuroko"
check "  or how to stay with Cargo" said "cargo install kuroko --locked"
rm -f "$CARGO_HOME/.crates2.json"
# A live Cargo installation (its own record beside binstall's) is refused and binstall's record is left alone.
mkdir -p "$CARGO_HOME/binstall"
other='{"name":"other-crate","version_req":"*","current_version":"1.0.0","bins":["other"]}'
ours='{"name":"personal-teams-assistant","version_req":"*","current_version":"0.11.2","bins":["personal-teams-assistant","pta"]}'
successor='{"name":"kuroko","version_req":"*","current_version":"0.18.0","bins":["kuroko","kuroko-host","personal-teams-assistant","pta"]}'
printf '%s\n%s\n%s\n' "$other" "$ours" "$successor" >"$CARGO_HOME/binstall/crates-v1.json"
printf '[v1]\n"personal-teams-assistant 0.11.2 (registry+https://github.com/rust-lang/crates.io-index)" = ["pta"]\n' >"$CARGO_HOME/.crates.toml"
binstall_before=$(sha "$CARGO_HOME/binstall/crates-v1.json")
check "a live binstall installation is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "  and its binstall record is untouched" test "$(sha "$CARGO_HOME/binstall/crates-v1.json")" = "$binstall_before"
rm -f "$CARGO_HOME/.crates.toml"
check "the test source needs the test mode" sh -c '! KUROKO_INSTALL_TEST= PTA_INSTALL_TEST= sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "a coordinator origin must be HTTPS" sh -c '! sh "$0" --root "$1" --test-source "$2" --origin http://portal.example >"$3" 2>&1' "$script" "$root" "$work/source" "$work/out"
check "nothing was installed by any refusal" test ! -e "$root/bin"

echo "# Install (with stale binstall entries left by cargo uninstall)"
if run --test-source "$work/source"; then pass "install.sh installs"; else bad "install.sh installs"; cat "$work/out"; fi
check "it checked the digest" said "matches its published SHA-256"
check "it says the existing profile is used" said "Using the existing profile"
check "it removed the stale binstall entries of both package names" said "Removed the stale cargo binstall entry for personal-teams-assistant and kuroko"
check "  keeping the other entry byte for byte" test "$(cat "$CARGO_HOME/binstall/crates-v1.json")" = "$other"
check "  and leaving no temporary file" sh -c 'for f in "$0"/*; do [ "$f" = "$0/crates-v1.json" ] || [ ! -e "$f" ] || exit 1; done' "$CARGO_HOME/binstall"
stale_after=$(sha "$CARGO_HOME/binstall/crates-v1.json")
check "kuroko --version from the installed path" sh -c '"$0/bin/kuroko" --version | grep -q "$1"' "$root" "$version"
for name in kuroko kuroko-host personal-teams-assistant pta; do
    check "$name is installed" test -x "$root/bin/$name"
done
check "  and the pta alias reports the same version" sh -c '"$0/bin/pta" --version | grep -q "$1"' "$root" "$version"
record="$root/install.json"
check "the record marks an archive installation" grep -q '"kind": "archive"' "$record"
check "  of the kuroko package" grep -q '"package": "kuroko"' "$record"
check "  with its four executables" grep -q '"bins": \["kuroko", "kuroko-host", "personal-teams-assistant", "pta"\]' "$record"
check "  from its repository" grep -q '"repository": "https://github.com/elvisbrevi/kuroko"' "$record"
check "  and version" grep -q "\"version\": \"$version\"" "$record"
check "  and target ($target)" grep -q "\"target\": \"$target\"" "$record"
check "  with the archive's digest" grep -q "\"sha256\": \"$(sha "$dist/$archive")\"" "$record"
update=$(kuroko --json update status 2>&1 || true)
check "the updater recognizes the archive kind" sh -c 'printf "%s" "$0" | grep -q "\"kind\":\"archive\""' "$update"
check "  as a supported installation it updates itself" sh -c 'printf "%s" "$0" | grep -q "\"supported\":true"' "$update"
check "  with unattended updates off by default" sh -c 'printf "%s" "$0" | grep -q "\"unattended\":{[^}]*\"enabled\":false"' "$update"
check "  and says how to turn them on" said "kuroko update unattended on --confirmed"
case "${SHELL:-/bin/sh}" in
    */zsh) shell_flags=-lic ;;
    *) shell_flags=-lc ;;
esac
found=$(env -i HOME="$HOME" USER="${USER:-}" SHELL="${SHELL:-/bin/sh}" PATH=/usr/bin:/bin "${SHELL:-/bin/sh}" $shell_flags 'command -v kuroko' 2>/dev/null | tail -n 1 || true)
check "a new shell finds kuroko ($found)" sh -c '[ "$0" = "$1/bin/kuroko" ] || [ "$(readlink "$0" 2>/dev/null)" = "$1/bin/kuroko" ]' "$found" "$root"
check "the profile was adopted, not replaced" grep -q "synthetic profile marker" "$KUROKO_PROFILE_DIR/adopted.txt"
machine=$(printf '{}' | "$root/bin/kuroko" --json tools call machine_settings 2>/dev/null | sed -n 's/.*"id":"\([0-9a-f-]*\)".*/\1/p')
check "the installation has a machine identity" test -n "$machine"
if [ "$service" = yes ]; then
    status=$(kuroko --json service status)
    name=$(printf '%s' "$status" | sed -n 's/.*"name":"\([^"]*\)".*/\1/p')
    check "the login service is installed ($name)" sh -c 'printf "%s" "$0" | grep -q "\"installed\":true"' "$status"
    check "  and starts this installation" sh -c 'printf "%s" "$0" | grep -q "\"current\":true"' "$status"
    check "  and runs the host now" sh -c 'printf "%s" "$0" | grep -q "\"running\":true"' "$status"
fi

echo "# Again: repair or upgrade in place"
before=$(sha "$record")
cp "$root/bin/kuroko" "$work/kuroko.before"
check "a failed upgrade is refused" sh -c '! sh "$0" --root "$1" --test-source "$2" >"$3" 2>&1' "$script" "$root" "$work/tampered" "$work/out"
check "  and leaves the installation working" sh -c '"$0/bin/kuroko" --version | grep -q "$1"' "$root" "$version"
check "  and its record unchanged" test "$(sha "$record")" = "$before"
rm -f "$root/bin/kuroko"
if run --test-source "$work/source" --unattended-updates; then pass "running it again succeeds"; else bad "running it again succeeds"; cat "$work/out"; fi
check "it repaired a missing binary" cmp -s "$root/bin/kuroko" "$work/kuroko.before"
check "a second run leaves the binstall record unchanged" test "$(sha "$CARGO_HOME/binstall/crates-v1.json")" = "$stale_after"
check "  and does not mention it" sh -c '! grep -q "stale cargo binstall" "$0"' "$work/out"
update=$(kuroko --json update status 2>&1 || true)
check "--unattended-updates turned unattended updates on" sh -c 'printf "%s" "$0" | grep -q "\"unattended\":{[^}]*\"enabled\":true"' "$update"
for file in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.zprofile"; do
    [ -f "$file" ] || continue
    check "at most one PATH block in $file" test "$(marks "$new_mark" "$file")" -le 1
    check "  and none written before the rename" test "$(marks "$old_mark" "$file")" -eq 0
done
again=$(printf '{}' | "$root/bin/kuroko" --json tools call machine_settings 2>/dev/null | sed -n 's/.*"id":"\([0-9a-f-]*\)".*/\1/p')
check "the same machine identity is kept" test "$again" = "$machine"
check "the profile is still the adopted one" grep -q "synthetic profile marker" "$KUROKO_PROFILE_DIR/adopted.txt"
if [ "$service" = yes ]; then
    status=$(kuroko --json service status)
    check "the service is still installed and running" sh -c 'printf "%s" "$0" | grep -q "\"running\":true" && printf "%s" "$0" | grep -q "\"current\":true"' "$status"
fi

echo "# Uninstall"
if sh "$script" --root "$root" --uninstall >"$work/out" 2>&1; then pass "--uninstall succeeds"; else bad "--uninstall succeeds"; cat "$work/out"; fi
check "the binaries are removed" test ! -e "$root/bin"
check "the record is removed" test ! -e "$record"
check "the profile is kept" grep -q "synthetic profile marker" "$KUROKO_PROFILE_DIR/adopted.txt"
for file in "$HOME/.profile" "$HOME/.bash_profile" "$HOME/.bashrc" "$HOME/.zshrc" "$HOME/.zprofile"; do
    [ -f "$file" ] || continue
    check "no PATH block left in $file" sh -c '! grep -q "kuroko (install.sh)" "$0" && ! grep -q "personal-teams-assistant (install.sh)" "$0"' "$file"
done
for name in kuroko pta; do
    check "no $name link left in ~/.local/bin" sh -c '! [ -L "$0/.local/bin/$1" ] || [ -e "$0/.local/bin/$1" ]' "$HOME" "$name"
done
if [ "$service" = yes ] && [ -n "${name:-}" ]; then
    case "$(uname -s)" in
        Darwin) entry="$HOME/Library/LaunchAgents/$name.plist" ;;
        *) entry="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$name" ;;
    esac
    check "the login entry is removed" test ! -e "$entry"
fi

# An archive installation made before the rename: the default root, two executables, its record
# and its PATH block in two startup files and fish. Running the command again without a root
# upgrades it where it is to the four executables, with one PATH block of the new name.
echo "# Upgrade of an installation made before the rename"
mkdir -p "$work/unpacked" "$legacy/bin" "$home/.config/fish/conf.d"
tar -xzf "$dist/$archive" -C "$work/unpacked"
for name in personal-teams-assistant pta; do
    cp "$work/unpacked/kuroko-$target-v$version/$name" "$legacy/bin/$name"
done
cat >"$legacy/install.json" <<EOF
{
  "schema": 1,
  "kind": "archive",
  "package": "personal-teams-assistant",
  "version": "0.17.0",
  "target": "$target",
  "bins": ["personal-teams-assistant", "pta"],
  "repository": "https://github.com/elvisbrevi/personal-teams-assistant",
  "archive": "personal-teams-assistant-$target-v0.17.0.tgz",
  "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
  "installed_at": "2026-11-01T00:00:00Z"
}
EOF
for file in .profile .bashrc; do
    {
        printf 'export SYNTHETIC_SETTING=kept\n'
        printf '%s\n' "$old_mark"
        printf 'case ":$PATH:" in *":%s:"*) ;; *) export PATH="%s:$PATH" ;; esac\n' "$legacy/bin" "$legacy/bin"
        printf '# <<< personal-teams-assistant (install.sh) <<<\n'
    } >"$home/$file"
done
printf '%s\ncontains "%s" $PATH; or set -gx PATH "%s" $PATH\n' "$old_mark" "$legacy/bin" "$legacy/bin" \
    >"$home/.config/fish/conf.d/personal-teams-assistant.fish"
if run_home --no-service --test-source "$work/source"; then pass "install.sh upgrades it"; else bad "install.sh upgrades it"; cat "$work/out"; fi
check "  in its own root" said "into $legacy\."
check "  without creating the new default root" test ! -e "$home/.local/share/kuroko"
for name in kuroko kuroko-host personal-teams-assistant pta; do
    check "  $name is installed there" test -x "$legacy/bin/$name"
done
check "  and kuroko runs" sh -c '"$0/bin/kuroko" --version | grep -q "$1"' "$legacy" "$version"
check "the record names the kuroko package" grep -q '"package": "kuroko"' "$legacy/install.json"
check "  with its four executables" grep -q '"bins": \["kuroko", "kuroko-host", "personal-teams-assistant", "pta"\]' "$legacy/install.json"
check "  from its repository" grep -q '"repository": "https://github.com/elvisbrevi/kuroko"' "$legacy/install.json"
check "  and the installed version" grep -q "\"version\": \"$version\"" "$legacy/install.json"
for file in .profile .bashrc; do
    check "one PATH block in $file" test "$(marks "$new_mark" "$home/$file")" -eq 1
    check "  and none written before the rename" test "$(marks "$old_mark" "$home/$file")" -eq 0
    check "  keeping the rest of the file" grep -qx 'export SYNTHETIC_SETTING=kept' "$home/$file"
done
check "the fish file written before the rename is replaced" test ! -e "$home/.config/fish/conf.d/personal-teams-assistant.fish"
check "  by one of the new name" grep -q "$legacy/bin" "$home/.config/fish/conf.d/kuroko.fish"
found=$(env -i HOME="$home" PATH=/usr/bin:/bin sh -lc 'command -v kuroko' 2>/dev/null | tail -n 1 || true)
check "a new shell finds kuroko there ($found)" test "$found" = "$legacy/bin/kuroko"
check "the profile was not touched" grep -q "synthetic profile marker" "$KUROKO_PROFILE_DIR/adopted.txt"
if run_home --uninstall; then pass "--uninstall finds and removes it"; else bad "--uninstall finds and removes it"; cat "$work/out"; fi
check "  the binaries are removed" test ! -e "$legacy/bin"
check "  the record is removed" test ! -e "$legacy/install.json"
for file in .profile .bashrc; do
    check "  no PATH block left in $file" sh -c '! grep -q "(install.sh)" "$0" && grep -qx "export SYNTHETIC_SETTING=kept" "$0"' "$home/$file"
done
check "  no fish file left" test ! -e "$home/.config/fish/conf.d/kuroko.fish"
check "  the profile is kept" grep -q "synthetic profile marker" "$KUROKO_PROFILE_DIR/adopted.txt"

# The real download path (release redirect, asset host, digest), with KUROKO_INSTALL_TEST_RELEASE
# set to a version already published with archives: no test source, no service. A release before
# 0.18.0 is installed in its own layout, which also has pta.
if [ -n "$release" ]; then
    echo "# A published release, downloaded from GitHub"
    net="$work/release-root"
    if KUROKO_INSTALL_TEST_TARGET='' PTA_INSTALL_TEST_TARGET='' sh "$script" --root "$net" --no-service --version "$release" >"$work/out" 2>&1; then
        pass "release $release installs from GitHub"
    else
        bad "release $release installs from GitHub"
        cat "$work/out"
    fi
    check "  after matching its published SHA-256" said "matches its published SHA-256"
    check "  and runs" sh -c '"$0/bin/pta" --version | grep -q "$1"' "$net" "$release"
    check "  and uninstalls" sh -c 'sh "$0" --root "$1" --uninstall >/dev/null 2>&1 && [ ! -e "$1/bin" ]' "$script" "$net"
fi

if [ "$failures" -gt 0 ]; then
    echo "$failures installer check(s) failed."
    exit 1
fi
echo "Every installer check passed."
