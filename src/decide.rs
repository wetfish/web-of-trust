//! Decisions made by hand about who is who, stored in their own tables so
//! that rebuilding identities never changes them:
//!
//! - **Host groups:** hosts that are one bridge or bouncer, which count as
//!   one host when identities are built, optionally with the ident as the
//!   person. Changing them needs an identity rebuild.
//! - **People:** the humans behind identities. A person holds identities from
//!   any network tag. Nothing needs rebuilding.
//! - **Cuts:** nick changes made by someone else at the keyboard. Changing
//!   them needs an identity rebuild.

use std::collections::HashMap;
use std::io::Write;

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;
use crate::identity;
use crate::lookup::{self, current_id};
use crate::report::{day, heading, table, thousands};

/// The network tag to use: the one given, or the only one imported.
pub fn resolve_tag(conn: &Connection, tag: Option<&str>) -> Result<String> {
    let tags: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT tag FROM files ORDER BY tag")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    match (tag, tags.as_slice()) {
        (_, []) => Err("no logs have been imported yet; run wot analyze first".into()),
        (Some(tag), tags) if tags.iter().any(|t| t == tag) => Ok(tag.to_string()),
        (Some(tag), tags) => Err(format!(
            "no logs are imported for network tag {tag}; imported tags: {}",
            tags.join(", ")
        )
        .into()),
        (None, [only]) => Ok(only.clone()),
        (None, tags) => Err(format!(
            "logs from several network tags are imported ({}); choose one with --tag",
            tags.join(", ")
        )
        .into()),
    }
}

// Host groups

fn group_id(conn: &Connection, tag: &str, name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM host_groups WHERE tag = ?1 AND name = ?2",
        params![tag, name],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| format!("there is no host group named {name} in network tag {tag}").into())
}

pub fn add_group(
    conn: &Connection,
    tag: &str,
    name: &str,
    kind: &str,
    ident_is_user: bool,
    note: Option<&str>,
) -> Result<()> {
    if group_id(conn, tag, name).is_ok() {
        return Err(format!("host group {name} already exists in network tag {tag}").into());
    }
    conn.execute(
        "INSERT INTO host_groups (tag, name, kind, ident_is_user, note)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![tag, name, kind, ident_is_user, note],
    )?;
    Ok(())
}

pub fn edit_group(
    conn: &Connection,
    tag: &str,
    name: &str,
    kind: Option<&str>,
    ident_is_user: Option<bool>,
    note: Option<&str>,
) -> Result<()> {
    let id = group_id(conn, tag, name)?;
    if let Some(kind) = kind {
        conn.execute(
            "UPDATE host_groups SET kind = ?1 WHERE id = ?2",
            params![kind, id],
        )?;
    }
    if let Some(ident_is_user) = ident_is_user {
        conn.execute(
            "UPDATE host_groups SET ident_is_user = ?1 WHERE id = ?2",
            params![ident_is_user, id],
        )?;
    }
    if let Some(note) = note {
        conn.execute(
            "UPDATE host_groups SET note = ?1 WHERE id = ?2",
            params![note, id],
        )?;
    }
    Ok(())
}

