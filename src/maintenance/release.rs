//! This package's release archives: the address derived from the package's own repository, a
//! bounded download that follows only the release host's own redirect, the published digest and an
//! unpacking that takes nothing but the two executables.
//!
//! No caller supplies a URL, a host or a file name: the address is built here from
//! `CARGO_PKG_REPOSITORY`, the version the registry listed and the target the installation
//! recorded. Everything is written under the staging directory the caller gives; the installed
//! executables are never opened.
use super::{
    ArchiveFile, CRATE, Manifest, OWN, Package, Version, exe_name, other_package, valid_target,
    verify_pair_for,
};
use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};

/// The repository this package was published from (`repository` in `Cargo.toml`).
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
/// The only hosts the release host redirects a download to; any other location is refused before
/// anything is downloaded (the install scripts follow the same list).
pub const REDIRECT_HOSTS: [&str; 3] = [
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
];
/// A release archive is about 10 to 30 MiB; anything far larger is not one.
pub const MAX_ARCHIVE: u64 = 256 << 20;
/// Bounds of each unpacked executable and of everything decompressed.
pub const MAX_BINARY: u64 = 512 << 20;
const MAX_UNPACKED: u64 = 2 << 30;
const MAX_DIGEST: u64 = 4 << 10;
const MAX_REDIRECTS: usize = 5;
pub const DOWNLOAD_SECONDS: u64 = 15 * 60;
const CONNECT_SECONDS: u64 = 20;

/// Where releases come from. Production uses [`Source::official`]; tests substitute a local double.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The repository's web address, `https://github.com/OWNER/NAME`.
    pub base: String,
    /// Hosts a redirect may lead to.
    pub hosts: Vec<String>,
    /// Refuse plain HTTP, including after a redirect.
    pub https_only: bool,
}

impl Source {
    /// This package's own repository on GitHub, over HTTPS only.
    pub fn official() -> Result<Self> {
        let base = REPOSITORY.trim_end_matches('/');
        let url = url::Url::parse(base).context("The package's repository is not an address.")?;
        ensure!(
            url.scheme() == "https"
                && url.host_str() == Some("github.com")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.path_segments().is_some_and(|s| s.count() == 2),
            "The package's repository is not a GitHub repository."
        );
        Ok(Self {
            base: base.to_owned(),
            hosts: REDIRECT_HOSTS.map(str::to_owned).to_vec(),
            https_only: true,
        })
    }

    /// The address of one asset of the release `version`.
    pub fn url(&self, version: &Version, file: &str) -> String {
        format!("{}/releases/download/v{version}/{file}", self.base)
    }

    /// The same source for `package`'s releases: its own repository under the same owner and host,
    /// with the same redirect rules. This package's source is returned unchanged.
    pub fn for_package(&self, package: Package) -> Self {
        let name = package.repository.trim_end_matches('/').rsplit('/').next();
        match (package == OWN, name, self.base.rsplit_once('/')) {
            (false, Some(name), Some((owner, _))) => Self {
                base: format!("{owner}/{name}"),
                ..self.clone()
            },
            _ => self.clone(),
        }
    }

    /// The scheme, host and owner path of `base` (`https://github.com/OWNER/`): a renamed
    /// repository answers its old address with a redirect there, which is followed for the
    /// release download only.
    fn owner_prefix(&self) -> Option<String> {
        self.base
            .rsplit_once('/')
            .map(|(owner, _)| format!("{owner}/"))
    }
}

fn windows_target(target: &str) -> bool {
    target.contains("-windows-")
}

/// The directory the archive holds, named like the archive.
pub fn directory_name(target: &str, version: &Version) -> String {
    directory_name_for(OWN, target, version)
}

/// [`directory_name`] of `package`'s archive.
pub fn directory_name_for(package: Package, target: &str, version: &Version) -> String {
    format!("{}-{target}-v{version}", package.name)
}

/// The archive `binaries.yml` attaches for `target`: `.zip` on Windows, `.tgz` elsewhere.
pub fn archive_name(target: &str, version: &Version) -> String {
    archive_name_for(OWN, target, version)
}

