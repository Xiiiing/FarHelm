use super::*;
use base64::Engine;
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
};

const IMAGE_LIMIT: u64 = 20 * 1024 * 1024;
const CHUNK_LIMIT: usize = 256 * 1024;

impl ExperimentStore {
    pub fn attachment_begin(
        &self,
        session: &str,
        id: &str,
        mime: &str,
        size: u64,
        ephemeral: bool,
        now: u64,
    ) -> Result<Value> {
        ensure!(
            matches!(mime, "image/png" | "image/jpeg" | "image/webp")
                && (1..=IMAGE_LIMIT).contains(&size),
            "invalid_attachment"
        );
        ensure!(
            self.session_binding(session)?.is_some(),
            "attachment_session_unapproved"
        );
        let ephemeral = ephemeral
            || self.lock()?.query_row(
                "SELECT EXISTS(SELECT 1 FROM temporary_sessions WHERE session_id=?1)",
                [session],
                |row| row.get::<_, bool>(0),
            )?;
        let root = self
            .path
            .parent()
            .context("attachment_state_directory_missing")?
            .parent()
            .context("attachment_data_directory_missing")?
            .join("attachments");
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let path = root.join(format!("{id}.part"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .context("attachment_exists")?;
        self.lock()?.execute("INSERT INTO managed_attachments(attachment_id,session_id,path,mime_type,size_bytes,ephemeral,state,created_at_unix) VALUES(?1,?2,?3,?4,?5,?6,'uploading',?7)", params![id,session,path.to_string_lossy(),mime,i64::try_from(size)?,ephemeral,as_i64(now)?])?;
        Ok(json!({"attachment_id":id,"state":"uploading","offset":0}))
    }

    pub fn attachment_chunk(
        &self,
        session: &str,
        id: &str,
        offset: u64,
        encoded: &str,
    ) -> Result<Value> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .context("invalid_attachment_chunk")?;
        ensure!(
            !bytes.is_empty() && bytes.len() <= CHUNK_LIMIT,
            "invalid_attachment_chunk"
        );
        let (owner,path,size,state):(String,String,i64,String)=self.lock()?.query_row("SELECT session_id,path,size_bytes,state FROM managed_attachments WHERE attachment_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).context("attachment_not_found")?;
        ensure!(
            owner == session && state == "uploading",
            "attachment_unavailable"
        );
        let mut file = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        ensure!(
            file.metadata()?.len() == offset,
            "attachment_offset_conflict"
        );
        let next = offset
            .checked_add(u64::try_from(bytes.len())?)
            .context("attachment_too_large")?;
        ensure!(
            next <= u64::try_from(size)? && next <= IMAGE_LIMIT,
            "attachment_too_large"
        );
        file.write_all(&bytes)?;
        file.sync_data()?;
        Ok(json!({"attachment_id":id,"state":"uploading","offset":next}))
    }

    pub fn attachment_finish(&self, session: &str, id: &str) -> Result<Value> {
        let (owner,path,mime,size,state):(String,String,String,i64,String)=self.lock()?.query_row("SELECT session_id,path,mime_type,size_bytes,state FROM managed_attachments WHERE attachment_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).context("attachment_not_found")?;
        ensure!(
            owner == session && state == "uploading",
            "attachment_unavailable"
        );
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        ensure!(
            file.metadata()?.len() == u64::try_from(size)?,
            "attachment_incomplete"
        );
        let mut magic = [0u8; 12];
        let read = file.read(&mut magic)?;
        let valid = match mime.as_str() {
            "image/png" => read >= 8 && magic[..8] == [137, 80, 78, 71, 13, 10, 26, 10],
            "image/jpeg" => read >= 3 && magic[..3] == [0xff, 0xd8, 0xff],
            "image/webp" => read >= 12 && &magic[..4] == b"RIFF" && &magic[8..12] == b"WEBP",
            _ => false,
        };
        ensure!(valid, "attachment_type_mismatch");
        drop(file);
        let final_path = Path::new(&path).with_extension(match mime.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            _ => "webp",
        });
        fs::rename(&path, &final_path)?;
        self.lock()?.execute("UPDATE managed_attachments SET path=?1,state='ready' WHERE attachment_id=?2 AND state='uploading'",params![final_path.to_string_lossy(),id])?;
        Ok(json!({"attachment_id":id,"state":"ready","mime_type":mime,"size_bytes":size}))
    }

    pub fn attachment_path(&self, session: &str, id: &str) -> Result<PathBuf> {
        let (owner, path, state): (String, String, String) = self
            .lock()?
            .query_row(
                "SELECT session_id,path,state FROM managed_attachments WHERE attachment_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .context("attachment_not_found")?;
        ensure!(
            owner == session && state == "ready",
            "attachment_unavailable"
        );
        let path = PathBuf::from(path);
        ensure!(path.is_file(), "attachment_unavailable");
        Ok(path)
    }

    pub fn cleanup_ephemeral_attachments(&self, session: &str) -> Result<()> {
        let paths = {
            let c = self.lock()?;
            let mut s = c.prepare(
                "SELECT path FROM managed_attachments WHERE session_id=?1 AND ephemeral=1",
            )?;
            s.query_map([session], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for path in paths {
            let _ = fs::remove_file(path);
        }
        self.lock()?.execute(
            "DELETE FROM managed_attachments WHERE session_id=?1 AND ephemeral=1",
            [session],
        )?;
        Ok(())
    }

    pub fn cleanup_stale_ephemeral_attachments(&self) -> Result<()> {
        let paths = {
            let c = self.lock()?;
            let mut s = c.prepare("SELECT path FROM managed_attachments WHERE ephemeral=1")?;
            s.query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for path in paths {
            let _ = fs::remove_file(path);
        }
        self.lock()?
            .execute("DELETE FROM managed_attachments WHERE ephemeral=1", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn image_upload_is_offset_checked_and_path_stays_agent_owned() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("state/agent.db");
        let store = ExperimentStore::open(&database).unwrap();
        let cwd = directory.path().canonicalize().unwrap();
        store.bind_session("s", "p", &cwd, "inspect", 1).unwrap();
        let bytes = [137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0];
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        store
            .attachment_begin("s", "att_test", "image/png", bytes.len() as u64, false, 1)
            .unwrap();
        assert!(
            store
                .attachment_chunk("s", "att_test", 1, &encoded)
                .is_err()
        );
        store
            .attachment_chunk("s", "att_test", 0, &encoded)
            .unwrap();
        store.attachment_finish("s", "att_test").unwrap();
        let path = store.attachment_path("s", "att_test").unwrap();
        assert!(path.starts_with(directory.path()));

        store.mark_temporary_session("s", 2).unwrap();
        store
            .attachment_begin("s", "att_temp", "image/png", bytes.len() as u64, false, 2)
            .unwrap();
        assert!(
            store
                .lock()
                .unwrap()
                .query_row(
                    "SELECT ephemeral FROM managed_attachments WHERE attachment_id='att_temp'",
                    [],
                    |row| row.get::<_, bool>(0)
                )
                .unwrap()
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
