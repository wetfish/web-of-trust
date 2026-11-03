//! `wot lines`: imported lines of a network tag, as they are in the database,
//! to track down lines irssi's format did not match, lines a bouncer replayed,
//! and lines left out as logged twice, each next to the copy that was kept.

use std::io::{self, Write};

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::db::Result;
use crate::history::escape_like;

/// Which lines to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// Every line.
    All,
    /// Lines irssi's format did not match.
    Unparsed,
    /// Lines a bouncer replayed, between its playback markers.
    Replayed,
    /// Second copies of lines logged twice, each with the copy kept.
    Duplicates,
}

pub struct Filter {
    /// A folder tag or a network.
    pub tag: String,
    pub channel: Option<String>,
    pub show: Show,
    /// Text the line contains, ignoring case for ASCII letters.
    pub contains: Option<String>,
    pub limit: usize,
}

/// One imported line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub path: String,
    pub line: i64,
    /// `YYYY-MM-DD`, or empty before the log gave a date.
    pub date: String,
    pub time: String,
    pub kind: String,
    pub nick: String,
    pub text: String,
    /// For a line left out as logged twice, the copy that was kept.
    pub kept: Option<Box<Found>>,
}

/// The columns of a line, from `events` as `alias` and `files` as `file`.
fn columns(alias: &str, file: &str) -> String {
    format!(
        "{file}.path, {alias}.line, coalesce({alias}.date, ''), coalesce({alias}.time, ''),
         {alias}.kind, coalesce({alias}.nick, ''), coalesce({alias}.text, '')"
    )
}

fn found(row: &rusqlite::Row, first: usize) -> rusqlite::Result<Found> {
    Ok(Found {
        path: row.get(first)?,
        line: row.get(first + 1)?,
        date: row.get(first + 2)?,
        time: row.get(first + 3)?,
        kind: row.get(first + 4)?,
        nick: row.get(first + 5)?,
        text: row.get(first + 6)?,
        kept: None,
    })
}

/// The lines matching the filter, in file and line order, and whether more
/// follow than the limit allows.
pub fn find(conn: &Connection, filter: &Filter) -> Result<(Vec<Found>, bool)> {
    let mut values = vec![Value::from(filter.tag.trim().to_string())];
    let mut conditions =
        vec!["(f.tag = ?1 COLLATE NOCASE OR f.folder_tag = ?1 COLLATE NOCASE)".to_string()];
    if let Some(channel) = &filter.channel {
        values.push(Value::from(channel.clone()));
        conditions.push(format!("f.target = ?{} COLLATE NOCASE", values.len()));
    }
    if let Some(text) = &filter.contains {
        values.push(Value::from(format!("%{}%", escape_like(text))));
        conditions.push(format!("e.text LIKE ?{} ESCAPE '\\'", values.len()));
    }
    let joins = match filter.show {
        Show::All => "",
        Show::Unparsed => {
            conditions.push("e.kind = 'unparsed'".to_string());
            ""
        }
        Show::Replayed => "JOIN replayed_lines r ON r.event_id = e.id",
        Show::Duplicates => {
            "JOIN left_out_lines l ON l.event_id = e.id AND l.reason = 'duplicate'
             JOIN events k ON k.id = l.kept_id
             JOIN files kf ON kf.id = k.file_id"
        }
    };
    let kept = if filter.show == Show::Duplicates {
        columns("k", "kf")
    } else {
        "NULL, NULL, NULL, NULL, NULL, NULL, NULL".to_string()
    };
    values.push(Value::from(filter.limit as i64 + 1));
    let mut stmt = conn.prepare(&format!(
        "SELECT {}, {kept}
         FROM events e JOIN files f ON f.id = e.file_id {joins}
         WHERE {}
         ORDER BY f.id, e.line
         LIMIT ?{}",
        columns("e", "f"),
        conditions.join(" AND "),
        values.len()
    ))?;
    let mut lines: Vec<Found> = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |row| {
            let mut line = found(row, 0)?;
            if row.get::<_, Option<String>>(7)?.is_some() {
                line.kept = Some(Box::new(found(row, 7)?));
            }
            Ok(line)
        })?
        .collect::<rusqlite::Result<_>>()?;
    let more = lines.len() > filter.limit;
    lines.truncate(filter.limit);
    Ok((lines, more))
}

