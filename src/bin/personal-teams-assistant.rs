//! `personal-teams-assistant`, the host's former name: the same program as `kuroko-host`, kept so
//! service entries, units and launchers written for it keep starting this build.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
fn main() {
    personal_teams_assistant::app::run();
}
