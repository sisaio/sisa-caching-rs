//! The user routes of the axum example. They are written against [`CacheStore`], so they run
//! the same on every store.
//!
//! - `GET /users/count` caches a scalar: how many users there are.
//! - `GET /users/{id}` caches a single object: one user.
//! - `GET /users` caches a list: every user.
//! - `PUT /users/{id}` and `DELETE /users/{id}` change the database, then the cache.

mod database;
mod telemetry;

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use sisa_caching::CacheStore;
use tracing::Instrument;

use crate::database::Database;

pub use crate::telemetry::{Telemetry, init_telemetry};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: u64,
    pub name: String,
}

#[derive(Deserialize)]
struct UpdateUser {
    name: String,
}

/// What every handler can reach. Cloning it is cheap: stores and the database are shared.
#[derive(Clone)]
struct App<C> {
    cache: C,
    db: Arc<Database>,
}

/// The routes, serving users through `cache`.
pub fn router<C>(cache: C) -> Router
where
    C: CacheStore + Clone + 'static,
{
    Router::new()
        .route("/users", get(list::<C>))
        .route("/users/count", get(count::<C>))
        .route(
            "/users/{id}",
            get(find::<C>).put(update::<C>).delete(delete::<C>),
        )
        .with_state(App {
            cache,
            db: Arc::default(),
        })
        .layer(middleware::from_fn(trace_request))
}

/// Runs each request in a span, so its cache calls, and the evictions it causes on other
/// instances, show up as one trace.
async fn trace_request(request: Request, next: Next) -> Response {
    let span =
        tracing::info_span!("request", method = %request.method(), path = %request.uri().path());

    next.run(request).instrument(span).await
}

/// Serves `app` on `PORT` (3000 by default) until Ctrl-C.
pub async fn serve(app: Router) -> std::io::Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".into());
    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{port}")).await?;

    tracing::info!("listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await
}

// Keys. A change removes the keys it makes stale by name: a `remove_prefix("users")` would drop the
// count and the list together, but on Redis it scans every key on every primary.
//
// Each key carries the version of what it holds, right after its name. Bump it when a field is
// renamed or changes type, or a value keeps its shape but changes meaning, then drop the old
// entries once the deploy is done, such as `remove_prefix("user:v1")`. A change to `User` bumps
// both `user` and `users:all`, since the list holds users too. Adding or removing a field needs no
// bump: `get_or_set` stores over a value it can't read. Nothing checks any of this.
const LIST_KEY: &str = "users:all:v1";
const COUNT_KEY: &str = "users:count:v1";

fn user_key(id: u64) -> String {
    format!("user:v1:{id}")
}

// Reads load from the database on a miss. Concurrent requests for the same key wait for that one
// load instead of each hitting the database. Each loader owns a handle to the database, which
// older compilers need to see the future is `Send`.

async fn count<C: CacheStore>(State(app): State<App<C>>) -> Result<Json<u64>, AppError> {
    let db = app.db.clone();
    let count = app
        .cache
        .get_or_set(
            COUNT_KEY,
            || async move { Ok::<_, AppError>(db.count().await) },
            None,
        )
        .await?;

    Ok(Json(count))
}

async fn find<C: CacheStore>(
    State(app): State<App<C>>,
    Path(id): Path<u64>,
) -> Result<Json<User>, AppError> {
    let db = app.db.clone();
    let user = app
        .cache
        .get_or_set(
            &user_key(id),
            || async move { db.find(id).await.ok_or(AppError::NotFound) },
            None,
        )
        .await?;

    Ok(Json(user))
}

async fn list<C: CacheStore>(State(app): State<App<C>>) -> Result<Json<Vec<User>>, AppError> {
    let db = app.db.clone();
    let users = app
        .cache
        .get_or_set(
            LIST_KEY,
            || async move { Ok::<_, AppError>(db.list().await) },
            None,
        )
        .await?;

    Ok(Json(users))
}

// Writes go to the database first, so a failed cache write leaves the cache stale, never ahead of
// the database. Then the user, the count and the list are removed and reloaded on their next read.
// Writing the new user back instead could race: two updates could leave the older one cached.

async fn update<C: CacheStore>(
    State(app): State<App<C>>,
    Path(id): Path<u64>,
    Json(update): Json<UpdateUser>,
) -> Result<Json<User>, AppError> {
    let user = User {
        id,
        name: update.name,
    };

    app.db.save(&user);
    remove_user(&app.cache, id).await?;

    Ok(Json(user))
}

async fn delete<C: CacheStore>(
    State(app): State<App<C>>,
    Path(id): Path<u64>,
) -> Result<StatusCode, AppError> {
    app.db.delete(id);
    remove_user(&app.cache, id).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Removes the keys that a change to user `id` makes stale.
async fn remove_user<C: CacheStore>(cache: &C, id: u64) -> Result<(), C::Error> {
    cache.remove(&user_key(id)).await?;
    cache.remove(LIST_KEY).await?;
    cache.remove(COUNT_KEY).await?;

    Ok(())
}

/// A missing user is a 404; anything else is a 500.
enum AppError {
    NotFound,
    Internal(Box<dyn std::error::Error + Send + Sync>),
}

impl<E: std::error::Error + Send + Sync + 'static> From<E> for AppError {
    fn from(error: E) -> Self {
        Self::Internal(Box::new(error))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound => StatusCode::NOT_FOUND.into_response(),
            Self::Internal(error) => {
                tracing::error!(%error, "request failed");

                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}
