//! The three definitions, the managers' command sequences through a scripted [`Runner`], the
//! authority of the operations and the assistant's intent across a login. Nothing here asks a real
//! service manager; `tests/service.rs` does, on the CI runners only.
use super::*;
use crate::app::variables::tests::{call, host};
use authority::Transport::{Agent, Browser, Local, Machine};
use std::cell::{Cell, RefCell};

/// Answers each command by the first scripted prefix it starts with that has answers left;
/// anything else succeeds empty.
#[derive(Default)]
struct Script {
    answers: Vec<(String, Ran, Cell<Option<usize>>)>,
    log: RefCell<Vec<String>>,
}

impl Script {
    fn answer(self, prefix: &str, ok: bool, stdout: &str) -> Self {
        self.scripted(prefix, ok, stdout, None)
    }
    /// An answer for the first `times` matching commands only.
    fn answer_times(self, prefix: &str, times: usize, ok: bool, stdout: &str) -> Self {
        self.scripted(prefix, ok, stdout, Some(times))
    }
    fn scripted(mut self, prefix: &str, ok: bool, stdout: &str, times: Option<usize>) -> Self {
        self.answers.push((
            prefix.to_owned(),
            Ran {
                ok,
                stdout: stdout.to_owned(),
            },
            Cell::new(times),
        ));
        self
    }
    fn commands(&self) -> Vec<String> {
        self.log.borrow().clone()
    }
    /// The commands that change something (status reads are left out).
    fn changes(&self) -> Vec<String> {
        self.commands()
            .into_iter()
            .filter(|c| {
                !c.starts_with("launchctl print")
                    && !c.starts_with("systemctl --user show")
                    && !c.starts_with("loginctl")
                    && !c.starts_with("schtasks /Query")
            })
            .collect()
    }
}

impl Runner for Script {
    fn run(&self, program: &str, args: &[String]) -> Ran {
        let line = format!("{program} {}", args.join(" "));
        self.log.borrow_mut().push(line.clone());
        self.answers
            .iter()
            .find(|(prefix, _, left)| line.starts_with(prefix.as_str()) && left.get() != Some(0))
            .map(|(_, ran, left)| {
                left.set(left.get().map(|n| n - 1));
                ran.clone()
            })
            .unwrap_or(Ran {
                ok: true,
                stdout: String::new(),
            })
    }
}

fn target(manager: Manager, root: &Path, isolated: bool) -> Target {
    let profile = root.join("profile with space");
    fs::create_dir_all(&profile).unwrap();
    Target {
        manager,
        exe: root.join("bin").join("kuroko-host"),
        id: id(&profile),
        profile,
        isolated,
        home: root.join("home"),
        config_home: root.join("home/.config"),
        uid: Some(501),
        user: "WORKSTATION\\synthetic".into(),
        legacy: false,
    }
}

const SHOW_STOPPED: &str = "LoadState=loaded\nActiveState=inactive\nSubState=dead\nResult=success\nExecMainStatus=0\nUnitFileState=enabled\n";
const SHOW_RUNNING: &str = "LoadState=loaded\nActiveState=active\nSubState=running\nResult=success\nExecMainStatus=0\nUnitFileState=enabled\n";
const SHOW_ABSENT: &str = "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n";

/// Writes an entry's definition file as the version that created it did.
fn place(t: &Target) {
    let file = t.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, t.definition()).unwrap();
}

/// What a scheduled task's CSV status line says, with its numeric last result.
fn task_csv(name: &str, result: &str) -> String {
    format!(
        "\"HOST\",\"\\{name}\",\"N/A\",\"Running\",\"Interactive only\",\"11/6/2026 9:00:00 AM\",\"{result}\",\"WORKSTATION\\synthetic\",\"x\"\r\n"
    )
}

/// Commands that would start or stop a host.
const STARTS_OR_STOPS: [&str; 9] = [
    "launchctl bootout",
    "launchctl bootstrap",
    "launchctl kickstart",
    "launchctl kill",
    "systemctl --user start",
    "systemctl --user stop",
    "systemctl --user restart",
    "schtasks /Run",
    "schtasks /End",
];

fn starts_or_stops(script: &Script) -> Vec<String> {
    script
        .changes()
        .into_iter()
        .filter(|c| STARTS_OR_STOPS.iter().any(|p| c.starts_with(p)))
        .collect()
}

#[test]
fn one_entry_per_profile_and_its_name_reveals_no_path() {
    let root = tempfile::tempdir().unwrap();
    let (a, b) = (root.path().join("a"), root.path().join("b"));
    assert_eq!(id(&a), id(&a));
    assert_ne!(id(&a), id(&b));
    for manager in [Manager::Launchd, Manager::Systemd, Manager::TaskScheduler] {
        let t = target(manager, root.path(), false);
        let name = t.name();
        assert!(name.contains(&t.id));
        assert!(!name.contains('/') && !name.contains('\\') && !name.contains("profile"));
    }
}

