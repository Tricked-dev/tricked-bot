//! Small admin UI for editing users and their memories. Templates are compiled into the binary.

mod routes;

use axum::{
    Router,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use deadpool_postgres::Pool;

pub async fn serve(db: Pool, port: u16) -> color_eyre::Result<()> {
    let app = Router::new()
        .route("/", get(routes::list_users))
        .route("/users", get(routes::list_users))
        .route("/user/{id}", get(routes::view_user))
        .route("/user/{id}/edit", get(routes::edit_user_form).post(routes::update_user))
        .route("/user/{id}/memories", get(routes::list_memories))
        .route("/user/{id}/memory/new", get(routes::new_memory_form).post(routes::create_memory))
        .route("/memory/{id}/edit", get(routes::edit_memory_form).post(routes::update_memory))
        .route("/memory/{id}/delete", post(routes::delete_memory))
        .route("/export/prompts.json", get(routes::export_prompts_json))
        .route("/export/users.csv", get(routes::export_users_csv))
        .route("/static/style.css", get(routes::serve_css))
        .with_state(db);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!("Web server listening on http://0.0.0.0:{port}");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Any failure while handling a request, shown to the admin as plain text.
#[derive(Debug)]
enum AppError {
    NotFound(&'static str),
    Internal(color_eyre::Report),
}

impl<E: Into<color_eyre::Report>> From<E> for AppError {
    fn from(error: E) -> Self {
        Self::Internal(error.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            Self::NotFound(what) => (StatusCode::NOT_FOUND, format!("{what} not found")).into_response(),
            Self::Internal(error) => {
                tracing::error!("Web request failed: {error:?}");
                (StatusCode::INTERNAL_SERVER_ERROR, format!("Error: {error}")).into_response()
            }
        }
    }
}

type AppResult<T> = Result<T, AppError>;
