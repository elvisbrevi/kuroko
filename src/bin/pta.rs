//! `pta`, the CLI's former name: the same program as `kuroko`, kept so scripts, MCP configurations
//! and habits written for it keep working.
#[tokio::main]
async fn main() {
    std::process::exit(personal_teams_assistant::app::cli::run().await);
}