#[test]
fn the_launch_agent_starts_the_host_in_the_background_with_a_minimal_path() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::Launchd, root.path(), true);
    // A Mac's paths, whatever the test runs on (a Windows drive letter is no PATH entry).
    t.exe = PathBuf::from("/Users/synthetic/.cargo/bin/personal-teams-assistant");
    t.home = PathBuf::from("/Users/synthetic");
    t.profile = PathBuf::from("/Users/synthetic/Profiles/work & play");
    let plist = t.definition();
    assert!(plist.contains(MARKER));
    assert!(plist.contains(&format!("<string>{}</string>", t.name())));
    let exe = format!("<string>{}</string>", xml(&t.exe.to_string_lossy()));
    assert!(plist.contains(&exe));
    for flag in ["--host", FLAG, PROFILE_FLAG] {
        assert!(
            plist.contains(&format!("<string>{flag}</string>")),
            "{flag}"
        );
    }
    assert!(plist.contains(&xml(&t.profile.to_string_lossy())));
    // A clean exit (a quit, a second start) is not restarted; a crash is.
    assert!(plist.contains("<key>SuccessfulExit</key>\n    <false/>"));
    assert!(plist.contains("<key>RunAtLoad</key>\n  <true/>"));
    // The update's helper is in its own process group and survives the host it replaces.
    assert!(plist.contains("<key>AbandonProcessGroup</key>\n  <true/>"));
    assert_eq!(
        plist.matches("<dict>").count(),
        plist.matches("</dict>").count()
    );
    // The only variable is PATH, which starts with the executable's directory.
    assert_eq!(plist.matches("<key>").count(), 9);
    let path = t.path();
    assert!(path.starts_with(&t.exe.parent().unwrap().to_string_lossy().into_owned()));
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"] {
        assert!(path.split(':').any(|d| d == dir), "{dir}");
    }
    // The standard profile is not named.
    let standard = target(Manager::Launchd, root.path(), false);
    assert!(!standard.definition().contains(PROFILE_FLAG));
}

#[test]
fn the_systemd_unit_quotes_its_command_and_keeps_the_update_helper_alive() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::Systemd, root.path(), true);
    t.exe = root
        .path()
        .join("odd \"dir\" 100%")
        .join("personal-teams-assistant");
    let unit = t.definition();
    assert!(unit.starts_with(&format!("# {MARKER}")));
    let exec = unit
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .unwrap();
    assert!(exec.starts_with('"'));
    assert!(exec.contains("\\\"dir\\\" 100%%"), "{exec}");
    assert!(exec.contains(&format!("\"--host\" \"{FLAG}\" \"{PROFILE_FLAG}\"")));
    for line in [
        "Type=simple",
        "Restart=on-failure",
        "KillMode=process",
        "WantedBy=default.target",
    ] {
        assert!(unit.lines().any(|l| l == line), "{line}");
    }
    let environment: Vec<&str> = unit
        .lines()
        .filter(|l| l.starts_with("Environment"))
        .collect();
    assert_eq!(environment.len(), 1);
    assert!(environment[0].starts_with("Environment=\"PATH="));
    assert!(!t.path().contains("homebrew"));
}

#[test]
fn the_scheduled_task_runs_at_this_users_logon_without_limits_or_elevation() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::TaskScheduler, root.path(), true);
    t.exe = PathBuf::from(r"C:\Users\Synthetic User\.cargo\bin\personal-teams-assistant.exe");
    t.profile = PathBuf::from(r"C:\Users\Synthetic User\Profiles\work\");
    let task = t.definition();
    assert!(task.contains(MARKER));
    assert!(task.contains("<LogonTrigger>"));
    assert_eq!(
        task.matches("<UserId>WORKSTATION\\synthetic</UserId>")
            .count(),
        2
    );
    for element in [
        "<LogonType>InteractiveToken</LogonType>",
        "<RunLevel>LeastPrivilege</RunLevel>",
        "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
        "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
        "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
        r"<Command>C:\Users\Synthetic User\.cargo\bin\personal-teams-assistant.exe</Command>",
    ] {
        assert!(task.contains(element), "{element}");
    }
    // The trailing backslash is doubled so the closing quote stays a quote.
    assert!(task.contains(&format!(
        r#"<Arguments>--host {FLAG} {PROFILE_FLAG} &quot;C:\Users\Synthetic User\Profiles\work\\&quot;</Arguments>"#
    )));
    // No variable can be set by a task: the profile is an argument, nothing else is passed.
    assert!(!task.contains("PATH"));
    let bytes = utf16(&task);
    assert_eq!(&bytes[..2], &[0xFF, 0xFE]);
    assert_eq!(bytes.len(), 2 + task.encode_utf16().count() * 2);
}

