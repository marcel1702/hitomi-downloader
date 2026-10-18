//! The login every part of the web version sits behind
//!
//! A login method ([`AuthMethod`]) checks who someone is, by password today and by OpenID
//! Connect later (#31), and hands the [`Identity`] to [`Auth::start_session`]. The session,
//! its cookie and the check in front of every other route do not care which method it was,
//! so a new method only adds its routes below `/auth`.
mod password;
mod sessions;
mod throttle;

use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::bail;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Json, Router,
};
use hitomi_core::errors::CommandError;
use serde::{Deserialize, Serialize};

pub use password::{check_password_hash, hash_password, PasswordMethod};
use sessions::Sessions;
pub use throttle::{Policy, Throttle};

use crate::error::ApiError;

/// Who is logged in, and how
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub user: String,
    /// The id of the method that logged them in, `password` for now
    pub method: String,
}

/// How the login page offers a method, listed by `GET /auth/methods`
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MethodDescriptor {
    /// A form that posts user name and password to `/auth/login`
    Password { id: String },
    /// A button that sends the browser to `url`, an identity provider for OpenID Connect
    #[allow(dead_code)]
    Redirect {
        id: String,
        label: String,
        url: String,
    },
}

pub trait AuthMethod: Send + Sync + 'static {
    fn descriptor(&self) -> MethodDescriptor;

    /// The routes of the method, below `/auth`, which call [`Auth::start_session`] once
    /// someone logged in
    fn router(self: Arc<Self>) -> Router<Auth>;
}

/// The login settings from the environment
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthSettings {
    /// `HITOMI_USERNAME`
    pub username: Option<String>,
    /// `HITOMI_PASSWORD_HASH` or `HITOMI_PASSWORD`
    pub password: Option<PasswordSetting>,
    /// `HITOMI_SESSION_DAYS`
    pub session_days: u64,
    /// `HITOMI_PUBLIC_URL`, where the server is reached, behind a reverse proxy above all
    pub public_url: Option<String>,
    /// `HITOMI_TRUST_FORWARDED_FOR`: take the client address from `X-Forwarded-For`
    pub trust_forwarded_for: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum PasswordSetting {
    Hash(String),
    Plain(String),
}

impl std::fmt::Debug for PasswordSetting {
    // never print a password, not even into a debug log
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PasswordSetting::Hash(_) => f.write_str("Hash(…)"),
            PasswordSetting::Plain(_) => f.write_str("Plain(…)"),
        }
    }
}

pub const DEFAULT_SESSION_DAYS: u64 = 30;
const SESSION_COOKIE: &str = "hitomi_session";
const LOGIN_PAGE: &str = include_str!("login.html");

#[derive(Clone)]
pub struct Auth {
    inner: Arc<AuthInner>,
}

struct AuthInner {
    sessions: Sessions,
    methods: Vec<Arc<dyn AuthMethod>>,
    /// Whether the cookie may only travel over https
    secure_cookie: bool,
    trust_forwarded_for: bool,
}

impl Auth {
    /// The login methods `settings` configure, with the sessions kept in `config_dir`
    ///
    /// Fails when no method is configured: the web version never runs without a login.
    pub fn new(settings: &AuthSettings, config_dir: &Path, policy: Policy) -> anyhow::Result<Auth> {
        let mut methods: Vec<Arc<dyn AuthMethod>> = Vec::new();
        match (&settings.username, &settings.password) {
            (Some(username), Some(password)) => {
                let password_hash = match password {
                    PasswordSetting::Hash(hash) => {
                        check_password_hash(hash)?;
                        hash.clone()
                    }
                    PasswordSetting::Plain(plain) => {
                        tracing::warn!(
                            "HITOMI_PASSWORD holds the password in plain text, HITOMI_PASSWORD_HASH is safer: `hitomi-server hash-password` creates the hash"
                        );
                        hash_password(plain)?
                    }
                };
                methods.push(Arc::new(PasswordMethod::new(
                    username.clone(),
                    password_hash,
                    Throttle::new(policy),
                )));
            }
            (Some(_), None) => bail!(
                "HITOMI_USERNAME is set, but neither HITOMI_PASSWORD_HASH nor HITOMI_PASSWORD"
            ),
            (None, Some(_)) => bail!("a password is set, but no HITOMI_USERNAME"),
            (None, None) => {}
        }
        if methods.is_empty() {
            bail!(
                "No login is configured, and the web version never runs without one. Set HITOMI_USERNAME and HITOMI_PASSWORD_HASH (create the hash with `hitomi-server hash-password`), or HITOMI_PASSWORD"
            );
        }

        let secure_cookie = settings
            .public_url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://"));
        Auth::with_methods(
            methods,
            config_dir,
            Duration::from_secs(settings.session_days * 24 * 60 * 60),
            secure_cookie,
            settings.trust_forwarded_for,
        )
    }

