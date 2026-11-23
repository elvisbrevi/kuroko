//! What the workflow engine reads from Azure DevOps, read-only, with the enabled source's read
//! credential: the pull requests of one repository whose source branch is the one a coding
//! session reported (`_apis/git/repositories/{id}/pullrequests?searchCriteria.sourceRefName=…`),
//! whether a reported branch still exists (`_apis/git/repositories/{id}/refs?filter=heads/…`),
//! whether a work item is assigned to the source's user, and what a coding run delivered
//! without reporting it: the branches the user pushed (`…/pushes`), a pull request by its ID,
//! and the pages the user changed in the project's Wiki (its repository's commits). Nothing here
//! writes, merges, completes or changes a pull request, a Wiki page or a work item.
use super::{Catalog, Source, project_url, request, same_user};
use crate::tools::todo::Detail;
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};

/// At most this many pull requests of one branch are read.
const TOP: usize = 50;

/// A pull request as a workflow run records it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PullRequest {
    pub id: u64,
    /// The repository's catalog ID.
    pub repository: String,
    pub title: String,
    /// `active`, `completed` or `abandoned`, as Azure DevOps reports it.
    pub status: String,
    /// Its verified link, built from the source's organization, the project, the repository's
    /// name and the pull request's ID.
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// The branch it merges into (`refs/heads/NAME`), when it was read (0.14.0 and later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Whether it merges into its target as it is (`succeeded`, `conflicts`, `queued`, ...), as
    /// Azure DevOps last computed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_status: Option<String>,
    /// The commit of the target branch that merge status was computed against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_commit: Option<String>,
    /// The last commit of its source branch it was evaluated with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_commit: Option<String>,
    /// Who opened it (the Azure DevOps identity ID): the run's own account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    /// The branch it merges from (a plain name, without `refs/heads/`): kept only for a pull
    /// request found for a child without a report (0.20.0 and later), so the pull requests of a
    /// reported branch are recorded as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Its reviewers and their votes, as read (10 approved, 5 with suggestions, 0 none, -5
    /// waiting for the author, -10 rejected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<Reviewer>,
    /// The person resumed the run with this pull request abandoned: it no longer stops it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepted: bool,
}

/// A reviewer of a pull request and the vote it cast.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reviewer {
    /// The Azure DevOps identity ID.
    pub id: String,
    pub name: String,
    pub vote: i32,
}

/// A comment thread of a pull request, as the review rounds read it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Thread {
    pub id: u64,
    /// `active`, `pending`, `fixed`, `wontFix`, `closed`, `byDesign` or `unknown`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    /// Its first comment: who wrote it, when and what (third-party text, untrusted).
    pub author_id: String,
    pub author: String,
    pub text: String,
    pub published_at: String,
    /// Every later comment: who wrote it and when.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<Reply>,
    /// A thread Azure DevOps writes itself (a vote, a push): never a request for a change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub system: bool,
}

/// A later comment of a thread.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub author_id: String,
    pub at: String,
    /// Its first characters (a summary comment starts with a fixed heading).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}

/// Whether a branch name a coding session reported is one this read asks for: a plain Git
/// branch name (letters, digits, `/`, `.`, `_`, `-`), without `..`, a leading `-` or `/`, or a
/// trailing `/`, `.` or `.lock`.
pub fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && branch.len() <= 250
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.starts_with(['-', '/', '.'])
        && !branch.ends_with(['/', '.'])
        && !branch.ends_with(".lock")
}

/// The configured source of an organization (`https://dev.azure.com/NAME`).
fn source(catalog: &str, organization: &str) -> Result<Source> {
    let wanted = crate::workflows::organization_name(organization)
        .context("invalid organization: use https://dev.azure.com/NAME")?;
    Catalog::parse(catalog)?
        .sources
        .into_iter()
        .find(|s| crate::workflows::organization_name(&s.organization).as_ref() == Some(&wanted))
        .context("invalid organization: it is not in the Azure source's catalog")
}

fn text(value: &serde_json::Value, max: usize) -> String {
    value
        .as_str()
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect()
}

/// The pull requests of `repository` (catalog ID and name) whose source branch is `branch`, of
/// any status, newest first.
pub(crate) async fn pull_requests(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: (&str, &str),
    branch: &str,
) -> Result<Vec<PullRequest>> {
    let (id, _) = repository;
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "invalid repository ID"
    );
    ensure!(valid_branch(branch), "invalid branch name");
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", id, "pullrequests"],
    )?;
    let reference = format!("refs/heads/{branch}");
    url.query_pairs_mut()
        .append_pair("searchCriteria.sourceRefName", &reference)
        .append_pair("searchCriteria.status", "all")
        .append_pair("$top", &TOP.to_string());
    let value = request(client, key, Method::GET, url, None).await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps pull requests")?;
    let mut found = Vec::new();
    for pull in page.iter().take(TOP) {
        // Only the branch and repository asked for, whatever else the reply holds.
        if pull["sourceRefName"].as_str() != Some(reference.as_str()) {
            continue;
        }
        if let Some(pull) = parse_pull(&source, project, repository, pull)? {
            found.push(pull);
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.id));
    Ok(found)
}