/// [`archive_name`] of `package`'s archive.
pub fn archive_name_for(package: Package, target: &str, version: &Version) -> String {
    let extension = if windows_target(target) { "zip" } else { "tgz" };
    format!(
        "{}.{extension}",
        directory_name_for(package, target, version)
    )
}

// ---------------------------------------------------------------------------------------------
// Downloading.

/// Why a download stopped, in words the user can act on.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    /// The release has no such asset (no archive for this target, or not published yet).
    Missing(String),
    Redirected,
    TooLarge,
    Truncated,
    Status(u16),
    Unreachable,
    Mismatch,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(file) => write!(
                f,
                "The release has no file {file}: there is no archive for this computer's target in that version yet. Nothing was staged."
            ),
            Self::Redirected => f.write_str(
                "The release download was redirected to an unexpected address, so nothing was downloaded.",
            ),
            Self::TooLarge => f.write_str(
                "The release download is larger than a release archive can be, so it was not used.",
            ),
            Self::Truncated => f.write_str(
                "The release download was interrupted before it was complete. Nothing was staged; try again.",
            ),
            Self::Status(status) => write!(
                f,
                "The release host answered with status {status}. Nothing was staged; try again later."
            ),
            Self::Unreachable => f.write_str(
                "The release host could not be reached. Check the network connection and proxy settings and try again.",
            ),
            Self::Mismatch => f.write_str(
                "The downloaded archive does not match the SHA-256 published with the release, so it was not used.",
            ),
        }
    }
}
impl std::error::Error for Failure {}

/// A client that follows only redirects to `source.hosts`, over HTTPS when required, sends no
/// credentials and gives up after `timeout`.
fn client(source: &Source, timeout: Duration) -> Result<reqwest::Client> {
    let hosts = source.hosts.clone();
    let https_only = source.https_only;
    let owner = source.owner_prefix();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        let url = attempt.url();
        // A repository renamed under the same owner: its old release address answers with a
        // redirect to the new one, on the same host, which then redirects to the asset storage.
        let renamed = owner.as_deref().is_some_and(|owner| {
            url.as_str().starts_with(owner) && url.path().contains("/releases/download/")
        });
        let allowed = attempt.previous().len() <= MAX_REDIRECTS
            && (url.scheme() == "https" || (!https_only && url.scheme() == "http"))
            && url.username().is_empty()
            && url.password().is_none()
            && (renamed
                || url
                    .host_str()
                    .is_some_and(|host| hosts.iter().any(|h| h.eq_ignore_ascii_case(host))));
        if allowed {
            attempt.follow()
        } else {
            attempt.error(Failure::Redirected)
        }
    });
    Ok(reqwest::Client::builder()
        .redirect(policy)
        .https_only(https_only)
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(CONNECT_SECONDS))
        .user_agent(format!(
            "{CRATE}/{} (update download)",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?)
}

fn failure(error: &reqwest::Error) -> Failure {
    if error.is_redirect() {
        Failure::Redirected
    } else if error.is_body() || error.is_decode() {
        Failure::Truncated
    } else {
        Failure::Unreachable
    }
}

/// Streams `url` into `sink`, at most `max` bytes, and checks the length the host announced.
/// Returns the SHA-256 and size of what was received.
async fn fetch(
    client: &reqwest::Client,
    url: &str,
    file: &str,
    max: u64,
    sink: &mut impl Write,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<ArchiveFile> {
    let mut response = client.get(url).send().await.map_err(|e| failure(&e))?;
    match response.status().as_u16() {
        200 => {}
        404 => bail!(Failure::Missing(file.to_owned())),
        status => bail!(Failure::Status(status)),
    }
    let announced = response.content_length();
    ensure!(announced.is_none_or(|n| n <= max), Failure::TooLarge);
    let mut hash = Sha256::new();
    let mut size = 0u64;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) if error.is_redirect() => bail!(Failure::Redirected),
            Err(_) => bail!(Failure::Truncated),
        };
        size += chunk.len() as u64;
        ensure!(size <= max, Failure::TooLarge);
        hash.update(&chunk);
        sink.write_all(&chunk)
            .context("The download could not be written to the staging directory.")?;
        progress(size, announced);
    }
    ensure!(
        size > 0 && announced.is_none_or(|n| n == size),
        Failure::Truncated
    );
    Ok(ArchiveFile {
        name: file.to_owned(),
        sha256: crate::hash::hex(hash.finalize()),
        size,
    })
}

