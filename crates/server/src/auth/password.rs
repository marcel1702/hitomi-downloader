//! Login with the user name and password from the environment
use std::{net::SocketAddr, sync::Arc, time::Instant};

use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{ConnectInfo, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Extension, Json, Router,
};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use super::{client_ip, throttle::Throttle, Auth, AuthMethod, Identity, MethodDescriptor};
use crate::error::{ApiError, Args};

/// The method id, stored with every session it starts
pub const METHOD: &str = "password";

pub struct PasswordMethod {
    username: String,
    /// Argon2 hash in the PHC string format
    password_hash: String,
    throttle: Throttle,
}

impl PasswordMethod {
    pub fn new(username: String, password_hash: String, throttle: Throttle) -> Self {
        Self {
            username,
            password_hash,
            throttle,
        }
    }

    /// Whether `username` and `password` are the configured ones
    ///
    /// The password is checked even when the user name is wrong, so the time the answer
    /// takes does not tell whether the user name was right.
    fn verify(&self, username: &str, password: &str) -> bool {
        let username_ok: bool = username.as_bytes().ct_eq(self.username.as_bytes()).into();
        let password_ok = PasswordHash::new(&self.password_hash).is_ok_and(|hash| {
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .is_ok()
        });
        username_ok & password_ok
    }
}

impl AuthMethod for PasswordMethod {
    fn descriptor(&self) -> MethodDescriptor {
        MethodDescriptor::Password {
            id: METHOD.to_string(),
        }
    }

    fn router(self: Arc<Self>) -> Router<Auth> {
        Router::new()
            .route("/login", post(login))
            .layer(Extension(self))
    }
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

async fn login(
    State(auth): State<Auth>,
    Extension(method): Extension<Arc<PasswordMethod>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Args(credentials): Args<Credentials>,
) -> Response {
    let client = client_ip(peer, &headers, auth.trust_forwarded_for());
    if let Err(remaining) = method.throttle.check(client, Instant::now()) {
        // rounded up, a client that comes back on time must not be turned away again
        let seconds = u64::try_from(remaining.as_millis().div_ceil(1000)).unwrap_or(u64::MAX);
        let minutes = seconds.div_ceil(60);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, seconds.to_string())],
            Json(error(&format!(
                "too many failed logins, try again in {minutes} minutes"
            ))),
        )
            .into_response();
    }

    // Argon2 takes its time on purpose, which must not hold up the async runtime
    let verifier = method.clone();
    let username = credentials.username.clone();
    let verified = tokio::task::spawn_blocking(move || {
        verifier.verify(&credentials.username, &credentials.password)
    })
    .await
    .unwrap_or(false);

    if !verified {
        let locked = method.throttle.failure(client, Instant::now());
        if locked {
            tracing::warn!(%client, "Failed login, the client is locked out for a while");
        } else {
            tracing::warn!(%client, "Failed login");
        }
        tokio::time::sleep(method.throttle.delay()).await;
        return (
            StatusCode::UNAUTHORIZED,
            Json(error("wrong user name or password")),
        )
            .into_response();
    }

    method.throttle.success(client);
    let identity = Identity {
        user: username,
        method: METHOD.to_string(),
    };
    match auth.start_session(&identity) {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], Json(identity)).into_response(),
        Err(err) => ApiError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "login failed", err)
            .into_response(),
    }
}

/// A login error in the shape of every other error the server answers with
fn error(message: &str) -> hitomi_core::errors::CommandError {
    hitomi_core::errors::CommandError {
        err_title: "login failed".to_string(),
        err_message: message.to_string(),
    }
}

/// An Argon2 hash of `password` in the PHC string format, for `HITOMI_PASSWORD_HASH`
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>())
        .map_err(|err| anyhow::anyhow!("Failed to create a salt: {err}"))?;
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|err| anyhow::anyhow!("Failed to hash the password: {err}"))?;
    Ok(hash.to_string())
}

/// Check that `hash` is an Argon2 hash in the PHC string format
pub fn check_password_hash(hash: &str) -> anyhow::Result<()> {
    let parsed = PasswordHash::new(hash).map_err(|err| {
        anyhow::anyhow!(
            "HITOMI_PASSWORD_HASH is no Argon2 hash ({err}). Create one with `hitomi-server hash-password`, and in a compose file write every `$` of it as `$$`"
        )
    })?;
    if !parsed.algorithm.as_str().starts_with("argon2") {
        anyhow::bail!(
            "HITOMI_PASSWORD_HASH uses `{}`, only Argon2 hashes are supported",
            parsed.algorithm
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::throttle::Policy;

    fn method() -> PasswordMethod {
        PasswordMethod::new(
            "marcel".to_string(),
            hash_password("correct horse").unwrap(),
            Throttle::new(Policy::default()),
        )
    }

    #[test]
    fn only_the_configured_credentials_pass() {
        let method = method();
        assert!(method.verify("marcel", "correct horse"));
        assert!(!method.verify("marcel", "wrong"));
        assert!(!method.verify("marcel", "correct horse "));
        assert!(!method.verify("Marcel", "correct horse"));
        assert!(!method.verify("", ""));
    }

    #[test]
    fn hashes_are_salted_argon2_phc_strings() {
        let first = hash_password("secret").unwrap();
        let second = hash_password("secret").unwrap();
        assert!(first.starts_with("$argon2id$"), "{first}");
        assert_ne!(first, second);
        assert!(check_password_hash(&first).is_ok());
    }

    #[test]
    fn a_broken_hash_is_explained() {
        // what a compose file makes of a hash whose `$` were not doubled
        let err = check_password_hash("argon2id=19m=19456").unwrap_err();
        assert!(err.to_string().contains("$$"), "{err}");
        assert!(check_password_hash("$pbkdf2-sha256$i=1000$c2FsdA$aGFzaA").is_err());
    }
}
