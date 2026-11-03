//! Decisions made by hand about who is who, stored in their own tables so
//! that rebuilding identities never changes them:
//!
//! - **Host groups:** hosts that are one bridge or bouncer, which count as
//!   one host when identities are built, optionally with the ident as the
//!   person. Changing them needs an identity rebuild.
//! - **People:** who is behind identities, a human or a bot. A person holds
//!   identities from any network tag. Nothing needs rebuilding.
//! - **Cuts:** nick changes made by someone else at the keyboard. Changing
//!   them needs an identity rebuild.

use std::collections::HashMap;
use std::io::Write;

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;
use crate::identity;
use crate::lookup::{self, current_id};
use crate::report::{day, heading, table, thousands};
use crate::stats::{self, Kind};

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
        // An empty note clears it.
        let note = Some(note.trim()).filter(|note| !note.is_empty());
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
    let groups = stats::host_groups(conn)?;
    heading(out, "Host groups", '=')?;
    if groups.is_empty() {
        writeln!(
            out,
            "No host groups yet. Create one with wot hosts add-group."
        )?;
        return Ok(());
    }
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
        if let Some(note) = &group.note {
            writeln!(out, "Note: {note}")?;
        }
        writeln!(out)?;
        if group.members.is_empty() {
            writeln!(
                out,
                "  No hosts yet. Add them with wot hosts add {}.",
                group.name
            )?;
            continue;
        }
        let rows: Vec<Vec<String>> = group
            .members
            .iter()
            .map(|member| {
                vec![
                    member.host.clone(),
                    thousands(member.identities as u64),
                    day(&member.created_at).to_string(),
                    member.note.clone().unwrap_or_default(),
                ]
            })
            .collect();
        table(
            out,
            &["Host", "Identities", "Added", "Note"],
            &rows,
            &[false, true, false, false],
        )?;
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
    .ok_or_else(|| format!("there is no person or bot named {name}").into())
}