#[test]
fn windows_arguments_survive_the_command_line() {
    for (word, quoted) in [
        ("plain", "plain"),
        ("", "\"\""),
        ("with space", "\"with space\""),
        (r"C:\a b\", r#""C:\a b\\""#),
        (r#"say "hi""#, r#""say \"hi\"""#),
        (r#"C:\x\"y"#, r#""C:\x\\\"y""#),
    ] {
        assert_eq!(windows_word(word), quoted, "{word}");
    }
}

#[test]
fn definitions_hold_no_account_variable_or_credential() {
    let root = tempfile::tempdir().unwrap();
    for manager in [Manager::Launchd, Manager::Systemd, Manager::TaskScheduler] {
        let definition = target(manager, root.path(), true).definition();
        for name in [
            "TOKEN",
            "SECRET",
            "STATE_ENCRYPTION_KEY",
            "_FILE",
            "PTA_PROFILE_DIR",
            "KUROKO_PROFILE_DIR",
            "PTA_UPDATE_HANDOVER",
        ] {
            assert!(!definition.contains(name), "{manager:?} {name}");
        }
    }
}

#[test]
fn launchd_install_is_idempotent_and_uninstall_removes_only_its_own_entry() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), false);
    let service = format!("gui/501/{}", t.name());
    // Not loaded yet.
    let first = Script::default().answer("launchctl print", false, "");
    assert_eq!(install(&t, &first), Ok(true));
    let file = t.file().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    assert_eq!(
        first.changes(),
        [format!("launchctl bootstrap gui/501 {}", file.display())]
    );
    // Loaded, same definition: nothing to do.
    let again = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    assert_eq!(install(&t, &again), Ok(false));
    assert!(again.changes().is_empty());
    // Running with another executable: refused, the file is untouched.
    let mut moved = t.clone();
    moved.exe = root.path().join("elsewhere/personal-teams-assistant");
    let running = Script::default().answer("launchctl print", true, "\tstate = running\n");
    assert_eq!(install(&moved, &running).unwrap_err().0, "state_conflict");
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    // Stopped with another executable: replaced and loaded again.
    let stopped = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    assert_eq!(install(&moved, &stopped), Ok(true));
    assert_eq!(
        stopped.changes(),
        [
            format!("launchctl bootout {service}"),
            format!("launchctl bootstrap gui/501 {}", file.display())
        ]
    );
    // Uninstall while the job runs this host: the file goes, the running host is left alone.
    let busy = Script::default().answer("launchctl print", true, "\tstate = running\n");
    assert_eq!(uninstall(&moved, &busy), Ok(true));
    assert!(busy.changes().is_empty());
    assert!(!file.exists());
    assert_eq!(
        uninstall(
            &moved,
            &Script::default().answer("launchctl print", false, "")
        ),
        Ok(false)
    );
    // A file with this name that kuroko did not write is never changed or removed.
    fs::write(&file, "<plist>someone else's</plist>").unwrap();
    let foreign = Script::default().answer("launchctl print", false, "");
    assert_eq!(install(&t, &foreign).unwrap_err().0, "state_conflict");
    assert_eq!(uninstall(&t, &foreign).unwrap_err().0, "state_conflict");
    assert!(foreign.changes().is_empty());
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "<plist>someone else's</plist>"
    );
}

#[test]
fn systemd_install_enables_the_user_unit_and_says_when_lingering_is_needed() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let unit = t.name();
    let fresh = Script::default()
        .answer(
            "systemctl --user show",
            true,
            "LoadState=not-found\nActiveState=inactive\nUnitFileState=\n",
        )
        .answer("loginctl show-user 501", true, "no\n");
    assert_eq!(install(&t, &fresh), Ok(true));
    assert_eq!(
        fresh.changes(),
        [
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable {unit}")
        ]
    );
    let file = t.file().unwrap();
    assert!(file.ends_with(Path::new("systemd/user").join(&unit)));
    assert_eq!(fs::read_to_string(&file).unwrap(), t.definition());
    let enabled = Script::default()
        .answer("systemctl --user show", true, SHOW_STOPPED)
        .answer("loginctl show-user 501", true, "no\n");
    assert_eq!(install(&t, &enabled), Ok(false));
    assert!(enabled.changes().is_empty());
    let found = status(&t, &enabled);
    assert!(found.installed && found.current && found.loaded && !found.running);
    assert_eq!(found.linger, Some(false));
    let shown = view(&t, &found);
    assert!(
        shown["notes"][0]
            .as_str()
            .unwrap()
            .contains("loginctl enable-linger")
    );
    // Without a user manager nothing is written.
    fs::remove_file(&file).unwrap();
    let absent = Script::default().answer("systemctl", false, "");
    assert_eq!(install(&t, &absent).unwrap_err().0, "not_ready");
    assert!(!file.exists());
    // Uninstall disables, removes and reloads.
    assert_eq!(install(&t, &fresh), Ok(true));
    let removing = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    assert_eq!(uninstall(&t, &removing), Ok(true));
    assert_eq!(
        removing.changes(),
        [
            format!("systemctl --user disable {unit}"),
            "systemctl --user daemon-reload".to_owned()
        ]
    );
    assert!(!file.exists());
}

#[test]
fn the_scheduled_task_is_created_from_a_temporary_definition_and_read_back_by_its_result() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::TaskScheduler, root.path(), true);
    let name = t.name();
    let absent = Script::default().answer("schtasks /Query", false, "");
    assert_eq!(install(&t, &absent), Ok(true));
    let changes = absent.changes();
    assert_eq!(changes.len(), 1);
    assert!(changes[0].starts_with(&format!("schtasks /Create /TN {name} /XML ")));
    assert!(changes[0].ends_with(" /F"));
    // The definition was handed over in a file that is gone again.
    let scratch = t.profile.join("service");
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    // schtasks reformats what it stores: compare the command and the arguments.
    let stored = t
        .definition()
        .replace("  ", "    ")
        .replace("<Exec>", "<Exec>\r\n");
    let csv = |result: &str| {
        format!(
            "\"HOST\",\"\\{name}\",\"N/A\",\"Running\",\"Interactive only\",\"11/6/2026 9:00:00 AM\",\"{result}\",\"WORKSTATION\\synthetic\",\"x\"\r\n"
        )
    };
    let installed = Script::default()
        .answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored)
        .answer(
            &format!("schtasks /Query /TN {name} /FO CSV"),
            true,
            &csv(TASK_RUNNING),
        );
    assert_eq!(install(&t, &installed), Ok(false));
    let found = status(&t, &installed);
    assert!(found.installed && found.current && found.running);
    assert_eq!(found.last_error, None);
    let failing = Script::default()
        .answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored)
        .answer(
            &format!("schtasks /Query /TN {name} /FO CSV"),
            true,
            &csv("2147942402"),
        );
    let found = status(&t, &failing);
    assert!(!found.running);
    assert_eq!(
        found.last_error.as_deref(),
        Some("The service manager reports result 2147942402.")
    );
    // Another executable while it runs: refused.
    let mut moved = t.clone();
    moved.exe = root.path().join("elsewhere.exe");
    assert_eq!(install(&moved, &installed).unwrap_err().0, "state_conflict");
    let removing =
        Script::default().answer(&format!("schtasks /Query /TN {name} /XML"), true, &stored);
    assert_eq!(uninstall(&t, &removing), Ok(true));
    assert_eq!(
        removing.changes(),
        [format!("schtasks /Delete /TN {name} /F")]
    );
    // A task with this name that kuroko did not create is left alone.
    let foreign = Script::default().answer(
        "schtasks /Query",
        true,
        "<Task><Description>mine</Description></Task>",
    );
    assert_eq!(install(&t, &foreign).unwrap_err().0, "state_conflict");
    assert_eq!(uninstall(&t, &foreign).unwrap_err().0, "state_conflict");
    assert!(foreign.changes().is_empty());
}

