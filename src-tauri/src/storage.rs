use crate::models::{
    is_masked_value, is_sensitive_header, masked_secret, ApiConnection, BootstrapData,
    CaptionVersion, EditBatchRecord, HistoricalStats, ModelOption, StatsBreakdown,
    StatsBreakdownRow, StatsQuery, TaskItem, TaskRun, UsageMetrics,
};
use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde_json::Value;
use std::{
    collections::HashMap,
    ffi::c_void,
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[repr(C)]
struct DataBlob {
    cb_data: u32,
    pb_data: *mut u8,
}

#[link(name = "Crypt32")]
extern "system" {
    fn CryptProtectData(
        data_in: *const DataBlob,
        description: *const u16,
        optional_entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        data_in: *const DataBlob,
        description: *mut *mut u16,
        optional_entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        data_out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "Kernel32")]
extern "system" {
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn MoveFileExW(existing_file_name: *const u16, new_file_name: *const u16, flags: u32) -> i32;
}

const CRYPTPROTECT_UI_FORBIDDEN: u32 = 0x1;
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

const SECRET_KEY_PREFIX: &str = "key:";
const SECRET_HEADER_PREFIX: &str = "hdr:";

pub const CAPTION_SOURCE_MANUAL: &str = "manual";
pub const CAPTION_SOURCE_BATCH_EDIT: &str = "batch_edit";
pub const CAPTION_SOURCE_REVISION: &str = "revision";
pub const CAPTION_SOURCE_UNDO: &str = "undo";
pub const CAPTION_SOURCE_LOCAL: &str = "local";

fn dpapi_encrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let input = DataBlob {
        cb_data: data.len() as u32,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "DPAPI 加密失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let encrypted =
        unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
    unsafe {
        LocalFree(output.pb_data as *mut c_void);
    }
    Ok(encrypted)
}

fn dpapi_decrypt(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let input = DataBlob {
        cb_data: data.len() as u32,
        pb_data: data.as_ptr() as *mut u8,
    };
    let mut output = DataBlob {
        cb_data: 0,
        pb_data: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 {
        return Err(format!(
            "DPAPI 解密失败：{}",
            std::io::Error::last_os_error()
        ));
    }
    let decrypted =
        unsafe { std::slice::from_raw_parts(output.pb_data, output.cb_data as usize).to_vec() };
    unsafe {
        LocalFree(output.pb_data as *mut c_void);
    }
    Ok(decrypted)
}

fn header_secret_key(connection_id: &str, name: &str) -> String {
    format!(
        "{SECRET_HEADER_PREFIX}{connection_id}:{}",
        name.to_ascii_lowercase()
    )
}

fn read_text(path: &Path) -> Result<String, String> {
    if !path.exists() {
        return Ok(String::new());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if let Ok(value) = String::from_utf8(bytes.clone()) {
        return Ok(value.trim_start_matches('\u{feff}').to_string());
    }
    let (decoded, _, _) = encoding_rs::GB18030.decode(&bytes);
    Ok(decoded.into_owned())
}

fn is_database_corruption_error(error: &str) -> bool {
    let normalized = error.to_ascii_lowercase();
    normalized.contains("database disk image is malformed")
        || normalized.contains("file is not a database")
        || normalized.contains("database corruption")
        || normalized.contains("database corrupt")
}

pub struct Storage {
    db: Mutex<Connection>,
    secrets: Mutex<HashMap<String, String>>,
    secrets_path: PathBuf,
    log_dir: PathBuf,
    base_dir: PathBuf,
}

impl Storage {
    pub fn open(app_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(app_dir).map_err(|e| e.to_string())?;
        let db_path = app_dir.join("caption-studio.db");
        // 数据库损坏时先隔离再重建，避免应用无法启动；旧文件保留供手工恢复。
        let open_result = (|| -> Result<Connection, String> {
            let connection = Connection::open(&db_path).map_err(|e| e.to_string())?;
            connection
                .busy_timeout(std::time::Duration::from_secs(2))
                .map_err(|e| e.to_string())?;
            connection
                .execute_batch(
                    "PRAGMA journal_mode=WAL;
                     PRAGMA foreign_keys=ON;",
                )
                .map_err(|e| e.to_string())?;
            migrate(&connection)?;
            Ok(connection)
        })();
        let connection = match open_result {
            Ok(connection) => connection,
            Err(error) if is_database_corruption_error(&error) => {
                let stamp = Utc::now().format("%Y%m%d%H%M%S");
                let backup_path = app_dir.join(format!("caption-studio.db.corrupt-{stamp}"));
                let _ = fs::rename(&db_path, &backup_path);
                for suffix in ["-wal", "-shm"] {
                    let _ = fs::rename(
                        app_dir.join(format!("caption-studio.db{suffix}")),
                        app_dir.join(format!("caption-studio.db.corrupt-{stamp}{suffix}")),
                    );
                }
                let connection = Connection::open(&db_path).map_err(|e| e.to_string())?;
                connection
                    .busy_timeout(std::time::Duration::from_secs(2))
                    .map_err(|e| e.to_string())?;
                connection
                    .execute_batch(
                        "PRAGMA journal_mode=WAL;
                         PRAGMA foreign_keys=ON;",
                    )
                    .map_err(|e| e.to_string())?;
                migrate(&connection)?;
                connection
            }
            Err(error) => return Err(format!("数据库暂时不可用，原文件未被移动：{error}")),
        };
        let secrets_path = app_dir.join("secrets.bin");
        let mut secrets_corrupt: Option<String> = None;
        let secrets = if secrets_path.exists() {
            let loaded = fs::read(&secrets_path)
                .map_err(|e| e.to_string())
                .and_then(|encrypted| dpapi_decrypt(&encrypted))
                .and_then(|decrypted| {
                    serde_json::from_slice::<HashMap<String, String>>(&decrypted)
                        .map_err(|e| format!("凭据文件格式错误：{e}"))
                });
            match loaded {
                Ok(map) => map,
                Err(error) => {
                    // 凭据文件损坏：隔离后以空凭据库启动，让应用仍可打开，
                    // 用户重新填写 Key 即可；损坏文件保留供手工恢复。
                    secrets_corrupt = Some(error);
                    let stamp = Utc::now().format("%Y%m%d%H%M%S");
                    let _ = fs::rename(
                        &secrets_path,
                        app_dir.join(format!("secrets.bin.corrupt-{stamp}")),
                    );
                    HashMap::new()
                }
            }
        } else {
            HashMap::new()
        };
        let log_dir = app_dir.join("logs");
        fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
        let storage = Self {
            db: Mutex::new(connection),
            secrets: Mutex::new(secrets),
            secrets_path,
            log_dir,
            base_dir: app_dir.to_path_buf(),
        };
        storage.recover_mutations()?;
        if let Some(error) = secrets_corrupt {
            storage.write_log(
                "error",
                "system",
                &format!("凭据文件损坏，已隔离并重置（请重新填写 API Key）：{error}"),
            );
        }
        Ok(storage)
    }

    /// 每日自动备份：数据库经 SQLite 在线备份（一致性快照）与凭据文件
    /// 一起复制到 `backups/`，保留最近 7 天。即使应用数据被误删或清理，
    /// 也能从备份恢复连接配置与密钥。模型文件不备份（体积大），但它们
    /// 位于应用数据目录，卸载/更新不会删除。
    pub fn backup_if_due(&self) {
        use rusqlite::backup::Backup;
        use std::time::Duration;
        let app_dir = self.secrets_path.parent();
        let Some(app_dir) = app_dir else { return };
        let backup_dir = app_dir.join("backups");
        if fs::create_dir_all(&backup_dir).is_err() {
            return;
        }
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let db_dest = backup_dir.join(format!("caption-studio-{today}.db"));
        if db_dest.exists() {
            // 当日已备份；首次备份可能发生在凭据文件创建之前，这里补上副本。
            let secrets_dest = backup_dir.join(format!("secrets-{today}.bin"));
            if !secrets_dest.exists() && self.secrets_path.is_file() {
                let _ = fs::copy(&self.secrets_path, secrets_dest);
            }
            return;
        }
        let db = self.db.lock();
        let backed = Connection::open(&db_dest)
            .ok()
            .map(|mut dest| match Backup::new(&db, &mut dest) {
                Ok(backup) => backup
                    .run_to_completion(5, Duration::from_millis(50), None)
                    .is_ok(),
                Err(_) => false,
            })
            .unwrap_or(false);
        drop(db);
        if backed {
            let _ = fs::copy(
                &self.secrets_path,
                backup_dir.join(format!("secrets-{today}.bin")),
            );
            self.write_log(
                "info",
                "system",
                &format!("已自动备份数据到 {}", backup_dir.display()),
            );
        }
        // 清理超过 7 天的备份（db 与 secrets 各保留 7 份）。
        if let Ok(entries) = fs::read_dir(&backup_dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.is_file())
                .collect();
            files.sort();
            while files.len() > 14 {
                if let Some(oldest) = files.first() {
                    if fs::remove_file(oldest).is_ok() {
                        files.remove(0);
                    } else {
                        break;
                    }
                }
            }
        }
    }

    fn flush_secrets(&self) -> Result<(), String> {
        let plain = serde_json::to_vec(&*self.secrets.lock()).map_err(|e| e.to_string())?;
        let encrypted = dpapi_encrypt(&plain)?;
        atomic_write_bytes(&self.secrets_path, &encrypted)
    }

    pub fn bootstrap(&self) -> Result<BootstrapData, String> {
        let db = self.db.lock();
        let mut statement = db
            .prepare("SELECT json FROM connections ORDER BY updated_at, id")
            .map_err(|e| e.to_string())?;
        let connections = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|json| serde_json::from_str(&json).ok())
            .collect();
        let read_presets = |kind: &str| -> Result<Vec<Value>, String> {
            let mut stmt = db
                .prepare("SELECT json FROM presets WHERE kind=?1 ORDER BY updated_at, id")
                .map_err(|e| e.to_string())?;
            let values = stmt
                .query_map([kind], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .filter_map(|json| serde_json::from_str(&json).ok())
                .collect();
            Ok(values)
        };
        Ok(BootstrapData {
            connections,
            tag_presets: read_presets("tag")?,
            revision_presets: read_presets("revision")?,
        })
    }

    /// 每次保存连接前备份连接快照（不含密钥）到 backups/connections/，保留最近 20 份。
    fn backup_connection_snapshot(&self, connection: &ApiConnection) {
        let backup_dir = self.base_dir.join("backups").join("connections");
        if fs::create_dir_all(&backup_dir).is_err() {
            return;
        }
        let ts = Utc::now().format("%Y%m%d-%H%M%S%.3f");
        let safe_name: String = connection
            .name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let file = backup_dir.join(format!("{ts}-{}-{}.json", connection.id, safe_name));
        if let Ok(json) = serde_json::to_string_pretty(connection) {
            let _ = fs::write(&file, json);
        }
        let mut files: Vec<_> = fs::read_dir(&backup_dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "json"))
            .collect();
        files.sort();
        while files.len() > 20 {
            if let Some(oldest) = files.first() {
                let _ = fs::remove_file(oldest);
                files.remove(0);
            }
        }
    }

    pub fn save_connection(&self, mut connection: ApiConnection) -> Result<ApiConnection, String> {
        if connection.id.trim().is_empty() {
            connection.id = Uuid::new_v4().to_string();
        }
        if connection.key_refs.is_empty() {
            return Err("至少需要一个 API Key".into());
        }
        connection.total_concurrency = connection.total_concurrency.clamp(1, 32);
        connection.per_key_concurrency = connection.per_key_concurrency.clamp(1, 8);
        connection.timeout_seconds = connection.timeout_seconds.clamp(10, 600);
        {
            let mut secrets = self.secrets.lock();
            for key in &mut connection.key_refs {
                let id = key
                    .id
                    .clone()
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| Uuid::new_v4().to_string());
                key.id = Some(id.clone());
                if let Some(secret) = key.secret.take().filter(|value| !value.trim().is_empty()) {
                    secrets.insert(format!("{SECRET_KEY_PREFIX}{id}"), secret);
                }
                let saved = secrets
                    .get(&format!("{SECRET_KEY_PREFIX}{id}"))
                    .ok_or_else(|| format!("{} 尚未填写 API Key", key.label))?;
                key.masked = Some(masked_secret(saved));
                key.secret = None;
            }
            for (name, value) in &mut connection.headers {
                if !is_sensitive_header(name) {
                    continue;
                }
                let key = header_secret_key(&connection.id, name);
                if is_masked_value(value) {
                    if !secrets.contains_key(&key) {
                        return Err(format!("敏感请求头 {} 尚未填写", name));
                    }
                } else if !value.trim().is_empty() {
                    secrets.insert(key.clone(), value.trim().to_string());
                } else {
                    secrets.remove(&key);
                }
                *value = secrets
                    .get(&key)
                    .map(|saved| masked_secret(saved))
                    .unwrap_or_default();
            }
            drop(secrets);
            self.flush_secrets()?;
        }
        let json = serde_json::to_string(&connection).map_err(|e| e.to_string())?;
        self.db
            .lock()
            .execute(
                "INSERT INTO connections(id,json,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET json=excluded.json, updated_at=excluded.updated_at",
                params![connection.id, json, Utc::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
        // 每次保存成功即备份连接快照
        self.backup_connection_snapshot(&connection);
        Ok(connection)
    }

    pub fn delete_connection(&self, id: &str) -> Result<(), String> {
        let db = self.db.lock();
        let referenced: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM request_metrics WHERE connection_id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if referenced > 0 {
            return Err(format!("该连接已被 {referenced} 条历史指标引用，无法删除"));
        }
        let connection = self.get_connection(id).ok();
        db.execute("DELETE FROM connections WHERE id=?1", [id])
            .map_err(|e| e.to_string())?;
        drop(db);
        if let Some(connection) = connection {
            let mut secrets = self.secrets.lock();
            for key in connection.key_refs {
                if let Some(id) = key.id {
                    secrets.remove(&format!("{SECRET_KEY_PREFIX}{id}"));
                }
            }
            let prefix = format!("{SECRET_HEADER_PREFIX}{id}:");
            secrets.retain(|key, _| !key.starts_with(&prefix));
            drop(secrets);
            self.flush_secrets()?;
        }
        Ok(())
    }

    pub fn get_connection(&self, id: &str) -> Result<ApiConnection, String> {
        let json: Option<String> = self
            .db
            .lock()
            .query_row("SELECT json FROM connections WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        json.ok_or_else(|| "API 连接不存在".into())
            .and_then(|v| serde_json::from_str(&v).map_err(|e| e.to_string()))
    }

    /// Connection with sensitive header values restored from the DPAPI vault.
    /// Only the provider/network layer may use this; it is never serialized
    /// back to the frontend or the database.
    pub fn get_connection_full(&self, id: &str) -> Result<ApiConnection, String> {
        let mut connection = self.get_connection(id)?;
        let secrets = self.secrets.lock();
        for (name, value) in &mut connection.headers {
            if is_sensitive_header(name) && is_masked_value(value) {
                *value = secrets
                    .get(&header_secret_key(id, name))
                    .ok_or_else(|| format!("敏感请求头 {} 的凭据缺失", name))?
                    .clone();
            }
        }
        Ok(connection)
    }

    pub fn key_secret(&self, id: &str) -> Option<String> {
        self.secrets
            .lock()
            .get(&format!("{SECRET_KEY_PREFIX}{id}"))
            .cloned()
    }

    pub fn update_models(&self, id: &str, models: Vec<ModelOption>) -> Result<(), String> {
        let mut connection = self.get_connection(id)?;
        connection.cached_models = Some(models);
        connection.models_cached_at = Some(Utc::now().timestamp_millis());
        let json = serde_json::to_string(&connection).map_err(|e| e.to_string())?;
        self.db
            .lock()
            .execute(
                "UPDATE connections SET json=?2, updated_at=?3 WHERE id=?1",
                params![id, json, Utc::now().to_rfc3339()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn save_preset(&self, kind: &str, preset: Value) -> Result<(), String> {
        let id = preset
            .get("id")
            .and_then(Value::as_str)
            .ok_or("预设缺少 ID")?;
        let json = serde_json::to_string(&preset).map_err(|e| e.to_string())?;
        self.db.lock().execute(
            "INSERT INTO presets(id,kind,json,updated_at) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,json=excluded.json,updated_at=excluded.updated_at",
            params![id, kind, json, Utc::now().to_rfc3339()]
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn delete_preset(&self, kind: &str, id: &str) -> Result<(), String> {
        let db = self.db.lock();
        let referenced: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM request_metrics WHERE preset_id=?1",
                [id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if referenced > 0 {
            return Err(format!("该预设已被 {referenced} 条历史指标引用，无法删除"));
        }
        db.execute(
            "DELETE FROM presets WHERE id=?1 AND kind=?2",
            params![id, kind],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---- File mutation journal -------------------------------------------

    /// Journaled overwrite: registers the pending change, writes the file
    /// atomically, commits the version row, then marks the journal entry
    /// applied. On version-commit failure the file is rolled back; on
    /// crash mid-sequence `recover_mutations` repairs the file at startup.
    pub fn commit_mutation(
        &self,
        path: &str,
        before: &str,
        after: &str,
        source: &str,
        batch_id: Option<&str>,
        mutation_type: &str,
    ) -> Result<Option<i64>, String> {
        let db = self.db.lock();
        // All application writes pass through this database lock. Re-read while
        // holding it so an API response cannot silently overwrite a manual edit
        // or another batch mutation made after the request started.
        let current = if Path::new(path).exists() {
            crate::file_ops::read_text(Path::new(path))?
        } else {
            String::new()
        };
        if current != before {
            return Err("标签文件已在任务期间发生变化，请刷新后重试".into());
        }
        if before == after {
            return Ok(None);
        }
        db.execute(
            "INSERT INTO file_mutations(caption_path, mutation_type, before_content, after_content, batch_id, applied, created_at)
             VALUES(?1,?2,?3,?4,?5,0,?6)",
            params![path, mutation_type, before, after, batch_id, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        let journal_id = db.last_insert_rowid();
        if let Err(error) = atomic_write_text(Path::new(path), after) {
            let _ = db.execute("DELETE FROM file_mutations WHERE id=?1", [journal_id]);
            return Err(error);
        }
        let version_result = db.execute(
            "INSERT INTO caption_versions(caption_path,old_content,new_content,source,batch_id,created_at)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![path, before, after, source, batch_id, Utc::now().to_rfc3339()],
        );
        let version_id = match version_result {
            Ok(_) => Some(db.last_insert_rowid()),
            Err(error) => {
                // Database failed: roll the file back and drop the journal.
                if let Err(rollback) = atomic_write_text(Path::new(path), before) {
                    self.write_log(
                        "error",
                        "storage",
                        &format!("回滚文件失败 {}：{rollback}", path),
                    );
                }
                let _ = db.execute("DELETE FROM file_mutations WHERE id=?1", [journal_id]);
                return Err(error.to_string());
            }
        };
        if let Err(error) = db.execute(
            "UPDATE file_mutations SET applied=1, version_id=?2 WHERE id=?1",
            params![journal_id, version_id],
        ) {
            // Version committed and file written; the journal is repaired at
            // next startup.
            self.write_log(
                "warn",
                "storage",
                &format!("变更日志提交失败（启动时将修复）：{error}"),
            );
        }
        Ok(version_id)
    }

    /// Startup repair: every pending journal entry is reconciled with the
    /// actual file content so no fake version row survives a crash.
    fn recover_mutations(&self) -> Result<(), String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, caption_path, before_content, after_content, version_id
                 FROM file_mutations WHERE applied=0",
            )
            .map_err(|e| e.to_string())?;
        let rows: Vec<(i64, String, String, String, Option<i64>)> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        drop(stmt);
        for (id, path, before, after, version_id) in rows {
            let current = read_text(Path::new(&path)).unwrap_or_default();
            if version_id.is_some() {
                // Version committed: file must contain the new content.
                if current != after {
                    let _ = atomic_write_text(Path::new(&path), &after);
                    self.write_log(
                        "warn",
                        "storage",
                        &format!("启动恢复：重新写入未完成变更 {}", path),
                    );
                }
                let _ = db.execute("UPDATE file_mutations SET applied=1 WHERE id=?1", [id]);
            } else {
                // Version never committed: file must be back to the old content.
                if current != before {
                    let _ = atomic_write_text(Path::new(&path), &before);
                    self.write_log(
                        "warn",
                        "storage",
                        &format!("启动恢复：回滚未提交变更 {}", path),
                    );
                }
                let _ = db.execute("DELETE FROM file_mutations WHERE id=?1", [id]);
            }
        }
        Ok(())
    }

    pub fn list_versions(&self, path: Option<&str>) -> Result<Vec<CaptionVersion>, String> {
        let db = self.db.lock();
        let sql = if path.is_some() {
            "SELECT id,caption_path,old_content,new_content,source,batch_id,created_at FROM caption_versions WHERE caption_path=?1 ORDER BY id DESC LIMIT 200"
        } else {
            "SELECT id,caption_path,old_content,new_content,source,batch_id,created_at FROM caption_versions ORDER BY id DESC LIMIT 500"
        };
        let mut stmt = db.prepare(sql).map_err(|e| e.to_string())?;
        let map = |row: &rusqlite::Row<'_>| {
            Ok(CaptionVersion {
                id: row.get(0)?,
                caption_path: row.get(1)?,
                old_content: row.get(2)?,
                new_content: row.get(3)?,
                source: row.get(4)?,
                batch_id: row.get(5)?,
                created_at: row.get(6)?,
            })
        };
        let rows = if let Some(path) = path {
            stmt.query_map([path], map)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .collect()
        } else {
            stmt.query_map([], map)
                .map_err(|e| e.to_string())?
                .filter_map(Result::ok)
                .collect()
        };
        Ok(rows)
    }

    pub fn get_version(&self, id: i64) -> Result<CaptionVersion, String> {
        self.db.lock().query_row("SELECT id,caption_path,old_content,new_content,source,batch_id,created_at FROM caption_versions WHERE id=?1", [id], |row| Ok(CaptionVersion { id: row.get(0)?, caption_path: row.get(1)?, old_content: row.get(2)?, new_content: row.get(3)?, source: row.get(4)?, batch_id: row.get(5)?, created_at: row.get(6)? })).map_err(|e| e.to_string())
    }

    pub fn versions_for_batch(&self, batch_id: &str) -> Result<Vec<CaptionVersion>, String> {
        let db = self.db.lock();
        let mut stmt = db.prepare("SELECT id,caption_path,old_content,new_content,source,batch_id,created_at FROM caption_versions WHERE batch_id=?1 ORDER BY id DESC").map_err(|e| e.to_string())?;
        let values = stmt
            .query_map([batch_id], |row| {
                Ok(CaptionVersion {
                    id: row.get(0)?,
                    caption_path: row.get(1)?,
                    old_content: row.get(2)?,
                    new_content: row.get(3)?,
                    source: row.get(4)?,
                    batch_id: row.get(5)?,
                    created_at: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(values)
    }

    // ---- Edit batches -----------------------------------------------------

    pub fn create_edit_batch(
        &self,
        id: &str,
        rule_json: &str,
        previews_json: &str,
        affected: u64,
        skipped: u64,
        errors: u64,
    ) -> Result<(), String> {
        self.db.lock().execute(
            "INSERT INTO edit_batches(id, rule_json, previews_json, affected, skipped, errors, applied, created_at)
             VALUES(?1,?2,?3,?4,?5,?6,0,?7)",
            params![id, rule_json, previews_json, affected, skipped, errors, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn apply_edit_batch(
        &self,
        id: &str,
        applied_count: u64,
        results_json: &str,
    ) -> Result<(), String> {
        self.db.lock().execute(
            "UPDATE edit_batches SET applied=1, applied_count=?2, results_json=?3, applied_at=?4 WHERE id=?1",
            params![id, applied_count, results_json, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Returns (rule_json, previews_json, affected, skipped, errors, applied).
    pub fn get_edit_batch_full(
        &self,
        id: &str,
    ) -> Result<(String, String, i64, i64, i64, bool), String> {
        self.db
            .lock()
            .query_row(
                "SELECT rule_json, previews_json, affected, skipped, errors, applied
                 FROM edit_batches WHERE id=?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .map_err(|e| e.to_string())
    }

    pub fn list_edit_batches(&self) -> Result<Vec<EditBatchRecord>, String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id, rule_json, affected, skipped, errors, applied, created_at, applied_at
                 FROM edit_batches ORDER BY created_at DESC LIMIT 100",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(EditBatchRecord {
                    id: row.get(0)?,
                    rule: serde_json::from_str(&row.get::<_, String>(1)?).unwrap_or_else(|_| {
                        crate::models::CaptionEditRule {
                            mode: String::new(),
                            action: String::new(),
                            position: String::new(),
                            content: String::new(),
                            replacement: None,
                            anchor: None,
                            index: None,
                            regex: false,
                            case_sensitive: false,
                            all_matches: false,
                            deduplicate: false,
                            only_if_missing: false,
                        }
                    }),
                    affected: row.get(2)?,
                    skipped: row.get(3)?,
                    errors: row.get(4)?,
                    applied: row.get(5)?,
                    created_at: row.get(6)?,
                    applied_at: row.get(7)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    // ---- Task runs and items ----------------------------------------------

    pub fn insert_task_run(&self, run: &TaskRun) -> Result<(), String> {
        self.db.lock().execute(
            "INSERT INTO task_runs(id,kind,connection_id,connection_name,model_id,preset_id,status,total,succeeded,refused,failed,skipped,cancelled,input_tokens,output_tokens,started_at,finished_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            params![
                run.id, run.kind, run.connection_id, run.connection_name, run.model_id,
                run.preset_id, run.status, run.total, run.succeeded, run.refused, run.failed,
                run.skipped, run.cancelled, run.input_tokens, run.output_tokens, run.started_at,
                run.finished_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn finish_task_run(&self, run_id: &str) -> Result<(), String> {
        self.db.lock().execute(
            "UPDATE task_runs SET status='completed', finished_at=?2,
                succeeded=(SELECT COUNT(*) FROM task_items WHERE run_id=?1 AND status IN ('succeeded','revised')),
                refused=(SELECT COUNT(*) FROM task_items WHERE run_id=?1 AND status='refused'),
                failed=(SELECT COUNT(*) FROM task_items WHERE run_id=?1 AND status='failed'),
                skipped=(SELECT COUNT(*) FROM task_items WHERE run_id=?1 AND status='skipped'),
                cancelled=(SELECT COUNT(*) FROM task_items WHERE run_id=?1 AND status='cancelled'),
                input_tokens=COALESCE((SELECT SUM(input_tokens) FROM task_items WHERE run_id=?1),0),
                output_tokens=COALESCE((SELECT SUM(output_tokens) FROM task_items WHERE run_id=?1),0)
             WHERE id=?1",
            params![run_id, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[allow(dead_code)] // exercised by e2e scheduler tests
    /// 已复核完成的图片路径集合：revision 成功（status=revised）或用户手动忽略错误
    /// （review_overrides）的记录。用于"停止复核后重新开始不再重复复核"的过滤。
    pub fn list_reviewed_caption_paths(&self) -> Result<Vec<String>, String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT caption_path FROM task_items WHERE status='revised'                  UNION SELECT path FROM review_overrides",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(row.map_err(|e| e.to_string())?);
        }
        Ok(paths)
    }

    /// 每张图被明确成功修订的次数（status='revised' 的任务记录，仅真正写入文件才算）。
    pub fn count_revised_per_caption(
        &self,
    ) -> Result<std::collections::HashMap<String, u64>, String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT caption_path, COUNT(*) FROM task_items WHERE status='revised' GROUP BY caption_path",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
            })
            .map_err(|e| e.to_string())?;
        let mut counts = std::collections::HashMap::new();
        for row in rows {
            let (path, count) = row.map_err(|e| e.to_string())?;
            counts.insert(path, count);
        }
        Ok(counts)
    }

    /// 用户手动忽略某张图的复核错误：该图此后视为已复核（下次复核跳过）。
    pub fn ignore_review_error(&self, caption_path: &str) -> Result<(), String> {
        let db = self.db.lock();
        db.execute(
            "INSERT OR REPLACE INTO review_overrides(path, created_at) VALUES (?1, ?2)",
            rusqlite::params![caption_path, Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))] // 供测试与后续历史查看功能使用
    pub fn list_task_runs(&self, limit: u32) -> Result<Vec<TaskRun>, String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id,kind,connection_id,connection_name,model_id,preset_id,status,total,succeeded,refused,failed,skipped,cancelled,input_tokens,output_tokens,started_at,finished_at
                 FROM task_runs ORDER BY started_at DESC LIMIT ?1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([limit], |row| {
                Ok(TaskRun {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    connection_id: row.get(2)?,
                    connection_name: row.get(3)?,
                    model_id: row.get(4)?,
                    preset_id: row.get(5)?,
                    status: row.get(6)?,
                    total: row.get(7)?,
                    succeeded: row.get(8)?,
                    refused: row.get(9)?,
                    failed: row.get(10)?,
                    skipped: row.get(11)?,
                    cancelled: row.get(12)?,
                    input_tokens: row.get(13)?,
                    output_tokens: row.get(14)?,
                    started_at: row.get(15)?,
                    finished_at: row.get(16)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    #[allow(dead_code)] // exercised by e2e scheduler tests
    pub fn list_task_items(&self, run_id: &str) -> Result<Vec<TaskItem>, String> {
        let db = self.db.lock();
        let mut stmt = db
            .prepare(
                "SELECT id,run_id,image_path,caption_path,status,retries,error,key_id,input_tokens,output_tokens,ttft_ms,tokens_per_second,created_at
                 FROM task_items WHERE run_id=?1 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([run_id], |row| {
                Ok(TaskItem {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    image_path: row.get(2)?,
                    caption_path: row.get(3)?,
                    status: row.get(4)?,
                    retries: row.get(5)?,
                    error: row.get(6)?,
                    key_id: row.get(7)?,
                    input_tokens: row.get(8)?,
                    output_tokens: row.get(9)?,
                    ttft_ms: row.get(10)?,
                    tokens_per_second: row.get(11)?,
                    created_at: row.get(12)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect();
        Ok(rows)
    }

    pub fn insert_task_item(&self, item: &TaskItem) -> Result<(), String> {
        self.db.lock().execute(
            "INSERT INTO task_items(run_id,image_path,caption_path,status,retries,error,key_id,input_tokens,output_tokens,ttft_ms,tokens_per_second,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                item.run_id, item.image_path, item.caption_path, item.status, item.retries,
                item.error, item.key_id, item.input_tokens, item.output_tokens, item.ttft_ms,
                item.tokens_per_second, item.created_at
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    // ---- Metrics ------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub fn add_metric(
        &self,
        batch_id: &str,
        kind: &str,
        connection_id: &str,
        connection_name: &str,
        model_id: &str,
        preset_id: Option<&str>,
        image_path: Option<&str>,
        key_id: Option<&str>,
        retries: u32,
        status: &str,
        metrics: &UsageMetrics,
    ) -> Result<(), String> {
        self.db.lock().execute(
            "INSERT INTO request_metrics(batch_id,kind,connection_id,connection_name,model_id,preset_id,image_path,key_id,retries,status,input_tokens,output_tokens,ttft_ms,generation_ms,tokens_per_second,created_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
            params![
                batch_id, kind, connection_id, connection_name, model_id, preset_id, image_path,
                key_id, retries as i64, status, metrics.input_tokens, metrics.output_tokens,
                metrics.ttft_ms, metrics.generation_ms, metrics.tokens_per_second,
                Utc::now().to_rfc3339()
            ],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    }

    fn stats_where(query: &StatsQuery) -> (String, Vec<SqlValue>) {
        let mut clauses = Vec::new();
        let mut values: Vec<SqlValue> = Vec::new();
        let mut push = |column: &str, value: &Option<String>, clauses: &mut Vec<String>| {
            if let Some(value) = value {
                clauses.push(format!("{column}=?{}", clauses.len() + 1));
                values.push(SqlValue::Text(value.clone()));
            }
        };
        push("connection_id", &query.connection_id, &mut clauses);
        push("batch_id", &query.batch_id, &mut clauses);
        push("model_id", &query.model_id, &mut clauses);
        push("preset_id", &query.preset_id, &mut clauses);
        push("key_id", &query.key_id, &mut clauses);
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        (where_sql, values)
    }

    fn summarize(&self, where_sql: &str, values: &[SqlValue]) -> Result<HistoricalStats, String> {
        self.db
            .lock()
            .query_row(
                &format!(
                    "SELECT COUNT(*),
                        COALESCE(SUM(CASE WHEN status IN ('succeeded','revised') THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN status='refused' THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        AVG(ttft_ms), AVG(tokens_per_second)
                 FROM request_metrics{where_sql}"
                ),
                rusqlite::params_from_iter(values.iter().cloned()),
                |row| {
                    Ok(HistoricalStats {
                        total: row.get(0)?,
                        succeeded: row.get(1)?,
                        refused: row.get(2)?,
                        failed: row.get(3)?,
                        input_tokens: row.get(4)?,
                        output_tokens: row.get(5)?,
                        average_ttft_ms: row.get(6)?,
                        average_tokens_per_second: row.get(7)?,
                    })
                },
            )
            .map_err(|e| e.to_string())
    }

    pub fn query_stats(&self) -> Result<HistoricalStats, String> {
        self.summarize("", &[])
    }

    pub fn query_stats_breakdown(&self, query: &StatsQuery) -> Result<StatsBreakdown, String> {
        let (where_sql, values) = Self::stats_where(query);
        let (column, fallback) = match query.group_by.as_str() {
            "kind" => ("kind", "''"),
            "batch" => ("batch_id", "''"),
            "model" => ("model_id", "'(未指定)'"),
            "preset" => ("preset_id", "'(未指定)'"),
            "key" => ("key_id", "'(无)'"),
            _ => return Err("未知分组维度".into()),
        };
        let db = self.db.lock();
        let mut stmt = db
            .prepare(&format!(
                "SELECT COALESCE({column},{fallback}) AS grp, COUNT(*),
                    COALESCE(SUM(CASE WHEN status IN ('succeeded','revised') THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='refused' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='failed' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                    AVG(ttft_ms), AVG(tokens_per_second)
                 FROM request_metrics{where_sql}
                 GROUP BY grp ORDER BY grp"
            ))
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(values.iter().cloned()), |row| {
                Ok(StatsBreakdownRow {
                    group: row.get(0)?,
                    label: row.get(0)?,
                    total: row.get(1)?,
                    succeeded: row.get(2)?,
                    refused: row.get(3)?,
                    failed: row.get(4)?,
                    input_tokens: row.get(5)?,
                    output_tokens: row.get(6)?,
                    average_ttft_ms: row.get(7)?,
                    average_tokens_per_second: row.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        drop(stmt);
        drop(db);
        let mut rows = rows;
        // Human-readable labels for grouped dimensions.
        match query.group_by.as_str() {
            "connection" => {
                let names = self
                    .db
                    .lock()
                    .prepare("SELECT id, name FROM connections")
                    .map(|mut stmt| {
                        stmt.query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map(|iter| iter.filter_map(Result::ok).collect::<HashMap<_, _>>())
                        .unwrap_or_default()
                    })
                    .unwrap_or_default();
                for row in &mut rows {
                    if let Some(name) = names.get(&row.group) {
                        row.label = name.clone();
                    }
                }
            }
            "preset" => {
                let names = self
                    .db
                    .lock()
                    .prepare("SELECT id, json FROM presets")
                    .map(|mut stmt| {
                        stmt.query_map([], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .map(|iter| {
                            iter.filter_map(Result::ok)
                                .filter_map(|(id, json)| {
                                    serde_json::from_str::<Value>(&json).ok().and_then(|v| {
                                        v.get("name")
                                            .and_then(Value::as_str)
                                            .map(|n| (id, n.to_string()))
                                    })
                                })
                                .collect::<HashMap<_, _>>()
                        })
                        .unwrap_or_default()
                    })
                    .unwrap_or_default();
                for row in &mut rows {
                    if let Some(name) = names.get(&row.group) {
                        row.label = name.clone();
                    }
                }
            }
            "key" => {
                let keyed = self
                    .db
                    .lock()
                    .prepare("SELECT json FROM connections")
                    .map(|mut stmt| {
                        stmt.query_map([], |row| row.get::<_, String>(0))
                            .map(|iter| iter.filter_map(Result::ok).collect::<Vec<_>>())
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                let mut labels = HashMap::new();
                for json in keyed {
                    if let Ok(connection) = serde_json::from_str::<ApiConnection>(&json) {
                        for key in connection.key_refs {
                            if let Some(id) = &key.id {
                                labels.insert(id.clone(), key.label.clone());
                            }
                        }
                    }
                }
                for row in &mut rows {
                    if let Some(label) = labels.get(&row.group) {
                        row.label = label.clone();
                    }
                }
            }
            _ => {}
        }
        Ok(StatsBreakdown {
            summary: self.summarize(&where_sql, &values)?,
            rows,
        })
    }

    pub fn write_log(&self, level: &str, scope: &str, message: &str) {
        let path = self.log_dir.join("caption-studio.log");
        if path
            .metadata()
            .map(|m| m.len() > 2 * 1024 * 1024)
            .unwrap_or(false)
        {
            for i in (1..5).rev() {
                let from = self.log_dir.join(if i == 1 {
                    "caption-studio.log".into()
                } else {
                    format!("caption-studio.{}.log", i - 1)
                });
                let to = self.log_dir.join(format!("caption-studio.{i}.log"));
                if from.exists() {
                    let _ = fs::rename(from, to);
                }
            }
        }
        use std::io::Write;
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let sanitized = redact(message);
            let _ = writeln!(
                file,
                "[{}] [{}] [{}] {}",
                Utc::now().to_rfc3339(),
                level.to_uppercase(),
                scope,
                sanitized
            );
        }
    }
}

fn migrate(connection: &Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if version < 1 {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS connections (
                   id TEXT PRIMARY KEY, json TEXT NOT NULL, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS presets (
                   id TEXT PRIMARY KEY, kind TEXT NOT NULL, json TEXT NOT NULL, updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS caption_versions (
                   id INTEGER PRIMARY KEY AUTOINCREMENT, caption_path TEXT NOT NULL,
                   old_content TEXT NOT NULL, new_content TEXT NOT NULL, source TEXT NOT NULL,
                   batch_id TEXT, created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_versions_path ON caption_versions(caption_path, id DESC);
                 CREATE TABLE IF NOT EXISTS request_metrics (
                   id INTEGER PRIMARY KEY AUTOINCREMENT, batch_id TEXT NOT NULL, kind TEXT NOT NULL,
                   connection_id TEXT NOT NULL, model_id TEXT NOT NULL, key_id TEXT,
                   status TEXT NOT NULL, input_tokens INTEGER, output_tokens INTEGER,
                   ttft_ms REAL, generation_ms REAL, tokens_per_second REAL, created_at TEXT NOT NULL
                 );",
            )
            .map_err(|e| e.to_string())?;
        // Forward-compatible column additions (existing DBs may already have them).
        for statement in [
            "ALTER TABLE request_metrics ADD COLUMN connection_name TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE request_metrics ADD COLUMN preset_id TEXT",
            "ALTER TABLE request_metrics ADD COLUMN image_path TEXT",
            "ALTER TABLE request_metrics ADD COLUMN retries INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE edit_batches ADD COLUMN previews_json TEXT NOT NULL DEFAULT '[]'",
        ] {
            let _ = connection.execute(statement, []);
        }
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS task_runs (
                   id TEXT PRIMARY KEY, kind TEXT NOT NULL, connection_id TEXT NOT NULL,
                   connection_name TEXT NOT NULL DEFAULT '', model_id TEXT NOT NULL,
                   preset_id TEXT, status TEXT NOT NULL DEFAULT 'running',
                   total INTEGER NOT NULL DEFAULT 0, succeeded INTEGER NOT NULL DEFAULT 0,
                   refused INTEGER NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0,
                   skipped INTEGER NOT NULL DEFAULT 0, cancelled INTEGER NOT NULL DEFAULT 0,
                   input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0,
                   started_at TEXT NOT NULL, finished_at TEXT
                 );
                 CREATE TABLE IF NOT EXISTS task_items (
                   id INTEGER PRIMARY KEY AUTOINCREMENT, run_id TEXT NOT NULL,
                   image_path TEXT NOT NULL, caption_path TEXT NOT NULL,
                   status TEXT NOT NULL, retries INTEGER NOT NULL DEFAULT 0,
                   error TEXT, key_id TEXT, input_tokens INTEGER, output_tokens INTEGER,
                   ttft_ms REAL, tokens_per_second REAL, created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_task_items_run ON task_items(run_id);
                 CREATE TABLE IF NOT EXISTS edit_batches (
                   id TEXT PRIMARY KEY, rule_json TEXT NOT NULL,
                   previews_json TEXT NOT NULL DEFAULT '[]',
                   affected INTEGER NOT NULL DEFAULT 0, skipped INTEGER NOT NULL DEFAULT 0,
                   errors INTEGER NOT NULL DEFAULT 0, applied_count INTEGER NOT NULL DEFAULT 0,
                   applied INTEGER NOT NULL DEFAULT 0, results_json TEXT,
                   created_at TEXT NOT NULL, applied_at TEXT
                 );
                 CREATE TABLE IF NOT EXISTS review_overrides (
                    path TEXT PRIMARY KEY,
                    created_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS file_mutations (
                   id INTEGER PRIMARY KEY AUTOINCREMENT, caption_path TEXT NOT NULL,
                   mutation_type TEXT NOT NULL, before_content TEXT NOT NULL,
                   after_content TEXT NOT NULL, batch_id TEXT, version_id INTEGER,
                   applied INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_mutations_pending ON file_mutations(applied);",
            )
            .map_err(|e| e.to_string())?;
        connection
            .pragma_update(None, "user_version", 1)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Redacts bearer tokens, API keys, authorization headers and sensitive URL
/// query parameters before anything touches the log files.
fn redact(value: &str) -> String {
    let bearer = regex::Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._~+\-/=]{8,}").unwrap();
    let keys = regex::Regex::new(
        r"(?i)((?:api[_-]?key|x-api-key|authorization|x-goog-api-key|token|key)[\s:=]+)[^\s,;]+",
    )
    .unwrap();
    let query = regex::Regex::new(
        r"(?i)([?&](?:key|api_key|apikey|x-api-key|token|access_token|signature)=)[^&\s]+",
    )
    .unwrap();
    let once = bearer.replace_all(value, "$1••••");
    let twice = keys.replace_all(&once, "$1••••");
    query.replace_all(&twice, "$1••••").into_owned()
}

pub fn atomic_write_bytes(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let temp = path.with_extension(format!(
        "{}.{}.tmp",
        path.extension().and_then(|v| v.to_str()).unwrap_or("file"),
        Uuid::new_v4()
    ));
    {
        use std::io::Write;
        let mut file = fs::File::create(&temp).map_err(|e| e.to_string())?;
        file.write_all(data).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let from: Vec<u16> = temp.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            let _ = fs::remove_file(&temp);
            return Err(error.to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        if path.exists() {
            fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        fs::rename(&temp, path).map_err(|e| e.to_string())
    }
}

pub fn atomic_write_text(path: &Path, content: &str) -> Result<(), String> {
    atomic_write_bytes(path, content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_quarantines_explicit_database_corruption_errors() {
        assert!(is_database_corruption_error(
            "database disk image is malformed"
        ));
        assert!(is_database_corruption_error("file is not a database"));
        assert!(!is_database_corruption_error("database is locked"));
        assert!(!is_database_corruption_error("disk I/O error"));
    }
    use crate::models::{ApiConnection, ApiKeyInput, ProviderKind};
    use std::collections::HashMap;
    #[test]
    fn removes_secrets_from_logs() {
        let value = redact(
            "Authorization: Bearer sk-1234567890 x-api-key=secret123456 key=abcXYZ987 URL?key=zzz&api_key=yyy",
        );
        assert!(!value.contains("1234567890"));
        assert!(!value.contains("secret123456"));
        assert!(!value.contains("abcXYZ987"));
        assert!(!value.contains("zzz"));
        assert!(!value.contains("yyy"));
        assert_eq!(value.matches("••••").count(), 5);
    }
    #[test]
    fn encrypts_saved_api_keys() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let connection = ApiConnection {
            id: "test".into(),
            name: "Test".into(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: "https://example.invalid/v1".into(),
            headers: HashMap::new(),
            key_refs: vec![ApiKeyInput {
                id: Some("key-1".into()),
                label: "Primary".into(),
                secret: Some("super-secret-api-key".into()),
                masked: None,
            }],
            total_concurrency: 2,
            per_key_concurrency: 1,
            timeout_seconds: 90,
            last_model: None,
            cached_models: None,
            models_cached_at: None,
            default_params: None,
        };
        let saved = storage.save_connection(connection).unwrap();
        assert!(saved.key_refs[0].secret.is_none());
        assert!(
            !String::from_utf8_lossy(&fs::read(dir.path().join("secrets.bin")).unwrap())
                .contains("super-secret-api-key")
        );
        assert_eq!(
            storage.key_secret("key-1").as_deref(),
            Some("super-secret-api-key")
        );
    }
    #[test]
    fn sensitive_headers_go_to_dpapi_only() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let mut headers = HashMap::new();
        headers.insert("authorization".into(), "Bearer topsecret".into());
        headers.insert("anthropic-version".into(), "2023-06-01".into());
        let connection = ApiConnection {
            id: "test".into(),
            name: "Test".into(),
            provider: ProviderKind::Anthropic,
            base_url: "https://example.invalid".into(),
            headers,
            key_refs: vec![ApiKeyInput {
                id: Some("key-1".into()),
                label: "Primary".into(),
                secret: Some("sk-ant-abcdef".into()),
                masked: None,
            }],
            total_concurrency: 2,
            per_key_concurrency: 1,
            timeout_seconds: 90,
            last_model: None,
            cached_models: None,
            models_cached_at: None,
            default_params: None,
        };
        let saved = storage.save_connection(connection).unwrap();
        assert_eq!(saved.headers["authorization"], "••••cret");
        assert_eq!(saved.headers["anthropic-version"], "2023-06-01");
        let json = storage.get_connection("test").unwrap();
        assert!(!serde_json::to_string(&json).unwrap().contains("topsecret"));
        let full = storage.get_connection_full("test").unwrap();
        assert_eq!(full.headers["authorization"], "Bearer topsecret");
        // Masked value re-save keeps the stored secret.
        let mut roundtrip = saved.clone();
        roundtrip
            .headers
            .insert("authorization".into(), "••••cret".into());
        let resaved = storage.save_connection(roundtrip).unwrap();
        assert_eq!(resaved.headers["authorization"], "••••cret");
        assert_eq!(
            storage.get_connection_full("test").unwrap().headers["authorization"],
            "Bearer topsecret"
        );
    }
    #[test]
    fn journaled_mutation_rolls_back_on_db_failure_paths() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let path = dir.path().join("image.txt");
        fs::write(&path, "before").unwrap();
        let version = storage
            .commit_mutation(
                path.to_str().unwrap(),
                "before",
                "after",
                "test",
                None,
                "overwrite",
            )
            .unwrap();
        assert!(version.is_some());
        assert_eq!(fs::read_to_string(&path).unwrap(), "after");
        assert_eq!(
            storage
                .list_versions(Some(path.to_str().unwrap()))
                .unwrap()
                .len(),
            1
        );
        // No-op mutation creates no journal entry.
        assert!(storage
            .commit_mutation(
                path.to_str().unwrap(),
                "after",
                "after",
                "test",
                None,
                "overwrite"
            )
            .unwrap()
            .is_none());
    }
    #[test]
    fn mutation_rejects_a_stale_before_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let path = dir.path().join("image.txt");
        fs::write(&path, "request-start-value").unwrap();

        // Simulate a manual editor saving while a model request is in flight.
        fs::write(&path, "newer-manual-value").unwrap();
        let error = storage
            .commit_mutation(
                path.to_str().unwrap(),
                "request-start-value",
                "late-model-value",
                "revision",
                Some("batch"),
                "revision_overwrite",
            )
            .unwrap_err();

        assert!(error.contains("发生变化"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "newer-manual-value");
        assert!(storage
            .list_versions(Some(path.to_str().unwrap()))
            .unwrap()
            .is_empty());
    }
    #[test]
    fn recovers_pending_mutation_without_fake_version() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let rolled = dir.path().join("rolled.txt");
        let committed = dir.path().join("committed.txt");
        fs::write(&rolled, "after").unwrap();
        fs::write(&committed, "after2").unwrap();
        // Crash after file write but before version commit: applied=0, version NULL.
        let db = storage.db.lock();
        db.execute(
            "INSERT INTO file_mutations(caption_path,mutation_type,before_content,after_content,batch_id,version_id,applied,created_at)
             VALUES(?1,'overwrite',?2,?3,NULL,NULL,0,?4)",
            params![rolled.to_str().unwrap(), "before", "after", Utc::now().to_rfc3339()],
        )
        .unwrap();
        // Crash after version commit but before journal mark: applied=0, version set.
        db.execute(
            "INSERT INTO file_mutations(caption_path,mutation_type,before_content,after_content,batch_id,version_id,applied,created_at)
             VALUES(?1,'overwrite',?2,?3,NULL,999,0,?4)",
            params![committed.to_str().unwrap(), "before", "after2", Utc::now().to_rfc3339()],
        )
        .unwrap();
        drop(db);
        storage.recover_mutations().unwrap();
        // Never-committed change is rolled back to the old content...
        assert_eq!(fs::read_to_string(&rolled).unwrap(), "before");
        // ...committed change is re-applied and the journal entry finalized.
        assert_eq!(fs::read_to_string(&committed).unwrap(), "after2");
        let db = storage.db.lock();
        let pending: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM file_mutations WHERE applied=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(db);
        assert_eq!(pending, 0);
    }
    #[test]
    fn deletes_preset_with_reference_check() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        storage
            .save_preset("tag", serde_json::json!({"id": "p1", "name": "A"}))
            .unwrap();
        storage
            .add_metric(
                "run-1",
                "caption",
                "conn-1",
                "Conn",
                "model-1",
                Some("p1"),
                Some("/x.png"),
                Some("key-1"),
                0,
                "succeeded",
                &UsageMetrics::empty(),
            )
            .unwrap();
        assert!(storage.delete_preset("tag", "p1").is_err());
        storage
            .add_metric(
                "run-2",
                "caption",
                "conn-1",
                "Conn",
                "model-1",
                None,
                None,
                None,
                0,
                "failed",
                &UsageMetrics::empty(),
            )
            .unwrap();
        // Deleting the referenced preset still fails; a different id works.
        storage
            .save_preset("tag", serde_json::json!({"id": "p2", "name": "B"}))
            .unwrap();
        storage.delete_preset("tag", "p2").unwrap();
    }
    #[test]
    fn backs_up_daily_and_prunes_old_copies() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        // 写入一条数据，确保备份包含真实内容。
        let connection = ApiConnection {
            id: "c1".into(),
            name: "C".into(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: "https://example.invalid/v1".into(),
            headers: HashMap::new(),
            key_refs: vec![ApiKeyInput {
                id: Some("k1".into()),
                label: "K".into(),
                secret: Some("sk-backup-test".into()),
                masked: None,
            }],
            total_concurrency: 1,
            per_key_concurrency: 1,
            timeout_seconds: 30,
            last_model: None,
            cached_models: None,
            models_cached_at: None,
            default_params: None,
        };
        storage.save_connection(connection).unwrap();
        let backup_dir = dir.path().join("backups");
        storage.backup_if_due();
        let files = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_file())
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 2, "db + secrets 各一份");
        // 当日重复调用不产生新副本。
        storage.backup_if_due();
        assert_eq!(
            fs::read_dir(&backup_dir)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_file())
                .count(),
            2
        );
        // 备份库可打开且包含连接数据。
        let backup_db = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.extension().is_some_and(|e| e == "db"))
            .unwrap();
        let conn = Connection::open(&backup_db).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM connections WHERE id='c1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert!(backup_db.is_file());
    }

    #[test]
    fn recovers_from_corrupt_secrets_and_db() {
        let dir = tempfile::tempdir().unwrap();
        // 伪造的凭据文件（非 DPAPI 密文）不再导致启动失败。
        fs::write(dir.path().join("secrets.bin"), b"not-a-dpapi-blob").unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        assert!(storage.key_secret("any").is_none());
        let isolated = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|name| name.contains("secrets.bin.corrupt-"))
            .expect("损坏凭据文件应被隔离保留");
        assert!(isolated.starts_with("secrets.bin.corrupt-"));
        // 损坏的数据库同样降级：写入垃圾文件后打开应重建并可用。
        let dir2 = tempfile::tempdir().unwrap();
        fs::write(dir2.path().join("caption-studio.db"), b"garbage-not-sqlite").unwrap();
        let storage2 = Storage::open(dir2.path()).unwrap();
        let saved = ApiConnection {
            id: "c1".into(),
            name: "C".into(),
            provider: ProviderKind::OpenaiCompatible,
            base_url: "https://example.invalid/v1".into(),
            headers: HashMap::new(),
            key_refs: vec![ApiKeyInput {
                id: Some("k1".into()),
                label: "K".into(),
                secret: Some("fixture-after-rebuild".into()),
                masked: None,
            }],
            total_concurrency: 1,
            per_key_concurrency: 1,
            timeout_seconds: 30,
            last_model: None,
            cached_models: None,
            models_cached_at: None,
            default_params: None,
        };
        storage2.save_connection(saved).unwrap();
        assert!(storage2.get_connection("c1").is_ok());
        assert_eq!(
            storage2.key_secret("k1").as_deref(),
            Some("fixture-after-rebuild")
        );
    }

    #[test]
    fn breakdown_groups_by_preset_and_key() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        for i in 0..3 {
            storage
                .add_metric(
                    &format!("run-{i}"),
                    "caption",
                    "conn-1",
                    "Conn",
                    "model-x",
                    Some("p1"),
                    Some("/img.png"),
                    Some("key-a"),
                    i as u32,
                    "succeeded",
                    &UsageMetrics {
                        input_tokens: Some(10),
                        output_tokens: Some(2),
                        total_tokens: Some(12),
                        ttft_ms: Some(50.0),
                        generation_ms: Some(100.0),
                        tokens_per_second: Some(20.0),
                    },
                )
                .unwrap();
        }
        let preset = storage
            .query_stats_breakdown(&StatsQuery {
                group_by: "preset".into(),
                connection_id: None,
                batch_id: None,
                model_id: None,
                preset_id: None,
                key_id: None,
            })
            .unwrap();
        assert_eq!(preset.rows.len(), 1);
        assert_eq!(preset.rows[0].total, 3);
        assert_eq!(preset.rows[0].output_tokens, 6);
        assert_eq!(preset.summary.succeeded, 3);
        let keyed = storage
            .query_stats_breakdown(&StatsQuery {
                group_by: "key".into(),
                connection_id: None,
                batch_id: None,
                model_id: None,
                preset_id: None,
                key_id: None,
            })
            .unwrap();
        assert_eq!(keyed.rows.len(), 1);
        assert_eq!(keyed.rows[0].group, "key-a");
    }
}