    pub fn with_methods(
        methods: Vec<Arc<dyn AuthMethod>>,
        config_dir: &Path,
        session_lifetime: Duration,
        secure_cookie: bool,
        trust_forwarded_for: bool,
    ) -> anyhow::Result<Auth> {
        let sessions = Sessions::load(config_dir.join("sessions.json"), session_lifetime)?;
        Ok(Auth {
            inner: Arc::new(AuthInner {
                sessions,
                methods,
                secure_cookie,
                trust_forwarded_for,
            }),
        })
    }

    pub fn trust_forwarded_for(&self) -> bool {
        self.inner.trust_forwarded_for
    }

    /// Start a session for `identity`, returning the `Set-Cookie` value that carries it
    pub fn start_session(&self, identity: &Identity) -> anyhow::Result<HeaderValue> {
        let token = self.inner.sessions.start(identity.clone())?;
        tracing::info!(user = identity.user, method = identity.method, "Logged in");
        Ok(self.cookie(&token, self.inner.sessions.lifetime().as_secs()))
    }

    /// Who the request belongs to, if it carries a valid session
    pub fn identity(&self, headers: &HeaderMap) -> Option<Identity> {
        session_token(headers).and_then(|token| self.inner.sessions.get(token))
    }

    fn cookie(&self, value: &str, max_age_secs: u64) -> HeaderValue {
        // `Lax` and not `Strict`: an identity provider sends the browser back from another
        // site, and `Strict` would leave the cookie off the first page after that. Every
        // request that changes something is a JSON `POST`, which `Lax` keeps from other sites.
        let secure = if self.inner.secure_cookie {
            "; Secure"
        } else {
            ""
        };
        HeaderValue::from_str(&format!(
            "{SESSION_COOKIE}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}{secure}"
        ))
        .expect("the token is hex, the rest is fixed")
    }

    /// `/auth`: the login page, the session, and the routes of every login method
    pub fn router(&self) -> Router {
        let methods = self
            .inner
            .methods
            .iter()
            .fold(Router::new(), |router, method| {
                router.merge(method.clone().router())
            });
        Router::new()
            .route("/login", get(login_page))
            .route("/logout", post(logout))
            .route("/session", get(session))
            .route("/methods", get(methods_list))
            .merge(methods)
            .with_state(self.clone())
    }
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .find_map(|cookie| {
            cookie
                .trim()
                .strip_prefix(SESSION_COOKIE)?
                .strip_prefix('=')
        })
}

/// The address of the client, from `X-Forwarded-For` only if the server is told to trust it
///
/// The last address in the header is the one the reverse proxy in front of the server saw,
/// those before it come from the client and prove nothing.
pub fn client_ip(peer: SocketAddr, headers: &HeaderMap, trust_forwarded_for: bool) -> IpAddr {
    if trust_forwarded_for {
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit(',').next())
            .and_then(|last| last.trim().parse().ok());
        if let Some(client) = forwarded {
            return client;
        }
    }
    peer.ip()
}

async fn login_page(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    if auth.identity(&headers).is_some() {
        return Redirect::to("/").into_response();
    }
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
        ],
        Html(LOGIN_PAGE),
    )
        .into_response()
}

async fn logout(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    if let Some(token) = session_token(&headers) {
        if let Err(err) = auth.inner.sessions.end(token) {
            return ApiError::with_status(StatusCode::INTERNAL_SERVER_ERROR, "logout failed", err)
                .into_response();
        }
    }
    ([(header::SET_COOKIE, auth.cookie("", 0))], Json(())).into_response()
}

