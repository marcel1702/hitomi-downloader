//! `POST /api/<command>` for the followed searches, and what checks them
//!
//! The commands only look at and change the subscriptions, a check runs in the background:
//! asking for one answers at once, and the list shows when it is done. A search or a request
//! to Hitomi is never made for the request that asks for the list.
use std::{sync::Arc, time::Duration};

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use hitomi_core::{
    errors::CommandError,
    subscriptions::{
        self, FirstRun, Preview, SubscriptionError, SubscriptionList, SubscriptionView,
        Subscriptions,
    },
};
use serde::Deserialize;
use tokio::sync::watch;

use crate::{
    error::{ApiError, ApiResult, Args},
    state::AppState,
};

/// How often the checks look whether a subscription is due
const TICK: Duration = Duration::from_secs(30);
/// The pause between two checks, so that Hitomi sees one client doing one thing at a time
const PAUSE_BETWEEN_CHECKS: Duration = Duration::from_secs(10);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/list_subscriptions", post(list_subscriptions))
        .route("/preview_subscription", post(preview_subscription))
        .route("/create_subscription", post(create_subscription))
        .route("/delete_subscription", post(delete_subscription))
        .route("/set_subscription_paused", post(set_subscription_paused))
        .route("/check_subscription", post(check_subscription))
        .route("/check_all_subscriptions", post(check_all_subscriptions))
}

/// Check the subscriptions that are due, for as long as the server runs
pub async fn run_scheduler(subscriptions: Arc<Subscriptions>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            _ = shutdown.changed() => return,
        }
        tokio::select! {
            () = subscriptions.run_due(subscriptions::now, PAUSE_BETWEEN_CHECKS) => {}
            _ = shutdown.changed() => return,
        }
    }
}

/// The failure of a command, for the dialog or the list to show
///
/// Not logged: a search that is followed already or a mistyped query are no errors of the
/// server, the answer carries the message and the frontend shows it where it was asked.
fn api_error(err: SubscriptionError) -> ApiError {
    let status = match &err {
        SubscriptionError::Empty
        | SubscriptionError::TooMany
        | SubscriptionError::TooManyMatches { .. } => StatusCode::BAD_REQUEST,
        SubscriptionError::Duplicate => StatusCode::CONFLICT,
        SubscriptionError::NotFound => StatusCode::NOT_FOUND,
        SubscriptionError::Search(_) => {
            tracing::warn!("{err}");
            StatusCode::BAD_GATEWAY
        }
    };
    ApiError::new(
        status,
        CommandError {
            err_title: "Followed searches".to_string(),
            err_message: err.to_string(),
        },
    )
}

#[derive(Deserialize)]
struct QueryArgs {
    query: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateArgs {
    query: String,
    first_run: FirstRun,
}

#[derive(Deserialize)]
struct IdArgs {
    id: String,
}

#[derive(Deserialize)]
struct PausedArgs {
    id: String,
    paused: bool,
}

async fn list_subscriptions(State(state): State<AppState>) -> Json<SubscriptionList> {
    Json(state.subscriptions.list(subscriptions::now()))
}

async fn preview_subscription(
    State(state): State<AppState>,
    Args(QueryArgs { query }): Args<QueryArgs>,
) -> ApiResult<Json<Preview>> {
    Ok(Json(
        state
            .subscriptions
            .preview(&query)
            .await
            .map_err(api_error)?,
    ))
}

async fn create_subscription(
    State(state): State<AppState>,
    Args(CreateArgs { query, first_run }): Args<CreateArgs>,
) -> ApiResult<Json<SubscriptionView>> {
    let view = state
        .subscriptions
        .create(&query, first_run, subscriptions::now())
        .await
        .map_err(api_error)?;
    if first_run == FirstRun::Everything {
        // queueing what is there takes a while, the list shows it as being checked
        spawn_check(&state.subscriptions, view.id.clone());
    }
    Ok(Json(view))
}

async fn delete_subscription(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<()>> {
    state.subscriptions.delete(&id).map_err(api_error)?;
    Ok(Json(()))
}

async fn set_subscription_paused(
    State(state): State<AppState>,
    Args(PausedArgs { id, paused }): Args<PausedArgs>,
) -> ApiResult<Json<()>> {
    state
        .subscriptions
        .set_paused(&id, paused)
        .map_err(api_error)?;
    Ok(Json(()))
}

async fn check_subscription(
    State(state): State<AppState>,
    Args(IdArgs { id }): Args<IdArgs>,
) -> ApiResult<Json<()>> {
    if !state.subscriptions.exists(&id) {
        return Err(api_error(SubscriptionError::NotFound));
    }
    // asking again while it is checked changes nothing
    if !state.subscriptions.is_checking(&id) {
        spawn_check(&state.subscriptions, id);
    }
    Ok(Json(()))
}

async fn check_all_subscriptions(State(state): State<AppState>) -> Json<()> {
    let subscriptions = state.subscriptions.clone();
    tokio::spawn(async move {
        subscriptions
            .check_all(subscriptions::now, PAUSE_BETWEEN_CHECKS)
            .await;
    });
    Json(())
}

fn spawn_check(subscriptions: &Arc<Subscriptions>, id: String) {
    let subscriptions = subscriptions.clone();
    tokio::spawn(async move {
        let _ = subscriptions.check(&id, subscriptions::now()).await;
    });
}
