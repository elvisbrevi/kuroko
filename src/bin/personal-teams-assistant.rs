//! `personal-teams-assistant`, the host's former name: the same program as `kuroko-host`, kept until
//! 0.21.0 so service entries, units and launchers written for it keep starting this build.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
fn main() {
    use personal_teams_assistant::app::cli;
    if let Some(note) = cli::former_name_note("personal-teams-assistant", "kuroko-host") {
        eprintln!("{note}");
    }
    personal_teams_assistant::app::run();
}