async fn session(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    match auth.identity(&headers) {
        Some(identity) => Json(identity).into_response(),
        None => not_logged_in(),
    }
}

async fn methods_list(State(auth): State<Auth>) -> Json<Vec<MethodDescriptor>> {
    Json(
        auth.inner
            .methods
            .iter()
            .map(|method| method.descriptor())
            .collect(),
    )
}

/// Let a request through only with a valid session, except for the health check
///
/// Without one, the API answers `401`, and a page of the frontend sends the browser to the
/// login page, which brings it back afterwards.
pub async fn require_session(
    State(auth): State<Auth>,
    mut request: Request,
    next: Next,
) -> Response {
    if request.uri().path() == "/api/health" {
        return next.run(request).await;
    }
    match auth.identity(request.headers()) {
        Some(identity) => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        None if is_api(request.uri().path()) => not_logged_in(),
        None => {
            let target = request
                .uri()
                .path_and_query()
                .map_or("/", |path_and_query| path_and_query.as_str());
            Redirect::to(&format!("/auth/login?next={}", percent_encode(target))).into_response()
        }
    }
}

fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// Not logged an error: every request a logged out browser still makes would end up in
/// the log otherwise
fn not_logged_in() -> Response {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        CommandError {
            err_title: "not logged in".to_string(),
            err_message: "log in at /auth/login".to_string(),
        },
    )
    .into_response()
}

/// `value` with everything but unreserved characters and `/` percent encoded
fn percent_encode(value: &str) -> String {
    value.bytes().fold(String::new(), |mut encoded, byte| {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
        encoded
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        assert_eq!(
            session_token(&headers(&[(
                "cookie",
                "theme=dark; hitomi_session=abc123; x=y"
            )])),
            Some("abc123")
        );
        assert_eq!(
            session_token(&headers(&[
                ("cookie", "a=b"),
                ("cookie", "hitomi_session=def")
            ])),
            Some("def")
        );
        assert_eq!(
            session_token(&headers(&[("cookie", "hitomi_sessionx=abc")])),
            None
        );
        assert_eq!(session_token(&HeaderMap::new()), None);
    }

    #[test]
    fn forwarded_for_counts_only_when_trusted_and_only_its_last_address() {
        let peer: SocketAddr = "10.0.0.2:51234".parse().unwrap();
        let forwarded = headers(&[("x-forwarded-for", "1.2.3.4, 198.51.100.9")]);
        assert_eq!(
            client_ip(peer, &forwarded, false),
            "10.0.0.2".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(peer, &forwarded, true),
            "198.51.100.9".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(peer, &HeaderMap::new(), true),
            "10.0.0.2".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn the_target_after_login_is_encoded() {
        assert_eq!(percent_encode("/"), "/");
        assert_eq!(
            percent_encode("/reader?id=1&x=ä"),
            "/reader%3Fid%3D1%26x%3D%C3%A4"
        );
    }

    #[test]
    fn startup_needs_a_login() {
        let dir = std::env::temp_dir();
        let settings = |username: Option<&str>, password: Option<PasswordSetting>| AuthSettings {
            username: username.map(str::to_string),
            password,
            session_days: 1,
            ..AuthSettings::default()
        };
        let err = |settings| {
            Auth::new(&settings, &dir, Policy::default())
                .err()
                .unwrap()
                .to_string()
        };

        assert!(err(settings(None, None)).contains("never runs without"));
        assert!(err(settings(Some("marcel"), None)).contains("HITOMI_PASSWORD"));
        assert!(
            err(settings(None, Some(PasswordSetting::Plain("x".into()))))
                .contains("HITOMI_USERNAME")
        );
        assert!(err(settings(
            Some("marcel"),
            Some(PasswordSetting::Hash("nonsense".into()))
        ))
        .contains("$$"));
    }

    #[test]
    fn passwords_never_show_in_debug_output() {
        let settings = AuthSettings {
            username: Some("marcel".into()),
            password: Some(PasswordSetting::Plain("hunter2".into())),
            ..AuthSettings::default()
        };
        assert!(!format!("{settings:?}").contains("hunter2"));
    }
}
