//! Azure DevOps writes (an explicit request, the children of a reviewed plan) use the enabled
//! Azure source's personal access token, the credential its reads already use. Kuroko never signs
//! in to Azure CLI: a security review flagged interactive (device-code) sign-in, and a PAT is the
//! remedy for automation. The token stays in host memory and never reaches IPC, logs or models.
use super::*;
use crate::ado::implementation::Identity;

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

pub(super) async fn status(state: &DesktopState, source: Option<&str>) -> Status {
    let mut status = Status {
        state: "not_configured",
        authenticated: false,
        user: None,
        method: "personal_access_token",
    };
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