#[test]
fn an_update_starts_and_stops_the_host_through_each_manager() {
    let root = tempfile::tempdir().unwrap();
    let launchd = target(Manager::Launchd, root.path(), false);
    let file = launchd.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, launchd.definition()).unwrap();
    let service = format!("gui/501/{}", launchd.name());
    let loaded = Script::default().answer("launchctl print", true, "\tstate = not running\n");
    start(&launchd, &loaded).unwrap();
    stop(&launchd, &loaded);
    assert_eq!(
        loaded.changes(),
        [
            format!("launchctl kickstart {service}"),
            format!("launchctl kill SIGTERM {service}")
        ]
    );
    let unloaded = Script::default().answer("launchctl print", false, "");
    start(&launchd, &unloaded).unwrap();
    assert_eq!(
        unloaded.changes(),
        [format!("launchctl bootstrap gui/501 {}", file.display())]
    );

    let systemd = target(Manager::Systemd, root.path(), false);
    let file = systemd.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, systemd.definition()).unwrap();
    let unit = systemd.name();
    let script = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    start(&systemd, &script).unwrap();
    stop(&systemd, &script);
    assert_eq!(
        script.changes(),
        [
            format!("systemctl --user reset-failed {unit}"),
            format!("systemctl --user start {unit}"),
            format!("systemctl --user stop {unit}")
        ]
    );
    let refused = Script::default()
        .answer("systemctl --user show", true, SHOW_STOPPED)
        .answer("systemctl --user start", false, "");
    assert!(start(&systemd, &refused).is_err());

    let task = target(Manager::TaskScheduler, root.path(), false);
    let name = task.name();
    let script = Script::default().answer("schtasks /Query", true, &task.definition());
    start(&task, &script).unwrap();
    stop(&task, &script);
    assert_eq!(
        script.changes(),
        [
            format!("schtasks /Run /TN {name}"),
            format!("schtasks /End /TN {name}")
        ]
    );
    // An entry removed meanwhile is not started.
    let gone = Script::default().answer("schtasks /Query", false, "");
    assert!(start(&task, &gone).is_err());
    assert!(gone.changes().is_empty());
}

#[test]
fn status_reports_the_managers_last_error_without_paths() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), true);
    let file = t.file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, t.definition()).unwrap();
    let crashed = Script::default().answer(
        "launchctl print",
        true,
        "\tstate = not running\n\tlast exit code = 78: EX_CONFIG\n",
    );
    let found = status(&t, &crashed);
    assert!(found.installed && found.current && found.loaded && !found.running);
    assert_eq!(
        found.last_error.as_deref(),
        Some("The service manager reports exit code 78.")
    );
    let systemd = target(Manager::Systemd, root.path(), true);
    let failed = Script::default().answer(
        "systemctl --user show",
        true,
        "ActiveState=failed\nResult=exit-code\nExecMainStatus=1\nUnitFileState=enabled\n",
    );
    assert_eq!(
        status(&systemd, &failed).last_error.as_deref(),
        Some("The service manager reports exit-code (exit status 1).")
    );
    let shown = view(&t, &found).to_string();
    for private in [
        t.profile.to_string_lossy(),
        t.home.to_string_lossy(),
        t.exe.to_string_lossy(),
    ] {
        assert!(!shown.contains(private.as_ref()), "{private}");
    }
}

#[test]
fn a_stored_task_is_read_back_whatever_its_encoding_and_escaping() {
    let root = tempfile::tempdir().unwrap();
    let mut t = target(Manager::TaskScheduler, root.path(), true);
    t.profile = PathBuf::from(r"C:\Users\Synthetic User\profile");
    // As schtasks writes it to a pipe: UTF-16, its own indentation, quotes not escaped.
    let stored = t
        .definition()
        .replace("&quot;", "\"")
        .replace("    <", "\t<");
    let name = t.name();
    let decoded = decode(&utf16(&stored));
    assert_eq!(decoded, stored);
    let script =
        Script::default().answer(&format!("schtasks /Query /TN {name} /XML"), true, &decoded);
    let found = status(&t, &script);
    assert!(found.installed && found.current && !found.foreign);
    assert_eq!(decode(b"plain"), "plain");
}

#[test]
fn csv_fields_follow_schtasks_quoting() {
    assert_eq!(
        csv_fields("\"a\",\"b, c\",\"d \"\"e\"\"\",\"0\""),
        ["a", "b, c", "d \"e\"", "0"]
    );
}

#[test]
fn the_profile_argument_is_split_off() {
    let (rest, profile) = profile_argument(
        ["--host", FLAG, PROFILE_FLAG, "/tmp/p q"]
            .map(String::from)
            .to_vec(),
    );
    assert_eq!(rest, ["--host", FLAG]);
    assert_eq!(profile, Some(PathBuf::from("/tmp/p q")));
    let (rest, profile) = profile_argument(vec!["--host".into()]);
    assert_eq!((rest.len(), profile), (1, None));
}