/// One line as `path:line  date time  text`, with messages and actions
/// written the way irssi shows them.
fn format(line: &Found) -> String {
    let what = match line.kind.as_str() {
        "message" => format!("<{}> {}", line.nick, line.text),
        "action" => format!("* {} {}", line.nick, line.text),
        "unparsed" => line.text.clone(),
        kind => format!("[{kind}] {} {}", line.nick, line.text)
            .trim_end()
            .to_string(),
    };
    let when = format!("{} {}", line.date, line.time);
    format!("{}:{}  {}  {}", line.path, line.line, when.trim(), what)
}

pub fn write(out: &mut impl Write, lines: &[Found], more: bool, filter: &Filter) -> io::Result<()> {
    if lines.is_empty() {
        writeln!(out, "No lines of {} match.", filter.tag)?;
        return Ok(());
    }
    for line in lines {
        match &line.kept {
            // Each pair together, the copy kept first.
            Some(kept) => {
                writeln!(out, "kept      {}", format(kept))?;
                writeln!(out, "left out  {}", format(line))?;
                writeln!(out)?;
            }
            None => writeln!(out, "{}", format(line))?,
        }
    }
    if more {
        writeln!(
            out,
            "\nShowing the first {}; raise --limit to see more.",
            filter.limit
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    #[test]
    fn finds_unparsed_replayed_and_duplicate_lines() {
        let root = TempDir::new("lines");
        for folder in ["client/wetfish", "bouncer/wetfish-bouncer"] {
            fs::create_dir_all(root.0.join(folder)).unwrap();
        }
        fs::write(
            root.0.join("client/wetfish/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:01 <quokka> hello\n\
             22:20 <quokka> [21:00] pasted, not replayed\n\
             something irssi never wrote\n",
        )
        .unwrap();
        fs::write(
            root.0.join("bouncer/wetfish-bouncer/#analysis.log"),
            "--- Log opened Sun Nov 01 22:00:00 2026\n\
             22:01 <quokka> hello\n\
             22:30 < ***> Buffer Playback...\n\
             22:30 <quokka> [22:25:05] replayed 50% of it\n\
             22:30 < ***> Playback Complete.\n",
        )
        .unwrap();
        let roots = [root.0.join("client"), root.0.join("bouncer")];
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&roots).unwrap();
        db::import(&mut conn, &inventory.files);
        conn.execute_batch(
            "INSERT INTO bouncer_tags (tag) VALUES ('wetfish-bouncer');
             UPDATE files SET tag = 'wetfish';",
        )
        .unwrap();
        identity::rebuild(&mut conn).unwrap();

        let filter = |show, contains: Option<&str>| Filter {
            tag: "WETFISH".into(),
            channel: Some("#analysis".into()),
            show,
            contains: contains.map(str::to_string),
            limit: 10,
        };
        let texts = |show, contains| -> Vec<String> {
            find(&conn, &filter(show, contains))
                .unwrap()
                .0
                .into_iter()
                .map(|line| line.text)
                .collect()
        };
        assert_eq!(texts(Show::Unparsed, None), ["something irssi never wrote"]);
        assert_eq!(
            texts(Show::Replayed, None),
            ["[22:25:05] replayed 50% of it"]
        );
        assert_eq!(texts(Show::All, Some("50%")).len(), 1);
        assert!(
            texts(Show::All, Some("5_%")).is_empty(),
            "_ is not a wildcard"
        );

        // Duplicates come in pairs: the bouncer's copy left out, the
        // client's kept.
        let (pairs, more) = find(&conn, &filter(Show::Duplicates, None)).unwrap();
        assert!(!more);
        assert_eq!(pairs.len(), 1);
        let kept = pairs[0].kept.as_deref().unwrap();
        assert!(pairs[0].path.contains("wetfish-bouncer"));
        assert!(!kept.path.contains("wetfish-bouncer"));
        assert_eq!(
            (kept.text.as_str(), pairs[0].text.as_str()),
            ("hello", "hello")
        );
        let mut out = Vec::new();
        write(&mut out, &pairs, false, &filter(Show::Duplicates, None)).unwrap();
        let shown = String::from_utf8(out).unwrap();
        assert!(shown.starts_with("kept      "));
        assert!(shown.contains("\nleft out  "));

        let (lines, more) = find(
            &conn,
            &Filter {
                limit: 2,
                ..filter(Show::All, None)
            },
        )
        .unwrap();
        assert_eq!(lines.len(), 2);
        assert!(more);
        let shown = format(&lines[1]);
        assert!(
            shown.ends_with(":2  2026-11-01 22:01  <quokka> hello"),
            "{shown}"
        );
    }
}
