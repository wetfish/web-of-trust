//! Finds identities by nick, ident or host, and loads their sessions for the
//! identity views of `wot analyze`.

use std::fmt::Write;

use rusqlite::{Connection, params};

use crate::db::Result;

/// What to look up. Each part may use `*` to match any characters, and
/// matching ignores case. An identity matches when one of its nick and
/// `ident@host` combinations matches every part given.
#[derive(Default)]
pub struct Filter {
    pub nick: Option<String>,
    pub ident: Option<String>,
    pub host: Option<String>,
}

impl Filter {
    pub fn is_empty(&self) -> bool {
        self.nick.is_none() && self.ident.is_none() && self.host.is_none()
    }

    /// The filter in words, such as `nick "rachel", host "Fish-*"`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (name, value) in [
            ("nick", &self.nick),
            ("ident", &self.ident),
            ("host", &self.host),
        ] {
            if let Some(value) = value {
                parts.push(format!("{name} \"{value}\""));
            }
        }
        parts.join(", ")
    }
}

/// One session of an identity, with the file it is in.
pub struct SessionRow {
    pub nicks: Vec<String>,
    pub ident: Option<String>,
    pub host: Option<String>,
    /// `host`, `inferred` or `nick_only`.
    pub link: String,
    /// `YYYY-MM-DD HH:MM`.
    pub start_at: String,
    pub end_at: String,
    pub minutes: i64,
    pub spoken: i64,
    pub logger: bool,
    /// Whether the file is a channel log rather than a private message log.
    pub channel: bool,
    /// The channel or private message target.
    pub target: String,
    /// The file as `tag/target.log`.
    pub file: String,
    pub start_line: i64,
    pub end_line: Option<i64>,
    /// For inferred sessions, the session they were attributed from, as
    /// `tag/target.log:line`.
    pub inferred_from: Option<String>,
}

pub struct IdentityView {
    pub id: String,
    pub tag: String,
    /// In order of when they started.
    pub sessions: Vec<SessionRow>,
}

impl IdentityView {
    /// When the identity was last seen, as `YYYY-MM-DD HH:MM`.
    pub fn last_seen(&self) -> &str {
        self.sessions
            .iter()
            .map(|session| session.end_at.as_str())
            .max()
            .unwrap_or_default()
    }
}

