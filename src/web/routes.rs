use std::{collections::HashMap, fmt::Write};

use askama::Template;
use axum::{
    Form,
    extract::{Path, State},
    http::header,
    response::{Html, IntoResponse, Redirect, Response},
};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};

use super::{AppError, AppResult};
use crate::db::{self, Memory, User};

#[derive(Template)]
#[template(path = "users.html")]
struct UsersPage {
    users: Vec<User>,
}

#[derive(Template)]
#[template(path = "user.html")]
struct UserPage {
    user: User,
}

#[derive(Template)]
#[template(path = "edit_user.html")]
struct EditUserPage {
    user: User,
}

#[derive(Template)]
#[template(path = "memories.html")]
struct MemoriesPage {
    user: User,
    memories: Vec<Memory>,
}

#[derive(Template)]
#[template(path = "new_memory.html")]
struct NewMemoryPage {
    user: User,
}

#[derive(Template)]
#[template(path = "edit_memory.html")]
struct EditMemoryPage {
    memory: Memory,
}

fn render(page: &impl Template) -> AppResult<Html<String>> {
    Ok(Html(page.render()?))
}

async fn find_user(db: &Pool, id: u64) -> AppResult<User> {
    db::user(db, id).await?.ok_or(AppError::NotFound("User"))
}

fn memories_page(user_id: impl std::fmt::Display) -> Redirect {
    Redirect::to(&format!("/user/{user_id}/memories"))
}

#[derive(Debug, Deserialize)]
pub struct UserForm {
    name: String,
    relationship: String,
    example_input: String,
    example_output: String,
}

#[derive(Debug, Deserialize)]
pub struct MemoryForm {
    key: String,
    content: String,
}

pub async fn list_users(State(db): State<Pool>) -> AppResult<Html<String>> {
    render(&UsersPage { users: db::users(&db).await? })
}

pub async fn view_user(State(db): State<Pool>, Path(id): Path<u64>) -> AppResult<Html<String>> {
    render(&UserPage { user: find_user(&db, id).await? })
}

pub async fn edit_user_form(State(db): State<Pool>, Path(id): Path<u64>) -> AppResult<Html<String>> {
    render(&EditUserPage { user: find_user(&db, id).await? })
}

pub async fn update_user(
    State(db): State<Pool>,
    Path(id): Path<u64>,
    Form(form): Form<UserForm>,
) -> AppResult<Redirect> {
    let user = User {
        name: form.name,
        relationship: form.relationship,
        example_input: form.example_input,
        example_output: form.example_output,
        ..find_user(&db, id).await?
    };
    db::update_profile(&db, &user).await?;
    Ok(Redirect::to(&format!("/user/{id}")))
}

pub async fn list_memories(State(db): State<Pool>, Path(id): Path<u64>) -> AppResult<Html<String>> {
    let (user, memories) = tokio::try_join!(find_user(&db, id), async { Ok(db::memories(&db, id).await?) })?;
    render(&MemoriesPage { user, memories })
}

pub async fn new_memory_form(State(db): State<Pool>, Path(id): Path<u64>) -> AppResult<Html<String>> {
    render(&NewMemoryPage { user: find_user(&db, id).await? })
}

pub async fn create_memory(
    State(db): State<Pool>,
    Path(id): Path<u64>,
    Form(form): Form<MemoryForm>,
) -> AppResult<Redirect> {
    db::create_memory(&db, id, &form.key, &form.content).await?;
    Ok(memories_page(id))
}

pub async fn edit_memory_form(State(db): State<Pool>, Path(id): Path<i64>) -> AppResult<Html<String>> {
    let memory = db::memory(&db, id).await?.ok_or(AppError::NotFound("Memory"))?;
    render(&EditMemoryPage { memory })
}

pub async fn update_memory(
    State(db): State<Pool>,
    Path(id): Path<i64>,
    Form(form): Form<MemoryForm>,
) -> AppResult<Redirect> {
    let owner = db::update_memory(&db, id, &form.key, &form.content).await?;
    Ok(memories_page(owner.ok_or(AppError::NotFound("Memory"))?))
}

pub async fn delete_memory(State(db): State<Pool>, Path(id): Path<i64>) -> AppResult<Redirect> {
    let owner = db::delete_memory(&db, id).await?;
    Ok(memories_page(owner.ok_or(AppError::NotFound("Memory"))?))
}