/// Adds hosts to a group. Every host must have appeared with an ident in the
/// group's network tag and not be in a group yet; otherwise nothing is added.
pub fn add_hosts(
    conn: &mut Connection,
    tag: &str,
    group: &str,
    hosts: &[String],
    note: Option<&str>,
) -> Result<()> {
    let id = group_id(conn, tag, group)?;
    for host in hosts {
        let seen: bool = conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM identity_keys
                            WHERE tag = ?1 AND host = ?2 COLLATE NOCASE)",
            params![tag, host],
            |row| row.get(0),
        )?;
        if !seen {
            return Err(format!(
                "{host} has not been seen in network tag {tag}; check the spelling, \
                 for example with wot analyze --host"
            )
            .into());
        }
        let current: Option<String> = conn
            .query_row(
                "SELECT g.name FROM host_group_members m JOIN host_groups g ON g.id = m.group_id
                 WHERE m.tag = ?1 AND m.host = ?2",
                params![tag, host],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(current) = current {
            return Err(format!("{host} is already in host group {current}").into());
        }
    }

    let tx = conn.transaction()?;
    for host in hosts {
        tx.execute(
            "INSERT INTO host_group_members (group_id, tag, host, note) VALUES (?1, ?2, ?3, ?4)",
            params![id, tag, host, note],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Removes hosts from whichever group they are in.
pub fn remove_hosts(conn: &mut Connection, tag: &str, hosts: &[String]) -> Result<()> {
    let tx = conn.transaction()?;
    for host in hosts {
        let removed = tx.execute(
            "DELETE FROM host_group_members WHERE tag = ?1 AND host = ?2",
            params![tag, host],
        )?;
        if removed == 0 {
            return Err(format!("{host} is not in a host group in network tag {tag}").into());
        }
    }
    tx.commit()?;
    Ok(())
}

/// Deletes a group and takes all its hosts out of it.
pub fn remove_group(conn: &Connection, tag: &str, name: &str) -> Result<()> {
    let id = group_id(conn, tag, name)?;
    conn.execute("DELETE FROM host_groups WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn list_hosts(conn: &Connection, out: &mut impl Write) -> Result<()> {
    struct Group {
        id: i64,
        tag: String,
        name: String,
        kind: String,
        ident_is_user: bool,
        note: Option<String>,
    }
    let groups: Vec<Group> = {
        let mut stmt = conn.prepare(
            "SELECT id, tag, name, kind, ident_is_user, note FROM host_groups ORDER BY tag, name",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Group {
                    id: row.get(0)?,
                    tag: row.get(1)?,
                    name: row.get(2)?,
                    kind: row.get(3)?,
                    ident_is_user: row.get(4)?,
                    note: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    heading(out, "Host groups", '=')?;
    if groups.is_empty() {
        writeln!(
            out,
            "No host groups yet. Create one with wot hosts add-group."
        )?;
        return Ok(());
    }
    let mut members = conn.prepare(
        "SELECT host, ifnull(note, ''), created_at FROM host_group_members
         WHERE group_id = ?1 ORDER BY host",
    )?;
    for group in groups {
        writeln!(out)?;
        heading(
            out,
            &format!("{} ({}, {})", group.name, group.kind, group.tag),
            '-',
        )?;
        if group.ident_is_user {
            writeln!(
                out,
                "The ident identifies the person on these hosts, across nicks."
            )?;
        } else {
            writeln!(
                out,
                "These hosts count as one host; nick and ident must both match."
            )?;
        }
        if let Some(note) = group.note {
            writeln!(out, "Note: {note}")?;
        }
        let rows: Vec<Vec<String>> = members
            .query_map(params![group.id], |row| {
                let added: String = row.get(2)?;
                Ok(vec![row.get(0)?, day(&added).to_string(), row.get(1)?])
            })?
            .collect::<rusqlite::Result<_>>()?;
        writeln!(out)?;
        if rows.is_empty() {
            writeln!(
                out,
                "  No hosts yet. Add them with wot hosts add {}.",
                group.name
            )?;
        } else {
            table(
                out,
                &["Host", "Added", "Note"],
                &rows,
                &[false, false, false],
            )?;
        }
    }
    Ok(())
}

// People

fn person_id(conn: &Connection, name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM people WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| format!("there is no person named {name}").into())
}

/// The current id of an identity, following merges, or an error when the
/// identity does not exist.
fn existing_identity(
    conn: &Connection,
    merges: &HashMap<String, String>,
    id: &str,
) -> Result<String> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM identities WHERE id = ?1)",
        params![id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(format!(
            "there is no identity {id}; identity ids are shown by wot analyze --nick, \
             --ident or --host"
        )
        .into());
    }
    Ok(current_id(merges, id))
}

pub fn create_person(
    conn: &mut Connection,
    name: &str,
    ids: &[String],
    note: Option<&str>,
) -> Result<()> {
    if person_id(conn, name).is_ok() {
        return Err(format!("a person named {name} already exists").into());
    }
    // Check the identities before creating anything.
    check_unassigned(conn, ids)?;
    conn.execute(
        "INSERT INTO people (name, note) VALUES (?1, ?2)",
        params![name, note],
    )?;
    add_to_person(conn, name, ids)
}

/// Fails when an identity does not exist or already belongs to a person.
fn check_unassigned(conn: &Connection, ids: &[String]) -> Result<()> {
    let merges = lookup::merges(conn)?;
    let people = lookup::people_by_identity(conn, &merges)?;
    for id in ids {
        let current = existing_identity(conn, &merges, id)?;
        if let Some(names) = people.get(&current) {
            return Err(format!(
                "identity {id} already belongs to {}; remove it first with wot people remove {id}",
                names.join(", ")
            )
            .into());
        }
    }
    Ok(())
}

pub fn add_to_person(conn: &mut Connection, name: &str, ids: &[String]) -> Result<()> {
    let person = person_id(conn, name)?;
    check_unassigned(conn, ids)?;
    let merges = lookup::merges(conn)?;
    let tx = conn.transaction()?;
    for id in ids {
        let current = current_id(&merges, id);
        tx.execute(
            "INSERT OR IGNORE INTO person_identities (identity_id, person_id) VALUES (?1, ?2)",
            params![current, person],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Takes identities out of whichever person they belong to, including
/// memberships recorded under an id that has since been merged into them.
pub fn remove_from_person(conn: &mut Connection, ids: &[String]) -> Result<()> {
    let merges = lookup::merges(conn)?;
    let members: Vec<String> = {
        let mut stmt = conn.prepare("SELECT identity_id FROM person_identities")?;
        let rows = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let tx = conn.transaction()?;
    for id in ids {
        let current = existing_identity(&tx, &merges, id)?;
        let mut removed = 0;
        for member in members
            .iter()
            .filter(|member| current_id(&merges, member) == current)
        {
            removed += tx.execute(
                "DELETE FROM person_identities WHERE identity_id = ?1",
                params![member],
            )?;
        }
        if removed == 0 {
            return Err(format!("identity {id} does not belong to a person").into());
        }
    }
    tx.commit()?;
    Ok(())
}

/// Deletes a person. Their identities are kept and belong to no one.
pub fn delete_person(conn: &Connection, name: &str) -> Result<()> {
    let id = person_id(conn, name)?;
    conn.execute("DELETE FROM people WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn list_people(conn: &Connection, out: &mut impl Write) -> Result<()> {
    let merges = lookup::merges(conn)?;
    let people: Vec<(i64, String, Option<String>)> = {
        let mut stmt = conn.prepare("SELECT id, name, note FROM people ORDER BY name")?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };

    heading(out, "People", '=')?;
    if people.is_empty() {
        writeln!(out, "No people yet. Create one with wot people create.")?;
        return Ok(());
    }
    let mut members =
        conn.prepare("SELECT identity_id FROM person_identities WHERE person_id = ?1 ORDER BY 1")?;
    let mut nicks = conn.prepare(
        "SELECT group_concat(nick, ', ') FROM (SELECT DISTINCT nick FROM identity_keys
                                             WHERE identity_id = ?1 ORDER BY nick)",
    )?;
    let mut seen = conn.prepare(
        "SELECT i.tag, max(s.end_at) FROM identities i LEFT JOIN sessions s ON s.identity_id = i.id
         WHERE i.id = ?1",
    )?;
    for (id, name, note) in people {
        writeln!(out)?;
        heading(out, &name, '-')?;
        if let Some(note) = note {
            writeln!(out, "Note: {note}")?;
        }
        let ids: Vec<String> = members
            .query_map(params![id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if ids.is_empty() {
            writeln!(
                out,
                "No identities yet. Add them with wot people add {name} ID..."
            )?;
            continue;
        }
        let mut rows = Vec::new();
        for member in ids {
            let current = current_id(&merges, &member);
            let label = if current == member {
                member
            } else {
                format!("{member} (now {current})")
            };
            let nick_list: Option<String> = nicks.query_row(params![current], |row| row.get(0))?;
            let (tag, last): (Option<String>, Option<String>) =
                seen.query_row(params![current], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.push(vec![
                label,
                tag.unwrap_or_default(),
                nick_list.unwrap_or_default(),
                last.as_deref().map(day).unwrap_or("-").to_string(),
            ]);
        }
        writeln!(out)?;
        table(
            out,
            &["Identity", "Tag", "Nicks", "Last seen"],
            &rows,
            &[false, false, false, false],
        )?;
    }
    Ok(())
}

// Cuts

/// Cuts the session at a nick change line, given as `tag/target.log:LINE` or
/// `/full/path/to/file.log:LINE`. Returns the nick change it was made on.
pub fn add_cut(conn: &Connection, at: &str, note: Option<&str>) -> Result<String> {
    let (file, line) = at
        .rsplit_once(':')
        .ok_or_else(|| format!("{at} is not FILE:LINE"))?;
    let line: i64 = line
        .parse()
        .map_err(|_| format!("{line} in {at} is not a line number"))?;

    let candidates: Vec<(i64, String)> = if file.starts_with('/') {
        let mut stmt = conn.prepare("SELECT id, path FROM files WHERE path = ?1")?;
        let rows = stmt
            .query_map(params![file], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    } else {
        let (tag, name) = file
            .split_once('/')
            .ok_or_else(|| format!("{file} is not tag/target.log or a full path"))?;
        let target = name.strip_suffix(".log").unwrap_or(name);
        let mut stmt = conn.prepare("SELECT id, path FROM files WHERE tag = ?1 AND target = ?2")?;
        let rows = stmt
            .query_map(params![tag, target], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    let (file_id, path) = match candidates.as_slice() {
        [] => return Err(format!("no imported log file matches {file}").into()),
        [only] => only.clone(),
        several => {
            let paths: Vec<&str> = several.iter().map(|(_, path)| path.as_str()).collect();
            return Err(format!(
                "{file} is in more than one log folder; give the full path instead: {}",
                paths.join(", ")
            )
            .into());
        }
    };

    let event: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT kind, nick, other_nick FROM events WHERE file_id = ?1 AND line = ?2",
            params![file_id, line],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let expected = event
        .and_then(|(kind, nick, new)| {
            identity::nick_change_text(&kind, nick.as_deref(), new.as_deref())
        })
        .ok_or_else(|| {
            format!(
                "line {line} of {file} is not a nick change; a cut goes on the nick change \
                 where someone else took over"
            )
        })?;

    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM session_cuts WHERE path = ?1 AND line = ?2)",
        params![path, line],
        |row| row.get(0),
    )?;
    if exists {
        return Err(format!("line {line} of {file} is already cut").into());
    }
    conn.execute(
        "INSERT INTO session_cuts (path, line, expected, note) VALUES (?1, ?2, ?3, ?4)",
        params![path, line, expected, note],
    )?;
    Ok(expected)
}

pub fn remove_cut(conn: &Connection, id: i64) -> Result<()> {
    let removed = conn.execute("DELETE FROM session_cuts WHERE id = ?1", params![id])?;
    if removed == 0 {
        return Err(format!("there is no cut {id}; wot cuts list shows them").into());
    }
    Ok(())
}

pub fn list_cuts(conn: &Connection, out: &mut impl Write) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.path, f.tag, f.target, c.line, c.expected, ifnull(c.note, ''),
                c.created_at, e.kind, e.nick, e.other_nick
         FROM session_cuts c
         LEFT JOIN files f ON f.path = c.path
         LEFT JOIN events e ON e.file_id = f.id AND e.line = c.line
         ORDER BY c.id",
    )?;
    let rows: Vec<Vec<String>> = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let tag: Option<String> = row.get(2)?;
            let target: Option<String> = row.get(3)?;
            let line: i64 = row.get(4)?;
            let expected: String = row.get(5)?;
            let created: String = row.get(7)?;
            let kind: Option<String> = row.get(8)?;
            let nick: Option<String> = row.get(9)?;
            let new: Option<String> = row.get(10)?;
            let file = match (tag, target) {
                (Some(tag), Some(target)) => format!("{tag}/{target}.log"),
                _ => path,
            };
            let actual = kind.and_then(|kind| {
                identity::nick_change_text(&kind, nick.as_deref(), new.as_deref())
            });
            let status = if actual.as_deref() == Some(expected.as_str()) {
                "applied"
            } else {
                "stale"
            };
            Ok(vec![
                id.to_string(),
                format!("{file}:{line}"),
                expected,
                status.to_string(),
                day(&created).to_string(),
                row.get(6)?,
            ])
        })?
        .collect::<rusqlite::Result<_>>()?;

    heading(out, "Cuts", '=')?;
    if rows.is_empty() {
        writeln!(out, "No cuts yet. Make one with wot cuts add FILE:LINE.")?;
        return Ok(());
    }
    let noun = if rows.len() == 1 { "cut" } else { "cuts" };
    writeln!(
        out,
        "{} {noun}. A stale cut's line no longer holds the nick change it was made on, \
         so it is not applied.",
        thousands(rows.len() as u64)
    )?;
    writeln!(out)?;
    table(
        out,
        &["Id", "Line", "Nick change", "Status", "Added", "Note"],
        &rows,
        &[true, false, false, false, false, false],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, identity};

    fn setup(name: &str) -> (TempDir, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#dryfish.log"),
            "--- Log opened Sun Nov 01 01:00:00 2026\n\
             01:00 -!- quokka [quokkawet@bridge.chat] has joined #dryfish\n\
             01:02 -!- quokka is now known as rachelaway\n\
             01:03 -!- marlowe [marlowewet@bridge.chat] has joined #dryfish\n",
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();
        (root, conn)
    }

    #[test]
    fn resolves_the_only_tag() {
        let (_root, conn) = setup("decide-tag");
        assert_eq!(resolve_tag(&conn, None).unwrap(), "wetfish");
        assert_eq!(resolve_tag(&conn, Some("wetfish")).unwrap(), "wetfish");
        assert!(resolve_tag(&conn, Some("other")).is_err());
    }

    #[test]
    fn adds_only_hosts_seen_in_the_logs_and_to_one_group() {
        let (_root, mut conn) = setup("decide-hosts");
        add_group(&conn, "wetfish", "matrix", "bridge", true, None).unwrap();
        assert!(add_group(&conn, "wetfish", "matrix", "bridge", true, None).is_err());
        assert!(add_hosts(&mut conn, "wetfish", "matrix", &["typo.chat".into()], None).is_err());
        add_hosts(
            &mut conn,
            "wetfish",
            "matrix",
            &["BRIDGE.chat".into()],
            None,
        )
        .unwrap();

        add_group(&conn, "wetfish", "znc", "bouncer", true, None).unwrap();
        assert!(add_hosts(&mut conn, "wetfish", "znc", &["bridge.chat".into()], None).is_err());

        remove_hosts(&mut conn, "wetfish", &["bridge.chat".into()]).unwrap();
        assert!(remove_hosts(&mut conn, "wetfish", &["bridge.chat".into()]).is_err());
        remove_group(&conn, "wetfish", "znc").unwrap();
        assert!(remove_group(&conn, "wetfish", "znc").is_err());
    }

    #[test]
    fn cuts_only_nick_change_lines() {
        let (_root, conn) = setup("decide-cuts");
        assert!(
            add_cut(&conn, "wetfish/#dryfish.log:2", None).is_err(),
            "a join"
        );
        assert!(add_cut(&conn, "wetfish/#nope.log:3", None).is_err());
        assert!(add_cut(&conn, "wetfish/#dryfish.log", None).is_err());
        assert_eq!(
            add_cut(&conn, "wetfish/#dryfish.log:3", None).unwrap(),
            "quokka is now known as rachelaway"
        );
        assert!(
            add_cut(&conn, "wetfish/#dryfish.log:3", None).is_err(),
            "already cut"
        );
        remove_cut(&conn, 1).unwrap();
        assert!(remove_cut(&conn, 1).is_err());
    }

    #[test]
    fn people_hold_each_identity_once() {
        let (_root, mut conn) = setup("decide-people");
        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT identity_id FROM sessions ORDER BY 1")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            rows
        };
        assert_eq!(ids.len(), 2);

        create_person(&mut conn, "rachel", &ids[..1], None).unwrap();
        assert!(
            create_person(&mut conn, "RACHEL", &[], None).is_err(),
            "names ignore case"
        );
        assert!(
            add_to_person(&mut conn, "rachel", &ids[..1]).is_err(),
            "already assigned"
        );
        assert!(add_to_person(&mut conn, "rachel", &["nope".into()]).is_err());

        create_person(&mut conn, "marlowe", &ids[1..], None).unwrap();
        assert!(
            add_to_person(&mut conn, "rachel", &ids[1..]).is_err(),
            "belongs to marlowe"
        );

        remove_from_person(&mut conn, &ids[..1]).unwrap();
        assert!(remove_from_person(&mut conn, &ids[..1]).is_err());
        delete_person(&conn, "marlowe").unwrap();
        let left: i64 = conn
            .query_row("SELECT count(*) FROM person_identities", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 0);
    }
}
