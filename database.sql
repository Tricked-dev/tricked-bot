CREATE TABLE IF NOT EXISTS users
    (
        id          INTEGER constraint table_name_pk primary key autoincrement,
        invite_used TEXT,
        "left"      BOOLEAN  default false not null,
        joined      DATETIME default CURRENT_TIMESTAMP,
        discord_id  INTEGER  not null
    );