pub async fn serve_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], include_str!("../../web/static/style.css"))
}

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

async fn user_exports(db: &Pool) -> AppResult<Vec<UserExport>> {
    let (users, memories) = tokio::try_join!(db::users(db), db::all_memories(db))?;
    let mut by_user: HashMap<i64, Vec<MemoryExport>> = HashMap::new();
    for Memory { user_id, key, content, .. } in memories {
        by_user.entry(user_id).or_default().push(MemoryExport { key, content });
    }
    Ok(users
        .into_iter()
        .map(|user| UserExport {
            memories: by_user.remove(&user.id).unwrap_or_default(),
            id: user.id.to_string(),
            name: user.name,
            level: user.level,
            xp: user.xp,
            relationship: user.relationship,
            example_input: user.example_input,
            example_output: user.example_output,
        })
        .collect())
}

fn attachment(content_type: &'static str, filename: &'static str, body: String) -> Response {
    let disposition = format!("attachment; filename=\"{filename}\"");
    ([(header::CONTENT_TYPE, content_type.to_owned()), (header::CONTENT_DISPOSITION, disposition)], body)
        .into_response()
}

pub async fn export_prompts_json(State(db): State<Pool>) -> AppResult<Response> {
    let json = serde_json::to_string_pretty(&user_exports(&db).await?)?;
    Ok(attachment("application/json", "prompts_export.json", json))
}

fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}

pub async fn export_users_csv(State(db): State<Pool>) -> AppResult<Response> {
    let mut csv = String::from("user_id,name,level,xp,relationship,example_input,example_output,memories\n");
    for user in user_exports(&db).await? {
        let memories = serde_json::to_string(&user.memories)?;
        let _ = writeln!(
            csv,
            "{},{},{},{},{},{},{},{}",
            user.id,
            csv_field(&user.name),
            user.level,
            user.xp,
            csv_field(&user.relationship),
            csv_field(&user.example_input),
            csv_field(&user.example_output),
            csv_field(&memories),
        );
    }
    Ok(attachment("text/csv", "users_export.csv", csv))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str) -> User {
        User {
            id: 7,
            level: 2,
            xp: 30,
            name: name.into(),
            relationship: "<b>nemesis</b>".into(),
            example_input: String::new(),
            example_output: String::new(),
        }
    }

    fn memory_with_id(id: i64) -> Memory {
        Memory { id, user_id: 7, key: "work".into(), content: "builds bots".into() }
    }

    #[test]
    fn pages_render_and_escape_user_content() {
        let html = UsersPage { users: vec![user("alice"), user("")] }.render().unwrap();
        assert!(html.contains("alice") && html.contains("User 7"));
        assert!(html.contains("&#60;b&#62;nemesis") || html.contains("&lt;b&gt;nemesis"));
        assert!(!html.contains("<b>nemesis"));
        let memory = memory_with_id(1);
        let html = MemoriesPage { user: user("alice"), memories: vec![memory.clone()] }.render().unwrap();
        assert!(html.contains("builds bots") && html.contains("/memory/1/edit"));
        assert!(MemoriesPage { user: user("alice"), memories: vec![] }.render().unwrap().contains("No memories"));
        for html in [
            UserPage { user: user("alice") }.render().unwrap(),
            EditUserPage { user: user("alice") }.render().unwrap(),
            NewMemoryPage { user: user("alice") }.render().unwrap(),
            EditMemoryPage { memory }.render().unwrap(),
        ] {
            assert!(html.contains("Memory Manager</title>"));
        }
        let quoted = Memory { key: "a\"b".into(), ..memory_with_id(2) };
        let html = EditMemoryPage { memory: quoted }.render().unwrap();
        assert!(html.contains(r#"value="a&#34;b""#) || html.contains(r#"value="a&quot;b""#));
        assert!(html.contains(r#"href="/user/7/memories" class="btn">Cancel"#) && html.contains("Save Changes"));
        let html = NewMemoryPage { user: user("alice") }.render().unwrap();
        assert!(html.contains(r#"value="""#) && html.contains("Create Memory"));
    }

    #[test]
    fn csv_fields_are_quoted_only_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\"\n"), "\"say \"\"hi\"\"\n\"");
    }
}
