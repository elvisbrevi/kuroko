//! A synthetic Azure DevOps catalog for the engine's tests: work items, who they are assigned
//! to and the pull requests of each repository branch. It has no way to write: every request
//! the engine makes is recorded, and each is a read.
// Only the Unix tests use the catalog (the fake provider CLIs are shell scripts), so on Windows its
// items are unused: allowed there rather than gated one by one.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
use crate::ado::workflow::PullRequest;
use crate::tools::todo::{Detail, DetailField, Item};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[derive(Default)]
pub(in crate::app) struct Catalog {
    items: Mutex<BTreeMap<u64, Detail>>,
    assigned: Mutex<BTreeSet<u64>>,
    pulls: Mutex<BTreeMap<(String, String), Vec<PullRequest>>>,
    /// The branches that exist, by repository (lowercased) and name.
    branches: Mutex<BTreeSet<(String, String)>>,
    /// Session summaries by implementation run, as the supervisor reads them.
    summaries: Mutex<BTreeMap<String, serde_json::Value>>,
    /// Predecessor links by successor, and the predecessors' facts (a test's dependencies).
    links: Mutex<BTreeMap<u64, Vec<u64>>>,
    nodes: Mutex<BTreeMap<u64, crate::ado::dependencies::Node>>,
    /// The files each pull request conflicts on.
    conflict_files: Mutex<BTreeMap<u64, Vec<String>>>,
    /// The comment threads of each pull request.
    pull_threads: Mutex<BTreeMap<u64, Vec<crate::ado::workflow::Thread>>>,
    /// Every request, as `GET what`.
    pub requests: Mutex<Vec<String>>,
    /// The items whose reads fail, as during an outage.
    pub failing_items: Mutex<BTreeSet<u64>>,
    /// Pull request reads fail.
    pub failing_pulls: std::sync::atomic::AtomicBool,
    /// The ready-state listings fail, as during an outage.
    pub failing_lists: std::sync::atomic::AtomicBool,
    /// The listing of this one state fails.
    pub failing_state: Mutex<Option<String>>,
    /// Pushes, by who pushed them (`me` is the source's user).
    pushes: Mutex<Vec<(String, crate::ado::workflow::Pushed)>>,
    /// Wiki page changes, by who made them (`me` is the source's user).
    wiki: Mutex<Vec<(String, crate::ado::workflow::WikiPage)>>,
    /// Wiki pages and repository files a lead reads, by `page:PATH` or `REPOSITORY:PATH`.
    texts: Mutex<BTreeMap<String, String>>,
    /// The most items a listing may return, whatever the limit asked (a catalog larger than
    /// the read's bound).
    pub list_cap: Mutex<Option<usize>>,
    /// The task-category types whose work item guide lists them with the bugs (a development
    /// bug): the To-Do list without a kind holds them, its `task` kind does not.
    pub listed_with_bugs: Mutex<BTreeSet<String>>,
    /// What the source's user wrote in the project (work items created, state changes).
    pub written: Mutex<Vec<crate::ado::workflow::Written>>,
}

#[cfg(unix)]
pub(in crate::app) const ORGANIZATION: &str = "https://dev.azure.com/example";

/// A work item of project Payments (also built by tests that run on every platform).
pub(in crate::app) fn detail(
    id: u64,
    kind: &str,
    category: &str,
    title: &str,
    text: &str,
) -> Detail {
    Detail {
        item: Item {
            id,
            title: title.into(),
            url: format!("https://dev.azure.com/example/Payments/_workitems/edit/{id}"),
            project: "Payments".into(),
            state: "Ready".into(),
            kind: kind.into(),
            category: category.into(),
            created_at: None,
            changed_at: None,
            dates: Vec::new(),
        },
        fields: vec![DetailField {
            reference: "System.Description".into(),
            name: "Description".into(),
            kind: "html".into(),
            value: serde_json::Value::String(format!("<p>{text}</p>")),
            content: None,
        }],
        comments: Vec::new(),
        relations: Vec::new(),
        history: Vec::new(),
        partial: false,
        warnings: Vec::new(),
    }
}

