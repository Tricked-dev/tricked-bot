use crate::database::{Memory, User};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Form,
};
use rusqlite::params;
use serde::Deserialize;
use tera::Context;

use super::server::AppState;

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

pub async fn index(State(state): State<AppState>) -> Response {
    let mut context = Context::new();
    context.insert("title", "Memory Manager");

    match state.templates.render("index.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn list_users(State(state): State<AppState>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let mut stmt = match conn.prepare("SELECT id, level, xp, social_credit, name, relationship, example_input, example_output FROM user ORDER BY id") {
        Ok(stmt) => stmt,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let users: Result<Vec<User>, _> = stmt
        .query_map([], |row| {
            let id_str: String = row.get(0)?;
            Ok(User {
                id: id_str.parse().unwrap_or(0),
                level: row.get(1)?,
                xp: row.get(2)?,
                social_credit: row.get(3)?,
                name: row.get(4)?,
                relationship: row.get(5)?,
                example_input: row.get(6)?,
                example_output: row.get(7)?,
            })
        })
        .and_then(|mapped| mapped.collect());

    let users = match users {
        Ok(users) => users,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let mut context = Context::new();
    context.insert("users", &users);
    context.insert("title", "Users");

    match state.templates.render("users.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn view_user(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let user: Result<User, _> = conn.query_row(
        "SELECT id, level, xp, social_credit, name, relationship, example_input, example_output FROM user WHERE id = ?",
        params![user_id.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            Ok(User {
                id: id_str.parse().unwrap_or(0),
                level: row.get(1)?,
                xp: row.get(2)?,
                social_credit: row.get(3)?,
                name: row.get(4)?,
                relationship: row.get(5)?,
                example_input: row.get(6)?,
                example_output: row.get(7)?,
            })
        },
    );

    let user = match user {
        Ok(user) => user,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "User not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let mut context = Context::new();
    context.insert("user", &user);
    context.insert("title", &format!("User {}", user_id));

    match state.templates.render("user.html", &context) {
        Ok(html) => Html(html).into_response(),
        Err(e) => {
            tracing::error!("Template error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Template error: {}", e)).into_response()
        }
    }
}

pub async fn edit_user_form(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let user: Result<User, _> = conn.query_row(
        "SELECT id, level, xp, social_credit, name, relationship, example_input, example_output FROM user WHERE id = ?",
        params![user_id.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            Ok(User {
                id: id_str.parse().unwrap_or(0),
                level: row.get(1)?,
                xp: row.get(2)?,
                social_credit: row.get(3)?,
                name: row.get(4)?,
                relationship: row.get(5)?,
                example_input: row.get(6)?,
                example_output: row.get(7)?,
            })
        },
    );

    let user = match user {
        Ok(user) => user,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "User not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
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
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    match conn.execute(
        "UPDATE user SET name = ?, relationship = ?, example_input = ?, example_output = ? WHERE id = ?",
        params![form.name, form.relationship, form.example_input, form.example_output, user_id.to_string()],
    ) {
        Ok(_) => {
            axum::response::Redirect::to(&format!("/user/{}", user_id)).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    }
}

pub async fn list_memories(State(state): State<AppState>, Path(user_id): Path<u64>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    // Get user info
    let user: Result<User, _> = conn.query_row(
        "SELECT id, level, xp, social_credit, name, relationship, example_input, example_output FROM user WHERE id = ?",
        params![user_id.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            Ok(User {
                id: id_str.parse().unwrap_or(0),
                level: row.get(1)?,
                xp: row.get(2)?,
                social_credit: row.get(3)?,
                name: row.get(4)?,
                relationship: row.get(5)?,
                example_input: row.get(6)?,
                example_output: row.get(7)?,
            })
        },
    );

    let user = match user {
        Ok(user) => user,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "User not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    // Get memories for this user
    let mut stmt = match conn.prepare("SELECT id, user_id, content, key FROM Memory WHERE user_id = ? ORDER BY id") {
        Ok(stmt) => stmt,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let memories: Result<Vec<Memory>, _> = stmt
        .query_map(params![user_id.to_string()], |row| {
            Ok(Memory {
                id: row.get(0)?,
                user_id: row.get(1)?,
                content: row.get(2)?,
                key: row.get(3)?,
            })
        })
        .and_then(|mapped| mapped.collect());

    let memories = match memories {
        Ok(memories) => memories,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
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
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let user: Result<User, _> = conn.query_row(
        "SELECT id, level, xp, social_credit, name, relationship, example_input, example_output FROM user WHERE id = ?",
        params![user_id.to_string()],
        |row| {
            let id_str: String = row.get(0)?;
            Ok(User {
                id: id_str.parse().unwrap_or(0),
                level: row.get(1)?,
                xp: row.get(2)?,
                social_credit: row.get(3)?,
                name: row.get(4)?,
                relationship: row.get(5)?,
                example_input: row.get(6)?,
                example_output: row.get(7)?,
            })
        },
    );

    let user = match user {
        Ok(user) => user,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "User not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
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
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    match conn.execute(
        "INSERT INTO Memory (user_id, content, key) VALUES (?, ?, ?)",
        params![user_id.to_string(), form.content, form.key],
    ) {
        Ok(_) => {
            axum::response::Redirect::to(&format!("/user/{}/memories", user_id)).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    }
}

pub async fn edit_memory_form(State(state): State<AppState>, Path(memory_id): Path<i32>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    let memory: Result<Memory, _> = conn.query_row(
        "SELECT id, user_id, content, key FROM Memory WHERE id = ?",
        params![memory_id],
        |row| {
            Ok(Memory {
                id: row.get(0)?,
                user_id: row.get(1)?,
                content: row.get(2)?,
                key: row.get(3)?,
            })
        },
    );

    let memory = match memory {
        Ok(memory) => memory,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "Memory not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
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
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    // Get user_id first for redirect
    let user_id: Result<String, _> = conn.query_row(
        "SELECT user_id FROM Memory WHERE id = ?",
        params![memory_id],
        |row| row.get(0),
    );

    let user_id = match user_id {
        Ok(user_id) => user_id,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    match conn.execute(
        "UPDATE Memory SET content = ?, key = ? WHERE id = ?",
        params![form.content, form.key, memory_id],
    ) {
        Ok(_) => {
            axum::response::Redirect::to(&format!("/user/{}/memories", user_id)).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    }
}

pub async fn delete_memory(State(state): State<AppState>, Path(memory_id): Path<i32>) -> Response {
    let conn = match state.db.get() {
        Ok(conn) => conn,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    // Get user_id first for redirect
    let user_id: Result<String, _> = conn.query_row(
        "SELECT user_id FROM Memory WHERE id = ?",
        params![memory_id],
        |row| row.get(0),
    );

    let user_id = match user_id {
        Ok(user_id) => user_id,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return (StatusCode::NOT_FOUND, "Memory not found").into_response()
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
    };

    match conn.execute("DELETE FROM Memory WHERE id = ?", params![memory_id]) {
        Ok(_) => {
            axum::response::Redirect::to(&format!("/user/{}/memories", user_id)).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, format!("Database error: {}", e)).into_response()
        }
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
