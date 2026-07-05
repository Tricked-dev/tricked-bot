use crate::db;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Form,
};
use serde::{Deserialize, Serialize};
use tera::Context;

use super::server::AppState;

#[derive(Debug, Serialize)]
struct UserExport {
    id: String,
    name: String,
    level: i32,
    xp: i32,
    relationship: String,
    example_input: String,
    example_output: String,
    memories: Vec<MemoryExport>,
}

#[derive(Debug, Serialize)]
struct MemoryExport {
    key: String,
    content: String,
}

#[derive(Debug, Deserialize)]
pub struct UserUpdateForm {
    pub name: String,
    pub relationship: String,
    pub example_input: String,
    pub example_output: String,
}

#[derive(Debug, Deserialize)]
pub struct MemoryForm {
    pub key: String,
    pub content: String,
}

pub async fn list_users(State(state): State<AppState>) -> Response {
    let users = match db::get_all_users_web(&state.db).await {
        Ok(u) => u,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };
    let mut context = Context::new();
    context.insert("users", &users);
    context.insert("title", "Users");
    match state.templates.render("users.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response(),
    }
}

pub async fn view_user(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let user = match db::get_user(&state.db, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };

    let mut context = Context::new();
    context.insert("user", &user);
    let candidates = match db::get_profile_candidates(&state.db, user_id).await {
        Ok(c) => c,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };
    context.insert("profile_candidates", &candidates);
    context.insert("title", &format!("User {}", user_id));

    match state.templates.render("user.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn approve_profile_candidate(State(state): State<AppState>, Path(candidate_id): Path<i64>) -> Response {
    match db::approve_profile_candidate(&state.db, candidate_id).await {
        Ok(Some(user_id)) => axum::response::Redirect::to(&format!("/user/{}", user_id)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Candidate not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn reject_profile_candidate(State(state): State<AppState>, Path(candidate_id): Path<i64>) -> Response {
    match db::reject_profile_candidate(&state.db, candidate_id).await {
        Ok(Some(user_id)) => axum::response::Redirect::to(&format!("/user/{}", user_id)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Candidate not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn edit_user_form(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let user = match db::get_user(&state.db, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };

    let mut context = Context::new();
    context.insert("user", &user);
    context.insert("title", &format!("Edit User {}", user_id));

    match state.templates.render("edit_user.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn update_user(
    State(state): State<AppState>,
    Path(user_id): Path<u64>,
    Form(form): Form<UserUpdateForm>,
) -> Response {
    match db::update_user_profile(&state.db, user_id, &form.name, &form.relationship, &form.example_input, &form.example_output).await {
        Ok(_) => axum::response::Redirect::to(&format!("/user/{}", user_id)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn list_memories(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let user = match db::get_user(&state.db, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };
    let memories = match db::get_memories_for_user_web(&state.db, user_id).await {
        Ok(m) => m,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };

    let mut context = Context::new();
    context.insert("user", &user);
    context.insert("memories", &memories);
    context.insert("title", &format!("Memories for User {}", user_id));

    match state.templates.render("memories.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn new_memory_form(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let user = match db::get_user(&state.db, user_id).await {
        Ok(Some(u)) => u,
        Ok(None) => return (StatusCode::NOT_FOUND, "User not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };

    let mut context = Context::new();
    context.insert("user", &user);
    context.insert("title", &format!("New Memory for User {}", user_id));

    match state.templates.render("new_memory.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn create_memory(
    State(state): State<AppState>,
    Path(user_id): Path<u64>,
    Form(form): Form<MemoryForm>,
) -> Response {
    match db::create_memory(&state.db, user_id, &form.key, &form.content).await {
        Ok(_) => axum::response::Redirect::to(&format!("/user/{}/memories", user_id)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn edit_memory_form(State(state): State<AppState>, Path(memory_id): Path<i32>) -> Response {
    let memory = match db::get_memory(&state.db, memory_id as i64).await {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, "Memory not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };

    let mut context = Context::new();
    context.insert("memory", &memory);
    context.insert("title", &format!("Edit Memory {}", memory_id));

    match state.templates.render("edit_memory.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn update_memory(
    State(state): State<AppState>,
    Path(memory_id): Path<i32>,
    Form(form): Form<MemoryForm>,
) -> Response {
    let memory = match db::get_memory(&state.db, memory_id as i64).await {
        Ok(Some(m)) => m,
        Ok(None) => return (StatusCode::NOT_FOUND, "Memory not found").into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    };
    match db::update_memory(&state.db, memory_id as i64, &form.key, &form.content).await {
        Ok(_) => axum::response::Redirect::to(&format!("/user/{}/memories", memory.user_id as u64)).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn delete_memory(State(state): State<AppState>, Path(memory_id): Path<i32>) -> Response {
    match db::delete_memory(&state.db, memory_id as i64).await {
        Ok(Some(user_id)) => axum::response::Redirect::to(&format!("/user/{}/memories", user_id as u64)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "Memory not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("DB error: {}", e)).into_response(),
    }
}

pub async fn serve_css() -> Response {
    let css = include_str!("../../web/static/style.css");
    (
        [(axum::http::header::CONTENT_TYPE, "text/css")],
        css,
    )
        .into_response()
}

// Helper function to get all user exports with memories
async fn get_all_user_exports(pool: &deadpool_postgres::Pool) -> Result<Vec<UserExport>, String> {
    let users = db::get_all_users_web(pool).await
        .map_err(|e| format!("Database error: {}", e))?;

    let exports = users.into_iter().map(|u| UserExport {
        id: u.id.to_string(),
        name: u.name,
        level: u.level,
        xp: u.xp,
        relationship: u.relationship,
        example_input: u.example_input,
        example_output: u.example_output,
        memories: Vec::new(),
    }).collect();

    Ok(exports)
}

pub async fn export_prompts_json(State(state): State<AppState>) -> Response {
    let exports = match get_all_user_exports(&state.db).await {
        Ok(exports) => exports,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    };

    match serde_json::to_string_pretty(&exports) {
        Ok(json) => (
            [
                (axum::http::header::CONTENT_TYPE, "application/json"),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"prompts_export.json\"",
                ),
            ],
            json,
        )
            .into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("JSON error: {}", e)).into_response()
        }
    }
}

pub async fn export_users_csv(State(state): State<AppState>) -> Response {
    let exports = match get_all_user_exports(&state.db).await {
        Ok(exports) => exports,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response()
        }
    };

    // Helper to escape CSV fields
    let escape_csv = |s: &str| -> String {
        if s.contains(',') || s.contains('"') || s.contains('\n') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };

    // Build CSV with memories
    let mut csv = String::from("user_id,name,level,xp,relationship,example_input,example_output,memories\n");

    for user in exports {
        // Serialize memories as JSON for the CSV field
        let memories_json = match serde_json::to_string(&user.memories) {
            Ok(json) => json,
            Err(e) => {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("JSON error: {}", e)).into_response()
            }
        };

        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{}\n",
            escape_csv(&user.id),
            escape_csv(&user.name),
            user.level,
            user.xp,
            escape_csv(&user.relationship),
            escape_csv(&user.example_input),
            escape_csv(&user.example_output),
            escape_csv(&memories_json),
        ));
    }

    (
        [
            (axum::http::header::CONTENT_TYPE, "text/csv"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"users_export.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}