#[test]
fn a_restore_waits_only_while_the_lease_is_unknown() {
    assert_eq!(decide(&Ok(true)), Restore::Start);
    assert_eq!(decide(&Ok(false)), Restore::Stay);
    assert_eq!(decide(&Err(anyhow::anyhow!("offline"))), Restore::Wait);
}

#[tokio::test]
async fn only_the_owner_at_this_computer_manages_the_entry() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    for transport in [Browser, Agent, Machine] {
        for method in ["service_status", "service_install", "service_uninstall"] {
            let reply = call(&host, transport, method, json!({})).await;
            assert_eq!(reply.code, "authority_denied", "{transport:?} {method}");
        }
    }
    // No argument chooses anything: not an executable, a profile or a name.
    for args in [json!({"exe":"/bin/sh"}), json!({"name":"other"})] {
        let reply = call(&host, Local, "service_install", args).await;
        assert_eq!(reply.code, "invalid_input");
    }
}

#[tokio::test]
async fn the_persons_start_and_stop_are_the_intent_a_login_restores() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let profile = host.state.settings.profile().to_path_buf();
    assert!(!intended(&profile));
    remember(&profile, true);
    assert!(intended(&profile));
    // A start that fails records nothing.
    let reply = call(&host, Local, "start_assistant", json!({})).await;
    assert!(!reply.ok);
    assert!(intended(&profile));
    let reply = call(&host, Local, "stop_assistant", json!({})).await;
    assert!(reply.ok, "{:?}", reply.message);
    assert!(!intended(&profile));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(profile.join(INTENT))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
}

#[tokio::test]
async fn a_login_never_starts_the_assistant_without_the_intent_or_the_teams_lease() {
    let root = tempfile::tempdir().unwrap();
    let host = host(root.path());
    let profile = host.state.settings.profile().to_path_buf();
    // Left stopped: nothing is asked.
    restore(host.clone(), Duration::ZERO).await;
    assert_eq!(*host.state.phase.borrow(), AssistantPhase::Stopped);
    // Left running, but another machine was selected for Teams meanwhile.
    remember(&profile, true);
    host.state
        .implementation
        .select_teams_machine("another-machine")
        .await;
    tokio::time::timeout(
        Duration::from_secs(5),
        restore(host.clone(), Duration::ZERO),
    )
    .await
    .unwrap();
    assert_eq!(*host.state.phase.borrow(), AssistantPhase::Stopped);
    assert!(host.state.running.lock().await.is_none());
    // The intent is kept for a later login on which this computer holds Teams again.
    assert!(intended(&profile));
}

#[test]
fn a_host_with_the_profiles_private_home_cannot_register_the_users_entry() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    // HOME is never under a profile in a person's session.
    assert!(!sandboxed(&profile));
}

#[test]
fn new_entries_are_named_for_kuroko_and_former_ones_keep_their_names() {
    let root = tempfile::tempdir().unwrap();
    let id = target(Manager::Launchd, root.path(), false).id;
    for (manager, new, old) in [
        (
            Manager::Launchd,
            format!("cl.elvisbrevi.kuroko.host.{id}"),
            format!("dev.personalteams.assistant.host.{id}"),
        ),
        (
            Manager::Systemd,
            format!("kuroko-{id}.service"),
            format!("personal-teams-assistant-{id}.service"),
        ),
        (
            Manager::TaskScheduler,
            format!("Kuroko-{id}"),
            format!("PersonalTeamsAssistant-{id}"),
        ),
    ] {
        let t = target(manager, root.path(), false);
        let former = t.former();
        assert!(!t.legacy && former.legacy);
        assert_eq!(t.name(), new);
        assert_eq!(former.name(), old);
        if let (Some(file), Some(former_file)) = (t.file(), former.file()) {
            assert!(file.to_string_lossy().contains(&new));
            assert!(former_file.to_string_lossy().contains(&old));
        }
        // Both carry the marker that recognizes them; the new one starts the host, the former one
        // the alias it was written for.
        let alias = root.path().join("bin").join("personal-teams-assistant");
        let (definition, previous) = (t.definition(), former.definition());
        assert!(definition.contains(MARKER) && previous.contains(MARKER));
        assert_eq!(previous, former.definition_of(&alias));
        assert!(definition.contains("kuroko-host"));
        assert!(!definition.contains("personal-teams-assistant"));
        assert!(previous.contains("personal-teams-assistant"));
        assert!(definition.contains("kuroko service uninstall"));
        assert!(previous.contains("pta service uninstall"));
    }
    // A former unit is read back exactly as the versions before Kuroko wrote it.
    let unit = target(Manager::Systemd, root.path(), false)
        .former()
        .definition();
    assert!(
        unit.lines()
            .any(|l| l == "Description=Personal Teams Assistant host")
    );
    let unit = target(Manager::Systemd, root.path(), false).definition();
    assert!(unit.lines().any(|l| l == "Description=Kuroko host"));
}

