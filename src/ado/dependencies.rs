//! The dependencies of a work item as Azure DevOps records them, read-only with the enabled
//! source's read credential: its Predecessor links (`System.LinkTypes.Dependency-Reverse`), each
//! predecessor's identity, state and the category of that state in its type's workflow
//! (`Proposed`, `InProgress`, `Resolved`, `Completed`, `Removed`), the pull requests linked to it,
//! and the predecessors of the predecessors as far as a cycle can reach (bounded). Nothing is
//! inferred: a dependency is a link someone made. Nothing here writes.
use super::{Catalog, Source, organization_url, project_url, request};
use anyhow::{Context, Result, ensure};
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The link type of a predecessor, as its successor holds it.
pub const PREDECESSOR: &str = "System.LinkTypes.Dependency-Reverse";
/// At most this many work items are read for one item's graph (its predecessors, theirs...).
pub const MAX_NODES: usize = 50;
/// At most this many links are followed from the item.
pub const MAX_DEPTH: usize = 10;
/// At most this many linked pull requests are read for one predecessor.
pub const MAX_PULLS: usize = 20;

/// A pull request linked to a work item, read fresh.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LinkedPull {
    pub id: u64,
    /// The repository's ID, in lowercase.
    pub repository: String,
    pub repository_name: String,
    /// `active`, `completed` or `abandoned`.
    pub status: String,
    /// The branch it merges into (`refs/heads/NAME`).
    pub target: String,
    /// Its verified link.
    pub url: String,
}

/// One work item of the graph.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: u64,
    /// Its verified link (empty when it could not be read).
    pub url: String,
    pub title: String,
    pub kind: String,
    pub project: String,
    pub state: String,
    /// The category of its state in its type's workflow, empty when it could not be read.
    pub category: String,
    /// Its own predecessors in the same organization.
    pub predecessors: Vec<u64>,
    /// The pull requests linked to it (read for the item's direct predecessors only).
    pub pull_requests: Vec<LinkedPull>,
    /// Why it cannot count: `missing` (no such item, or not readable with this credential).
    pub problem: Option<String>,
}

/// An item's predecessors and theirs, as far as the bounds reach.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub root: u64,
    /// The item's own predecessors, in the order of its links.
    pub direct: Vec<u64>,
    /// Predecessor links that name another organization (their URL as recorded).
    pub foreign: Vec<String>,
    pub nodes: BTreeMap<u64, Node>,
    /// The bounds stopped the walk before every link was followed.
    pub truncated: bool,
}

/// The organization a link names: `NAME` of `https://dev.azure.com/NAME/…` or
/// `https://NAME.visualstudio.com/…`, lowercase.
fn link_organization(url: &str) -> Option<String> {
    let url = reqwest::Url::parse(url).ok()?;
    let host = url.host_str()?.to_lowercase();
    if host == "dev.azure.com" {
        return url
            .path_segments()?
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_lowercase);
    }
    host.strip_suffix(".visualstudio.com").map(str::to_owned)
}

/// The predecessors an item's relations name: those of the source's organization by ID, the
/// others by their link.
fn predecessors(source: &Source, item: &Value) -> (Vec<u64>, Vec<String>) {
    let own = link_organization(&source.organization);
    let (mut ids, mut foreign) = (Vec::new(), Vec::new());
    for relation in item["relations"].as_array().into_iter().flatten() {
        if relation["rel"].as_str() != Some(PREDECESSOR) {
            continue;
        }
        let url = relation["url"].as_str().unwrap_or_default();
        let id = url
            .rsplit('/')
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0);
        match id {
            Some(id) if link_organization(url) == own => {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            _ => foreign.push(url.chars().take(600).collect()),
        }
    }
    (ids, foreign)
}

/// The pull requests an item's relations link (`vstfs:///Git/PullRequestId/PROJECT%2FREPO%2FID`),
/// as (repository ID, pull request ID).
fn linked_pulls(item: &Value) -> Vec<(String, u64)> {
    let mut found = Vec::new();
    for relation in item["relations"].as_array().into_iter().flatten() {
        let url = relation["url"].as_str().unwrap_or_default();
        let Some(rest) = url.strip_prefix("vstfs:///Git/PullRequestId/") else {
            continue;
        };
        let decoded = rest.replace("%2F", "/").replace("%2f", "/");
        let parts: Vec<&str> = decoded.split('/').collect();
        if let [_, repository, id] = parts.as_slice()
            && uuid::Uuid::parse_str(repository).is_ok()
            && let Ok(id) = id.parse::<u64>()
            && id > 0
            && !found
                .iter()
                .any(|(r, i): &(String, u64)| *i == id && r == repository)
        {
            found.push((repository.to_lowercase(), id));
        }
    }
    found.truncate(MAX_PULLS);
    found
}

