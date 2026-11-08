//! HUD credentials are independent from Magpie API keys. Only SHA-256 checksums
//! of 256-bit random secrets are persisted. Revocation is checked on every request
//! and by open SSE streams; browser sessions never contain the original key.
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;

pub const SESSION_SECONDS: u64 = 43_200;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyRecord {
    pub name: String,
    pub checksum: String,
    pub created_at: String,
}
#[derive(Default, Serialize, Deserialize)]
pub struct KeyDatabase {
    pub keys: BTreeMap<String, KeyRecord>,
}
impl KeyDatabase {
    pub fn load(directory: &Path) -> Result<Self, String> {
        match fs::read(directory.join("keys.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "HUD 密钥文件无效".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err("无法读取 HUD 密钥文件".into()),
        }
    }
    pub fn save(&self, directory: &Path) -> Result<(), String> {
        private_directory(directory)?;
        let tmp = directory.join(format!("keys-{}.tmp", random_secret()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let result = (|| {
            let mut file = options.open(&tmp).map_err(|_| "无法创建 HUD 密钥文件")?;
            let bytes = serde_json::to_vec_pretty(self).map_err(|_| "无法编码 HUD 密钥文件")?;
            file.write_all(&bytes)
                .map_err(|_| "无法写入 HUD 密钥文件")?;
            file.sync_all().map_err(|_| "无法同步 HUD 密钥文件")?;
            fs::rename(&tmp, directory.join("keys.json")).map_err(|_| "无法保存 HUD 密钥文件")
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result.map_err(str::to_string)
    }
    pub fn create(&mut self, name: String) -> (String, String) {
        let id = random_secret()[..16].to_string();
        let token = format!("hud_{id}.{}", random_secret());
        self.keys.insert(
            id.clone(),
            KeyRecord {
                name,
                checksum: checksum(&token),
                created_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        (id, token)
    }
    pub fn verify(&self, token: &str) -> Option<String> {
        if token.len() > 128 {
            return None;
        }
        let id = token.strip_prefix("hud_")?.split_once('.')?.0;
        let record = self.keys.get(id)?;
        bool::from(record.checksum.as_bytes().ct_eq(checksum(token).as_bytes()))
            .then(|| id.to_owned())
    }
}

struct Session {
    key_id: String,
    expires: Instant,
}
pub struct Auth {
    directory: PathBuf,
    sessions: Mutex<BTreeMap<String, Session>>,
    login_window: Mutex<(Instant, u32)>,
}
impl Auth {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            sessions: Mutex::new(BTreeMap::new()),
            login_window: Mutex::new((Instant::now(), 0)),
        }
    }
    pub fn verify_key(&self, token: &str) -> Option<String> {
        KeyDatabase::load(&self.directory).ok()?.verify(token)
    }
    pub fn active_key(&self, id: &str) -> bool {
        KeyDatabase::load(&self.directory).is_ok_and(|db| db.keys.contains_key(id))
    }
    pub fn allow_login(&self) -> bool {
        let mut window = self.login_window.lock().unwrap_or_else(|e| e.into_inner());
        if window.0.elapsed() >= Duration::from_secs(60) {
            *window = (Instant::now(), 0);
        }
        window.1 += 1;
        window.1 <= 30
    }
    pub fn login(&self, key: &str) -> Option<String> {
        let key_id = self.verify_key(key)?;
        let token = random_secret();
        let mut sessions = self.sessions.lock().ok()?;
        sessions.retain(|_, session| session.expires > Instant::now());
        if sessions.len() >= 512 {
            return None;
        }
        sessions.insert(
            checksum(&token),
            Session {
                key_id,
                expires: Instant::now() + Duration::from_secs(SESSION_SECONDS),
            },
        );
        Some(token)
    }
    pub fn session_key(&self, token: &str) -> Option<String> {
        if token.len() != 64 {
            return None;
        }
        let key_id = {
            let mut sessions = self.sessions.lock().ok()?;
            sessions.retain(|_, s| s.expires > Instant::now());
            sessions.get(&checksum(token))?.key_id.clone()
        };
        self.active_key(&key_id).then_some(key_id)
    }
    pub fn logout(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.remove(&checksum(token));
        }
    }
}

pub fn private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|_| "无法创建 HUD 数据目录".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "无法设置 HUD 数据目录权限".to_string())?;
    }
    Ok(())
}
pub fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn checksum(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_are_hashed_and_revocation_invalidates_browser_sessions() {
        let directory = std::env::temp_dir().join(format!("hud-auth-{}", random_secret()));
        let mut db = KeyDatabase::default();
        let (id, key) = db.create("test".into());
        db.save(&directory).unwrap();
        assert!(!fs::read_to_string(directory.join("keys.json"))
            .unwrap()
            .contains(&key));
        let auth = Auth::new(directory.clone());
        assert!(auth.verify_key(&format!("{key}x")).is_none());
        let cookie = auth.login(&key).unwrap();
        assert_eq!(auth.session_key(&cookie), Some(id.clone()));
        db.keys.remove(&id);
        db.save(&directory).unwrap();
        assert!(auth.session_key(&cookie).is_none());
        assert!(auth.verify_key(&key).is_none());
        fs::remove_dir_all(directory).unwrap();
    }
}