/// Whether the release of `version` already holds both the archive for `target` and its `.sha256`.
/// The crate reaches the registry minutes before the release job attaches the archives, so an
/// archive installation counts a version as available only once this is true. Nothing is
/// downloaded and no redirect is followed: the release host answers an attached asset with its own
/// redirect to the download (the download itself still follows only `source.hosts`) and a missing
/// one, or a release that does not exist yet, with 404.
pub async fn published(source: &Source, version: &Version, target: &str) -> Result<bool> {
    published_for(OWN, source, version, target).await
}

/// [`published`] for `package`'s release, in its own repository.
pub async fn published_for(
    package: Package,
    source: &Source,
    version: &Version,
    target: &str,
) -> Result<bool> {
    ensure!(
        valid_target(target),
        "The recorded build target is not valid."
    );
    let source = &source.for_package(package);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(source.https_only)
        .timeout(Duration::from_secs(CONNECT_SECONDS))
        .connect_timeout(Duration::from_secs(CONNECT_SECONDS))
        .user_agent(format!(
            "{CRATE}/{} (update check)",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;
    let name = archive_name_for(package, target, version);
    for file in [format!("{name}.sha256"), name] {
        let response = client
            .get(source.url(version, &file))
            .send()
            .await
            .map_err(|_| Failure::Unreachable)?;
        match response.status().as_u16() {
            200 | 301 | 302 | 303 | 307 | 308 => {}
            404 => return Ok(false),
            status => bail!(Failure::Status(status)),
        }
    }
    Ok(true)
}

/// The digest a `.sha256` asset publishes for `file`: `HEX  NAME` (as `sha256sum` writes it, the
/// name possibly marked binary with `*`). The named file must be exactly `file`.
pub fn published_digest(text: &str, file: &str) -> Result<String> {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .context("The published SHA-256 is empty.")?;
    let mut fields = line.split_whitespace();
    let digest = fields.next().unwrap_or_default().to_ascii_lowercase();
    let named = fields.next().map(|n| n.trim_start_matches('*'));
    ensure!(
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()),
        "The published SHA-256 is not a digest."
    );
    ensure!(
        named == Some(file) && fields.next().is_none(),
        "The published SHA-256 names another file, so the archive was not used."
    );
    Ok(digest)
}

// ---------------------------------------------------------------------------------------------
// Unpacking.

fn escape() -> anyhow::Error {
    anyhow::anyhow!(
        "The archive holds a path outside its own directory, so nothing was taken from it."
    )
}

/// Where an archive entry lands: the directory itself, one of the two executables (by index) or
/// anything else inside the directory, which is skipped. A path that is absolute, climbs out, names
/// a drive or lies outside the expected directory refuses the whole archive.
fn place(path: &str, directory: &str, wanted: &[String]) -> Result<Option<usize>> {
    ensure!(
        !path.is_empty() && !path.starts_with(['/', '\\']) && !path.contains([':', '\0']),
        escape()
    );
    let mut parts: Vec<&str> = path.split(['/', '\\']).collect();
    while parts.last() == Some(&"") {
        parts.pop();
    }
    while parts.first() == Some(&".") {
        parts.remove(0);
    }
    ensure!(
        !parts.is_empty()
            && parts
                .iter()
                .all(|p| !p.is_empty() && *p != "." && *p != ".."),
        escape()
    );
    ensure!(parts[0] == directory, escape());
    Ok(match parts.as_slice() {
        [_, name] => wanted.iter().position(|w| w == name),
        _ => None,
    })
}

fn links() -> anyhow::Error {
    anyhow::anyhow!("The archive holds a link, so nothing was taken from it.")
}

/// Writes one executable, new and bounded, with permission to run it.
fn write_binary(mut from: impl Read, to: &Path) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o755);
    }
    let mut file = options
        .open(to)
        .context("The archive holds the same executable twice, so nothing was taken from it.")?;
    // The creation mode is narrowed by the process umask; the executables get the same mode
    // whatever it is.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    let copied = std::io::copy(&mut (&mut from).take(MAX_BINARY + 1), &mut file)
        .context("The archive could not be unpacked.")?;
    ensure!(
        copied > 0 && copied <= MAX_BINARY,
        "An executable in the archive is empty or too large, so nothing was taken from it."
    );
    file.sync_all()?;
    Ok(())
}