/// A pull request of `repository` as a run records it, or none when the value is not one of
/// that repository's (another repository's, or without an ID).
fn parse_pull(
    source: &Source,
    project: &str,
    repository: (&str, &str),
    pull: &serde_json::Value,
) -> Result<Option<PullRequest>> {
    let (id, name) = repository;
    let Some(number) = pull["pullRequestId"].as_u64().filter(|n| *n > 0) else {
        return Ok(None);
    };
    if !pull["repository"]["id"]
        .as_str()
        .is_some_and(|r| r.eq_ignore_ascii_case(id))
    {
        return Ok(None);
    }
    let status = match pull["status"].as_str() {
        Some(status @ ("active" | "completed" | "abandoned")) => status.to_owned(),
        _ => "unknown".to_owned(),
    };
    let mut link = project_url(
        source,
        project,
        "dev.azure.com",
        &["_git", name, "pullrequest", &number.to_string()],
    )?;
    link.set_query(None);
    Ok(Some(PullRequest {
        id: number,
        repository: id.to_lowercase(),
        title: text(&pull["title"], 400),
        status,
        url: crate::tools::todo::https_link(link.as_str()).context("invalid pull request link")?,
        created_at: pull["creationDate"]
            .is_string()
            .then(|| text(&pull["creationDate"], 40)),
        closed_at: pull["closedDate"]
            .is_string()
            .then(|| text(&pull["closedDate"], 40)),
        target: pull["targetRefName"]
            .is_string()
            .then(|| text(&pull["targetRefName"], 300)),
        merge_status: pull["mergeStatus"]
            .is_string()
            .then(|| text(&pull["mergeStatus"], 40)),
        target_commit: pull["lastMergeTargetCommit"]["commitId"]
            .is_string()
            .then(|| text(&pull["lastMergeTargetCommit"]["commitId"], 64)),
        source_commit: pull["lastMergeSourceCommit"]["commitId"]
            .is_string()
            .then(|| text(&pull["lastMergeSourceCommit"]["commitId"], 64)),
        source: None,
        created_by: pull["createdBy"]["id"]
            .is_string()
            .then(|| text(&pull["createdBy"]["id"], 64)),
        reviewers: pull["reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .take(50)
            .filter_map(|r| {
                Some(Reviewer {
                    id: text(&r["id"], 64),
                    name: text(&r["displayName"], 200),
                    vote: i32::try_from(r["vote"].as_i64()?).ok()?,
                })
            })
            .filter(|r| !r.id.is_empty())
            .collect(),
        accepted: false,
    }))
}

/// The comment threads of a pull request of `repository` (its catalog ID), read-only: at most
/// 200, each comment's text cut to 2000 characters. Deleted threads and comments are left out.
pub(crate) async fn threads(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: &str,
    pull_request: u64,
) -> Result<Vec<Thread>> {
    ensure!(
        uuid::Uuid::parse_str(repository).is_ok() && repository.len() == 36,
        "invalid repository ID"
    );
    let source = source(catalog, organization)?;
    let url = project_url(
        &source,
        project,
        "dev.azure.com",
        &[
            "_apis",
            "git",
            "repositories",
            repository,
            "pullRequests",
            &pull_request.to_string(),
            "threads",
        ],
    )?;
    let value = request(client, key, Method::GET, url, None).await?;
    let mut found = Vec::new();
    for thread in value["value"]
        .as_array()
        .context("invalid Azure DevOps threads")?
        .iter()
        .take(200)
    {
        let Some(id) = thread["id"].as_u64() else {
            continue;
        };
        if thread["isDeleted"] == true {
            continue;
        }
        let comments: Vec<&serde_json::Value> = thread["comments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["isDeleted"] != true)
            .collect();
        let Some(first) = comments.first() else {
            continue;
        };
        let line = thread["threadContext"]["rightFileStart"]["line"]
            .as_u64()
            .or_else(|| thread["threadContext"]["leftFileStart"]["line"].as_u64());
        found.push(Thread {
            id,
            status: text(&thread["status"], 20),
            file: thread["threadContext"]["filePath"]
                .is_string()
                .then(|| text(&thread["threadContext"]["filePath"], 400)),
            line,
            author_id: text(&first["author"]["id"], 64),
            author: text(&first["author"]["displayName"], 200),
            text: first["content"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control() || *c == '\n')
                .take(2000)
                .collect(),
            published_at: text(&first["publishedDate"], 40),
            replies: comments
                .iter()
                .skip(1)
                .map(|c| Reply {
                    author_id: text(&c["author"]["id"], 64),
                    at: text(&c["publishedDate"], 40),
                    text: text(&c["content"], 80),
                })
                .collect(),
            system: first["commentType"] == "system"
                || thread["properties"]["CodeReviewThreadType"]["$value"].is_string(),
        });
    }
    Ok(found)
}

/// The files a pull request of `repository` (its catalog ID) conflicts on with its target
/// branch, as Azure DevOps lists its merge conflicts (at most 100 paths).
pub(crate) async fn conflicts(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: &str,
    pull_request: u64,
) -> Result<Vec<String>> {
    ensure!(
        uuid::Uuid::parse_str(repository).is_ok() && repository.len() == 36,
        "invalid repository ID"
    );
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &[
            "_apis",
            "git",
            "repositories",
            repository,
            "pullRequests",
            &pull_request.to_string(),
            "conflicts",
        ],
    )?;
    url.query_pairs_mut().append_pair("$top", "100");
    let value = request(client, key, Method::GET, url, None).await?;
    let mut files: Vec<String> = value["value"]
        .as_array()
        .context("invalid Azure DevOps conflicts")?
        .iter()
        .filter_map(|c| c["conflictPath"].as_str())
        .map(|p| text(&serde_json::json!(p.trim_start_matches('/')), 400))
        .filter(|p| !p.is_empty())
        .collect();
    files.sort();
    files.dedup();
    files.truncate(100);
    Ok(files)
}

/// Whether `branch` exists in `repository` (its catalog ID) now: a ref named exactly
/// `refs/heads/BRANCH` in the repository's refs that start so.
pub(crate) async fn branch_exists(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: &str,
    branch: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(repository).is_ok() && repository.len() == 36,
        "invalid repository ID"
    );
    ensure!(valid_branch(branch), "invalid branch name");
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", repository, "refs"],
    )?;
    url.query_pairs_mut()
        .append_pair("filter", &format!("heads/{branch}"))
        .append_pair("$top", &TOP.to_string());
    let value = request(client, key, Method::GET, url, None).await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps refs")?;
    let wanted = format!("refs/heads/{branch}");
    Ok(page
        .iter()
        .any(|r| r["name"].as_str() == Some(wanted.as_str())))
}

