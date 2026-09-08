//! Local directory capabilities and recoverable project registration. No paths leave this module.
use super::*;
use farhelm_protocol::projects::{
    DirectoryPage, ProjectDirectory, ProjectInfo, ProjectRoots, valid_directory_name,
};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
};

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS project_roots(id TEXT PRIMARY KEY,path TEXT NOT NULL UNIQUE,name TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS project_directories(id TEXT PRIMARY KEY,root_id TEXT NOT NULL,relative_path TEXT NOT NULL,dev INTEGER NOT NULL,ino INTEGER NOT NULL,expires_at INTEGER NOT NULL);
        CREATE INDEX IF NOT EXISTS project_directory_expiry ON project_directories(expires_at);
        CREATE TABLE IF NOT EXISTS project_creations(command_id TEXT PRIMARY KEY,root_id TEXT NOT NULL,parent_relative TEXT NOT NULL,parent_dev INTEGER NOT NULL,parent_ino INTEGER NOT NULL,name TEXT NOT NULL,staging TEXT NOT NULL,dev INTEGER,ino INTEGER);
        CREATE TABLE IF NOT EXISTS project_sync(project_id TEXT PRIMARY KEY,state TEXT NOT NULL,updated_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS session_lifecycle(session_id TEXT PRIMARY KEY,archived INTEGER NOT NULL DEFAULT 0,operation_id TEXT);
        CREATE TABLE IF NOT EXISTS archive_operations(command_id TEXT PRIMARY KEY,session_ids TEXT NOT NULL,archived INTEGER NOT NULL);")?;
    Ok(())
}

fn random_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "{prefix}{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

pub(crate) fn public_error(error: &anyhow::Error) -> Option<&'static str> {
    [
        "project_root_revoked",
        "project_root_is_container",
        "project_directory_expired",
        "project_directory_changed",
        "project_directory_unavailable",
        "project_directory_permission",
        "project_invalid_name",
        "project_name_conflict",
        "project_creation_unconfirmed",
        "project_candidate_missing",
        "project_history_sync_failed",
        "project_not_approved",
        "codex_session_archived",
        "codex_archive_busy",
        "codex_archive_unverified",
        "codex_archive_changed",
        "codex_archive_unapproved",
        "codex_archive_unsaved",
    ]
    .into_iter()
    .find(|code| error.chain().any(|e| e.to_string().contains(code)))
}

fn owned_directory(file: &File) -> Result<fs::Metadata> {
    let meta = file.metadata().context("project_directory_unavailable")?;
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "project_directory_permission"
    );
    Ok(meta)
}