impl Catalog {
    #[cfg(unix)]
    pub(in crate::app) fn add(&self, detail: Detail, assigned: bool) {
        if assigned {
            self.assigned.lock().unwrap().insert(detail.item.id);
        }
        self.items.lock().unwrap().insert(detail.item.id, detail);
    }
    /// The pull requests of a repository's branch, as the next read returns them.
    #[cfg(unix)]
    pub(in crate::app) fn set_pulls(
        &self,
        repository: &str,
        branch: &str,
        pulls: Vec<PullRequest>,
    ) {
        self.pulls
            .lock()
            .unwrap()
            .insert((repository.to_lowercase(), branch.to_owned()), pulls);
    }
    /// The pull requests of a repository's branch as they are now.
    #[cfg(unix)]
    pub(in crate::app) fn clone_pulls(&self, repository: &str, branch: &str) -> Vec<PullRequest> {
        self.pulls
            .lock()
            .unwrap()
            .get(&(repository.to_lowercase(), branch.to_owned()))
            .cloned()
            .unwrap_or_default()
    }
    /// The summary of an implementation run's session, as the next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn set_summary(&self, run: &str, summary: serde_json::Value) {
        self.summaries.lock().unwrap().insert(run.into(), summary);
    }
    pub(in crate::app) fn sessions(&self, runs: &[String]) -> Vec<serde_json::Value> {
        let summaries = self.summaries.lock().unwrap();
        runs.iter()
            .filter_map(|run| summaries.get(run).cloned())
            .collect()
    }
    /// `successor` needs `predecessor` (a Predecessor link), as the next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn link(&self, successor: u64, predecessor: u64) {
        self.links
            .lock()
            .unwrap()
            .entry(successor)
            .or_default()
            .push(predecessor);
    }
    /// A predecessor's facts (state, category, linked pull requests), as the next read
    /// returns them; one without facts reads as missing.
    #[cfg(unix)]
    pub(in crate::app) fn set_node(&self, node: crate::ado::dependencies::Node) {
        self.nodes.lock().unwrap().insert(node.id, node);
    }
    /// The predecessor graph of `root`, as `ado::dependencies::graph` reads it.
    pub(in crate::app) fn graph(
        &self,
        root: u64,
    ) -> Result<crate::ado::dependencies::Graph, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET dependencies {root}"));
        let links = self.links.lock().unwrap();
        let nodes = self.nodes.lock().unwrap();
        let direct = links.get(&root).cloned().unwrap_or_default();
        let mut graph = crate::ado::dependencies::Graph {
            root,
            direct: direct.clone(),
            ..Default::default()
        };
        let mut queue: Vec<u64> = direct;
        while let Some(id) = queue.pop() {
            if id == root || graph.nodes.contains_key(&id) {
                continue;
            }
            let mut node = nodes
                .get(&id)
                .cloned()
                .unwrap_or(crate::ado::dependencies::Node {
                    id,
                    problem: Some("missing".into()),
                    ..Default::default()
                });
            if node.problem.is_none() {
                node.predecessors = links.get(&id).cloned().unwrap_or_default();
                queue.extend(node.predecessors.iter().copied());
            }
            graph.nodes.insert(id, node);
        }
        Ok(graph)
    }
    /// The files a pull request conflicts on, as the next read returns them.
    #[cfg(unix)]
    pub(in crate::app) fn set_conflicts(&self, pull_request: u64, files: &[&str]) {
        self.conflict_files
            .lock()
            .unwrap()
            .insert(pull_request, files.iter().map(|f| f.to_string()).collect());
    }
    pub(in crate::app) fn conflicts(&self, pull_request: u64) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET conflicts {pull_request}"));
        self.conflict_files
            .lock()
            .unwrap()
            .get(&pull_request)
            .cloned()
            .unwrap_or_default()
    }
    /// The comment threads of a pull request, as the next read returns them.
    #[cfg(unix)]
    pub(in crate::app) fn set_threads(
        &self,
        pull_request: u64,
        threads: Vec<crate::ado::workflow::Thread>,
    ) {
        self.pull_threads
            .lock()
            .unwrap()
            .insert(pull_request, threads);
    }
    pub(in crate::app) fn threads(&self, pull_request: u64) -> Vec<crate::ado::workflow::Thread> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET threads {pull_request}"));
        self.pull_threads
            .lock()
            .unwrap()
            .get(&pull_request)
            .cloned()
            .unwrap_or_default()
    }
    /// Every repository's default branch: `main`.
    pub(in crate::app) fn default_branches(&self) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::new()
    }
    /// A branch that exists in a repository, as the next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn set_branch(&self, repository: &str, branch: &str) {
        self.branches
            .lock()
            .unwrap()
            .insert((repository.to_lowercase(), branch.to_owned()));
    }
    pub(in crate::app) fn branch_exists(&self, repository: &str, branch: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET refs {repository} heads/{branch}"));
        self.branches
            .lock()
            .unwrap()
            .contains(&(repository.to_lowercase(), branch.to_owned()))
    }
    /// Add a comment to an item, as its next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn comment(&self, id: u64, author: &str, text: &str) {
        if let Some(detail) = self.items.lock().unwrap().get_mut(&id) {
            detail.comments.push(crate::tools::todo::Comment {
                author: author.into(),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
                modified_at: None,
                text: text.into(),
                format: "markdown".into(),
                content: None,
            });
        }
    }
    pub(in crate::app) fn item(&self, id: u64) -> Result<Detail, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET workitems/{id}"));
        if self.failing_items.lock().unwrap().contains(&id) {
            return Err("[network] Azure work items could not be read.".into());
        }
        self.items
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("[invalid_input] invalid work item: {id} not found"))
    }
    pub(in crate::app) fn assigned(&self, id: u64) -> bool {
        self.assigned.lock().unwrap().contains(&id)
    }
    pub(in crate::app) fn pull_requests(
        &self,
        repository: &str,
        branch: &str,
    ) -> Result<Vec<PullRequest>, String> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET pullrequests {repository} refs/heads/{branch}"));
        if self.failing_pulls.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("[network] The project's pull requests could not be read.".into());
        }
        Ok(self
            .pulls
            .lock()
            .unwrap()
            .get(&(repository.to_lowercase(), branch.to_owned()))
            .cloned()
            .unwrap_or_default())
    }
    /// A push of `branch` to a repository by `user` (`me` is the source's user), at `at`.
    #[cfg(unix)]
    pub(in crate::app) fn push(&self, user: &str, repository: &str, branch: &str, at: i64) {
        let at = chrono::DateTime::from_timestamp(at, 0)
            .unwrap()
            .to_rfc3339();
        self.pushes.lock().unwrap().push((
            user.into(),
            crate::ado::workflow::Pushed {
                repository: repository.to_lowercase(),
                branch: branch.into(),
                pushed_at: at,
                url: format!(
                    "https://dev.azure.com/example/Payments/_git/{repository}?version=GB{branch}"
                ),
            },
        ));
    }
    /// Forget every push, as when the branches were deleted.
    #[cfg(unix)]
    pub(in crate::app) fn clear_pushes(&self) {
        self.pushes.lock().unwrap().clear();
    }
    /// Link a work item to something (a pull request, a branch), as its next read returns it.
    #[cfg(unix)]
    pub(in crate::app) fn relate(&self, id: u64, name: &str, url: &str) {
        if let Some(detail) = self.items.lock().unwrap().get_mut(&id) {
            detail.relations.push(crate::tools::todo::Relation {
                name: name.into(),
                url: Some(url.into()),
                detail: name.into(),
                item: None,
                task: false,
            });
        }
    }
    /// The branches `me` pushed to a repository in a window.
    pub(in crate::app) fn pushed_branches(
        &self,
        repository: &str,
        (since, until): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
    ) -> Vec<crate::ado::workflow::Pushed> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET pushes {repository}"));
        self.pushes
            .lock()
            .unwrap()
            .iter()
            .filter(|(user, p)| {
                user == "me"
                    && p.repository.eq_ignore_ascii_case(repository)
                    && chrono::DateTime::parse_from_rfc3339(&p.pushed_at)
                        .is_ok_and(|d| (since..=until).contains(&d.with_timezone(&chrono::Utc)))
            })
            .map(|(_, p)| p.clone())
            .collect()
    }
    /// A pull request of a repository by its ID, among those of every branch, when it was
    /// opened since `since` (a test's pull requests without a date are always recent).
    pub(in crate::app) fn pull_request(
        &self,
        repository: &str,
        number: u64,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Option<PullRequest> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET pullrequest {repository} {number}"));
        self.pulls
            .lock()
            .unwrap()
            .iter()
            .filter(|((r, _), _)| r.eq_ignore_ascii_case(repository))
            .flat_map(|((_, branch), pulls)| {
                pulls.iter().map(move |p| PullRequest {
                    source: Some(branch.clone()),
                    ..p.clone()
                })
            })
            .find(|p| p.id == number)
            .filter(|p| {
                p.created_at.as_deref().is_none_or(|at| {
                    chrono::DateTime::parse_from_rfc3339(at)
                        .is_ok_and(|d| d.with_timezone(&chrono::Utc) >= since)
                })
            })
    }
    /// A change of a Wiki page by `user` (`me` is the source's user), at `at`.
    #[cfg(unix)]
    pub(in crate::app) fn wiki_change(&self, user: &str, path: &str, at: i64) {
        let at = chrono::DateTime::from_timestamp(at, 0)
            .unwrap()
            .to_rfc3339();
        self.wiki.lock().unwrap().push((
            user.into(),
            crate::ado::workflow::WikiPage {
                wiki: "Payments.wiki".into(),
                path: path.into(),
                changed_at: at,
                url: format!(
                    "https://dev.azure.com/example/Payments/_wiki/wikis/Payments.wiki?pagePath={}",
                    path.replace(' ', "+")
                ),
            },
        ));
    }
    /// The Wiki pages `me` changed in a window.
    pub(in crate::app) fn wiki_pages(
        &self,
        (since, until): (chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>),
    ) -> Vec<crate::ado::workflow::WikiPage> {
        self.requests
            .lock()
            .unwrap()
            .push("GET wiki commits".into());
        self.wiki
            .lock()
            .unwrap()
            .iter()
            .filter(|(user, p)| {
                user == "me"
                    && chrono::DateTime::parse_from_rfc3339(&p.changed_at)
                        .is_ok_and(|d| (since..=until).contains(&d.with_timezone(&chrono::Utc)))
            })
            .map(|(_, p)| p.clone())
            .collect()
    }
    /// Change the state an item is in.
    #[cfg(unix)]
    pub(in crate::app) fn set_state(&self, id: u64, state: &str) {
        if let Some(detail) = self.items.lock().unwrap().get_mut(&id) {
            detail.item.state = state.into();
        }
    }
    /// The items assigned to the user in a state of a project, newest ID first, at most
    /// `limit` (with `has_more` when there are others): the To-Do read the watcher makes.
    /// Without a kind it holds the HUs, the bugs and the task types listed with them; the
    /// `task` kind holds every other task-category type, as the To-Do list groups them.
    pub(in crate::app) fn ready_items(
        &self,
        source: Option<&str>,
        project: &str,
        state: &str,
        kind: Option<&str>,
        limit: usize,
    ) -> Result<crate::tools::todo::List, String> {
        let asked = kind.map(|k| format!(" kind {k}")).unwrap_or_default();
        self.requests.lock().unwrap().push(format!(
            "GET list {project} {state}{asked} limit {limit} source {}",
            source.unwrap_or("-")
        ));
        if self.failing_state.lock().unwrap().as_deref() == Some(state) {
            return Err("[network] Azure work items could not be read.".into());
        }
        if self.failing_lists.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("[network] Azure work items could not be read.".into());
        }
        let listed = self.listed_with_bugs.lock().unwrap().clone();
        let group = |item: &Item| match item.category.as_str() {
            "requirement" => "hu",
            "task" if !listed.contains(&item.kind) => "task",
            _ => "bug",
        };
        let assigned = self.assigned.lock().unwrap().clone();
        let mut items: Vec<Item> = self
            .items
            .lock()
            .unwrap()
            .values()
            .filter(|d| {
                assigned.contains(&d.item.id)
                    && kind.map_or(group(&d.item) != "task", |k| group(&d.item) == k)
                    && d.item.state == state
                    && d.item.project.eq_ignore_ascii_case(project)
            })
            .map(|d| d.item.clone())
            .collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.id));
        let limit = self
            .list_cap
            .lock()
            .unwrap()
            .map_or(limit, |c| c.min(limit));
        let has_more = items.len() > limit;
        items.truncate(limit);
        Ok(crate::tools::todo::List {
            items,
            has_more,
            ..Default::default()
        })
    }
    /// A created work item: recorded as a request, with the next ID and its link.
    pub(in crate::app) fn create_child(&self, child: &serde_json::Value) -> serde_json::Value {
        let mut requests = self.requests.lock().unwrap();
        requests.push(format!("POST workitems {}", child["title"]));
        let id = 9000
            + requests
                .iter()
                .filter(|r| r.starts_with("POST workitems"))
                .count() as u64;
        serde_json::json!({"id": id, "url": format!("{}/Payments/_workitems/edit/{id}", ORGANIZATION), "state": "created"})
    }
    /// What the source's user wrote since `since`: every item it created, and the state changes
    /// of `items`.
    pub(in crate::app) fn written_since(
        &self,
        items: &[u64],
        since: i64,
    ) -> Vec<crate::ado::workflow::Written> {
        self.requests
            .lock()
            .unwrap()
            .push(format!("GET written since {since}"));
        self.written
            .lock()
            .unwrap()
            .iter()
            .filter(|w| w.at >= since && (w.what == "created" || items.contains(&w.id)))
            .cloned()
            .collect()
    }
    /// The states every work item type allows here.
    pub(in crate::app) fn type_states(&self, _kind: &str) -> Vec<String> {
        ["To Do", "In Progress", "En revisión", "Done"]
            .map(String::from)
            .into()
    }
    /// A lead's write of an item's fields: recorded as a request, its state applied.
    pub(in crate::app) fn update_item(&self, id: u64, fields: &[(String, serde_json::Value)]) {
        self.requests.lock().unwrap().push(format!(
            "PATCH workitems/{id} {}",
            serde_json::json!(fields)
        ));
        if let Some((_, state)) = fields.iter().find(|(r, _)| r == "System.State")
            && let Some(state) = state.as_str()
        {
            self.set_state(id, state);
        }
    }
    /// A Wiki page (`page:PATH`) or a repository file (`REPOSITORY:PATH`) a lead may read.
    #[cfg(unix)]
    pub(in crate::app) fn set_text(&self, key: &str, text: &str) {
        self.texts.lock().unwrap().insert(key.into(), text.into());
    }
    pub(in crate::app) fn lead_read(
        &self,
        read: crate::app::tools::todo::LeadRead<'_>,
    ) -> Result<serde_json::Value, String> {
        use crate::app::tools::todo::LeadRead;
        let texts = self.texts.lock().unwrap();
        let text = |key: String, path: &str| {
            texts
                .get(&key)
                .map(|t| serde_json::json!({"path": path, "content": t, "truncated": false}))
                .ok_or_else(|| format!("[network] {path} could not be read."))
        };
        match read {
            LeadRead::Page(page) => text(format!("page:{page}"), page),
            LeadRead::File(repository, path, _) => text(format!("{repository}:{path}"), path),
            LeadRead::Folder(repository, path, _) => {
                let prefix = format!("{repository}:{}/", path.trim_end_matches('/'));
                let entries: Vec<_> = texts
                    .keys()
                    .filter_map(|k| k.strip_prefix(&prefix))
                    .map(|rest| serde_json::json!({"path": format!("{}/{rest}", path.trim_end_matches('/')), "folder": false}))
                    .collect();
                Ok(serde_json::json!({"path": path, "entries": entries, "truncated": false}))
            }
        }
    }
    /// Every request was a read.
    #[cfg(unix)]
    pub(in crate::app) fn only_reads(&self) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .all(|r| r.starts_with("GET "))
    }
}