/// Unpacks only `<directory>/personal-teams-assistant` and `<directory>/pta` (with `.exe` on
/// Windows) from `archive` into `bin`. Any link, special file or path outside `<directory>` refuses
/// the archive; other regular files inside it (the licence, the readme) are skipped.
pub fn unpack(archive: &Path, zip: bool, directory: &str, bin: &Path) -> Result<()> {
    unpack_for(OWN, archive, zip, directory, bin)
}

/// [`unpack`] of every executable `package` installs, and nothing else.
pub fn unpack_for(
    package: Package,
    archive: &Path,
    zip: bool,
    directory: &str,
    bin: &Path,
) -> Result<()> {
    fs::create_dir_all(bin)?;
    let wanted: Vec<String> = package.bins.iter().map(|name| exe_name(name)).collect();
    let mut found = vec![false; wanted.len()];
    let corrupt = "The archive is damaged or not a release archive, so nothing was taken from it.";
    let file = fs::File::open(archive)?;
    if zip {
        let mut zip = zip::ZipArchive::new(file).context(corrupt)?;
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).context(corrupt)?;
            let name = entry.name().map_err(|_| escape())?.into_owned();
            let target = place(&name, directory, &wanted)?;
            ensure!(!entry.is_symlink(), links());
            if entry.is_dir() {
                continue;
            }
            ensure!(entry.is_file(), links());
            if let Some(i) = target {
                write_binary(&mut entry, &bin.join(&wanted[i]))?;
                found[i] = true;
            }
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(file).take(MAX_UNPACKED);
        let mut tar = tar::Archive::new(decoder);
        for entry in tar.entries().context(corrupt)? {
            let mut entry = entry.context(corrupt)?;
            let kind = entry.header().entry_type();
            if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
                continue;
            }
            let path = String::from_utf8(entry.path_bytes().into_owned()).map_err(|_| escape())?;
            let target = place(&path, directory, &wanted)?;
            ensure!(!kind.is_symlink() && !kind.is_hard_link(), links());
            if kind.is_dir() {
                continue;
            }
            // A sparse file (some `tar` writers store large executables that way) is a regular
            // file whose holes the reader fills in.
            ensure!(kind.is_file() || kind.is_gnu_sparse(), links());
            if let Some(i) = target {
                write_binary(&mut entry, &bin.join(&wanted[i]))?;
                found[i] = true;
            }
        }
    }
    ensure!(
        found.iter().all(|found| *found),
        "The archive does not hold every executable of the application, so nothing was taken from it."
    );
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Staging.

