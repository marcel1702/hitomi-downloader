//! Who is logged in, kept in the config volume so a restart logs nobody out
//!
//! The cookie holds a random token. Only its SHA-256 hash is stored, so the file alone
//! lets no one log in, and ending a session in the store ends it for good, which a signed
//! cookie could not do.
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context as _;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Identity;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredSession {
    identity: Identity,
    /// Seconds since the Unix epoch
    expires_at: u64,
}

pub struct Sessions {
    file: PathBuf,
    lifetime: Duration,
    /// By the hex encoded SHA-256 of their token
    sessions: Mutex<HashMap<String, StoredSession>>,
}

impl Sessions {
    /// The sessions stored in `file`, which does not have to exist yet
    pub fn load(file: PathBuf, lifetime: Duration) -> anyhow::Result<Sessions> {
        let mut sessions: HashMap<String, StoredSession> = match std::fs::read(&file) {
            Ok(content) => serde_json::from_slice(&content)
                .with_context(|| format!("Failed to read the sessions in `{}`", file.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(err) => {
                return Err(err).context(format!("Failed to read `{}`", file.display()));
            }
        };
        let now = unix_now();
        sessions.retain(|_, session| session.expires_at > now);

        Ok(Sessions {
            file,
            lifetime,
            sessions: Mutex::new(sessions),
        })
    }

    pub fn lifetime(&self) -> Duration {
        self.lifetime
    }

    /// Start a session for `identity`, returning the token for the cookie
    pub fn start(&self, identity: Identity) -> anyhow::Result<String> {
        let token = hex(&rand::random::<[u8; 32]>());
        let session = StoredSession {
            identity,
            expires_at: unix_now() + self.lifetime.as_secs(),
        };
        let mut sessions = self.sessions.lock();
        let now = unix_now();
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(token_hash(&token), session);
        self.save(&sessions)?;
        Ok(token)
    }

    /// Who the session of `token` belongs to, while it has not expired
    pub fn get(&self, token: &str) -> Option<Identity> {
        let sessions = self.sessions.lock();
        let session = sessions.get(&token_hash(token))?;
        (session.expires_at > unix_now()).then(|| session.identity.clone())
    }

    /// End the session of `token`
    pub fn end(&self, token: &str) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock();
        if sessions.remove(&token_hash(token)).is_some() {
            self.save(&sessions)?;
        }
        Ok(())
    }

    /// Write the sessions to a temporary file and move it over the old one, readable by the
    /// owner only
    ///
    /// The file is created that way, set afterwards it would be readable by everyone for a
    /// moment. A file left from before is removed first: opening it would keep its mode.
    fn save(&self, sessions: &HashMap<String, StoredSession>) -> anyhow::Result<()> {
        use std::io::Write as _;

        let temp = self.file.with_extension("json.tmp");
        match std::fs::remove_file(&temp) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                return Err(err).context(format!("Failed to remove `{}`", temp.display()));
            }
            _ => {}
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&temp)
            .and_then(|mut out| {
                out.write_all(&serde_json::to_vec_pretty(sessions)?)?;
                out.sync_all()
            })
            .with_context(|| format!("Failed to write `{}`", temp.display()))?;
        std::fs::rename(&temp, &self.file)
            .with_context(|| format!("Failed to write `{}`", self.file.display()))?;
        Ok(())
    }

    #[cfg(test)]
    fn expire_all(&self) {
        for session in self.sessions.lock().values_mut() {
            session.expires_at = 0;
        }
    }
}

fn token_hash(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file() -> PathBuf {
        std::env::temp_dir().join(format!("hitomi-sessions-{}.json", rand::random::<u64>()))
    }

    fn identity() -> Identity {
        Identity {
            user: "marcel".to_string(),
            method: "password".to_string(),
        }
    }

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    #[test]
    fn a_session_survives_a_restart_and_the_file_holds_no_token() {
        let file = file();
        let token = Sessions::load(file.clone(), DAY)
            .unwrap()
            .start(identity())
            .unwrap();

        let reloaded = Sessions::load(file.clone(), DAY).unwrap();
        assert_eq!(reloaded.get(&token), Some(identity()));
        let stored = std::fs::read_to_string(&file).unwrap();
        assert!(!stored.contains(&token));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_file(file).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_session_file_is_never_readable_by_others() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let file = file();
        let temp = file.with_extension("json.tmp");
        // left from an older version, readable by everyone
        std::fs::write(&temp, b"{}").unwrap();
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (file, temp, done) = (file.clone(), temp.clone(), done.clone());
            std::thread::spawn(move || {
                let mut modes = Vec::new();
                while !done.load(Ordering::SeqCst) {
                    for path in [&file, &temp] {
                        if let Ok(metadata) = std::fs::metadata(path) {
                            modes.push(metadata.permissions().mode() & 0o777);
                        }
                    }
                }
                modes
            })
        };
        let sessions = Sessions::load(file.clone(), DAY).unwrap();
        for _ in 0..200 {
            sessions.start(identity()).unwrap();
        }
        done.store(true, Ordering::SeqCst);
        let modes = watcher.join().unwrap();

        // the leftover was seen before the first save, every file after it is the owner's
        let after_leftover: Vec<_> = modes.iter().skip_while(|mode| **mode == 0o644).collect();
        assert!(!after_leftover.is_empty());
        assert!(
            after_leftover.iter().all(|mode| **mode == 0o600),
            "{:?}",
            modes
                .iter()
                .filter(|mode| **mode != 0o600)
                .collect::<Vec<_>>()
        );
        assert!(!temp.exists());
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn an_ended_session_is_gone_after_a_restart_too() {
        let file = file();
        let sessions = Sessions::load(file.clone(), DAY).unwrap();
        let token = sessions.start(identity()).unwrap();
        sessions.end(&token).unwrap();
        assert_eq!(sessions.get(&token), None);
        assert_eq!(Sessions::load(file.clone(), DAY).unwrap().get(&token), None);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn an_expired_or_unknown_session_is_no_session() {
        let file = file();
        let sessions = Sessions::load(file.clone(), DAY).unwrap();
        let token = sessions.start(identity()).unwrap();
        assert_eq!(sessions.get("not a token"), None);
        sessions.expire_all();
        assert_eq!(sessions.get(&token), None);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn tokens_are_random() {
        let file = file();
        let sessions = Sessions::load(file.clone(), DAY).unwrap();
        let first = sessions.start(identity()).unwrap();
        let second = sessions.start(identity()).unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
        std::fs::remove_file(file).unwrap();
    }
}