/// The kind of the person record with this name, ignoring case, if any.
fn kind_of(conn: &Connection, name: &str) -> Result<Option<Kind>> {
    let kind: Option<String> = conn
        .query_row(
            "SELECT kind FROM people WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(kind.as_deref().and_then(Kind::parse))
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

/// Creates a person record, a human or a bot, with identities. Names are
/// shared by both kinds, so a bot cannot have the name of a person.
pub fn create_person(
    conn: &mut Connection,
    name: &str,
    kind: Kind,
    ids: &[String],
    note: Option<&str>,
) -> Result<()> {
    if let Some(existing) = kind_of(conn, name)? {
        return Err(format!("a {} named {name} already exists", existing.as_str()).into());
    }
    // Check the identities before creating anything.
    check_unassigned(conn, ids, None)?;
    conn.execute(
        "INSERT INTO people (name, kind, note) VALUES (?1, ?2, ?3)",
        params![name, kind.as_str(), note],
    )?;
    add_to_person(conn, name, ids)
}

/// Makes a person record a human or a bot, keeping its identities.
pub fn set_kind(conn: &Connection, name: &str, kind: Kind) -> Result<()> {
    match kind_of(conn, name)? {
        None => Err(format!("there is no person or bot named {name}").into()),
        Some(current) if current == kind => {
            Err(format!("{name} is already a {}", kind.as_str()).into())
        }
        Some(_) => {
            conn.execute(
                "UPDATE people SET kind = ?1 WHERE name = ?2",
                params![kind.as_str(), name],
            )?;
            Ok(())
        }
    }
}

/// Fails when an identity does not exist or belongs to a person other than
/// `person`. Returns the ids, as they are now, that do not belong to `person`
/// yet.
fn check_unassigned(
    conn: &Connection,
    ids: &[String],
    person: Option<&str>,
) -> Result<Vec<String>> {
    let merges = lookup::merges(conn)?;
    let people = lookup::people_by_identity(conn, &merges)?;
    let mut missing = Vec::new();
    for id in ids {
        let current = existing_identity(conn, &merges, id)?;
        let Some(names) = people.get(&current) else {
            missing.push(current);
            continue;
        };
        let theirs =
            person.is_some_and(|person| names.iter().any(|name| name.eq_ignore_ascii_case(person)));
        if !theirs {
            return Err(format!(
                "identity {id} already belongs to {}; remove it first with wot people remove {id}",
                names.join(", ")
            )
            .into());
        }
    }
    Ok(missing)
}

/// Adds identities to a person. Identities that already belong to them are
/// left as they are, so adding them again changes nothing.
pub fn add_to_person(conn: &mut Connection, name: &str, ids: &[String]) -> Result<()> {
    let person = person_id(conn, name)?;
    let missing = check_unassigned(conn, ids, Some(name))?;
    let tx = conn.transaction()?;
    for id in missing {
        tx.execute(
            "INSERT OR IGNORE INTO person_identities (identity_id, person_id) VALUES (?1, ?2)",
            params![id, person],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Takes identities out of whichever person they belong to, including
/// memberships recorded under an id that has since been merged into them.
/// Identities that belong to no one are skipped; it fails only when none of
/// them belonged to a person.
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
    let mut removed = 0;
    for id in ids {
        let current = existing_identity(&tx, &merges, id)?;
        for member in members
            .iter()
            .filter(|member| current_id(&merges, member) == current)
        {
            removed += tx.execute(
                "DELETE FROM person_identities WHERE identity_id = ?1",
                params![member],
            )?;
        }
    }
    if removed == 0 {
        return Err(match ids {
            [id] => format!("identity {id} does not belong to a person or bot"),
            _ => "none of these identities belong to a person or bot".to_string(),
        }
        .into());
    }
    tx.commit()?;
    Ok(())
}

/// Deletes a person or bot. Their identities are kept and belong to no one.
pub fn delete_person(conn: &Connection, name: &str) -> Result<()> {
    let id = person_id(conn, name)?;
    conn.execute("DELETE FROM people WHERE id = ?1", params![id])?;
    Ok(())
}

/// Lists people, then bots, with their identities.
pub fn list_people(conn: &Connection, out: &mut impl Write) -> Result<()> {
    let everyone = stats::people(conn)?;
    for (title, kind, empty) in [
        (
            "People",
            Kind::Person,
            "No people yet. Create one with wot people create NAME.",
        ),
        (
            "Bots",
            Kind::Bot,
            "No bots yet. Create one with wot people create NAME --bot.",
        ),
    ] {
        if kind == Kind::Bot {
            writeln!(out)?;
        }
        heading(out, title, '=')?;
        let records: Vec<&stats::Person> = everyone.iter().filter(|p| p.kind == kind).collect();
        if records.is_empty() {
            writeln!(out, "{empty}")?;
        }
        list_records(out, &records)?;
    }
    Ok(())
}

fn list_records(out: &mut impl Write, people: &[&stats::Person]) -> Result<()> {
    for person in people {
        writeln!(out)?;
        heading(out, &person.name, '-')?;
        if let Some(note) = &person.note {
            writeln!(out, "Note: {note}")?;
        }
        if person.identities.is_empty() {
            writeln!(
                out,
                "No identities yet. Add them with wot people add {} ID...",
                person.name
            )?;
            continue;
        }
        let rows: Vec<Vec<String>> = person
            .identities
            .iter()
            .map(|identity| {
                let label = if identity.current == identity.recorded {
                    identity.current.clone()
                } else {
                    format!("{} (now {})", identity.recorded, identity.current)
                };
                vec![
                    label,
                    identity.tag.clone().unwrap_or_default(),
                    identity.nicks.join(", "),
                    identity
                        .last_seen
                        .as_deref()
                        .map(day)
                        .unwrap_or("-")
                        .to_string(),
                ]
            })
            .collect();
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
    let cuts = stats::cuts(conn)?;
    heading(out, "Cuts", '=')?;
    if cuts.is_empty() {
        writeln!(out, "No cuts yet. Make one with wot cuts add FILE:LINE.")?;
        return Ok(());
    }
    let noun = if cuts.len() == 1 { "cut" } else { "cuts" };
    writeln!(
        out,
        "{} {noun}. A stale cut's line no longer holds the nick change it was made on, \
         so it is not applied.",
        thousands(cuts.len() as u64)
    )?;
    writeln!(out)?;
    let rows: Vec<Vec<String>> = cuts
        .iter()
        .map(|cut| {
            vec![
                cut.id.to_string(),
                format!("{}:{}", cut.file, cut.line),
                cut.expected.clone(),
                if cut.applied { "applied" } else { "stale" }.to_string(),
                day(&cut.created_at).to_string(),
                cut.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
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
    use crate::{activity, db, identity};

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

        create_person(&mut conn, "rachel", Kind::Person, &ids[..1], None).unwrap();
        assert!(
            create_person(&mut conn, "RACHEL", Kind::Person, &[], None).is_err(),
            "names ignore case"
        );
        // Adding an identity to the person it belongs to changes nothing.
        add_to_person(&mut conn, "RACHEL", &ids[..1]).unwrap();
        assert!(add_to_person(&mut conn, "rachel", &["nope".into()]).is_err());

        create_person(&mut conn, "marlowe", Kind::Person, &ids[1..], None).unwrap();
        assert!(
            add_to_person(&mut conn, "rachel", &ids[1..]).is_err(),
            "belongs to marlowe"
        );

        // Identities that belong to no one are skipped, unless none of
        // them belong to anyone.
        remove_from_person(&mut conn, &ids[..1]).unwrap();
        assert!(remove_from_person(&mut conn, &ids[..1]).is_err());
        remove_from_person(&mut conn, &ids).unwrap();
        delete_person(&conn, "marlowe").unwrap();
        let left: i64 = conn
            .query_row("SELECT count(*) FROM person_identities", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn makes_bots_out_of_people_records() {
        let (_root, mut conn) = setup("decide-bots");
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
        create_person(
            &mut conn,
            "relay",
            Kind::Bot,
            &ids,
            Some("announces builds"),
        )
        .unwrap();
        assert_eq!(activity::bots(&conn).unwrap().len(), 2);
        assert!(
            create_person(&mut conn, "RELAY", Kind::Person, &[], None).is_err(),
            "names are shared by people and bots"
        );

        set_kind(&conn, "relay", Kind::Person).unwrap();
        assert!(activity::bots(&conn).unwrap().is_empty());
        assert!(set_kind(&conn, "relay", Kind::Person).is_err(), "no change");
        assert!(set_kind(&conn, "nobody", Kind::Bot).is_err());

        set_kind(&conn, "RELAY", Kind::Bot).unwrap();
        remove_from_person(&mut conn, &ids[..1]).unwrap();
        assert_eq!(activity::bots(&conn).unwrap().len(), 1);
    }
}