/// Read work items by ID with their relations; a missing one is absent from the answer.
async fn read_items(
    client: &Client,
    key: &str,
    source: &Source,
    ids: &[u64],
) -> Result<BTreeMap<u64, Value>> {
    let mut found = BTreeMap::new();
    for chunk in ids.chunks(200) {
        let mut url = organization_url(source, &["_apis", "wit", "workitems"])?;
        url.query_pairs_mut()
            .append_pair(
                "ids",
                &chunk
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            )
            .append_pair("$expand", "relations")
            .append_pair("errorPolicy", "Omit");
        let reply = request(client, key, Method::GET, url, None).await?;
        for item in reply["value"].as_array().into_iter().flatten() {
            if let Some(id) = item["id"].as_u64().filter(|id| chunk.contains(id)) {
                found.insert(id, item.clone());
            }
        }
    }
    Ok(found)
}

/// The category of each state of a type, read from its workflow (cached like the process).
async fn categories(
    client: &Client,
    key: &str,
    cache: &crate::state::Store,
    source: &Source,
    project: &str,
    kind: &str,
) -> Result<BTreeMap<String, String>> {
    let url = project_url(
        source,
        project,
        "dev.azure.com",
        &["_apis", "wit", "workitemtypes", kind, "states"],
    )?;
    let reply = super::todo::metadata(client, key, cache, url).await?;
    Ok(reply["value"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some((
                s["name"].as_str()?.to_owned(),
                s["category"].as_str()?.to_owned(),
            ))
        })
        .collect())
}