/// At most this many pushes of one repository are read, and this many branches kept.
const PUSHES: usize = 100;
const PUSHED_BRANCHES: usize = 20;
/// At most this many Wikis of a project, commits of each and pages in all are read.
const WIKIS: usize = 3;
const WIKI_COMMITS: usize = 10;
const WIKI_PAGES: usize = 20;
/// The object ID a push gives a deleted branch.
const DELETED: &str = "0000000000000000000000000000000000000000";

/// A branch the source's user pushed to a repository since a time, as the pushes read it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Pushed {
    /// The repository's catalog ID, in lowercase.
    pub repository: String,
    pub branch: String,
    /// When the user last pushed it (RFC 3339, as Azure DevOps gives it).
    pub pushed_at: String,
    /// Its verified link: the repository at that branch.
    pub url: String,
}

/// A page of the project's Wiki the source's user changed since a time.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WikiPage {
    /// The Wiki's name.
    pub wiki: String,
    /// The page's path in the Wiki (`/Procedures/Deploy the portal`).
    pub path: String,
    /// When the user last changed it (RFC 3339).
    pub changed_at: String,
    /// Its verified link.
    pub url: String,
}

fn after(date: &str, since: &chrono::DateTime<chrono::Utc>) -> bool {
    chrono::DateTime::parse_from_rfc3339(date)
        .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= *since)
}