#[test]
fn an_entry_installed_from_the_alias_starts_the_host_beside_it() {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let suffix = std::env::consts::EXE_SUFFIX;
    let alias = bin.join(format!("personal-teams-assistant{suffix}"));
    let host = bin.join(format!("kuroko-host{suffix}"));
    fs::write(&alias, "").unwrap();
    // Without the host beside it, the alias stays.
    assert_eq!(host_program(&alias), alias);
    fs::write(&host, "").unwrap();
    assert_eq!(host_program(&alias), host);
    assert_eq!(host_program(&host), host);
    let other = bin.join(format!("something-else{suffix}"));
    assert_eq!(host_program(&other), other);
    if Manager::current().is_some() {
        let profile = root.path().join("profile");
        fs::create_dir_all(&profile).unwrap();
        let t = Target::detect(&profile, &alias).unwrap();
        assert_eq!(t.exe, host);
        assert!(!t.legacy);
        // A former entry starts the alias, or the host when a host started under its new name
        // wrote it.
        assert_eq!(t.former().programs(), [alias, host]);
    }
}

#[test]
fn the_entry_that_exists_is_the_new_one_then_the_former_one() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let script = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    // Neither: the new one.
    assert_eq!(entry(&t, &script).name(), t.name());
    place(&t.former());
    let found = entry(&t, &script);
    assert!(found.legacy);
    assert_eq!(found.name(), t.former().name());
    // A former entry starts and stops the host until it is renamed.
    start(&found, &script).unwrap();
    assert!(
        script
            .changes()
            .contains(&format!("systemctl --user start {}", t.former().name()))
    );
    place(&t);
    assert!(!entry(&t, &script).legacy);
    assert!(!entry(&t.former(), &script).legacy);

    let task = target(Manager::TaskScheduler, root.path(), false);
    let (new, old) = (task.name(), task.former().name());
    let only_former = Script::default()
        .answer(
            &format!("schtasks /Query /TN {old} /XML"),
            true,
            &task.former().definition(),
        )
        .answer(&format!("schtasks /Query /TN {new} /XML"), false, "");
    assert!(entry(&task, &only_former).legacy);
    let both = Script::default().answer("schtasks /Query", true, &task.definition());
    assert!(!entry(&task, &both).legacy);
}

#[test]
fn a_host_its_former_launch_agent_started_renames_it_without_stopping_itself() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), false);
    let former = t.former();
    place(&former);
    let (new_file, old_file) = (t.file().unwrap(), former.file().unwrap());
    let script = || {
        Script::default()
            .answer(
                &format!("launchctl print gui/501/{}", former.name()),
                true,
                "\tstate = running\n",
            )
            .answer("launchctl print gui/501/", false, "")
    };
    let running = script();
    assert_eq!(migrate(&t, false, &running), Migration::Renamed);
    // The new agent is only written (launchd loads it at the next login); the former one is
    // disabled, not booted out.
    assert_eq!(
        running.changes(),
        [format!("launchctl disable gui/501/{}", former.name())]
    );
    assert_eq!(fs::read_to_string(&new_file).unwrap(), t.definition());
    assert_eq!(fs::read_to_string(&old_file).unwrap(), former.definition());
    // Its status names the new entry and the former one beside it.
    let after = script().answer(
        "launchctl print-disabled gui/501",
        true,
        &format!(
            "disabled services = {{\n\t\"{}\" => disabled\n}}\n",
            former.name()
        ),
    );
    let shown = report(&t, &after);
    assert_eq!(shown["name"], t.name());
    assert_eq!(shown["installed"], true);
    assert_eq!(shown["current"], true);
    assert_eq!(
        shown["former"],
        json!({"name": former.name(), "installed": true, "enabled": false})
    );
    // Started through the former entry again (it was not disabled in time): nothing more.
    let again = script();
    assert_eq!(migrate(&t, false, &again), Migration::Unchanged);
    assert!(again.changes().is_empty());

    // A refused step removes the new agent and leaves the former one as it was.
    fs::remove_file(&new_file).unwrap();
    let refused = script().answer("launchctl disable", false, "");
    assert_eq!(migrate(&t, false, &refused), Migration::Failed);
    assert!(!new_file.exists());
    assert_eq!(fs::read_to_string(&old_file).unwrap(), former.definition());
    assert!(starts_or_stops(&refused).is_empty());
}

#[test]
fn a_host_the_new_launch_agent_started_removes_the_former_one() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), false);
    let former = t.former();
    place(&t);
    place(&former);
    let script = |former_state: &str| {
        Script::default()
            .answer(
                &format!("launchctl print gui/501/{}", former.name()),
                true,
                former_state,
            )
            .answer(
                &format!("launchctl print gui/501/{}", t.name()),
                true,
                "\tstate = running\n",
            )
    };
    // Through the alias the new entry did not start this host: wait.
    let alias = script("\tstate = not running\n");
    assert_eq!(migrate(&t, false, &alias), Migration::Unchanged);
    assert!(alias.changes().is_empty());
    // A former agent that runs a host is never booted out or removed.
    let busy = script("\tstate = running\n");
    assert_eq!(migrate(&t, true, &busy), Migration::Unchanged);
    assert!(busy.changes().is_empty());
    let idle = script("\tstate = not running\n");
    assert_eq!(migrate(&t, true, &idle), Migration::Retired);
    assert_eq!(
        idle.changes(),
        [format!("launchctl bootout gui/501/{}", former.name())]
    );
    assert!(!former.file().unwrap().exists());
    assert_eq!(
        fs::read_to_string(t.file().unwrap()).unwrap(),
        t.definition()
    );
    // Nothing is left to do.
    assert_eq!(
        migrate(&t, true, &script("\tstate = not running\n")),
        Migration::Unchanged
    );
}

