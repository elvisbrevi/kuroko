# Renaming the product to Kuroko

Status: **phase 0, inventory**, waiting for the owner's approval. No code has changed yet.

The visible brand has been Kuroko since 0.16.0 (#321). This plan renames the crate, the binaries,
the CLI, MCP, the skill, the environment variables, the service entries, the URLs and the text, so
a new user only sees `kuroko`, while existing installations (the VPS, macOS and Windows) move to
Kuroko through the updater without signing in to Microsoft again and without losing credentials,
the profile or account data.

Counts below come from `git grep` on `main` at c1a8518 (0.16.0).

| Pattern | Lines | Files |
| --- | ---: | ---: |
| `personal-teams-assistant` | 335 | 75 |
| `pta` (word) | 1262 | 142 (src 381, GUI 87, docs and skill 516) |
| `PTA_` | 185 | 56 (33 distinct variables) |
| `Teams Assistant` | 62 | 25 |
| `dev.personalteams` | 23 | 15 |
| `pta-cloud` | 19 | 8 |
| `teams-assistant.elvisbrevi.cl` | 8 | 4 |

## 1. Findings that change the plan

These come from the code and the machines, and each one changes an order or a decision of the
original plan.

1. **Older updaters reject a package with extra binaries.** `maintenance::verify` (0.15.0 and
   0.16.0) requires the staged Cargo record to list exactly `{personal-teams-assistant, pta}`, and
   `archive()` rejects an archive record with any other set. If the old crate's 0.17.0 adds
   `kuroko` and `kuroko-host`, the cargo and binstall installations never install it ("not the
   matched application and pta pair"). **Change:** the 0.17.0 bridge keeps exactly two binaries. It
   already understands being invoked as `kuroko`/`kuroko-host` and accepts the four-binary set of
   the successor. The four binaries first ship in the `kuroko` crate, 0.18.0. (The archive path
   would tolerate extra files, because `unpack` skips them, but the Cargo paths would not.)
2. **Renaming the repository breaks release downloads of builds before the bridge.** Downloads
   follow redirects only to `*.githubusercontent.com` (`release::REDIRECT_HOSTS`, mirrored by the
   install scripts). After a rename, GitHub answers the old
   `github.com/elvisbrevi/personal-teams-assistant/releases/download/…` with a `github.com` 301,
   which these builds refuse (`Failure::Redirected`), so archive installations (`install.sh`,
   `install.ps1`) stop updating. Also, 0.18.0's `repository`, binstall's `pkg-url` (`{ repo }`)
   and the successor's archive names need the new repository to exist. **Change:** the repository
   rename moves from phase 4 to **between phases 1 and 2**, after every archive installation runs
   0.17.0. 0.17.0 also accepts a `github.com` redirect when the owner, the release path and the file
   name stay the same and only the repository name changes.
3. **crates.io Trusted Publishing.** You can configure Trusted Publishing only for a crate that
   already exists, so `kuroko` 0.18.0 must first be published by hand with an API token (only the
   owner holds one), and Trusted Publishing for `kuroko` with `elvisbrevi/kuroko` and `release.yml`
   is configured after that. The old crate's Trusted Publishing names `elvisbrevi/personal-teams-assistant`; after the
   rename, `release.yml` can no longer publish `personal-teams-assistant` unless the owner updates
   it. **Change:** 0.17.0 is the old crate's final version. Its README and description already say
   that the product continues as `kuroko` from 0.18.0, so phase 2 needs no simultaneous 0.17.x
   publication. If the bridge needs a fix after the rename, a 0.17.x would be published by hand from
   a maintenance branch (the fallback in `docs/delivery.md`). No yank.
4. **The VPS does not use the in-app updater.** It runs a hand-installed copy of
   `scripts/vps-update.py` (`~/.local/bin/personal-teams-assistant-update`, identical to `main`
   today) from `personal-teams-assistant-update.timer`. That copy only reads the
   `personal-teams-assistant` index entry and runs `cargo install` directly into `~/.cargo`. There,
   `cargo install kuroko` fails because Cargo records `pta` and `personal-teams-assistant` as owned
   by another package, unless it uses `--force` or first uninstalls the old package. **Change:** the
   script learns the successor (with `--force`, a backup and restore of both packages' records and
   its existing rollback) in phase 1. The owner must confirm copying it to `~/.local/bin` before
   0.18.0 is published. Without that step, the VPS stays on 0.17.0 and keeps working.
5. **MCP tool names come from the client's configuration key, not from `serverInfo.name`.** The
   `mcp__personal-teams-assistant__*` names exist because `~/.claude.json` registers the server
   under the key `personal-teams-assistant`. Changing `serverInfo.name` renames nothing; only
   `kuroko mcp migrate` (or `claude mcp remove`/`add`) does. Permission lists that name the old
   prefix also need rewriting. On the VPS, `~/.claude/settings.json` has none today.
6. **A manual `cargo install kuroko` hits the same binary-ownership conflict** on any computer with
   the old crate installed. The README, the skill and the CLI's update hint must say
   `cargo uninstall personal-teams-assistant` first, or `cargo install kuroko --locked --force`.
   binstall must be tested for the same case.
7. **Nothing recognizes the old session prefix today.** `Teams Assistant - …` is only written
   (`llm/implementation/name.rs`); no code filters existing sessions by it. Renaming to `Kuroko - …`
   affects only new sessions. If phase 3 adds a filter, it must accept both prefixes.
8. **Some `PTA_` names cross versions or are credential names, so they stay.** `PTA_UPDATE_HANDOVER`
   and `PTA_RESTORE_HANDOVER` are set by an old helper or host and read by the new one, in the middle
   of an update or a restore. `PTA_MACHINE_TOKEN` is a credential's name in the system store. All
   three are invariants, not aliases.
9. **The cloud Worker reads the host's version from its User-Agent**
   (`personal-teams-assistant/X.Y.Z`, `cloud/src/index.ts:153`, sent by `src/app/cloud.rs:345`).
   This is part of the machine–coordinator protocol, so it stays. If a later change sends
   `kuroko/X.Y.Z`, the Worker must accept both first, in a separate deployment.
10. **Texts stored in profiles are not rewritten.** Implementation-agent prompts the user edited,
    workflow blocks and notification templates may name `pta` commands. The aliases keep them
    working. Phase 3 lists them (`agent_prompts_list`, `workflow_blocks_list`) and proposes edits;
    it does not rewrite them silently.
11. **The machines' state.** All three are trusted and online. The VPS (`VPS Ubuntu`, the Teams
    machine) and `EMERGYA` (Windows) run 0.16.0. `macos` runs **0.15.0 and has had 0.16.0 staged
    without applying it**. Before the repository rename, two checks are needed: why the macOS update
    has not been applied, and whether macOS and EMERGYA are archive installations or cargo/binstall
    installations (`update_status` on each machine).
12. **The deprecation notice of the alias binaries** goes only to stderr, once per process, and
    never to stdout (`pta mcp` speaks JSON-RPC on stdout, and `--json` output is parsed). It starts
    with the `kuroko` crate (0.18.0). In 0.17.0, `pta` is still the primary name and prints no
    notice.

## 2. Revised order

| Step | What | Who |
| --- | --- | --- |
| 1 | `personal-teams-assistant` 0.17.0, the bridge: two binaries; `KUROKO_*` with a `PTA_*` fallback; invocation-name handling; an updater that knows the successor (crate, archive names, four binaries, new repository, same-owner `github.com` redirect, removal of the old Cargo, binstall and archive records); migration of service entries; `Kuroko - …` sessions; `localStorage` migration; a successor-aware `vps-update.py`; README that says "continues as `kuroko`" | agent, PR |
| 1b | Copy the new `vps-update.py` to `~/.local/bin/personal-teams-assistant-update` | owner confirms, agent runs |
| 1c | Wait until all three machines report 0.17.0 (fix the macOS one first) | — |
| 2 | Rename the repository to `elvisbrevi/kuroko`; update crates.io Trusted Publishing for the old crate (only needed if a 0.17.x fix is ever published); check the Cloudflare Workers Builds connection | **owner** |
| 3 | `kuroko` 0.18.0: package name, four binaries, `repository`, binstall `pkg-url`, `release.yml`, `binaries.yml`, install scripts; first publication by hand with the owner's token, then Trusted Publishing for `kuroko`; verify each machine moves 0.17.0 → 0.18.0 | agent + **owner** (token, Trusted Publishing) |
| 3b | `kuroko` 0.18.1 (trivial), published by `release.yml`: proves the updaters follow the new crate | agent |
| 4 | CLI, MCP and skill (`kuroko mcp migrate`, skill directory, messages, catalog) | agent, PR |
| 5 | Landing at `kuroko.elvisbrevi.cl` with a 301 from the old host; optional `kuroko-cloud` route and Access app; optional portal hostname | **owner** confirms each Cloudflare change |
| 6 | Documentation, known limits, alias retirement date | agent, PR |

## 3. Inventory in the repository

Classes: **rename**, **alias + migration** (write the new name, keep reading or recognizing the
old one), **stays** (invariant or protocol, with the reason).

### Package, binaries and updater

| Use | Where | Class | Reason / phase |
| --- | --- | --- | --- |
| Package `name`, `default-run`, `description`, `repository` | `Cargo.toml`, `Cargo.lock` | rename | Phase 3 (step 3); `repository` only after the repository rename |
| `[[bin]]` `personal-teams-assistant`, `pta` | `Cargo.toml` | alias + migration | `kuroko-host` and `kuroko` become primary in 0.18.0; the old names stay as aliases, because existing units, launchd entries, scheduled tasks and MCP configurations point at their paths |
| binstall `pkg-url`/`bin-dir` (`{ name }`) | `Cargo.toml` | rename | Follow the package name; archives become `kuroko-<target>-v<version>` |
| Linker stack size for the host binary | `build.rs:8` | rename | Applied to `kuroko-host` and to the alias (Windows) |
| `CRATE`, `CLI` | `src/maintenance.rs:23-24` | alias + migration | The bridge knows both crates; 0.18.0 makes `kuroko` its own crate and still recognizes old Cargo, binstall and archive records (#231) |
| `HOST_BINARY`, `LEGACY_HOST_BINARY` | `src/app/control.rs:236-237` | alias + migration | A running host started from any of `kuroko-host`, `personal-teams-assistant` or `personal-teams-desktop` counts as outdated after an install |
| `BINARIES`, `RECORDS` | `src/maintenance/apply.rs:28-30` | alias + migration | Apply replaces four binaries and the records of both packages |
| `directory_name`, `archive_name`, `unpack` | `src/maintenance/release.rs` | alias + migration | Successor archive names and binaries; the redirect policy accepts a same-owner `github.com` rename redirect |
| `REPOSITORY` (`CARGO_PKG_REPOSITORY`) | `src/maintenance/release.rs:21` | rename | Points at `elvisbrevi/kuroko` from 0.18.0 |
| Update User-Agents `{CRATE}/{version} (update …)` | `release.rs`, `maintenance.rs` | rename | Informational only |
| Recovery hints `cargo install personal-teams-assistant …` | `src/maintenance/apply.rs:998`, `src/app/cli.rs:596`, `catalog.js:2376-2377` | rename | Say `cargo install kuroko --locked --force` |
| `release.yml` `CRATE`, release title, User-Agents | `.github/workflows/release.yml` | rename | Step 3 |
| `binaries.yml` archive names and copied binaries | `.github/workflows/binaries.yml` | rename | Step 3: four binaries per archive |
| `ci.yml` package check and crates.io version check | `.github/workflows/ci.yml:75,90-91` | rename | Step 3 |
| `ci.yml` temp directories `pta-…` | `.github/workflows/ci.yml` | stays | Internal names in the runner, never seen by a user |
| Install scripts: `REPOSITORY`, `PACKAGE`, `CLI`, rc block markers, default roots | `scripts/install/install.sh`, `install.ps1` | alias + migration | New installs default to a `kuroko` root; an existing root (`~/.local/share/personal-teams-assistant`, `%LOCALAPPDATA%\Programs\personal-teams-assistant`) is updated in place; the rc block recognizes the old markers and rewrites them |
| Installer tests | `scripts/install/test-install.*`, `tests/archive_update.rs` | rename | They follow the scripts; they add a bridge → successor case |
| VPS updater `CRATE`, `BINARIES`, `SERVICE`, `STATE`, `TARGET`, User-Agent | `scripts/vps-update.py`, `scripts/test-vps-update.py`, `scripts/systemd/*` | alias + migration | Finding 4; the state and cache directories may keep their paths |

### CLI, MCP, skill and host text

| Use | Where | Class | Reason / phase |
| --- | --- | --- | --- |
| `pta` in help, errors, `next`, hints (~381 lines in `src`) | `src/app/cli.rs`, `readiness.rs`, `tools/mod.rs`, … | rename | Phase 4: the text follows the invoked name or always says `kuroko` |
| `pta --version` output `pta X.Y.Z (contract N)` | `src/app/cli.rs` | alias + migration | `scripts/vps-update.py` parses it; the format stays the same with the new name, and the script accepts both |
| Host `--version` `personal-teams-assistant X.Y.Z` | `src/app.rs:1895` | alias + migration | `verify_pair` in older updaters compares it with `{CRATE} {version}`; each crate's binaries report their own package |
| `serverInfo.name` | `src/app/mcp.rs:77` | rename | Phase 4 (finding 5) |
| `INSTRUCTIONS` "Kuroko (Personal Teams Assistant) …" | `src/app/mcp.rs:31` | rename | Phase 4 |
| `mcp config` snippets (key `personal-teams-assistant`) | `src/app/mcp.rs:338-346`, `desktop/ui/settings-account.js:183`, `scripts/ui-preview/check.mjs:370` | rename | Phase 4, plus `kuroko mcp migrate` |
| Codex `clientInfo.name` | `src/llm/implementation/codex.rs:56` | rename | Informational |
| Skill directory and `name:` | `desktop/skills/personal-teams-assistant/`, `src/app/skill.rs` | rename | Phase 4: `desktop/skills/kuroko/`; the profile copy `skills/personal-teams-assistant-<version>` becomes `skills/kuroko-<version>`, and old copies are removed |
| `skill show` JSON `name` | `src/app/skill.rs:128` | rename | Phase 4 |
| Host doc comments and messages naming `personal-teams-assistant --host/--web` | `src/app.rs:1878`, `src/app/cli.rs:50`, `src/app/web/store.rs:379`, `src/app/service.rs:696`, `catalog.js:1425` | rename | Phase 4: `kuroko-host` |
| Work item link comments "Linked/Registered by Personal Teams Assistant" | `src/ado/link.rs:272`, `src/ado/registration.rs:623` | rename | Only new links; existing comments in Azure are not touched |
| GitHub and delivery User-Agents | `src/app/github.rs:205`, `src/deliveries.rs:428` | rename | Informational |
| Agent prompts and templates that name commands | `src/llm/**`, `src/app/workflows/**` | rename | Phase 4 (finding 10 for the copies stored in profiles) |

### Environment variables

| Variable | Class | Reason |
| --- | --- | --- |
| `PTA_PROFILE_DIR` | alias + migration | Read in `control.rs:152`, set for children (`web/profiles.rs:114`, `app.rs:1892`, `agent.rs:1013`), written by older launchers and units; `KUROKO_PROFILE_DIR` first, `PTA_PROFILE_DIR` as fallback; children get both until the aliases retire |
| `PTA_HEADLESS` | alias + migration | Already accepted and ignored; `KUROKO_HEADLESS` is accepted the same way |
| `PTA_DEV_UI_DIR`, `PTA_DEV_CREDENTIALS_DIR` | alias + migration | Development only (`scripts/dev-host.sh`) |
| `PTA_UPDATE_INDEX` and the other `PTA_UPDATE_*` of `vps-update.py` | alias + migration | Configured in the VPS units' environment, if at all |
| `PTA_INSTALL_ROOT`, `PTA_INSTALL_TEST*` | alias + migration | Install scripts and CI |
| `PTA_ACCESS_GITHUB_CLIENT_ID/SECRET[_FILE]` | alias + migration | Read by `configure-web-access.py`; may live in a PTA variable |
| `PTA_UPDATE_HANDOVER`, `PTA_RESTORE_HANDOVER` | **stays** | Finding 8: cross-version protocol |
| `PTA_MACHINE_TOKEN` | **stays** | Finding 8: credential name in the system store |
| `PERSONAL_TEAMS_GIT_ASKPASS`, `PERSONAL_TEAMS_GIT_TOKEN` | **stays** | Internal between the host and its own askpass child; never set by a person |
| Test-only `PTA_*` (`PTA_FAKE_*`, `*_TEST_SECRET`, `PTA_SERVICE_LIVE_TEST`, `PTA_REMOTE_SMOKE_PROVIDER`, `PTA_UI_CHROMIUM`) | stays | Internal to tests and CI; may be renamed at will, but there is no reason to |

### Service entries

| Use | Where | Class | Reason |
| --- | --- | --- | --- |
| systemd `personal-teams-assistant-<id>.service` | `src/app/service.rs:146` | alias + migration | Becomes `kuroko-<id>.service`, pointing at `kuroko-host`; `service install`/repair starts the new one, checks its health, then stops and removes the old one; both are recognized |
| launchd `dev.personalteams.assistant.host.<id>` | `src/app/service.rs:145` | alias + migration | New label to decide (§5); same procedure |
| Task Scheduler `PersonalTeamsAssistant-<id>` | `src/app/service.rs:147` | alias + migration | `Kuroko-<id>`; same procedure |
| `MARKER` "Personal Teams Assistant host, created by pta service install" | `src/app/service.rs:25` | **stays** | Proves that an entry was written by this module; changing it would orphan existing entries |
| Unit `Description=` | `src/app/service.rs:307` | rename | Text only |

### GUI

| Use | Where | Class | Reason |
| --- | --- | --- | --- |
| `localStorage` `pta-theme`, `pta-palette`, `pta-style`, `pta-language`, `pta-sign-in-skipped` | `appearance.js`, `i18n.js`, `settings-account.js` | alias + migration | Read `kuroko-*`, fall back to `pta-*` and copy it over |
| `sessionStorage` `pta-local-key` | `transport.js` | stays | It lasts one tab, so it is internal |
| `<meta name="pta-transport">`, `window.__pta*` preview hooks | `transport.js`, `page.rs`, `scripts/ui-preview` | stays | Internal between a page and the host of the same build |
| Catalog entries that name commands | `desktop/ui/catalog.js` (56 lines) | rename | Phase 4, with each English key |
| Preview mock names | `scripts/ui-preview/mock.js` | rename | Synthetic data |

### Messages, sessions and cards

| Use | Where | Class | Reason |
| --- | --- | --- | --- |
| Card band id `pta-band` | `src/adapters/card.rs:29` | **stays** | `message_text` uses it to strip the band from messages already sent |
| `MessageStyle::KUROKO_URL` | `src/config.rs:341` | rename | `https://kuroko.elvisbrevi.cl/` once that host serves the landing; old cards keep working through the 301 |
| Session prefix `Teams Assistant` | `src/llm/implementation/name.rs:7` | rename | `Kuroko - …` (finding 7) |

### Cryptographic labels and domain separation (all stay)

`personal-teams-assistant/…/v1` in `src/mirror/handoff.rs`, `src/mirror/seal.rs`,
`src/variables.rs`, `src/portable.rs`, `src/app/migration.rs`, `src/app/portable.rs`,
`src/app/standing.rs`, `src/app/variables/sync.rs` and `src/app/web/agents.rs`;
`pta-cloud-key-fingerprint:` (`src/app/cloud/mirror.rs:886`); `pta-cloud-machine-1`
(`src/mirror/handoff.rs:32`) and `pta-cloud-content-1` (`src/mirror/key.rs:15`). They go into
hashes, signatures, envelopes or stored records, so changing them makes existing data unreadable.

### Profile, credentials and Microsoft (all stay)

`dev.personalteams.assistant` (`src/app/control.rs:149`; the profile and data directories), the
Keychain service `personal-teams-assistant.<profile>` (`src/security/mod.rs:153,246,290`), the
credential names, `data_dir`, `STATE_ENCRYPTION_KEY`, the `Config`/`KnowledgeMap` fields, the Graph
scopes, the OAuth flow and the Entra app.

### Cloud Worker and landing

| Use | Where | Class | Reason |
| --- | --- | --- | --- |
| Worker `pta-cloud`, `pta-cloud-staging`, DO classes and bindings, R2 `pta-cloud-backups*` | `cloud/wrangler.jsonc` | **stays** | Renaming a Worker creates an empty namespace |
| Route `pta-cloud.elvisbrevi.cl` | `cloud/wrangler.jsonc:16` | stays (+ optional extra route) | Enrolled hosts use it; `kuroko-cloud.elvisbrevi.cl` may be added beside it (step 5) |
| User-Agent parsing `personal-teams-assistant/X.Y.Z` | `cloud/src/index.ts:153`, `src/app/cloud.rs:345` | **stays** | Finding 9 |
| Health `service: "personal-teams-assistant-cloud"`, `cloud/package.json` name | `cloud/src/index.ts:285`, `cloud/package.json` | stays | Internal; tests and monitoring compare it |
| Access app prefix `pta-cloud`, token names | `scripts/configure-cloud-access.py:50` | **stays** | The script finds existing apps and tokens by these names |
| Landing Worker `personal-teams-assistant` | `site/wrangler.jsonc:5` | stays | Workers Builds is bound to this name; the landing has no state, but nothing is gained by a new Worker |
| Landing route `teams-assistant.elvisbrevi.cl` | `site/wrangler.jsonc:13` | alias + migration | `kuroko.elvisbrevi.cl` serves it; the old host answers with a permanent 301 that keeps the path |
| Landing text, canonical, `og:*`, install commands | `site/public/index.html` (20 + 15 lines) | rename | Phase 6 (and the domain in step 5) |

### Web portal and Access scripts

| Use | Where | Class | Reason |
| --- | --- | --- | --- |
| Access app `personal-teams-assistant-web`, IdP `personal-teams-assistant-login` | `scripts/configure-web-access.py:33-34`, `configure-web-cloudflare.py:79` | **stays** | The scripts find the existing app and identity provider by name; a rename creates duplicates |
| Portal host `assistant.elvisbrevi.cl` | owner's configuration | stays (+ optional new host) | A new host means re-adding the iPhone PWA |

### Documentation and repository files

| Use | Where | Class |
| --- | --- | --- |
| Name, commands, URLs | `README.md` (29 + 42), `docs/architecture.md` (34 + 151), `docs/delivery.md`, `docs/private-deployment.md`, `docs/organization-and-cloud-storage.md`, `CONTEXT.md` (title), `SECURITY.md`, `AGENTS.md`, `.agents/skills/save-knowledge/SKILL.md`, the skill and its `references/` | rename (phase 6; the skill in phase 4) |
| `LICENSE` "Personal Teams Assistant contributors" | `LICENSE:3` | rename (owner's choice) |
| Rehearsal and drill scripts | `scripts/rehearse-cloud-migration.py` (65 `pta`), `scripts/test-windows-startup.py`, `scripts/dev-host.sh` | rename (they call the CLI; the alias covers them meanwhile) |

## 4. Outside the repository

| Item | Where | Proposal |
| --- | --- | --- |
| VPS updater `~/.local/bin/personal-teams-assistant-update` (same as `scripts/vps-update.py`) | VPS | Step 1b: copy the successor-aware version (needs confirmation). Renaming the file, timer and unit to `kuroko-update*` is optional; if you want it, do it with the web units in one window |
| `personal-teams-assistant-update.{service,timer}` | VPS `~/.config/systemd/user/` | As above |
| `personal-teams-assistant-web.service` (`ExecStart=~/.cargo/bin/personal-teams-assistant --web`) and `-web-connector.service` (+ `.d/oom.conf`) | VPS | The alias keeps them working. Recommendation: after 0.18.0, in one confirmed window, install `kuroko-web*.service` pointing at `kuroko-host --web` with the same drop-ins, start them, check health, disable the old ones and keep their files for rollback (`systemctl --user enable --now personal-teams-assistant-web*` restores them). `vps-update.py` reads `PTA_UPDATE_SERVICE`, so it learns the new name |
| `claude-remote-personal-teams-assistant.service` and the checkout `~/src/personal-teams-assistant` | VPS | **Leave them.** Renaming the directory breaks this bridge, its worktrees and the implementation roots; the remote URL keeps working through GitHub's redirect (`git remote set-url` is optional) |
| Claude Code MCP `personal-teams-assistant` → `~/.cargo/bin/pta mcp` | VPS `~/.claude.json` (user scope) | Phase 4: `claude mcp remove personal-teams-assistant -s user && claude mcp add -s user kuroko -- ~/.cargo/bin/kuroko mcp` (with confirmation); restart the Claude sessions so that `mcp__kuroko__*` appears |
| MCP permission lists `mcp__personal-teams-assistant__*` | VPS `~/.claude/settings.json`: none; macOS and EMERGYA: unknown | `kuroko mcp migrate` rewrites them with confirmation, or prints the commands |
| Codex or other agents' MCP entries | each machine (`~/.codex/config.toml`: none on the VPS) | Same |
| Installed skills | VPS `~/.claude/skills`: none; profile copy `skills/personal-teams-assistant-<version>`; other machines unknown | Phase 4 |
| Service entries `personal-teams-assistant-<id>` / launchd / `PersonalTeamsAssistant-<id>` | macOS, EMERGYA (and any `service install` on the VPS) | Migrated by `service install`/repair in 0.17.0 |
| Install roots and rc blocks from `install.sh`/`install.ps1` | macOS, EMERGYA if they are archive installs | Updated in place (finding 11 says to check the method) |
| crates.io: `kuroko` free (index 404), Trusted Publishing for `personal-teams-assistant` | crates.io | Step 3: first publication with the owner's token; then Trusted Publishing for `kuroko` |
| GitHub repository `elvisbrevi/kuroko` (free today, 404) | GitHub | Step 2, by the owner |
| Cloudflare Workers Builds of `site/` and `cloud/` | Cloudflare dashboard | Check that the repository connection survives the rename (step 2) |
| Access apps (web portal, `pta-cloud*`) | Cloudflare | Stay; new ones only for new hosts (step 5) |
| Profile-stored prompts, workflow blocks, templates | each account | Finding 10 |
| Agent memories that name `pta` and `mcp__personal-teams-assistant__*` | the bridge's Claude memory | Updated after phase 4 |

## 5. Decisions for the owner

1. **Revised order** (§2): the repository rename moves before the `kuroko` crate, and 0.17.0 is
   the old crate's final version instead of a later 0.17.x.
2. **The 0.17.0 bridge keeps two binaries** (finding 1); `kuroko` and `kuroko-host` appear in 0.18.0.
3. **The first publication of `kuroko` by hand** with your crates.io token (a PTA variable delivered
   by reference, or you run `cargo publish` yourself), then Trusted Publishing for `kuroko`.
4. **The new launchd label**: `cl.elvisbrevi.kuroko.host.<id>` (reverse DNS of the domain you own)
   or `kuroko.host.<id>`.
5. **The VPS units**: rename `personal-teams-assistant-web*` and the updater to `kuroko-*` after
   0.18.0 with the rollback above, or keep the names (they are internal, and the alias covers them).
6. **When the aliases retire**: the proposal is three minor versions after 0.18.0 (0.21.0), and
   only once every machine reports migrated service entries and MCP configurations.
7. **Optional hosts**: `kuroko-cloud.elvisbrevi.cl` and a new portal host (the iPhone PWA would need
   to be added again).