/// The branches the source's user pushed to `repository` (catalog ID and name) between `since`
/// and `until`, newest first: only pushes by that user, only branches (`refs/heads/…`) with a
/// plain name whose latest push did not delete them, each once with that push's time; and
/// whether the page was full (older pushes in the window may be missing).
pub(crate) async fn pushed_branches(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: (&str, &str),
    (since, until): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
) -> Result<(Vec<Pushed>, bool)> {
    let (id, name) = repository;
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "invalid repository ID"
    );
    let source = source(catalog, organization)?;
    let mut url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "git", "repositories", id, "pushes"],
    )?;
    url.query_pairs_mut()
        .append_pair("searchCriteria.fromDate", &since.to_rfc3339())
        .append_pair("searchCriteria.toDate", &until.to_rfc3339())
        .append_pair("searchCriteria.includeRefUpdates", "true")
        .append_pair("$top", &PUSHES.to_string());
    let value = request(client, key, Method::GET, url, None).await?;
    let page = value["value"]
        .as_array()
        .context("invalid Azure DevOps pushes")?;
    // Each branch as its latest push in the window left it: (time, deleted).
    let mut latest: std::collections::BTreeMap<String, (String, bool)> = Default::default();
    for push in page.iter().take(PUSHES) {
        let date = text(&push["date"], 40);
        if !same_user(&push["pushedBy"], &source.author_email)
            || !after(&date, &since)
            || push["repository"]["id"]
                .as_str()
                .is_some_and(|r| !r.eq_ignore_ascii_case(id))
        {
            continue;
        }
        for update in push["refUpdates"].as_array().into_iter().flatten() {
            let Some(branch) = update["name"]
                .as_str()
                .and_then(|n| n.strip_prefix("refs/heads/"))
                .filter(|b| valid_branch(b))
            else {
                continue;
            };
            let deleted = update["newObjectId"].as_str().is_none_or(|o| o == DELETED);
            let entry = latest.entry(branch.to_owned()).or_default();
            if date > entry.0 {
                *entry = (date.clone(), deleted);
            }
        }
    }
    let mut pushed = Vec::new();
    for (branch, (pushed_at, deleted)) in latest {
        if deleted {
            continue;
        }
        let mut link = project_url(&source, project, "dev.azure.com", &["_git", name])?;
        link.set_query(None);
        link.query_pairs_mut()
            .append_pair("version", &format!("GB{branch}"));
        pushed.push(Pushed {
            repository: id.to_lowercase(),
            url: crate::tools::todo::https_link(link.as_str()).context("invalid branch link")?,
            branch,
            pushed_at,
        });
    }
    pushed.sort_by(|a, b| {
        b.pushed_at
            .cmp(&a.pushed_at)
            .then_with(|| a.branch.cmp(&b.branch))
    });
    pushed.truncate(PUSHED_BRANCHES);
    Ok((pushed, page.len() >= PUSHES))
}

/// One pull request of `repository` (catalog ID and name) by its ID, when the source's user
/// opened it since `since`; none when the repository has no such pull request, or it is someone
/// else's or older.
#[allow(clippy::too_many_arguments)] // The engine's read, beside `pull_requests`, plus its time.
pub(crate) async fn pull_request(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    repository: (&str, &str),
    number: u64,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<Option<PullRequest>> {
    let (id, _) = repository;
    ensure!(
        uuid::Uuid::parse_str(id).is_ok() && id.len() == 36,
        "invalid repository ID"
    );
    ensure!(number > 0, "invalid pull request ID");
    let source = source(catalog, organization)?;
    let url = project_url(
        &source,
        project,
        "dev.azure.com",
        &[
            "_apis",
            "git",
            "repositories",
            id,
            "pullrequests",
            &number.to_string(),
        ],
    )?;
    let pull = match request(client, key, Method::GET, url, None).await {
        Ok(pull) => pull,
        // The repository has no such pull request.
        Err(error) if error.to_string().ends_with("HTTP 404") => return Ok(None),
        Err(error) => return Err(error),
    };
    if pull["pullRequestId"].as_u64() != Some(number)
        || !same_user(&pull["createdBy"], &source.author_email)
        || !after(&text(&pull["creationDate"], 40), &since)
    {
        return Ok(None);
    }
    // Its source branch is what a linked pull request delivers; the pull requests of a reported
    // branch are recorded without it, as before.
    Ok(
        parse_pull(&source, project, repository, &pull)?.map(|p| PullRequest {
            source: pull["sourceRefName"]
                .as_str()
                .and_then(|r| r.strip_prefix("refs/heads/"))
                .filter(|b| valid_branch(b))
                .map(str::to_owned),
            ..p
        }),
    )
}

/// The longest Wiki page path kept.
const PAGE_PATH: usize = 300;

/// A Wiki page's path from its file in the Wiki's repository (`/Deploy-the-portal%2Dv2.md` is
/// `/Deploy the portal-v2`: Azure DevOps writes a title's spaces as `-` and its own `-` and other
/// reserved characters percent-encoded), or none for a file that is not a page.
fn page_path(file: &str) -> Option<String> {
    let file = file.strip_suffix(".md")?;
    if !file.starts_with('/')
        || file
            .split('/')
            .skip(1)
            .any(|s| s.is_empty() || s.starts_with('.'))
    {
        return None;
    }
    let spaced = file.replace('-', " ");
    let bytes = spaced.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| spaced.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                decoded.push(byte);
                i += 3;
            }
            None => {
                decoded.push(bytes[i]);
                i += 1;
            }
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    (path.chars().count() <= PAGE_PATH && !path.chars().any(char::is_control)).then_some(path)
}

