//! Azure DevOps writes (an explicit request, the children of a reviewed plan) use the enabled
//! Azure source's personal access token, the credential its reads already use. Kuroko never signs
//! in to Azure CLI: a security review flagged interactive (device-code) sign-in, and a PAT is the
//! remedy for automation. The token stays in host memory and never reaches IPC, logs or models.
use super::*;
use crate::ado::implementation::Identity;
use serde_json::{Value, json};

const NOT_CONFIGURED: &str = "Azure DevOps writes use the enabled Azure source's personal access \
token. Store a PAT with Work Items (Read & write) in that source's variable.";
const REJECTED: &str = "Azure DevOps refused the Azure source's personal access token: it expired \
or was revoked. Store a new PAT with Work Items (Read & write) in that source's variable.";
const UNAVAILABLE: &str =
    "Azure DevOps could not be reached to check the Azure source's personal access token.";

/// Whether Azure DevOps accepts the token; never the token. `authenticated` does not prove a
/// project's write permissions.
#[derive(Serialize)]
pub(super) struct Status {
    pub state: &'static str,
    pub authenticated: bool,
    pub user: Option<String>,
    pub method: &'static str,
}
impl Status {
    fn new(state: &'static str) -> Self {
        Self {
            state,
            authenticated: false,
            user: None,
            method: "personal_access_token",
        }
    }
}

/// The coordinator's answer, taken only for a known state and a bounded account name. An older
/// coordinator that does not answer the operation reads as `unavailable`.
fn from_coordinator(data: &Value) -> Status {
    let mut status = Status::new(
        ["authenticated", "rejected", "not_configured", "unavailable"]
            .into_iter()
            .find(|state| data["state"] == *state)
            .unwrap_or("unavailable"),
    );
    status.authenticated = status.state == "authenticated" && data["authenticated"] == true;
    if !status.authenticated && status.state == "authenticated" {
        status.state = "unavailable";
    }
    status.user = data["user"]
        .as_str()
        .filter(|s| s.len() <= 320 && !s.chars().any(char::is_control))
        .map(str::to_owned);
    status
}

pub(super) async fn status(state: &DesktopState, source: Option<&str>) -> Status {
    // A computer signed in to a coordinator keeps no `[secrets]`: the coordinator holds the token
    // (its agents receive it from there) and answers for it.
    if implementation::link(state).ok().flatten().is_some() {
        let reply = implementation::fleet::account_reply(
            state,
            "azure_cli_status",
            &json!({"source":source}),
        )
        .await;
        return match reply {
            Ok(Some(reply)) if reply["ok"] == true => from_coordinator(&reply["data"]),
            _ => Status::new("unavailable"),
        };
    }
    let mut status = Status::new("not_configured");
    let Ok((client, key, catalog)) = tools::todo::azure_credential(state, source) else {
        return status;
    };
    match crate::ado::implementation::identity(&client, &key, &catalog).await {
        Identity::User(user) => {
            status.state = "authenticated";
            status.authenticated = true;
            status.user = user;
        }
        Identity::Rejected => status.state = "rejected",
        Identity::Unavailable => status.state = "unavailable",
    }
    status
}

/// Checked before a model drafts a request or a plan's children are sent, so a refused token
/// stops before any work.
pub(super) async fn require(state: &DesktopState, source: Option<&str>) -> Result<()> {
    match status(state, source).await.state {
        "authenticated" => Ok(()),
        "rejected" => Err(SetupError(REJECTED).into()),
        "unavailable" => Err(anyhow::anyhow!(UNAVAILABLE)),
        _ => Err(SetupError(NOT_CONFIGURED).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_in_computer_takes_only_a_known_state_from_its_coordinator() {
        let ok = from_coordinator(
            &json!({"state":"authenticated","authenticated":true,"user":"Ana Example"}),
        );
        assert!(ok.authenticated);
        assert_eq!(ok.user.as_deref(), Some("Ana Example"));
        assert_eq!(
            from_coordinator(&json!({"state":"rejected"})).state,
            "rejected"
        );
        for odd in [
            json!({"state":"authenticated","authenticated":false}),
            json!({"state":"granted","authenticated":true}),
            json!(null),
        ] {
            let status = from_coordinator(&odd);
            assert_eq!(status.state, "unavailable");
            assert!(!status.authenticated);
        }
        let user = "x".repeat(400);
        assert_eq!(
            from_coordinator(&json!({"state":"rejected","user":user})).user,
            None
        );
    }
}