/// The identities matching the filter, most recently seen first.
pub fn find(conn: &Connection, filter: &Filter) -> Result<Vec<IdentityView>> {
    let ids: Vec<(String, String)> = {
        let mut stmt = conn.prepare(
            r"SELECT DISTINCT k.identity_id, i.tag
              FROM identity_keys k JOIN identities i ON i.id = k.identity_id
              WHERE (?1 IS NULL OR k.nick LIKE ?1 ESCAPE '\')
                AND (?2 IS NULL OR k.ident LIKE ?2 ESCAPE '\')
                AND (?3 IS NULL OR k.host LIKE ?3 ESCAPE '\')",
        )?;
        let rows = stmt
            .query_map(
                params![
                    filter
                        .nick
                        .as_deref()
                        .map(|nick| like_pattern(&nick.to_lowercase())),
                    filter.ident.as_deref().map(like_pattern),
                    filter.host.as_deref().map(like_pattern),
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    let mut stmt = conn.prepare(
        "SELECT s.nicks, s.ident, s.host, s.link, s.start_at, s.end_at, s.minutes, s.spoken,
                s.logger, f.kind, f.tag, f.target, s.start_line, s.end_line,
                sf.tag, sf.target, src.start_line
         FROM sessions s
         JOIN files f ON f.id = s.file_id
         LEFT JOIN sessions src ON src.id = s.inferred_from
         LEFT JOIN files sf ON sf.id = src.file_id
         WHERE s.identity_id = ?1
         ORDER BY s.start_at, s.id",
    )?;

    let mut views = Vec::with_capacity(ids.len());
    for (id, tag) in ids {
        let sessions = stmt
            .query_map(params![id], |row| {
                let nicks: String = row.get(0)?;
                let kind: String = row.get(9)?;
                let file_tag: String = row.get(10)?;
                let target: String = row.get(11)?;
                let source_tag: Option<String> = row.get(14)?;
                let source_target: Option<String> = row.get(15)?;
                let source_line: Option<i64> = row.get(16)?;
                let inferred_from = match (source_tag, source_target, source_line) {
                    (Some(tag), Some(target), Some(line)) => {
                        Some(format!("{tag}/{target}.log:{line}"))
                    }
                    _ => None,
                };
                Ok(SessionRow {
                    nicks: nicks.split(' ').map(str::to_string).collect(),
                    ident: row.get(1)?,
                    host: row.get(2)?,
                    link: row.get(3)?,
                    start_at: row.get(4)?,
                    end_at: row.get(5)?,
                    minutes: row.get(6)?,
                    spoken: row.get(7)?,
                    logger: row.get(8)?,
                    channel: kind == "channel",
                    file: format!("{file_tag}/{target}.log"),
                    target,
                    start_line: row.get(12)?,
                    end_line: row.get(13)?,
                    inferred_from,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !sessions.is_empty() {
            views.push(IdentityView { id, tag, sessions });
        }
    }

    views.sort_by(|a, b| {
        b.last_seen()
            .cmp(a.last_seen())
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(views)
}

/// Turns a pattern with `*` wildcards into a SQL `LIKE` pattern, escaping
/// `LIKE`'s own wildcards with a backslash.
fn like_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '\\' | '%' | '_' => {
                let _ = write!(out, "\\{c}");
            }
            '*' => out.push('%'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    #[test]
    fn turns_wildcards_into_like_patterns() {
        assert_eq!(like_pattern("Fish-*.chat"), "Fish-%.chat");
        assert_eq!(like_pattern("rachel_"), r"rachel\_");
        assert_eq!(like_pattern("100%"), r"100\%");
    }

    #[test]
    fn finds_identities_by_nick_ident_and_host() {
        let root = TempDir::new("lookup");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 01:00:00 2026\n\
             01:00 -!- rachel [rachel@hostR] has joined #analysis\n\
             01:01 <rachel> hi\n\
             01:02 -!- quokka [quokkawet@bridge.chat] has joined #analysis\n\
             01:03 -!- marlowe [marlowewet@bridge.chat] has joined #analysis\n\
             01:04 <marlowe> hello\n\
             01:05 -!- marlowe is now known as marlowe_\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/rachel.log"),
            "--- Log opened Sun Nov 01 02:00:00 2026\n02:00 <rachel> psst\n",
        )
        .unwrap();

        let path = root.0.join("wot.sqlite");
        let mut conn = db::open(&path).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();

        let nicks = |filter: Filter| -> Vec<String> {
            find(&conn, &filter)
                .unwrap()
                .iter()
                .map(|view| view.sessions[0].nicks.join(" "))
                .collect()
        };

        let rachel = Filter {
            nick: Some("RACHEL".into()),
            ..Filter::default()
        };
        assert_eq!(nicks(rachel), ["rachel"]);

        let bridge = Filter {
            host: Some("*.chat".into()),
            ..Filter::default()
        };
        let mut found = nicks(bridge);
        found.sort();
        assert_eq!(found, ["marlowe marlowe_", "quokka"]);

        let both = Filter {
            ident: Some("marlowe*".into()),
            host: Some("bridge.chat".into()),
            ..Filter::default()
        };
        assert_eq!(nicks(both), ["marlowe marlowe_"]);

        // The private message session is inferred from the channel session.
        let rachel = find(
            &conn,
            &Filter {
                nick: Some("rachel".into()),
                ..Filter::default()
            },
        )
        .unwrap();
        let inferred: Vec<Option<&str>> = rachel[0]
            .sessions
            .iter()
            .map(|session| session.inferred_from.as_deref())
            .collect();
        assert_eq!(inferred, [None, Some("wetfish/#analysis.log:2")]);
    }
}