/// One linked pull request, read fresh by its repository and ID.
async fn read_pull(
    client: &Client,
    key: &str,
    source: &Source,
    (repository, id): &(String, u64),
) -> Result<LinkedPull> {
    let url = organization_url(
        source,
        &[
            "_apis",
            "git",
            "repositories",
            repository,
            "pullrequests",
            &id.to_string(),
        ],
    )?;
    let pull = request(client, key, Method::GET, url, None).await?;
    ensure!(
        pull["pullRequestId"].as_u64() == Some(*id)
            && pull["repository"]["id"]
                .as_str()
                .is_some_and(|r| r.eq_ignore_ascii_case(repository)),
        "invalid Azure DevOps pull request"
    );
    let name = pull["repository"]["name"]
        .as_str()
        .context("invalid Azure DevOps pull request")?;
    let project = pull["repository"]["project"]["name"]
        .as_str()
        .context("invalid Azure DevOps pull request")?;
    let mut link = project_url(
        source,
        project,
        "dev.azure.com",
        &["_git", name, "pullrequest", &id.to_string()],
    )?;
    link.set_query(None);
    let status = pull["status"]
        .as_str()
        .filter(|s| matches!(*s, "active" | "completed" | "abandoned"))
        .unwrap_or("unknown");
    Ok(LinkedPull {
        id: *id,
        repository: repository.clone(),
        repository_name: name.chars().take(200).collect(),
        status: status.into(),
        target: pull["targetRefName"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(300)
            .collect(),
        url: crate::tools::todo::https_link(link.as_str()).context("invalid pull request link")?,
    })
}

/// The predecessor graph of item `root` of `organization`: its predecessors, each with its
/// state, the category of that state and its linked pull requests, and theirs as far as
/// [`MAX_NODES`] and [`MAX_DEPTH`] reach (their pull requests are not read).
pub(crate) async fn graph(
    client: &Client,
    key: &str,
    catalog: &str,
    cache: &crate::state::Store,
    organization: &str,
    root: u64,
) -> Result<Graph> {
    let wanted = crate::workflows::organization_name(organization)
        .context("invalid organization: use https://dev.azure.com/NAME")?;
    let source = Catalog::parse(catalog)?
        .sources
        .into_iter()
        .find(|s| crate::workflows::organization_name(&s.organization).as_ref() == Some(&wanted))
        .context("invalid organization: it is not in the Azure source's catalog")?;
    let read = read_items(client, key, &source, &[root]).await?;
    let item = read
        .get(&root)
        .context("The work item could not be read.")?;
    let (direct, foreign) = predecessors(&source, item);
    let mut graph = Graph {
        root,
        direct: direct.clone(),
        foreign,
        ..Default::default()
    };
    let mut seen: BTreeSet<u64> = BTreeSet::from([root]);
    let mut queue: VecDeque<(u64, usize)> = direct.iter().map(|id| (*id, 1)).collect();
    let mut raw = BTreeMap::new();
    while !queue.is_empty() {
        // One batch per level of the walk.
        let mut level = Vec::new();
        while let Some((id, depth)) = queue.pop_front() {
            if seen.contains(&id) {
                continue;
            }
            if seen.len() > MAX_NODES || depth > MAX_DEPTH {
                graph.truncated = true;
                continue;
            }
            seen.insert(id);
            level.push((id, depth));
        }
        let ids: Vec<u64> = level.iter().map(|(id, _)| *id).collect();
        let found = read_items(client, key, &source, &ids).await?;
        for (id, depth) in level {
            let Some(item) = found.get(&id) else {
                graph.nodes.insert(
                    id,
                    Node {
                        id,
                        problem: Some("missing".into()),
                        ..Default::default()
                    },
                );
                continue;
            };
            let (own, _) = predecessors(&source, item);
            for next in &own {
                queue.push_back((*next, depth + 1));
            }
            raw.insert(id, item.clone());
            let project = super::field(item, "System.TeamProject").to_owned();
            let mut url = project_url(
                &source,
                &project,
                "dev.azure.com",
                &["_workitems", "edit", &id.to_string()],
            )?;
            url.set_query(None);
            graph.nodes.insert(
                id,
                Node {
                    id,
                    url: crate::tools::todo::https_link(url.as_str()).unwrap_or_default(),
                    title: super::field(item, "System.Title")
                        .chars()
                        .take(500)
                        .collect(),
                    kind: super::field(item, "System.WorkItemType").into(),
                    project,
                    state: super::field(item, "System.State").into(),
                    predecessors: own,
                    ..Default::default()
                },
            );
        }
    }
    // The category of each state, by project and type (each read once, cached).
    let mut kinds: BTreeMap<(String, String), BTreeMap<String, String>> = BTreeMap::new();
    for node in graph.nodes.values_mut().filter(|n| n.problem.is_none()) {
        let pair = (node.project.clone(), node.kind.clone());
        if !kinds.contains_key(&pair) {
            let read = categories(client, key, cache, &source, &pair.0, &pair.1)
                .await
                .unwrap_or_default();
            kinds.insert(pair.clone(), read);
        }
        node.category = kinds[&pair].get(&node.state).cloned().unwrap_or_default();
    }
    // The pull requests linked to the item's own predecessors.
    for id in &graph.direct {
        let Some(item) = raw.get(id) else { continue };
        let mut pulls = Vec::new();
        for linked in linked_pulls(item) {
            pulls.push(read_pull(client, key, &source, &linked).await?);
        }
        if let Some(node) = graph.nodes.get_mut(id) {
            node.pull_requests = pulls;
        }
    }
    Ok(graph)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, query_param},
    };

    const CATALOG: &str = "[[sources]]\norganization='https://dev.azure.com/example'\nprojects=['Payments']\nauthor_email='me@example.test'";
    const REPO: &str = "1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f";

    fn item(id: u64, state: &str, relations: serde_json::Value) -> serde_json::Value {
        json!({"id": id, "fields": {"System.Title": format!("Item {id}"), "System.TeamProject": "Payments",
            "System.WorkItemType": "Task", "System.State": state}, "relations": relations})
    }
    fn predecessor(id: u64) -> serde_json::Value {
        json!({"rel": PREDECESSOR, "url": format!("https://dev.azure.com/example/_apis/wit/workItems/{id}")})
    }

    #[tokio::test]
    async fn the_graph_follows_predecessors_reads_states_and_linked_pull_requests_and_writes_nothing()
     {
        let server = MockServer::start().await;
        super::super::TEST_SERVER
            .with(|s| *s.borrow_mut() = Some(reqwest::Url::parse(&server.uri()).unwrap()));
        // 50 needs 41 (with pull request 6119) and 99 (missing); 41 needs 42; 42 needs 50.
        let pr = json!({"rel":"ArtifactLink","url":format!("vstfs:///Git/PullRequestId/0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f%2F{REPO}%2F6119"),"attributes":{"name":"Pull Request"}});
        Mock::given(method("GET"))
            .and(path("/example/_apis/wit/workitems"))
            .and(query_param("ids", "50"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value":[item(50, "Ready", json!([predecessor(41), predecessor(99)]))]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/_apis/wit/workitems"))
            .and(query_param("ids", "41,99"))
            .and(query_param("$expand", "relations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"value":[item(41, "In Progress", json!([predecessor(42), pr])), null]}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/_apis/wit/workitems"))
            .and(query_param("ids", "42"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"value":[item(42, "Active", json!([predecessor(50)]))]})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/example/Payments/_apis/wit/workitemtypes/Task/states"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value":[
                {"name":"In Progress","category":"InProgress"},{"name":"Active","category":"InProgress"},{"name":"Done","category":"Completed"}]})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/example/_apis/git/repositories/{REPO}/pullrequests/6119")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"pullRequestId":6119,"status":"active",
                "targetRefName":"refs/heads/develop","repository":{"id":REPO,"name":"api admin","project":{"name":"Payments"}}})))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = crate::state::Store::open(&dir.path().join("cache.db")).unwrap();
        let graph = graph(
            &reqwest::Client::new(),
            "key",
            CATALOG,
            &cache,
            "https://dev.azure.com/example",
            50,
        )
        .await
        .unwrap();
        assert_eq!(graph.direct, vec![41, 99]);
        assert!(!graph.truncated);
        let first = &graph.nodes[&41];
        assert_eq!(
            first.url,
            "https://dev.azure.com/example/Payments/_workitems/edit/41"
        );
        assert_eq!(
            (first.state.as_str(), first.category.as_str()),
            ("In Progress", "InProgress")
        );
        assert_eq!(first.predecessors, vec![42]);
        assert_eq!(first.pull_requests.len(), 1);
        assert_eq!(first.pull_requests[0].target, "refs/heads/develop");
        assert_eq!(
            first.pull_requests[0].url,
            "https://dev.azure.com/example/Payments/_git/api%20admin/pullrequest/6119"
        );
        assert_eq!(graph.nodes[&99].problem.as_deref(), Some("missing"));
        assert_eq!(graph.nodes[&42].predecessors, vec![50]);
        // Only the indirect predecessor's pull requests are not read; nothing is written.
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.method.as_str() == "GET"));
        // Another organization is never asked.
        assert!(graph_other(&cache).await.is_err());
    }

    async fn graph_other(cache: &crate::state::Store) -> Result<Graph> {
        graph(
            &reqwest::Client::new(),
            "key",
            CATALOG,
            cache,
            "https://dev.azure.com/other",
            50,
        )
        .await
    }

    fn source() -> Source {
        Source {
            organization: "https://dev.azure.com/Example".into(),
            projects: vec!["Payments".into()],
            author_email: "person@example.test".into(),
        }
    }

    #[test]
    fn only_predecessor_links_count_and_another_organization_is_kept_apart() {
        let item = serde_json::json!({"relations":[
            {"rel":PREDECESSOR,"url":"https://dev.azure.com/example/_apis/wit/workItems/41"},
            {"rel":"System.LinkTypes.Dependency-Forward","url":"https://dev.azure.com/example/_apis/wit/workItems/42"},
            {"rel":"System.LinkTypes.Related","url":"https://dev.azure.com/example/_apis/wit/workItems/43"},
            {"rel":PREDECESSOR,"url":"https://example.visualstudio.com/_apis/wit/workItems/44"},
            {"rel":PREDECESSOR,"url":"https://dev.azure.com/other/_apis/wit/workItems/45"},
            {"rel":PREDECESSOR,"url":"https://dev.azure.com/example/_apis/wit/workItems/41"},
            {"rel":"ArtifactLink","url":"vstfs:///Git/PullRequestId/0f3c6a2e-5b1d-4f7a-9c8e-1a2b3c4d5e6f%2F1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f%2F6119","attributes":{"name":"Pull Request"}},
            {"rel":"ArtifactLink","url":"vstfs:///Git/Ref/project%2Frepo%2FGBfeature","attributes":{"name":"Branch"}}
        ]});
        let (ids, foreign) = predecessors(&source(), &item);
        assert_eq!(ids, vec![41, 44]);
        assert_eq!(
            foreign,
            vec!["https://dev.azure.com/other/_apis/wit/workItems/45".to_owned()]
        );
        assert_eq!(
            linked_pulls(&item),
            vec![("1a2b3c4d-5b1d-4f7a-9c8e-0f3c6a2e5e6f".to_owned(), 6119)]
        );
    }
}