fn open_absolute(path: &Path) -> Result<File> {
    let canonical = fs::canonicalize(path).context("project_directory_unavailable")?;
    ensure!(
        canonical == path && path != Path::new("/"),
        "project_directory_changed"
    );
    if let Some(home) = std::env::var_os("HOME").and_then(|p| fs::canonicalize(p).ok()) {
        ensure!(path != home, "project_directory_permission");
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .context("project_directory_permission")?;
    owned_directory(&file)?;
    Ok(file)
}

fn component(name: &str) -> Result<CString> {
    ensure!(
        !name.is_empty() && !matches!(name, "." | "..") && !name.contains(['/', '\\']),
        "project_invalid_name"
    );
    CString::new(name).context("project_invalid_name")
}

fn child(parent: &File, name: &str) -> Result<File> {
    let name = component(name)?;
    let raw = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    ensure!(raw >= 0, "project_directory_unavailable");
    let file = unsafe { File::from_raw_fd(raw) };
    owned_directory(&file)?;
    Ok(file)
}

type CreationRecord = (String, String, i64, i64, String, Option<i64>, Option<i64>);

struct Directory {
    root_id: String,
    root_path: PathBuf,
    relative: String,
    file: File,
    name: String,
}

fn open_relative(c: &Connection, root: &str, relative: &str) -> Result<Directory> {
    let (path, name, dev, ino): (String, String, i64, i64) = c
        .query_row(
            "SELECT path,name,dev,ino FROM project_roots WHERE id=?1",
            [root],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?
        .context("project_root_revoked")?;
    let root_path = PathBuf::from(path);
    let mut file = open_absolute(&root_path)?;
    let meta = owned_directory(&file)?;
    ensure!(
        (meta.dev() as i64) == dev && (meta.ino() as i64) == ino,
        "project_directory_changed"
    );
    if !relative.is_empty() {
        for part in relative.split('/') {
            file = child(&file, part)?;
        }
    }
    let name = relative
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&name)
        .to_owned();
    Ok(Directory {
        root_id: root.into(),
        root_path,
        relative: relative.into(),
        file,
        name,
    })
}

fn resolve(c: &Connection, id: &str, now: u64) -> Result<Directory> {
    ensure!(
        farhelm_protocol::projects::valid_directory_id(id),
        "project_directory_expired"
    );
    if id.starts_with("rtd_") {
        return open_relative(c, id, "");
    }
    let (root, relative, dev, ino): (String,String,i64,i64) = c.query_row("SELECT root_id,relative_path,dev,ino FROM project_directories WHERE id=?1 AND expires_at>?2", params![id, as_i64(now)?], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?.context("project_directory_expired")?;
    let directory = open_relative(c, &root, &relative)?;
    let meta = directory.file.metadata()?;
    ensure!(
        (meta.dev() as i64) == dev && (meta.ino() as i64) == ino,
        "project_directory_changed"
    );
    Ok(directory)
}

fn handle(c: &Connection, directory: &Directory, now: u64) -> Result<String> {
    if directory.relative.is_empty() {
        return Ok(directory.root_id.clone());
    }
    let id = random_id("dir_");
    let meta = directory.file.metadata()?;
    c.execute(
        "INSERT INTO project_directories VALUES(?1,?2,?3,?4,?5,?6)",
        params![
            id,
            directory.root_id,
            directory.relative,
            (meta.dev() as i64),
            (meta.ino() as i64),
            as_i64(now.saturating_add(600))?
        ],
    )?;
    Ok(id)
}

fn canonical_directory(directory: &Directory) -> Result<PathBuf> {
    let path = directory.root_path.join(&directory.relative);
    let checked = open_absolute(&path)?;
    let expected = directory.file.metadata()?;
    let actual = checked.metadata()?;
    ensure!(
        expected.dev() == actual.dev() && expected.ino() == actual.ino(),
        "project_directory_changed"
    );
    Ok(path)
}

fn approve_path(c: &Connection, path: &Path, now: u64) -> Result<ProjectCandidate> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("project_invalid_name")?;
    let existing: Option<ProjectCandidate> = c.query_row("SELECT candidate_id,path,display_name,suggested_project_id,session_count,state,updated_at_unix FROM discovered_projects WHERE path=?1",[path.to_string_lossy().as_ref()],project_candidate_from_row).optional()?;
    let candidate = if let Some(mut candidate) = existing {
        candidate.state = "approved".into();
        candidate.updated_at_unix = now;
        candidate
    } else {
        let approved: Option<String> = c
            .query_row(
                "SELECT project_id FROM approved_projects WHERE path=?1",
                [path.to_string_lossy().as_ref()],
                |r| r.get(0),
            )
            .optional()?;
        ProjectCandidate {
            candidate_id: random_id("prj_"),
            path: path.into(),
            display_name: name.into(),
            suggested_project_id: match approved {
                Some(id) => id,
                None => available_project_id(c, &crate::suggested_project_id(name), path)?,
            },
            session_count: 0,
            state: "approved".into(),
            updated_at_unix: now,
        }
    };
    ensure!(candidate.path == path, "project_directory_changed");
    c.execute("INSERT INTO approved_projects(project_id,path,updated_at_unix) VALUES(?1,?2,?3) ON CONFLICT(project_id) DO UPDATE SET updated_at_unix=excluded.updated_at_unix",params![candidate.suggested_project_id,path.to_string_lossy(),as_i64(now)?])?;
    c.execute("INSERT INTO discovered_projects VALUES(?1,?2,?3,?4,?5,'approved',?6) ON CONFLICT(path) DO UPDATE SET state='approved',updated_at_unix=excluded.updated_at_unix",params![candidate.candidate_id,path.to_string_lossy(),candidate.display_name,candidate.suggested_project_id,as_i64(candidate.session_count)?,as_i64(now)?])?;
    c.execute("INSERT INTO project_sync VALUES(?1,'pending',?2) ON CONFLICT(project_id) DO UPDATE SET state='pending',updated_at=excluded.updated_at",params![candidate.suggested_project_id,as_i64(now)?])?;
    Ok(candidate)
}

fn registered(
    c: &Connection,
    command: &str,
    projects: &[ProjectCandidate],
    now: u64,
) -> Result<Value> {
    for p in projects {
        insert_event(
            c,
            &format!("{command}:project:{}", p.candidate_id),
            "project.updated",
            &json!({"candidate_id":p.candidate_id,"display_name":p.display_name,"suggested_project_id":p.suggested_project_id,"session_count":p.session_count,"state":"approved","sync_state":"pending","updated_at_unix":now}),
            now,
        )?;
    }
    let mut result =
        json!({"approved":projects.iter().map(|p|p.candidate_id.as_str()).collect::<Vec<_>>()});
    if let [p] = projects {
        result["candidate_id"] = json!(p.candidate_id);
        result["project_id"] = json!(p.suggested_project_id);
    }
    ensure!(c.execute("UPDATE remote_codex_commands SET state='completed',data_json=?2,detail=NULL,terminal_reported=0,updated_at_unix=?3 WHERE command_id=?1 AND state='running'",params![command,serde_json::to_string(&result)?,as_i64(now)?])?==1,"project_creation_unconfirmed");
    Ok(result)
}

impl ExperimentStore {
    pub fn recover_project_creations(&self, now: u64) -> Result<()> {
        let pending = {
            let c = self.lock()?;
            let mut stmt=c.prepare("SELECT command_id,payload_json FROM remote_codex_commands WHERE action='project.add' AND state='running'")?;
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, payload) in pending {
            if let Err(error) = self.add_project_command(&id, &serde_json::from_str(&payload)?, now)
            {
                self.finish_remote_command(
                    &id,
                    farhelm_protocol::CommandState::Failed,
                    None,
                    Some(public_error(&error).unwrap_or("project_creation_unconfirmed")),
                    now,
                )?;
            }
        }
        Ok(())
    }
    pub fn add_project_root(&self, path: &Path, name: &str) -> Result<String> {
        ensure!(valid_directory_name(name), "project_invalid_name");
        let path = fs::canonicalize(path).context("project_directory_unavailable")?;
        let meta = open_absolute(&path)?.metadata()?;
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        let existing: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT id,dev,ino FROM project_roots WHERE path=?1",
                [path.to_string_lossy().as_ref()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let id = existing
            .filter(|(_, dev, ino)| *dev == (meta.dev() as i64) && *ino == (meta.ino() as i64))
            .map(|(id, _, _)| id)
            .unwrap_or_else(|| random_id("rtd_"));
        tx.execute(
            "DELETE FROM project_roots WHERE path=?1",
            [path.to_string_lossy().as_ref()],
        )?;
        tx.execute(
            "INSERT INTO project_roots VALUES(?1,?2,?3,?4,?5)",
            params![
                id,
                path.to_string_lossy(),
                name,
                (meta.dev() as i64),
                (meta.ino() as i64)
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn project_roots(&self) -> Result<ProjectRoots> {
        let c = self.lock()?;
        let mut stmt = c.prepare("SELECT id,name FROM project_roots ORDER BY name,id")?;
        Ok(ProjectRoots {
            roots: stmt
                .query_map([], |r| {
                    Ok(ProjectDirectory {
                        directory_id: r.get(0)?,
                        name: r.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?,
        })
    }

    pub fn remove_project_root(&self, id: &str) -> Result<()> {
        ensure!(
            self.lock()?
                .execute("DELETE FROM project_roots WHERE id=?1", [id])?
                == 1,
            "project_root_revoked"
        );
        Ok(())
    }

    pub fn browse_projects(
        &self,
        id: &str,
        cursor: Option<&str>,
        now: u64,
    ) -> Result<DirectoryPage> {
        let c = self.lock()?;
        c.execute(
            "DELETE FROM project_directories WHERE expires_at<=?1",
            [as_i64(now)?],
        )?;
        let directory = resolve(&c, id, now)?;
        let after = match cursor {
            Some(cursor) => {
                let previous = resolve(&c, cursor, now)?;
                let parent = previous
                    .relative
                    .rsplit_once('/')
                    .map(|(p, _)| p)
                    .unwrap_or("");
                ensure!(
                    previous.root_id == directory.root_id && parent == directory.relative,
                    "project_directory_expired"
                );
                previous.name
            }
            None => String::new(),
        };
        let mut names = std::collections::BTreeSet::new();
        for entry in fs::read_dir(format!("/proc/self/fd/{}", directory.file.as_raw_fd()))
            .context("project_directory_permission")?
        {
            let entry = entry.context("project_directory_unavailable")?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name <= after
                || name == ".git"
                || name.starts_with(".farhelm-")
                || name.chars().any(char::is_control)
                || !entry.file_type()?.is_dir()
            {
                continue;
            }
            if child(&directory.file, &name).is_err() {
                continue;
            }
            names.insert(name);
            if names.len() > 101 {
                names.pop_last();
            }
        }
        let has_more = names.len() > 100;
        let mut entries = Vec::new();
        for name in names.into_iter().take(100) {
            let relative = if directory.relative.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", directory.relative)
            };
            let target = open_relative(&c, &directory.root_id, &relative)?;
            entries.push(ProjectDirectory {
                directory_id: handle(&c, &target, now)?,
                name,
            });
        }
        let parent_id = if directory.relative.is_empty() {
            None
        } else {
            let parent = directory
                .relative
                .rsplit_once('/')
                .map(|(p, _)| p)
                .unwrap_or("");
            Some(handle(
                &c,
                &open_relative(&c, &directory.root_id, parent)?,
                now,
            )?)
        };
        let next_cursor = has_more.then(|| {
            entries
                .last()
                .expect("full directory page")
                .directory_id
                .clone()
        });
        Ok(DirectoryPage {
            directory: ProjectDirectory {
                directory_id: id.into(),
                name: directory.name,
            },
            parent_id,
            entries,
            next_cursor,
        })
    }

    pub fn approve_project_command(
        &self,
        command: &str,
        ids: &[String],
        now: u64,
    ) -> Result<Value> {
        ensure!(
            !ids.is_empty() && ids.len() <= 100,
            "invalid_project_import"
        );
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        let mut projects = Vec::new();
        for id in ids {
            let path: String = tx
                .query_row(
                    "SELECT path FROM discovered_projects WHERE candidate_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
                .context("project_candidate_missing")?;
            open_absolute(Path::new(&path))?;
            projects.push(approve_path(&tx, Path::new(&path), now)?);
        }
        let result = registered(&tx, command, &projects, now)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn add_project_command(&self, command: &str, payload: &Value, now: u64) -> Result<Value> {
        let c = self.lock()?;
        if let Some(saved)=c.query_row("SELECT data_json FROM remote_codex_commands WHERE command_id=?1 AND state='completed'",[command],|r|r.get::<_,String>(0)).optional()? {return Ok(serde_json::from_str(&saved)?);}
        let directory_id = required_payload_string(payload, "directory_id")?;
        let creating = payload["kind"] == "create";
        if creating {
            let name = required_payload_string(payload, "name")?;
            ensure!(valid_directory_name(name), "project_invalid_name");
            let existing:Option<CreationRecord>=c.query_row("SELECT root_id,parent_relative,parent_dev,parent_ino,staging,dev,ino FROM project_creations WHERE command_id=?1",[command],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
            let (parent, staging, identity) = if let Some((
                root,
                relative,
                dev,
                ino,
                staging,
                target_dev,
                target_ino,
            )) = existing
            {
                let parent = open_relative(&c, &root, &relative)?;
                let meta = parent.file.metadata()?;
                ensure!(
                    (meta.dev() as i64) == dev && (meta.ino() as i64) == ino,
                    "project_directory_changed"
                );
                (parent, staging, target_dev.zip(target_ino))
            } else {
                let parent = resolve(&c, directory_id, now)?;
                let meta = parent.file.metadata()?;
                let staging = random_id(".farhelm-create-");
                c.execute("INSERT INTO project_creations(command_id,root_id,parent_relative,parent_dev,parent_ino,name,staging) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![command,parent.root_id,parent.relative,(meta.dev() as i64),(meta.ino() as i64),name,staging])?;
                (parent, staging, None)
            };
            let identity = if let Some(identity) = identity {
                identity
            } else {
                let creation_tx = crate::migrations::write_transaction(&c)?;
                let checked = open_relative(&creation_tx, &parent.root_id, &parent.relative)?;
                let held = parent.file.metadata()?;
                let current = checked.file.metadata()?;
                ensure!(
                    held.dev() == current.dev() && held.ino() == current.ino(),
                    "project_directory_changed"
                );
                let temp = component(&staging)?;
                let rc = unsafe { libc::mkdirat(parent.file.as_raw_fd(), temp.as_ptr(), 0o700) };
                ensure!(rc == 0, "project_creation_unconfirmed");
                let file = child(&parent.file, &staging)?;
                file.sync_all()?;
                parent.file.sync_all()?;
                let meta = file.metadata()?;
                creation_tx.execute(
                    "UPDATE project_creations SET dev=?2,ino=?3 WHERE command_id=?1",
                    params![command, (meta.dev() as i64), (meta.ino() as i64)],
                )?;
                creation_tx.commit()?;
                ((meta.dev() as i64), (meta.ino() as i64))
            };
            let tx = crate::migrations::write_transaction(&c)?;
            let checked = open_relative(&tx, &parent.root_id, &parent.relative)?;
            let held = parent.file.metadata()?;
            let current = checked.file.metadata()?;
            ensure!(
                held.dev() == current.dev() && held.ino() == current.ino(),
                "project_directory_changed"
            );
            if let Ok(target) = child(&parent.file, name) {
                let meta = target.metadata()?;
                ensure!(
                    ((meta.dev() as i64), (meta.ino() as i64)) == identity,
                    "project_name_conflict"
                );
            } else {
                let file = child(&parent.file, &staging)?;
                let meta = file.metadata()?;
                ensure!(
                    ((meta.dev() as i64), (meta.ino() as i64)) == identity,
                    "project_directory_changed"
                );
                let old = component(&staging)?;
                let new = component(name)?;
                let rc = unsafe {
                    libc::renameat2(
                        parent.file.as_raw_fd(),
                        old.as_ptr(),
                        parent.file.as_raw_fd(),
                        new.as_ptr(),
                        libc::RENAME_NOREPLACE,
                    )
                };
                if rc != 0 {
                    bail!(
                        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                            "project_name_conflict"
                        } else {
                            "project_directory_permission"
                        }
                    );
                }
                parent.file.sync_all()?;
            }
            let parent_path = canonical_directory(&parent)?;
            let path = parent_path.join(name);
            let meta = open_absolute(&path)?.metadata()?;
            ensure!(
                ((meta.dev() as i64), (meta.ino() as i64)) == identity,
                "project_directory_changed"
            );
            let project = approve_path(&tx, &path, now)?;
            let result = registered(&tx, command, &[project], now)?;
            tx.commit()?;
            Ok(result)
        } else {
            ensure!(payload["kind"] == "attach", "invalid_project_request");
            let tx = crate::migrations::write_transaction(&c)?;
            let directory = resolve(&tx, directory_id, now)?;
            ensure!(!directory.relative.is_empty(), "project_root_is_container");
            let path = canonical_directory(&directory)?;
            let project = approve_path(&tx, &path, now)?;
            let result = registered(&tx, command, &[project], now)?;
            tx.commit()?;
            Ok(result)
        }
    }

    pub fn set_project_sync(&self, project: &str, state: &str, now: u64) -> Result<()> {
        ensure!(
            matches!(state, "pending" | "ready" | "failed"),
            "invalid_sync_state"
        );
        let c = self.lock()?;
        let tx = crate::migrations::write_transaction(&c)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT state FROM project_sync WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?;
        if old.as_deref() == Some(state) {
            return Ok(());
        }
        tx.execute("INSERT INTO project_sync VALUES(?1,?2,?3) ON CONFLICT(project_id) DO UPDATE SET state=excluded.state,updated_at=excluded.updated_at",params![project,state,as_i64(now)?])?;
        if let Some(id)=tx.query_row("SELECT candidate_id FROM discovered_projects WHERE suggested_project_id=?1 AND state='approved'",[project],|r|r.get::<_,String>(0)).optional()? {
            insert_event(&tx,&random_id("project-sync-"),"project.sync.updated",&json!({"candidate_id":id,"suggested_project_id":project,"sync_state":state,"updated_at_unix":now}),now)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn project_info(&self, project: &str) -> Result<ProjectInfo> {
        let path: String = self
            .lock()?
            .query_row(
                "SELECT path FROM approved_projects WHERE project_id=?1",
                [project],
                |r| r.get(0),
            )
            .optional()?
            .context("project_not_approved")?;
        open_absolute(Path::new(&path))?;
        let output = std::process::Command::new("git")
            .args(["-C", &path, "rev-parse", "--verify", "HEAD^{commit}"])
            .output()
            .context("project_git_unavailable")?;
        Ok(ProjectInfo {
            can_create_worktree: output.status.success(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn command(store: &ExperimentStore, id: &str, payload: Value) {
        store
            .receive_remote_command(
                &AgentCommand {
                    protocol: FARHELM_PROTOCOL.into(),
                    command_id: id.into(),
                    agent_id: "a".into(),
                    action: CommandAction::ProjectAdd,
                    created_at_unix: 10,
                    expires_at_unix: 1000,
                    payload: Some(payload),
                },
                10,
            )
            .unwrap();
        store
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_codex_commands SET accepted_reported=1 WHERE command_id=?1",
                [id],
            )
            .unwrap();
        assert!(store.claim_remote_command(id, 11).unwrap());
    }
    #[test]
    fn empty_project_registration_is_atomic_and_retries_survive_reopen_and_revocation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let db = temp.path().join("agent.db");
        let store = ExperimentStore::open(&db).unwrap();
        let id = store.add_project_root(&root, "Projects").unwrap();
        let request = json!({"kind":"create","directory_id":id,"name":"empty"});
        command(&store, "create", request.clone());
        let result = store.add_project_command("create", &request, 12).unwrap();
        assert!(root.join("empty").is_dir());
        assert_eq!(fs::read_dir(root.join("empty")).unwrap().count(), 0);
        assert_eq!(
            store.remote_receipt("create").unwrap().data,
            Some(result.clone())
        );
        let events = store.pending_events("a", 50).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            !serde_json::to_string(&events)
                .unwrap()
                .contains(root.to_str().unwrap())
        );
        store.remove_project_root(&id).unwrap();
        drop(store);
        let store = ExperimentStore::open(&db).unwrap();
        assert_eq!(
            store.add_project_command("create", &request, 5000).unwrap(),
            result
        );
        assert_eq!(store.approved_projects().unwrap().len(), 1);
        assert!(store.browse_projects(&id, None, 20).is_err());
    }
    #[test]
    fn directory_tokens_are_scoped_expiring_and_reject_replacements_symlinks_and_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("child")).unwrap();
        std::os::unix::fs::symlink(temp.path(), root.join("escape")).unwrap();
        let store = ExperimentStore::open(&temp.path().join("agent.db")).unwrap();
        let root_id = store.add_project_root(&root, "Projects").unwrap();
        let page = store.browse_projects(&root_id, None, 10).unwrap();
        assert_eq!(page.entries.len(), 1);
        let child_id = &page.entries[0].directory_id;
        assert!(store.browse_projects(child_id, None, 610).is_err());
        let child_id = store.browse_projects(&root_id, None, 20).unwrap().entries[0]
            .directory_id
            .clone();
        fs::rename(root.join("child"), root.join("old")).unwrap();
        fs::create_dir(root.join("child")).unwrap();
        assert!(
            store
                .browse_projects(&child_id, None, 21)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        let replacement = store
            .browse_projects(&root_id, None, 21)
            .unwrap()
            .entries
            .into_iter()
            .find(|e| e.name == "child")
            .unwrap();
        fs::remove_dir(root.join("child")).unwrap();
        assert!(
            store
                .browse_projects(&replacement.directory_id, None, 22)
                .is_err()
        );
        // Root identity and ownership are rechecked, even for previously issued handles.
        store
            .lock()
            .unwrap()
            .execute("UPDATE project_roots SET ino=ino+1 WHERE id=?1", [&root_id])
            .unwrap();
        assert!(
            store
                .browse_projects(&root_id, None, 22)
                .unwrap_err()
                .to_string()
                .contains("changed")
        );
        store
            .lock()
            .unwrap()
            .execute("UPDATE project_roots SET ino=ino-1 WHERE id=?1", [&root_id])
            .unwrap();
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let permissions = fs::metadata(&root).unwrap().permissions();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
            let denied = store.browse_projects(&root_id, None, 22);
            fs::set_permissions(&root, permissions).unwrap();
            assert!(denied.is_err());
        }
        for name in ["../escape", "/escape", "a/b", ".", "..", "a\\b"] {
            let p = json!({"kind":"create","directory_id":root_id,"name":name});
            assert!(store.add_project_command("bad", &p, 21).is_err());
        }
        store.remove_project_root(&root_id).unwrap();
        assert!(store.browse_projects(&child_id, None, 21).is_err());
    }
    #[test]
    fn directory_pagination_and_duplicate_attach_preserve_existing_data() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        for i in 0..105 {
            fs::create_dir(root.join(format!("p{i:03}"))).unwrap();
        }
        let store = ExperimentStore::open(&temp.path().join("agent.db")).unwrap();
        let id = store.add_project_root(&root, "Projects").unwrap();
        let first = store.browse_projects(&id, None, 10).unwrap();
        assert_eq!(first.entries.len(), 100);
        let last = store
            .browse_projects(&id, first.next_cursor.as_deref(), 11)
            .unwrap();
        assert_eq!(last.entries.len(), 5);
        assert_eq!(last.entries[0].name, "p100");
        let payload = json!({"kind":"attach","directory_id":first.entries[0].directory_id});
        command(&store, "attach1", payload.clone());
        command(&store, "attach2", payload.clone());
        let a = store.add_project_command("attach1", &payload, 12).unwrap();
        let b = store.add_project_command("attach2", &payload, 13).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.approved_projects().unwrap().len(), 1);
        let conflict = json!({"kind":"create","directory_id":id,"name":"p000"});
        command(&store, "conflict", conflict.clone());
        assert_eq!(
            store
                .add_project_command("conflict", &conflict, 14)
                .unwrap_err()
                .to_string(),
            "project_name_conflict"
        );
        assert!(root.join("p000").is_dir());
        assert_eq!(store.approved_projects().unwrap().len(), 1);
    }
    #[test]
    fn recovery_recognizes_published_inode_without_repeating_mkdir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let path = root.join("new");
        fs::create_dir(&path).unwrap();
        let db = temp.path().join("agent.db");
        let store = ExperimentStore::open(&db).unwrap();
        let id = store.add_project_root(&root, "Projects").unwrap();
        let p = json!({"kind":"create","directory_id":id,"name":"new"});
        command(&store, "interrupted", p);
        let parent = fs::metadata(&root).unwrap();
        let meta = fs::metadata(&path).unwrap();
        store.lock().unwrap().execute("INSERT INTO project_creations VALUES('interrupted',?1,'',?2,?3,'new','.farhelm-missing',?4,?5)",params![id,parent.dev() as i64,parent.ino() as i64,meta.dev() as i64,meta.ino() as i64]).unwrap();
        drop(store);
        let store = ExperimentStore::open(&db).unwrap();
        store.recover_project_creations(20).unwrap();
        assert_eq!(
            store.remote_receipt("interrupted").unwrap().state,
            farhelm_protocol::CommandState::Completed
        );
        assert_eq!(store.approved_projects().unwrap().len(), 1);
    }

    #[test]
    fn lost_registration_transaction_rolls_back_and_sync_failure_keeps_receipt() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("project")).unwrap();
        let store = ExperimentStore::open(&temp.path().join("agent.db")).unwrap();
        let id = store.add_project_root(&root, "Projects").unwrap();
        let directory = store
            .browse_projects(&id, None, 10)
            .unwrap()
            .entries
            .remove(0)
            .directory_id;
        let request = json!({"kind":"attach","directory_id":directory});
        assert!(
            store
                .add_project_command("missing-command", &request, 11)
                .is_err()
        );
        assert!(store.approved_projects().unwrap().is_empty());
        assert!(store.pending_events("a", 100).unwrap().is_empty());
        command(&store, "attach", request.clone());
        let receipt = store.add_project_command("attach", &request, 12).unwrap();
        let id = receipt["project_id"].as_str().unwrap();
        store.set_project_sync(id, "failed", 13).unwrap();
        assert_eq!(
            store.remote_receipt("attach").unwrap().state,
            farhelm_protocol::CommandState::Completed
        );
        assert_eq!(store.approved_projects().unwrap().len(), 1);
        store.set_project_sync(id, "ready", 14).unwrap();
        assert_eq!(store.remote_receipt("attach").unwrap().data, Some(receipt));
    }

    #[test]
    fn schema_seven_upgrade_preserves_authorization_events_and_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir(&root).unwrap();
        let db = temp.path().join("agent.db");
        let store = ExperimentStore::open(&db).unwrap();
        let id = store.add_project_root(&root, "Projects").unwrap();
        let request = json!({"kind":"create","directory_id":id,"name":"new"});
        command(&store, "create", request.clone());
        let receipt = store.add_project_command("create", &request, 12).unwrap();
        store.lock().unwrap().execute_batch("DROP TABLE project_roots;DROP TABLE project_directories;DROP TABLE project_creations;DROP TABLE project_sync;DROP TABLE session_lifecycle;DROP TABLE archive_operations;PRAGMA user_version=7;").unwrap();
        drop(store);
        let store = ExperimentStore::open(&db).unwrap();
        assert_eq!(store.approved_projects().unwrap().len(), 1);
        assert_eq!(store.remote_receipt("create").unwrap().data, Some(receipt));
        assert_eq!(store.pending_events("a", 100).unwrap().len(), 1);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            8
        );
        store
            .lock()
            .unwrap()
            .pragma_update(None, "user_version", 9)
            .unwrap();
        drop(store);
        assert!(ExperimentStore::open(&db).is_err());
    }
}