#[test]
fn a_host_its_former_unit_started_renames_it_without_stopping_itself() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let former = t.former();
    place(&former);
    let (unit, old) = (t.name(), former.name());
    let (new_file, old_file) = (t.file().unwrap(), former.file().unwrap());
    let script = || {
        Script::default()
            .answer(&format!("systemctl --user show {old}"), true, SHOW_RUNNING)
            .answer(&format!("systemctl --user show {unit}"), true, SHOW_STOPPED)
    };
    let running = script();
    assert_eq!(migrate(&t, false, &running), Migration::Renamed);
    assert_eq!(
        running.changes(),
        [
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable {unit}"),
            format!("systemctl --user disable {old}"),
        ]
    );
    assert_eq!(fs::read_to_string(&new_file).unwrap(), t.definition());
    assert_eq!(fs::read_to_string(&old_file).unwrap(), former.definition());

    // The new unit cannot be enabled: it is removed, the former one is not touched.
    fs::remove_file(&new_file).unwrap();
    let refused = script().answer(&format!("systemctl --user enable {unit}"), false, "");
    assert_eq!(migrate(&t, false, &refused), Migration::Failed);
    assert_eq!(
        refused.changes(),
        [
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable {unit}"),
            format!("systemctl --user disable {unit}"),
            "systemctl --user daemon-reload".to_owned(),
        ]
    );
    assert!(!new_file.exists());
    assert_eq!(fs::read_to_string(&old_file).unwrap(), former.definition());

    // The former unit cannot be disabled: the new one is removed again.
    let kept = script().answer(&format!("systemctl --user disable {old}"), false, "");
    assert_eq!(migrate(&t, false, &kept), Migration::Failed);
    assert_eq!(
        kept.changes()[2..],
        [
            format!("systemctl --user disable {old}"),
            format!("systemctl --user disable {unit}"),
            "systemctl --user daemon-reload".to_owned(),
        ]
    );
    assert!(!new_file.exists());
    assert!(old_file.exists());
    for script in [&running, &refused, &kept] {
        assert!(starts_or_stops(script).is_empty());
    }

    // Without a user manager nothing is written.
    let absent = Script::default()
        .answer(&format!("systemctl --user show {old}"), true, SHOW_RUNNING)
        .answer("systemctl", false, "");
    assert_eq!(migrate(&t, false, &absent), Migration::Failed);
    assert!(!new_file.exists());
}

#[test]
fn a_host_the_new_unit_started_removes_the_former_one() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let former = t.former();
    place(&t);
    place(&former);
    let old = former.name();
    let disabled = SHOW_STOPPED.replace("UnitFileState=enabled", "UnitFileState=disabled");
    let script = Script::default()
        .answer(&format!("systemctl --user show {old}"), true, &disabled)
        .answer(
            &format!("systemctl --user show {}", t.name()),
            true,
            SHOW_RUNNING,
        );
    let shown = report(&t, &script);
    assert_eq!(
        shown["former"],
        json!({"name": old, "installed": true, "enabled": false})
    );
    assert_eq!(migrate(&t, true, &script), Migration::Retired);
    assert_eq!(
        script.changes(),
        [
            format!("systemctl --user disable {old}"),
            "systemctl --user daemon-reload".to_owned(),
        ]
    );
    assert!(!former.file().unwrap().exists());
    assert!(t.file().unwrap().exists());
    assert!(report(&t, &script).get("former").is_none());
}

#[test]
fn a_host_its_former_task_started_renames_it_without_ending_itself() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::TaskScheduler, root.path(), false);
    let former = t.former();
    let (new, old) = (t.name(), former.name());
    // The new task is absent until it is created (the migration and the install each ask once).
    let script = |absent: usize| {
        Script::default()
            .answer(
                &format!("schtasks /Query /TN {old} /XML"),
                true,
                &former.definition(),
            )
            .answer(
                &format!("schtasks /Query /TN {old} /FO CSV"),
                true,
                &task_csv(&old, TASK_RUNNING),
            )
            .answer_times(&format!("schtasks /Query /TN {new} "), absent, false, "")
            .answer(
                &format!("schtasks /Query /TN {new} /XML"),
                true,
                &t.definition(),
            )
    };
    let running = script(2);
    assert_eq!(migrate(&t, false, &running), Migration::Renamed);
    let changes = running.changes();
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes[0].starts_with(&format!("schtasks /Create /TN {new} /XML ")));
    assert_eq!(changes[1], format!("schtasks /Change /TN {old} /DISABLE"));
    assert_eq!(fs::read_dir(t.profile.join("service")).unwrap().count(), 0);

    // The former task cannot be disabled: the new one is deleted again.
    let kept = script(2).answer("schtasks /Change", false, "");
    assert_eq!(migrate(&t, false, &kept), Migration::Failed);
    assert_eq!(
        kept.changes()[1..],
        [
            format!("schtasks /Change /TN {old} /DISABLE"),
            format!("schtasks /Delete /TN {new} /F"),
        ]
    );
    // The new task cannot be created: nothing else happens.
    let refused = script(3).answer("schtasks /Create", false, "");
    assert_eq!(migrate(&t, false, &refused), Migration::Failed);
    assert_eq!(refused.changes().len(), 1);
    for script in [&running, &kept, &refused] {
        assert!(starts_or_stops(script).is_empty());
        assert!(
            !script
                .changes()
                .iter()
                .any(|c| c.contains(&old) && c.contains("/Delete"))
        );
    }
}

