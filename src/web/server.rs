use axum::{
    routing::{get, post},
    Router,
};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;
use tera::Tera;

#[derive(Clone)]
pub struct AppState {
    pub db: Pool<SqliteConnectionManager>,
    pub templates: Arc<Tera>,
}

pub async fn run_web_server(db: Pool<SqliteConnectionManager>, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    // Determine template path based on environment
    // For Nix builds: try to find data directory relative to executable
    let template_path = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .and_then(|bin_dir| bin_dir.parent().map(|p| p.to_path_buf()))
        .and_then(|out_dir| {
            let nix_path = out_dir.join("share/tricked-bot/web/templates");
            if nix_path.exists() {
                Some(format!("{}/**/*", nix_path.display()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| "web/templates/**/*".to_string());

    let mut tera = Tera::new(&template_path)?;
    tera.autoescape_on(vec!["html"]);

    let state = AppState {
        db,
        templates: Arc::new(tera),
    };

    let app = Router::new()
        .route("/", get(super::routes::index))
        .route("/users", get(super::routes::list_users))
        .route("/user/{id}", get(super::routes::view_user))
        .route("/user/{id}/edit", get(super::routes::edit_user_form))
        .route("/user/{id}/edit", post(super::routes::update_user))
        .route("/user/{id}/memories", get(super::routes::list_memories))
        .route("/user/{id}/memory/new", get(super::routes::new_memory_form))
        .route("/user/{id}/memory/new", post(super::routes::create_memory))
        .route("/memory/{id}/edit", get(super::routes::edit_memory_form))
        .route("/memory/{id}/edit", post(super::routes::update_memory))
        .route("/memory/{id}/delete", post(super::routes::delete_memory))
        .route("/static/style.css", get(super::routes::serve_css))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    tracing::info!("Web server listening on http://0.0.0.0:{}", port);

    axum::serve(listener, app).await?;

    Ok(())
}