/// Downloads the archive of `version` for `target` into `partial`, checks it against its published
/// SHA-256, unpacks the two executables into `partial/root/bin` and verifies that they report
/// `version`. Returns the manifest to store. On any failure the caller removes `partial`: nothing
/// in it counts as staged. `progress` sees the bytes received and announced.
pub async fn stage(
    source: &Source,
    version: &Version,
    target: &str,
    partial: &Path,
    env: &[(OsString, OsString)],
    now: i64,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<Manifest> {
    stage_for(OWN, source, version, target, partial, env, now, progress).await
}

/// [`stage`] of `package`'s archive, from `package`'s own repository (see [`Source::for_package`]).
#[allow(clippy::too_many_arguments)]
pub async fn stage_for(
    package: Package,
    source: &Source,
    version: &Version,
    target: &str,
    partial: &Path,
    env: &[(OsString, OsString)],
    now: i64,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<Manifest> {
    ensure!(
        valid_target(target),
        "The recorded build target is not valid."
    );
    let source = &source.for_package(package);
    let name = archive_name_for(package, target, version);
    let downloads = partial.join("download");
    fs::create_dir_all(&downloads)?;
    let client = client(source, Duration::from_secs(DOWNLOAD_SECONDS))?;
    let digest_name = format!("{name}.sha256");
    let mut text = Vec::new();
    fetch(
        &client,
        &source.url(version, &digest_name),
        &digest_name,
        MAX_DIGEST,
        &mut text,
        |_, _| {},
    )
    .await?;
    let expected = published_digest(
        std::str::from_utf8(&text).context("The published SHA-256 is not text.")?,
        &name,
    )?;
    let path = downloads.join(&name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let received = fetch(
        &client,
        &source.url(version, &name),
        &name,
        MAX_ARCHIVE,
        &mut file,
        progress,
    )
    .await?;
    file.sync_all()?;
    drop(file);
    ensure!(received.sha256 == expected, Failure::Mismatch);
    let root = partial.join("root");
    let (archive, directory, bin) = (
        path.clone(),
        directory_name_for(package, target, version),
        root.join("bin"),
    );
    let zip = windows_target(target);
    tokio::task::spawn_blocking(move || unpack_for(package, &archive, zip, &directory, &bin))
        .await
        .context("The archive could not be unpacked.")??;
    let _ = fs::remove_dir_all(&downloads);
    let binaries = verify_pair_for(package, &root, version, env).await?;
    Ok(Manifest {
        schema: 1,
        version: version.to_string(),
        staged_at: now,
        features: Vec::new(),
        all_features: false,
        no_default_features: false,
        target: Some(target.to_owned()),
        binaries,
        archive: Some(received),
        package: other_package(package),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::maintenance::CLI;
    use std::io::Cursor;

    /// What an archive entry is.
    pub enum Kind {
        File(Vec<u8>),
        Dir,
        Symlink(&'static str),
        HardLink(&'static str),
    }

    /// A `.tgz` whose entry names are written byte for byte, so a hostile name (absolute, `..`)
    /// reaches the reader exactly as an attacker would put it.
    pub fn tgz(entries: &[(&str, Kind)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, kind) in entries {
            let mut header = tar::Header::new_gnu();
            let raw = &mut header.as_gnu_mut().unwrap().name;
            raw[..name.len()].copy_from_slice(name.as_bytes());
            header.set_mode(0o755);
            let body: &[u8] = match kind {
                Kind::File(body) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(body.len() as u64);
                    body
                }
                Kind::Dir => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    &[]
                }
                Kind::Symlink(to) | Kind::HardLink(to) => {
                    header.set_entry_type(if matches!(kind, Kind::Symlink(_)) {
                        tar::EntryType::Symlink
                    } else {
                        tar::EntryType::Link
                    });
                    header.set_link_name(to).unwrap();
                    header.set_size(0);
                    &[]
                }
            };
            header.set_cksum();
            builder.append(&header, body).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    pub fn zip(entries: &[(&str, Kind)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o755);
        for (name, kind) in entries {
            match kind {
                Kind::File(body) => {
                    writer.start_file(*name, options).unwrap();
                    writer.write_all(body).unwrap();
                }
                Kind::Dir => writer.add_directory(*name, options).unwrap(),
                Kind::Symlink(to) | Kind::HardLink(to) => {
                    writer.add_symlink(*name, *to, options).unwrap()
                }
            }
        }
        writer.finish().unwrap().into_inner()
    }

    const DIR: &str = "personal-teams-assistant-x86_64-unknown-linux-gnu-v0.9.14";

    fn pair(prefix: &str) -> Vec<(String, Kind)> {
        vec![
            (format!("{prefix}/"), Kind::Dir),
            (
                format!("{prefix}/{}", exe_name(CRATE)),
                Kind::File(b"host".to_vec()),
            ),
            (
                format!("{prefix}/{}", exe_name(CLI)),
                Kind::File(b"cli".to_vec()),
            ),
            (format!("{prefix}/LICENSE"), Kind::File(b"MIT".to_vec())),
            (format!("{prefix}/README.md"), Kind::File(b"read".to_vec())),
        ]
    }

    fn unpacked(bytes: &[u8], zip: bool) -> (tempfile::TempDir, Result<()>) {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        fs::write(&archive, bytes).unwrap();
        let result = unpack(&archive, zip, DIR, &dir.path().join("root/bin"));
        (dir, result)
    }

    fn refs(entries: &[(String, Kind)]) -> Vec<(&str, Kind)> {
        entries
            .iter()
            .map(|(n, k)| {
                (
                    n.as_str(),
                    match k {
                        Kind::File(b) => Kind::File(b.clone()),
                        Kind::Dir => Kind::Dir,
                        Kind::Symlink(t) => Kind::Symlink(t),
                        Kind::HardLink(t) => Kind::HardLink(t),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_address_is_derived_from_the_package_repository_version_and_target() {
        let source = Source::official().unwrap();
        assert_eq!(
            source.base,
            "https://github.com/elvisbrevi/personal-teams-assistant"
        );
        assert!(source.https_only);
        assert_eq!(source.hosts, REDIRECT_HOSTS);
        let version = Version(0, 11, 6);
        assert_eq!(
            source.url(
                &version,
                &archive_name("x86_64-unknown-linux-gnu", &version)
            ),
            "https://github.com/elvisbrevi/personal-teams-assistant/releases/download/v0.11.6/personal-teams-assistant-x86_64-unknown-linux-gnu-v0.11.6.tgz"
        );
        assert_eq!(
            archive_name("x86_64-pc-windows-msvc", &version),
            "personal-teams-assistant-x86_64-pc-windows-msvc-v0.11.6.zip"
        );
        assert_eq!(
            archive_name("aarch64-apple-darwin", &version),
            "personal-teams-assistant-aarch64-apple-darwin-v0.11.6.tgz"
        );
    }

    #[test]
    fn the_published_digest_must_name_exactly_this_archive() {
        let hex = "ab".repeat(32);
        let name = "personal-teams-assistant-x86_64-unknown-linux-gnu-v0.9.14.tgz";
        assert_eq!(
            published_digest(&format!("{hex}  {name}\n"), name).unwrap(),
            hex
        );
        assert_eq!(
            published_digest(&format!("{}  *{name}", hex.to_uppercase()), name).unwrap(),
            hex
        );
        for text in [
            format!("{hex}  other.tgz"),
            hex.clone(),
            format!("{hex}  {name} extra"),
            format!("{}  {name}", &hex[..63]),
            format!("{}  {name}", "zz".repeat(32)),
            String::new(),
        ] {
            assert!(published_digest(&text, name).is_err(), "{text}");
        }
    }

    #[test]
    fn only_the_two_executables_are_taken_from_a_release_archive() {
        for zip in [false, true] {
            let entries = pair(DIR);
            let bytes = if zip {
                super::tests::zip(&refs(&entries))
            } else {
                tgz(&refs(&entries))
            };
            let (dir, result) = unpacked(&bytes, zip);
            result.unwrap();
            let bin = dir.path().join("root/bin");
            let mut names: Vec<String> = fs::read_dir(&bin)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            let mut wanted = vec![exe_name(CRATE), exe_name(CLI)];
            wanted.sort();
            assert_eq!(names, wanted, "zip: {zip}");
            assert_eq!(fs::read(bin.join(exe_name(CRATE))).unwrap(), b"host");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(bin.join(exe_name(CLI)))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o777, 0o755);
            }
            // Nothing else was written anywhere.
            let mut all: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            all.sort();
            assert_eq!(all, ["archive", "root"]);
        }
    }

    #[test]
    fn a_path_escape_a_link_or_a_missing_executable_refuses_the_whole_archive() {
        let host = exe_name(CRATE);
        let escapes: Vec<(String, &str)> = vec![
            (format!("/{DIR}/{host}"), "absolute"),
            (format!("{DIR}/../{host}"), "climbs out"),
            (format!("../{DIR}/{host}"), "climbs out first"),
            (format!("other-dir/{host}"), "another directory"),
            (host.clone(), "no directory"),
            (format!("C:/{DIR}/{host}"), "a drive"),
            (format!("{DIR}\\..\\..\\{host}"), "backslashes"),
        ];
        for zip in [false, true] {
            for (name, why) in &escapes {
                let mut entries = pair(DIR);
                entries.push((name.clone(), Kind::File(b"evil".to_vec())));
                let bytes = if zip {
                    super::tests::zip(&refs(&entries))
                } else {
                    tgz(&refs(&entries))
                };
                let (dir, result) = unpacked(&bytes, zip);
                let error = result.expect_err(why).to_string();
                assert!(
                    error.contains("outside its own directory"),
                    "{why}: {error}"
                );
                assert!(!dir.path().join(&host).exists(), "{why}");
                assert!(!dir.path().parent().unwrap().join(&host).exists(), "{why}");
            }
            // A link, even one pointing inside, is refused.
            let mut entries = pair(DIR);
            entries.push((format!("{DIR}/link"), Kind::Symlink("/etc/passwd")));
            let bytes = if zip {
                super::tests::zip(&refs(&entries))
            } else {
                tgz(&refs(&entries))
            };
            let (_dir, result) = unpacked(&bytes, zip);
            assert!(
                result.unwrap_err().to_string().contains("link"),
                "zip: {zip}"
            );
            // An executable that is itself a link is refused, not followed.
            let entries = vec![
                (
                    format!("{DIR}/{}", exe_name(CLI)),
                    Kind::File(b"cli".to_vec()),
                ),
                (format!("{DIR}/{host}"), Kind::Symlink("/bin/sh")),
            ];
            let bytes = if zip {
                super::tests::zip(&refs(&entries))
            } else {
                tgz(&refs(&entries))
            };
            let (dir, result) = unpacked(&bytes, zip);
            assert!(result.is_err(), "zip: {zip}");
            assert!(!dir.path().join("root/bin").join(&host).exists());
            // Only one of the two executables.
            let entries = vec![(format!("{DIR}/{host}"), Kind::File(b"host".to_vec()))];
            let bytes = if zip {
                super::tests::zip(&refs(&entries))
            } else {
                tgz(&refs(&entries))
            };
            let (_dir, result) = unpacked(&bytes, zip);
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("every executable of the application"),
                "zip: {zip}"
            );
            // Not an archive at all.
            let (_dir, result) = unpacked(b"not an archive", zip);
            assert!(result.is_err(), "zip: {zip}");
        }
        // A hard link in a tar archive is refused too.
        let mut entries = pair(DIR);
        entries.push((format!("{DIR}/hard"), Kind::HardLink("/etc/shadow")));
        let (_dir, result) = unpacked(&tgz(&refs(&entries)), false);
        assert!(result.unwrap_err().to_string().contains("link"));
        // The same executable twice is refused instead of overwritten.
        let mut entries = pair(DIR);
        entries.push((format!("{DIR}/{host}"), Kind::File(b"second".to_vec())));
        let (_dir, result) = unpacked(&tgz(&refs(&entries)), false);
        assert!(result.unwrap_err().to_string().contains("twice"));
    }
}

#[cfg(test)]
mod hash_pins {
    use super::*;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    #[tokio::test]
    async fn a_downloaded_archive_digest_is_the_lowercase_sha256_of_its_bytes() {
        let server = MockServer::start().await;
        Mock::given(path("/a"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; 100_000]))
            .mount(&server)
            .await;
        Mock::given(path("/abc"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc".to_vec()))
            .mount(&server)
            .await;
        let client = reqwest::Client::new();
        let mut sink = Vec::new();
        let large = fetch(
            &client,
            &format!("{}/a", server.uri()),
            "a",
            1 << 20,
            &mut sink,
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(
            large.sha256,
            "6d1cf22d7cc09b085dfc25ee1a1f3ae0265804c607bc2074ad253bcc82fd81ee"
        );
        assert_eq!(large.size, 100_000);
        let mut sink = Vec::new();
        let small = fetch(
            &client,
            &format!("{}/abc", server.uri()),
            "abc",
            1 << 20,
            &mut sink,
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(
            small.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

/// The successor package (docs/rename-kuroko.md): its releases live in its own repository under the
/// same owner, its archive holds its four executables, and the old repository's address keeps
/// working after the rename through GitHub's redirect.
#[cfg(test)]
mod successor {
    use super::tests::{Kind, tgz, zip};
    use super::*;
    use crate::maintenance::SUCCESSOR;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

    fn successor() -> Package {
        SUCCESSOR.expect("this package names its successor")
    }

    #[test]
    fn the_successor_is_fetched_from_its_own_repository_under_the_same_owner() {
        let official = Source::official().unwrap();
        assert_eq!(
            official.for_package(successor()).base,
            "https://github.com/elvisbrevi/kuroko"
        );
        assert_eq!(official.for_package(OWN), official);
        let double = Source {
            base: "http://127.0.0.1:9/owner/repo".into(),
            hosts: vec!["127.0.0.1".into()],
            https_only: false,
        };
        let moved = double.for_package(successor());
        assert_eq!(moved.base, "http://127.0.0.1:9/owner/kuroko");
        assert_eq!((moved.hosts, moved.https_only), (double.hosts, false));
        let version = Version(0, 18, 0);
        assert_eq!(
            archive_name_for(successor(), "x86_64-pc-windows-msvc", &version),
            "kuroko-x86_64-pc-windows-msvc-v0.18.0.zip"
        );
        assert_eq!(
            directory_name_for(successor(), "aarch64-apple-darwin", &version),
            "kuroko-aarch64-apple-darwin-v0.18.0"
        );
    }

    #[test]
    fn every_executable_of_the_successor_is_unpacked_and_none_may_be_missing() {
        let dir_name = "kuroko-x86_64-unknown-linux-gnu-v0.18.0";
        for packed in [false, true] {
            let mut entries: Vec<(String, Kind)> = successor()
                .bins
                .iter()
                .map(|n| {
                    (
                        format!("{dir_name}/{}", exe_name(n)),
                        Kind::File(n.as_bytes().to_vec()),
                    )
                })
                .collect();
            entries.push((format!("{dir_name}/README.md"), Kind::File(b"r".to_vec())));
            let build = |entries: &[(String, Kind)]| {
                let refs: Vec<(&str, Kind)> = entries
                    .iter()
                    .map(|(n, k)| {
                        (
                            n.as_str(),
                            match k {
                                Kind::File(b) => Kind::File(b.clone()),
                                _ => unreachable!(),
                            },
                        )
                    })
                    .collect();
                if packed { zip(&refs) } else { tgz(&refs) }
            };
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("archive");
            fs::write(&archive, build(&entries)).unwrap();
            let bin = dir.path().join("bin");
            unpack_for(successor(), &archive, packed, dir_name, &bin).unwrap();
            for name in successor().bins {
                assert_eq!(fs::read(bin.join(exe_name(name))).unwrap(), name.as_bytes());
            }
            assert!(!bin.join("README.md").exists());
            // Without one of the aliases the archive is not the successor's.
            entries.retain(|(n, _)| !n.ends_with(&format!("/{}", exe_name("pta"))));
            let dir = tempfile::tempdir().unwrap();
            let archive = dir.path().join("archive");
            fs::write(&archive, build(&entries)).unwrap();
            let error = unpack_for(
                successor(),
                &archive,
                packed,
                dir_name,
                &dir.path().join("bin"),
            )
            .unwrap_err();
            assert!(error.to_string().contains("every executable"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_renamed_repository_redirect_is_followed_only_under_the_same_owner() {
        let server = MockServer::start().await;
        let asset = "/releases/download/v0.17.1/file.tgz";
        Mock::given(path(format!("/owner/repo{asset}")))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("location", format!("{}/owner/kuroko{asset}", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/owner/kuroko{asset}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"archive".to_vec()))
            .mount(&server)
            .await;
        Mock::given(path(format!("/owner/elsewhere{asset}")))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("location", format!("{}/stranger/repo{asset}", server.uri())),
            )
            .mount(&server)
            .await;
        Mock::given(path(format!("/stranger/repo{asset}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"other".to_vec()))
            .mount(&server)
            .await;
        // No asset host is allowed: only the rename rule can let a redirect through.
        let source = |name: &str| Source {
            base: format!("{}/owner/{name}", server.uri()),
            hosts: Vec::new(),
            https_only: false,
        };
        let version = Version(0, 17, 1);
        let renamed = source("repo");
        let follows = client(&renamed, Duration::from_secs(10)).unwrap();
        let mut sink = Vec::new();
        fetch(
            &follows,
            &renamed.url(&version, "file.tgz"),
            "file.tgz",
            1 << 20,
            &mut sink,
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(sink, b"archive");
        let elsewhere = source("elsewhere");
        let refuses = client(&elsewhere, Duration::from_secs(10)).unwrap();
        let error = fetch(
            &refuses,
            &elsewhere.url(&version, "file.tgz"),
            "file.tgz",
            1 << 20,
            &mut Vec::new(),
            |_, _| {},
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<Failure>(),
            Some(&Failure::Redirected),
            "{error}"
        );
    }
}