#[test]
fn a_host_the_new_task_started_removes_the_former_one() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::TaskScheduler, root.path(), false);
    let former = t.former();
    let (new, old) = (t.name(), former.name());
    // `/Change /DISABLE` turned the task's own setting off; its trigger stays enabled.
    let disabled = former.definition().replace(
        "<Enabled>true</Enabled>\n    <Hidden>",
        "<Enabled>false</Enabled>\n    <Hidden>",
    );
    assert_ne!(disabled, former.definition());
    let script = |result: &str| {
        Script::default()
            .answer(&format!("schtasks /Query /TN {old} /XML"), true, &disabled)
            .answer(
                &format!("schtasks /Query /TN {old} /FO CSV"),
                true,
                &task_csv(&old, result),
            )
            .answer(
                &format!("schtasks /Query /TN {new} /XML"),
                true,
                &t.definition(),
            )
            .answer(
                &format!("schtasks /Query /TN {new} /FO CSV"),
                true,
                &task_csv(&new, TASK_RUNNING),
            )
    };
    let idle = script("0");
    let shown = report(&t, &idle);
    assert_eq!(shown["name"], new);
    assert_eq!(shown["running"], true);
    assert_eq!(
        shown["former"],
        json!({"name": old, "installed": true, "enabled": false})
    );
    assert_eq!(migrate(&t, false, &idle), Migration::Unchanged);
    assert_eq!(migrate(&t, true, &idle), Migration::Retired);
    assert_eq!(idle.changes(), [format!("schtasks /Delete /TN {old} /F")]);
    // A former task that runs is never deleted.
    let busy = script(TASK_RUNNING);
    assert_eq!(migrate(&t, true, &busy), Migration::Unchanged);
    assert!(busy.changes().is_empty());
}

#[test]
fn installing_again_retires_the_former_entry_and_uninstall_removes_both() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    let former = t.former();
    let (unit, old) = (t.name(), former.name());
    let (new_file, old_file) = (t.file().unwrap(), former.file().unwrap());
    place(&former);
    // The former unit runs nothing: it is removed once the new one is installed.
    let stopped = Script::default()
        .answer(&format!("systemctl --user show {old}"), true, SHOW_STOPPED)
        .answer_times(
            &format!("systemctl --user show {unit}"),
            1,
            true,
            SHOW_ABSENT,
        )
        .answer(&format!("systemctl --user show {unit}"), true, SHOW_STOPPED);
    assert_eq!(repair(&t, &stopped), Ok(true));
    assert_eq!(
        stopped.changes(),
        [
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user enable {unit}"),
            format!("systemctl --user disable {old}"),
            "systemctl --user daemon-reload".to_owned(),
        ]
    );
    assert!(new_file.exists() && !old_file.exists());
    // The former unit runs this host: it is only disabled, never stopped.
    place(&former);
    let running = Script::default()
        .answer(&format!("systemctl --user show {old}"), true, SHOW_RUNNING)
        .answer(&format!("systemctl --user show {unit}"), true, SHOW_STOPPED);
    assert_eq!(repair(&t, &running), Ok(true));
    assert_eq!(
        running.changes(),
        [format!("systemctl --user disable {old}")]
    );
    assert!(old_file.exists());
    // Once disabled, installing again changes nothing.
    let disabled = Script::default()
        .answer(
            &format!("systemctl --user show {old}"),
            true,
            &SHOW_RUNNING.replace("UnitFileState=enabled", "UnitFileState=disabled"),
        )
        .answer(&format!("systemctl --user show {unit}"), true, SHOW_STOPPED);
    assert_eq!(repair(&t, &disabled), Ok(false));
    assert!(disabled.changes().is_empty());
    // Uninstall removes both entries and stops neither.
    let removing = Script::default()
        .answer(&format!("systemctl --user show {old}"), true, SHOW_RUNNING)
        .answer(&format!("systemctl --user show {unit}"), true, SHOW_STOPPED);
    assert_eq!(remove(&t, &removing), Ok(true));
    assert_eq!(
        removing.changes(),
        [
            format!("systemctl --user disable {unit}"),
            "systemctl --user daemon-reload".to_owned(),
            format!("systemctl --user disable {old}"),
            "systemctl --user daemon-reload".to_owned(),
        ]
    );
    assert!(!new_file.exists() && !old_file.exists());
    assert_eq!(remove(&t, &removing), Ok(false));
    // Only a former entry left: uninstall removes it too.
    place(&former);
    let only_former = Script::default().answer("systemctl --user show", true, SHOW_STOPPED);
    assert_eq!(remove(&t, &only_former), Ok(true));
    assert!(!old_file.exists());
}

#[test]
fn installing_again_while_the_former_launch_agent_runs_this_host_only_disables_it() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Launchd, root.path(), false);
    let former = t.former();
    place(&former);
    let script = Script::default()
        .answer(
            &format!("launchctl print gui/501/{}", former.name()),
            true,
            "\tstate = running\n",
        )
        .answer("launchctl print gui/501/", false, "");
    assert_eq!(repair(&t, &script), Ok(true));
    let file = t.file().unwrap();
    assert_eq!(
        script.changes(),
        [
            format!("launchctl bootstrap gui/501 {}", file.display()),
            format!("launchctl disable gui/501/{}", former.name()),
        ]
    );
    assert!(former.file().unwrap().exists());
}

#[test]
fn a_status_with_only_the_former_entry_names_it() {
    let root = tempfile::tempdir().unwrap();
    let t = target(Manager::Systemd, root.path(), false);
    place(&t.former());
    let script = Script::default().answer("systemctl --user show", true, SHOW_RUNNING);
    let shown = report(&t, &script);
    assert_eq!(shown["name"], t.former().name());
    assert_eq!(shown["installed"], true);
    assert_eq!(shown["current"], true);
    assert!(shown.get("former").is_none());
    assert!(
        shown["notes"][0]
            .as_str()
            .unwrap()
            .contains("before Kuroko")
    );
}