/// The pages of the project's Wiki that the source's user changed between `since` and `until`
/// (added or edited, not deleted), newest first, each once; and whether part of it could not be
/// read (a read failed, or a page of commits was full).
// shortcut: authorship is the commit's author email, which a client sets; the window and the
// person's acceptance bound it. Read the Wiki repository's pushes (`pushedBy`) if it matters.
pub(crate) async fn wiki_pages(
    client: &Client,
    key: &str,
    catalog: &str,
    organization: &str,
    project: &str,
    (since, until): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
) -> Result<(Vec<WikiPage>, bool)> {
    let source = source(catalog, organization)?;
    let url = project_url(
        &source,
        project,
        "dev.azure.com",
        &["_apis", "wiki", "wikis"],
    )?;
    let value = request(client, key, Method::GET, url, None).await?;
    let wikis = value["value"]
        .as_array()
        .context("invalid Azure DevOps wikis")?;
    let mut pages: std::collections::BTreeMap<(String, String), String> = Default::default();
    let mut partial = false;
    for wiki in wikis
        .iter()
        .filter(|w| w["type"].as_str() == Some("projectWiki"))
        .take(WIKIS)
    {
        let (Some(repository), Some(name)) = (
            wiki["repositoryId"]
                .as_str()
                .filter(|r| uuid::Uuid::parse_str(r).is_ok() && r.len() == 36),
            wiki["name"]
                .as_str()
                .filter(|n| !n.is_empty() && n.len() <= 200),
        ) else {
            continue;
        };
        let mut url = project_url(
            &source,
            project,
            "dev.azure.com",
            &["_apis", "git", "repositories", repository, "commits"],
        )?;
        url.query_pairs_mut()
            .append_pair("searchCriteria.author", &source.author_email)
            .append_pair("searchCriteria.fromDate", &since.to_rfc3339())
            .append_pair("searchCriteria.toDate", &until.to_rfc3339())
            .append_pair("searchCriteria.$top", &WIKI_COMMITS.to_string());
        let Ok(commits) = request(client, key, Method::GET, url, None).await else {
            partial = true;
            continue;
        };
        partial |= commits["value"]
            .as_array()
            .is_some_and(|c| c.len() >= WIKI_COMMITS);
        for commit in commits["value"]
            .as_array()
            .into_iter()
            .flatten()
            .take(WIKI_COMMITS)
        {
            let date = text(&commit["author"]["date"], 40);
            let Some(sha) = commit["commitId"]
                .as_str()
                .filter(|s| s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit()))
            else {
                continue;
            };
            if !commit["author"]["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(&source.author_email))
                || !after(&date, &since)
            {
                continue;
            }
            let url = project_url(
                &source,
                project,
                "dev.azure.com",
                &[
                    "_apis",
                    "git",
                    "repositories",
                    repository,
                    "commits",
                    sha,
                    "changes",
                ],
            )?;
            let Ok(changes) = request(client, key, Method::GET, url, None).await else {
                partial = true;
                continue;
            };
            for change in changes["changes"].as_array().into_iter().flatten().take(50) {
                if change["item"]["gitObjectType"].as_str() != Some("blob")
                    || change["changeType"]
                        .as_str()
                        .is_none_or(|kind| kind.contains("delete"))
                {
                    continue;
                }
                let Some(path) = change["item"]["path"].as_str().and_then(page_path) else {
                    continue;
                };
                let entry = pages.entry((name.to_owned(), path)).or_default();
                if date > *entry {
                    *entry = date.clone();
                }
            }
        }
    }
    let mut found = Vec::new();
    for ((wiki, path), changed_at) in pages {
        let mut link = project_url(
            &source,
            project,
            "dev.azure.com",
            &["_wiki", "wikis", &wiki],
        )?;
        link.set_query(None);
        link.query_pairs_mut().append_pair("pagePath", &path);
        found.push(WikiPage {
            url: crate::tools::todo::https_link(link.as_str()).context("invalid Wiki page link")?,
            wiki,
            path,
            changed_at,
        });
    }
    found.sort_by(|a, b| {
        b.changed_at
            .cmp(&a.changed_at)
            .then_with(|| a.path.cmp(&b.path))
    });
    found.truncate(WIKI_PAGES);
    Ok((found, partial))
}

/// Whether the item is assigned to the source's user: its Assigned To, or a developer role of
/// its process (an identity field named «developer» or «desarrollador», as To-Do reads them),
/// names the user of the source of its organization.
pub(crate) fn assigned(catalog: &str, detail: &Detail) -> Result<bool> {
    let organization = crate::workflows::director::item_organization(&detail.item.url)
        .context("The work item has no Azure DevOps link.")?;
    let source = source(catalog, &format!("https://dev.azure.com/{organization}"))?;
    let email = source.author_email.to_lowercase();
    Ok(detail.fields.iter().any(|field| {
        let name = field.name.to_lowercase();
        let role = field.reference == "System.AssignedTo"
            || name.contains("developer")
            || name.contains("desarrollador");
        let identity = field.value["uniqueName"]
            .as_str()
            .or_else(|| field.value.as_str())
            .unwrap_or_default()
            .to_lowercase();
        role && (identity == email || identity.ends_with(&format!("<{email}>")))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use url::Url;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    const CATALOG: &str = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'";
    const REPO: &str = "0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f";

    #[test]
    fn only_plain_branch_names_are_asked_for() {
        for good in ["feature/42-pay", "fix_1", "release/2026.11"] {
            assert!(valid_branch(good), "{good}");
        }
        for bad in [
            "",
            "-x",
            "a..b",
            "a b",
            "refs/heads/../x",
            "x.lock",
            "x/",
            "/x",
            "a?b",
            "a//b",
        ] {
            assert!(!valid_branch(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn pull_requests_are_read_by_source_branch_with_verified_links_and_nothing_is_written() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/pullrequests"
            )))
            .and(query_param(
                "searchCriteria.sourceRefName",
                "refs/heads/feature/42",
            ))
            .and(query_param("searchCriteria.status", "all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"pullRequestId":7,"status":"completed","title":"Pay","sourceRefName":"refs/heads/feature/42",
                 "repository":{"id":REPO},"creationDate":"2026-11-01T10:00:00Z","closedDate":"2026-11-02T10:00:00Z"},
                {"pullRequestId":9,"status":"active","title":"Other branch","sourceRefName":"refs/heads/other",
                 "repository":{"id":REPO}},
                {"pullRequestId":8,"status":"strange","title":"Pay again\u{7}","sourceRefName":"refs/heads/feature/42",
                 "repository":{"id":REPO}}
            ]})))
            .mount(&server)
            .await;
        let client = Client::new();
        let found = pull_requests(
            &client,
            "key",
            CATALOG,
            "https://dev.azure.com/example",
            "Payments",
            (REPO, "payments api"),
            "feature/42",
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, 8);
        assert_eq!(found[0].status, "unknown");
        assert_eq!(found[0].title, "Pay again");
        assert_eq!(found[1].status, "completed");
        assert_eq!(
            found[1].url,
            "https://dev.azure.com/example/Payments/_git/payments%20api/pullrequest/7"
        );
        // A branch that is not a plain name, or another organization, is never asked.
        assert!(
            pull_requests(
                &client,
                "key",
                CATALOG,
                "https://dev.azure.com/example",
                "Payments",
                (REPO, "payments-api"),
                "x;rm"
            )
            .await
            .is_err()
        );
        assert!(
            pull_requests(
                &client,
                "key",
                CATALOG,
                "https://dev.azure.com/other",
                "Payments",
                (REPO, "payments-api"),
                "feature/42"
            )
            .await
            .is_err()
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[tokio::test]
    async fn pushed_branches_are_the_users_own_since_the_time_and_never_a_deleted_one() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        let me = json!({"uniqueName": "Me@Example.Test"});
        let other = json!({"uniqueName": "someone@example.test"});
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/pushes"
            )))
            .and(query_param("searchCriteria.includeRefUpdates", "true"))
            .and(query_param("searchCriteria.toDate", "2026-11-20T12:00:00+00:00"))
            // Newest first, as Azure DevOps lists them.
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"date":"2026-11-20T10:09:00Z","pushedBy":me,"repository":{"id":REPO},"refUpdates":[
                    {"name":"refs/heads/feature/42","newObjectId":"b1"},
                    {"name":"refs/heads/gone","newObjectId":"0000000000000000000000000000000000000000"},
                    {"name":"refs/heads/back","newObjectId":"e2"}]},
                {"date":"2026-11-20T10:06:00Z","pushedBy":other,"repository":{"id":REPO},"refUpdates":[
                    {"name":"refs/heads/theirs","newObjectId":"c1"}]},
                {"date":"2026-11-20T10:05:00Z","pushedBy":me,"repository":{"id":REPO},"refUpdates":[
                    {"name":"refs/heads/feature/42","newObjectId":"a1"},
                    {"name":"refs/heads/gone","newObjectId":"a4"},
                    {"name":"refs/heads/back","newObjectId":"0000000000000000000000000000000000000000"},
                    {"name":"refs/tags/v1","newObjectId":"a2"},
                    {"name":"refs/heads/bad name","newObjectId":"a3"}]},
                {"date":"2026-11-19T10:00:00Z","pushedBy":me,"repository":{"id":REPO},"refUpdates":[
                    {"name":"refs/heads/old","newObjectId":"d1"}]}
            ]})))
            .mount(&server)
            .await;
        let at = |t: &str| {
            chrono::DateTime::parse_from_rfc3339(t)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let (pushed, full) = pushed_branches(
            &Client::new(),
            "key",
            CATALOG,
            "https://dev.azure.com/example",
            "Payments",
            (REPO, "payments api"),
            (at("2026-11-20T10:00:00Z"), at("2026-11-20T12:00:00Z")),
        )
        .await
        .unwrap();
        assert!(!full);
        // Each branch as its latest push left it: deleted last is gone, deleted then pushed again
        // is there.
        assert_eq!(
            pushed.iter().map(|p| p.branch.as_str()).collect::<Vec<_>>(),
            ["back", "feature/42"],
            "{pushed:?}"
        );
        assert_eq!(pushed[1].pushed_at, "2026-11-20T10:09:00Z");
        assert_eq!(pushed[1].repository, REPO);
        assert_eq!(
            pushed[1].url,
            "https://dev.azure.com/example/Payments/_git/payments%20api?version=GBfeature%2F42"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[tokio::test]
    async fn a_pull_request_is_read_by_its_id_and_a_missing_one_is_none() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/pullrequests/9"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "pullRequestId":9,"status":"active","title":"Linked","sourceRefName":"refs/heads/feature/linked",
                "repository":{"id":REPO},"createdBy":{"uniqueName":"me@example.test"},"creationDate":"2026-11-20T10:30:00Z"
            })))
            .mount(&server)
            .await;
        for (number, by, date) in [
            (11, "someone@example.test", "2026-11-20T10:30:00Z"),
            (12, "me@example.test", "2026-11-01T10:30:00Z"),
        ] {
            Mock::given(method("GET"))
                .and(path(format!(
                    "/example/Payments/_apis/git/repositories/{REPO}/pullrequests/{number}"
                )))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "pullRequestId":number,"status":"active","title":"Not this run's","sourceRefName":"refs/heads/x",
                    "repository":{"id":REPO},"createdBy":{"uniqueName":by},"creationDate":date
                })))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/pullrequests/10"
            )))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = Client::new();
        let since = chrono::DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let read = |number| {
            pull_request(
                &client,
                "key",
                CATALOG,
                "https://dev.azure.com/example",
                "Payments",
                (REPO, "payments-api"),
                number,
                since,
            )
        };
        let pull = read(9).await.unwrap().unwrap();
        assert_eq!((pull.id, pull.status.as_str()), (9, "active"));
        assert_eq!(pull.source.as_deref(), Some("feature/linked"));
        assert_eq!(
            pull.url,
            "https://dev.azure.com/example/Payments/_git/payments-api/pullrequest/9"
        );
        assert!(read(10).await.unwrap().is_none());
        // Someone else's, and one opened before the time, are not the run's.
        assert!(read(11).await.unwrap().is_none());
        assert!(read(12).await.unwrap().is_none());
        assert!(read(0).await.is_err());
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[test]
    fn a_wiki_file_is_read_as_its_page_path() {
        assert_eq!(
            page_path("/Procedures/Deploy-the-portal%2Dv2.md").as_deref(),
            Some("/Procedures/Deploy the portal-v2")
        );
        assert_eq!(page_path("/Home.md").as_deref(), Some("/Home"));
        assert_eq!(page_path("/Q%26A+notes.md").as_deref(), Some("/Q&A+notes"));
        assert_eq!(page_path(&format!("/{}.md", "a".repeat(400))), None);
        for not_a_page in ["/.attachments/x.md", "/Procedures/.order", "/a.png", "x.md"] {
            assert_eq!(page_path(not_a_page), None, "{not_a_page}");
        }
    }

    #[tokio::test]
    async fn wiki_pages_are_the_users_own_changes_to_the_project_wiki_since_the_time() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        const WIKI: &str = "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f";
        const CODE: &str = "2a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f";
        let (mine, theirs) = ("a".repeat(40), "b".repeat(40));
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wiki/wikis"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"Payments.wiki","type":"projectWiki","repositoryId":WIKI},
                {"name":"Code docs","type":"codeWiki","repositoryId":CODE}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{WIKI}/commits"
            )))
            .and(query_param("searchCriteria.author", "me@example.test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"commitId":mine,"author":{"email":"me@example.test","date":"2026-11-20T11:00:00Z"}},
                {"commitId":theirs,"author":{"email":"someone@example.test","date":"2026-11-20T11:00:00Z"}}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{WIKI}/commits/{mine}/changes"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"changes":[
                {"changeType":"edit","item":{"gitObjectType":"blob","path":"/Procedures/Deploy-the-portal.md"}},
                {"changeType":"delete","item":{"gitObjectType":"blob","path":"/Old.md"}},
                {"changeType":"add","item":{"gitObjectType":"blob","path":"/.attachments/shot.png"}}
            ]})))
            .mount(&server)
            .await;
        let at = |t: &str| {
            chrono::DateTime::parse_from_rfc3339(t)
                .unwrap()
                .with_timezone(&chrono::Utc)
        };
        let (pages, partial) = wiki_pages(
            &Client::new(),
            "key",
            CATALOG,
            "https://dev.azure.com/example",
            "Payments",
            (at("2026-11-20T10:00:00Z"), at("2026-11-20T12:00:00Z")),
        )
        .await
        .unwrap();
        assert!(!partial);
        assert_eq!(pages.len(), 1, "{pages:?}");
        assert_eq!(pages[0].path, "/Procedures/Deploy the portal");
        assert_eq!(pages[0].wiki, "Payments.wiki");
        assert_eq!(
            pages[0].url,
            "https://dev.azure.com/example/Payments/_wiki/wikis/Payments.wiki?pagePath=%2FProcedures%2FDeploy+the+portal"
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        // The code Wiki's repository is never read, nor the other author's commit.
        assert!(!requests.iter().any(|r| r.url.path().contains(CODE)));
        assert!(!requests.iter().any(|r| r.url.path().contains(&theirs)));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[tokio::test]
    async fn a_branch_exists_only_when_its_exact_ref_is_listed_and_nothing_is_written() {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(Url::parse(&server.uri()).unwrap()));
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/refs"
            )))
            .and(query_param("filter", "heads/feature/42"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"refs/heads/feature/42-other","objectId":"a"},
                {"name":"refs/heads/feature/42","objectId":"b"}
            ]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/example/Payments/_apis/git/repositories/{REPO}/refs"
            )))
            .and(query_param("filter", "heads/feature/4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"refs/heads/feature/42","objectId":"b"}
            ]})))
            .mount(&server)
            .await;
        let client = Client::new();
        let exists = |branch: &'static str| {
            let client = client.clone();
            async move {
                branch_exists(
                    &client,
                    "key",
                    CATALOG,
                    "https://dev.azure.com/example",
                    "Payments",
                    REPO,
                    branch,
                )
                .await
            }
        };
        assert!(exists("feature/42").await.unwrap());
        // A prefix of an existing branch is not that branch.
        assert!(!exists("feature/4").await.unwrap());
        assert!(exists("x;rm").await.is_err());
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        super::super::TEST_SERVER.with(|s| *s.borrow_mut() = None);
    }

    #[test]
    fn an_item_is_assigned_when_its_assigned_to_or_a_developer_role_names_the_user() {
        let detail = |reference: &str, name: &str, value: serde_json::Value| Detail {
            item: crate::tools::todo::Item {
                id: 1,
                title: "T".into(),
                url: "https://dev.azure.com/example/Payments/_workitems/edit/1".into(),
                project: "Payments".into(),
                state: "New".into(),
                kind: "User Story".into(),
                category: "requirement".into(),
                created_at: None,
                changed_at: None,
                dates: Vec::new(),
            },
            fields: vec![crate::tools::todo::DetailField {
                reference: reference.into(),
                name: name.into(),
                kind: "string".into(),
                value,
                content: None,
            }],
            comments: Vec::new(),
            relations: Vec::new(),
            history: Vec::new(),
            partial: false,
            warnings: Vec::new(),
        };
        let me = json!({"displayName":"Me","uniqueName":"ME@example.test"});
        let other = json!({"displayName":"Other","uniqueName":"other@example.test"});
        assert!(
            assigned(
                CATALOG,
                &detail("System.AssignedTo", "Assigned To", me.clone())
            )
            .unwrap()
        );
        assert!(
            assigned(
                CATALOG,
                &detail("Custom.Dev1", "Desarrollador 1", me.clone())
            )
            .unwrap()
        );
        assert!(!assigned(CATALOG, &detail("System.AssignedTo", "Assigned To", other)).unwrap());
        assert!(!assigned(CATALOG, &detail("System.CreatedBy", "Created By", me)).unwrap());
    }
}
