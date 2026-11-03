//! Decisions made by hand about who is who, stored in their own tables so
//! that rebuilding identities never changes them:
//!
//! - **Host groups:** hosts that are one bridge or bouncer, which count as
//!   one host when identities are built, optionally with the ident as the
//!   person. Changing them needs an identity rebuild.
//! - **People:** who is behind identities, a human or a bot. A person holds
//!   identities from any network tag, and has your rating of them, notes and
//!   who invited them. Nothing needs rebuilding.
//! - **Channel reviews:** when you last went through a channel's users.
//! - **Cuts:** nick changes made by someone else at the keyboard. Changing
//!   them needs an identity rebuild.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;

use rusqlite::{Connection, OptionalExtension, params};

use crate::db::Result;
use crate::discover::LogFile;
use crate::identity;
use crate::lookup::{self, current_id};
use crate::progress;
use crate::report::{day, heading, table, thousands};
use crate::stats::{self, Kind, Rating};

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

/// Sets how well you know a person. Bots are not rated.
pub fn set_rating(conn: &Connection, name: &str, rating: Rating) -> Result<()> {
    match kind_of(conn, name)? {
        None => Err(format!("there is no person or bot named {name}").into()),
        Some(Kind::Bot) => Err(format!("{name} is a bot, and bots are not rated").into()),
        Some(Kind::Person) => {
            conn.execute(
                "UPDATE people SET rating = ?1 WHERE name = ?2",
                params![rating.as_str(), name],
            )?;
            Ok(())
        }
    }
}

/// Adds a note to a person or bot.
pub fn add_note(conn: &Connection, name: &str, text: &str) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Err("the note is empty".into());
    }
    let id = person_id(conn, name)?;
    conn.execute(
        "INSERT INTO person_notes (person_id, text) VALUES (?1, ?2)",
        params![id, text],
    )?;
    Ok(())
}

/// Deletes one of a person's or bot's notes.
pub fn delete_note(conn: &Connection, name: &str, note: i64) -> Result<()> {
    let id = person_id(conn, name)?;
    let deleted = conn.execute(
        "DELETE FROM person_notes WHERE id = ?1 AND person_id = ?2",
        params![note, id],
    )?;
    if deleted == 0 {
        return Err(format!("{name} has no note {note}").into());
    }
    Ok(())
}

/// Records who invited or vouches for a person, or clears it with `None`.
/// Only a person can invite, and never in a circle.
pub fn set_invited_by(conn: &Connection, name: &str, by: Option<&str>) -> Result<()> {
    let id = person_id(conn, name)?;
    let by_id = match by {
        None => None,
        Some(by) => {
            let by_id = person_id(conn, by)?;
            if kind_of(conn, by)? != Some(Kind::Person) {
                return Err(format!("{by} is a bot, and only people invite").into());
            }
            // Walk up from the inviter: reaching this person would make a
            // circle. The step limit only guards against a circle already
            // in the database.
            let mut next = Some(by_id);
            for _ in 0..1000 {
                let Some(current) = next else { break };
                if current == id {
                    return Err(if by_id == id {
                        format!("{name} cannot invite themselves")
                    } else {
                        format!(
                            "{by} was invited through {name}, so {name} cannot be invited by them"
                        )
                    }
                    .into());
                }
                next = conn.query_row(
                    "SELECT invited_by FROM people WHERE id = ?1",
                    params![current],
                    |row| row.get(0),
                )?;
            }
            Some(by_id)
        }
    };
    conn.execute(
        "UPDATE people SET invited_by = ?1 WHERE id = ?2",
        params![by_id, id],
    )?;
    Ok(())
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
        if person.kind == Kind::Person {
            writeln!(out, "Rating: {}", person.rating.label())?;
        }
        if let Some(by) = &person.invited_by {
            writeln!(out, "Invited by: {by}")?;
        }
        if let Some(note) = &person.note {
            writeln!(out, "Note: {note}")?;
        }
        for note in &person.notes {
            writeln!(
                out,
                "Note {} ({}): {}",
                note.id,
                day(&note.created_at),
                note.text
            )?;
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

// Ignored tags

/// The network tags left out of imports, lowercased.
pub fn ignored_tags(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT lower(tag) FROM ignored_tags")?;
    let tags = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(tags)
}

/// Takes the files of ignored tags out of `files`, and returns how many were
/// taken out of each tag.
pub fn leave_out_ignored(
    conn: &Connection,
    files: &mut Vec<LogFile>,
) -> Result<Vec<(String, usize)>> {
    let ignored = ignored_tags(conn)?;
    let mut left_out: BTreeMap<String, usize> = BTreeMap::new();
    files.retain(|file| {
        let ignore = ignored.contains(&file.tag.to_lowercase());
        if ignore {
            *left_out.entry(file.tag.clone()).or_default() += 1;
        }
        !ignore
    });
    Ok(left_out.into_iter().collect())
}

/// What ignoring a tag deleted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Deleted {
    pub files: i64,
    pub lines: i64,
}

/// Leaves a network tag out of imports and deletes its files and lines. The
/// sessions of every tag are deleted too, so identities have to be rebuilt
/// afterwards; decisions are kept.
pub fn ignore_tag(conn: &mut Connection, tag: &str, note: Option<&str>) -> Result<Deleted> {
    let tag = tag.trim();
    if tag.is_empty() {
        return Err("give the network tag to ignore".into());
    }
    let ignored: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM ignored_tags WHERE tag = ?1)",
        params![tag],
        |row| row.get(0),
    )?;
    if ignored {
        return Err(format!("network tag {tag} is ignored already").into());
    }
    let tx = conn.transaction()?;
    let (files, lines): (i64, i64) = tx.query_row(
        "SELECT count(*), ifnull(sum(lines_read), 0) FROM files
         WHERE folder_tag = ?1 COLLATE NOCASE",
        params![tag],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if files > 0 {
        // Sessions refer to files, and are all rebuilt afterwards anyway.
        tx.execute("DELETE FROM session_nicks", [])?;
        tx.execute("DELETE FROM sessions", [])?;
        tx.execute("DELETE FROM file_totals", [])?;
        tx.execute(
            "DELETE FROM events WHERE file_id IN
                 (SELECT id FROM files WHERE folder_tag = ?1 COLLATE NOCASE)",
            params![tag],
        )?;
        tx.execute(
            "DELETE FROM files WHERE folder_tag = ?1 COLLATE NOCASE",
            params![tag],
        )?;
    }
    // An ignored tag leaves its network, if it was in one.
    tx.execute("DELETE FROM network_tags WHERE tag = ?1", params![tag])?;
    tx.execute(
        "INSERT INTO ignored_tags (tag, note) VALUES (?1, ?2)",
        params![tag, note],
    )?;
    tx.commit()?;
    Ok(Deleted { files, lines })
}

/// Imports a network tag again from the next `wot analyze`.
pub fn unignore_tag(conn: &Connection, tag: &str) -> Result<()> {
    let removed = conn.execute(
        "DELETE FROM ignored_tags WHERE tag = ?1",
        params![tag.trim()],
    )?;
    if removed == 0 {
        return Err(format!("network tag {tag} is not ignored").into());
    }
    Ok(())
}

/// A folder tag with what was imported from it, and its network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderTag {
    pub folder: String,
    /// The network it was linked into, if any.
    pub network: Option<String>,
    /// Marked as logged through a bouncer.
    pub bouncer: bool,
    pub files: i64,
    pub channels: i64,
    pub lines: i64,
    pub bytes: i64,
}

/// Every folder tag imported, the biggest first.
pub fn folder_tags(conn: &Connection) -> Result<Vec<FolderTag>> {
    let mut stmt = conn.prepare(
        "SELECT f.folder_tag, n.name, count(*), sum(f.kind = 'channel'),
                ifnull(sum(f.lines_read), 0), ifnull(sum(f.bytes_read), 0),
                EXISTS (SELECT 1 FROM bouncer_tags b WHERE b.tag = f.folder_tag)
         FROM files f
         LEFT JOIN network_tags t ON t.tag = f.folder_tag
         LEFT JOIN networks n ON n.id = t.network_id
         GROUP BY f.folder_tag COLLATE NOCASE
         ORDER BY sum(f.bytes_read) DESC, f.folder_tag",
    )?;
    let tags = stmt
        .query_map([], |row| {
            Ok(FolderTag {
                folder: row.get(0)?,
                network: row.get(1)?,
                files: row.get(2)?,
                channels: row.get(3)?,
                lines: row.get(4)?,
                bytes: row.get(5)?,
                bouncer: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(tags)
}

/// The ignored tags: tag, note and since when.
pub fn ignored_tag_list(conn: &Connection) -> Result<Vec<(String, Option<String>, String)>> {
    let mut stmt = conn.prepare("SELECT tag, note, created_at FROM ignored_tags ORDER BY tag")?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Lists every imported folder tag with its size and network, then the
/// ignored ones.
pub fn list_tags(conn: &Connection, out: &mut impl Write) -> Result<()> {
    let tags = folder_tags(conn)?;
    heading(out, "Network tags", '=')?;
    if tags.is_empty() {
        writeln!(out, "Nothing imported yet.")?;
    } else {
        let rows: Vec<Vec<String>> = tags
            .iter()
            .map(|tag| {
                vec![
                    tag.folder.clone(),
                    tag.network.clone().unwrap_or_else(|| "-".into()),
                    if tag.bouncer { "yes" } else { "-" }.to_string(),
                    thousands(tag.files as u64),
                    thousands(tag.channels as u64),
                    thousands(tag.lines as u64),
                    progress::bytes(tag.bytes as u64),
                ]
            })
            .collect();
        table(
            out,
            &[
                "Tag", "Network", "Bouncer", "Files", "Channels", "Lines", "Size",
            ],
            &rows,
            &[false, false, false, true, true, true, true],
        )?;
        writeln!(out)?;
        writeln!(
            out,
            "Link tags into a network with wot networks create NAME TAG..., or leave one out of \
             imports and delete its lines with wot tags ignore TAG."
        )?;
    }

    let ignored = ignored_tag_list(conn)?;
    if !ignored.is_empty() {
        writeln!(out)?;
        heading(out, "Ignored tags", '-')?;
        let rows: Vec<Vec<String>> = ignored
            .into_iter()
            .map(|(tag, note, created_at)| {
                vec![tag, day(&created_at).to_string(), note.unwrap_or_default()]
            })
            .collect();
        table(
            out,
            &["Tag", "Since", "Note"],
            &rows,
            &[false, false, false],
        )?;
    }
    Ok(())
}

// Bouncers

/// Marks a folder tag as logged through a bouncer, such as ZNC: its playback
/// markers are read for replayed lines, and its copies of lines logged twice
/// are the ones left out. Identities have to be rebuilt afterwards.
pub fn mark_bouncer(conn: &Connection, tag: &str) -> Result<()> {
    let folder = imported_folder(conn, tag)?;
    let added = conn.execute(
        "INSERT OR IGNORE INTO bouncer_tags (tag) VALUES (?1)",
        params![folder],
    )?;
    if added == 0 {
        return Err(format!("network tag {folder} is marked as a bouncer already").into());
    }
    Ok(())
}

/// Takes the bouncer mark off a folder tag. Identities have to be rebuilt
/// afterwards.
pub fn unmark_bouncer(conn: &Connection, tag: &str) -> Result<()> {
    let removed = conn.execute(
        "DELETE FROM bouncer_tags WHERE tag = ?1",
        params![tag.trim()],
    )?;
    if removed == 0 {
        return Err(format!("network tag {tag} is not marked as a bouncer").into());
    }
    Ok(())
}

// Networks

/// The decision tables that refer to a network tag, which move with it.
const TAGGED_DECISIONS: [&str; 4] = [
    "host_groups",
    "host_group_members",
    "channel_reviews",
    "who_lists",
];

fn network_id(conn: &Connection, name: &str) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM networks WHERE name = ?1",
        params![name],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| format!("there is no network named {name}").into())
}

/// A folder tag as imported, or an error when nothing was imported from it.
fn imported_folder(conn: &Connection, tag: &str) -> Result<String> {
    conn.query_row(
        "SELECT folder_tag FROM files WHERE folder_tag = ?1 COLLATE NOCASE LIMIT 1",
        params![tag.trim()],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| {
        format!("nothing from network tag {tag} has been imported; see wot tags list").into()
    })
}

/// Links folder tags into a network: their files, and the decisions made
/// under them, move to the network's name. Identities have to be rebuilt
/// afterwards, which links them across the tags.
fn link_tags(conn: &Connection, network: i64, name: &str, tags: &[String]) -> Result<()> {
    for tag in tags {
        let folder = imported_folder(conn, tag)?;
        let current: Option<String> = conn
            .query_row(
                "SELECT n.name FROM network_tags t JOIN networks n ON n.id = t.network_id
                 WHERE t.tag = ?1",
                params![folder],
                |row| row.get(0),
            )
            .optional()?;
        match current {
            Some(current) if current.eq_ignore_ascii_case(name) => continue,
            Some(current) => {
                return Err(format!(
                    "network tag {folder} belongs to network {current}; remove it from there first"
                )
                .into());
            }
            None => {}
        }
        conn.execute(
            "INSERT INTO network_tags (tag, network_id) VALUES (?1, ?2)",
            params![folder, network],
        )?;
        conn.execute(
            "UPDATE files SET tag = ?1 WHERE folder_tag = ?2 COLLATE NOCASE",
            params![name, folder],
        )?;
        for table in TAGGED_DECISIONS {
            conn.execute(
                &format!("UPDATE {table} SET tag = ?1 WHERE tag = ?2 COLLATE NOCASE"),
                params![name, folder],
            )
            .map_err(|err| match err {
                rusqlite::Error::SqliteFailure(failure, _)
                    if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    format!(
                        "network tag {folder} has a host group or host with the same name as \
                         one already in {name}; rename or remove one of them first"
                    )
                    .into()
                }
                err => Box::<dyn std::error::Error + Send + Sync>::from(err),
            })?;
        }
    }
    Ok(())
}

/// Creates a network from folder tags, such as a server's old and new
/// hostnames, or a bouncer's and a client's. Identities have to be rebuilt
/// afterwards.
pub fn create_network(
    conn: &mut Connection,
    name: &str,
    tags: &[String],
    note: Option<&str>,
) -> Result<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err("give the network a name".into());
    }
    if network_id(conn, name).is_ok() {
        return Err(format!("a network named {name} already exists").into());
    }
    if tags.is_empty() {
        return Err("give at least one network tag to link".into());
    }
    // Files of another tag already under this name would join it unasked.
    let mut stmt =
        conn.prepare("SELECT DISTINCT folder_tag FROM files WHERE tag = ?1 COLLATE NOCASE")?;
    let others: Vec<String> = stmt
        .query_map(params![name], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .into_iter()
        .filter(|folder| {
            !tags
                .iter()
                .any(|tag| tag.trim().eq_ignore_ascii_case(folder))
        })
        .collect();
    drop(stmt);
    if let Some(other) = others.first() {
        return Err(format!(
            "network tag {other} is already called {name}; link it into the network too, or \
             choose another name"
        )
        .into());
    }
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO networks (name, note) VALUES (?1, ?2)",
        params![name, note],
    )?;
    let id = tx.last_insert_rowid();
    link_tags(&tx, id, name, tags)?;
    tx.commit()?;
    Ok(())
}

/// Links more folder tags into a network. Identities have to be rebuilt
/// afterwards.
pub fn add_to_network(conn: &mut Connection, name: &str, tags: &[String]) -> Result<()> {
    let id = network_id(conn, name)?;
    let name: String = conn.query_row(
        "SELECT name FROM networks WHERE id = ?1",
        params![id],
        |row| row.get(0),
    )?;
    let tx = conn.transaction()?;
    link_tags(&tx, id, &name, tags)?;
    tx.commit()?;
    Ok(())
}

/// Takes folder tags out of their network. Their files go back to their own
/// tag; decisions made while linked stay with the network. Identities have
/// to be rebuilt afterwards; they keep their ids where they can.
pub fn remove_from_network(conn: &mut Connection, tags: &[String]) -> Result<()> {
    let tx = conn.transaction()?;
    let mut removed = 0;
    for tag in tags {
        let linked: Option<(String, String)> = tx
            .query_row(
                "SELECT t.tag, n.name FROM network_tags t JOIN networks n ON n.id = t.network_id
                 WHERE t.tag = ?1",
                params![tag.trim()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((folder, network)) = linked else {
            continue;
        };
        tx.execute("DELETE FROM network_tags WHERE tag = ?1", params![folder])?;
        tx.execute(
            "UPDATE files SET tag = folder_tag WHERE folder_tag = ?1 COLLATE NOCASE",
            params![folder],
        )?;
        // The network's keys, copied to the folder tag, let its identities
        // keep their ids when rebuilt.
        tx.execute(
            "INSERT OR IGNORE INTO identity_keys (tag, nick, ident, host, identity_id)
             SELECT ?1, nick, ident, host, identity_id FROM identity_keys WHERE tag = ?2",
            params![folder, network],
        )?;
        removed += 1;
    }
    if removed == 0 {
        return Err(match tags {
            [tag] => format!("network tag {tag} is not in a network"),
            _ => "none of these network tags are in a network".to_string(),
        }
        .into());
    }
    tx.commit()?;
    Ok(())
}

/// Deletes a network, taking all its tags out of it first.
pub fn delete_network(conn: &mut Connection, name: &str) -> Result<()> {
    let id = network_id(conn, name)?;
    let tags: Vec<String> = {
        let mut stmt = conn.prepare("SELECT tag FROM network_tags WHERE network_id = ?1")?;
        let rows = stmt
            .query_map(params![id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        rows
    };
    if !tags.is_empty() {
        remove_from_network(conn, &tags)?;
    }
    conn.execute("DELETE FROM networks WHERE id = ?1", params![id])?;
    Ok(())
}

/// A network with its folder tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Network {
    pub name: String,
    pub note: Option<String>,
    pub tags: Vec<String>,
}

pub fn networks(conn: &Connection) -> Result<Vec<Network>> {
    let mut stmt = conn.prepare(
        "SELECT n.name, n.note, t.tag FROM networks n
         LEFT JOIN network_tags t ON t.network_id = n.id
         ORDER BY n.name, t.tag",
    )?;
    let rows: Vec<(String, Option<String>, Option<String>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut networks: Vec<Network> = Vec::new();
    for (name, note, tag) in rows {
        if networks.last().is_none_or(|last| last.name != name) {
            networks.push(Network {
                name,
                note,
                tags: Vec::new(),
            });
        }
        if let (Some(tag), Some(network)) = (tag, networks.last_mut()) {
            network.tags.push(tag);
        }
    }
    Ok(networks)
}

pub fn list_networks(conn: &Connection, out: &mut impl Write) -> Result<()> {
    let networks = networks(conn)?;
    heading(out, "Networks", '=')?;
    if networks.is_empty() {
        writeln!(
            out,
            "No networks yet. Link network tags into one with wot networks create NAME TAG..."
        )?;
        return Ok(());
    }
    let rows: Vec<Vec<String>> = networks
        .into_iter()
        .map(|network| {
            vec![
                network.name,
                network.tags.join(", "),
                network.note.unwrap_or_default(),
            ]
        })
        .collect();
    table(
        out,
        &["Network", "Tags", "Note"],
        &rows,
        &[false, false, false],
    )?;
    Ok(())
}

// Channel reviews

/// Records that you went through a channel's users now. A review stays valid
/// for six months.
pub fn mark_reviewed(conn: &Connection, tag: &str, channel: &str) -> Result<()> {
    let known: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM files WHERE tag = ?1 AND target = ?2 COLLATE NOCASE
                                              AND kind = 'channel')",
        params![tag, channel],
        |row| row.get(0),
    )?;
    if !known {
        return Err(format!("no log of {channel} in network tag {tag} has been imported").into());
    }
    conn.execute(
        "INSERT INTO channel_reviews (tag, channel) VALUES (?1, ?2)",
        params![tag, channel],
    )?;
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
        // Logs are named by the folder they are in, not their network.
        let mut stmt =
            conn.prepare("SELECT id, path FROM files WHERE folder_tag = ?1 AND target = ?2")?;
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
    fn rates_notes_and_invites_people() {
        let (_root, mut conn) = setup("decide-trust");
        for name in ["rachel", "quokka", "marlowe"] {
            create_person(&mut conn, name, Kind::Person, &[], None).unwrap();
        }
        create_person(&mut conn, "relay", Kind::Bot, &[], None).unwrap();

        set_rating(&conn, "quokka", Rating::Vouched).unwrap();
        assert!(
            set_rating(&conn, "relay", Rating::Known).is_err(),
            "bots are not rated"
        );
        assert!(set_rating(&conn, "nobody", Rating::Known).is_err());

        add_note(&conn, "quokka", "  met at a meetup  ").unwrap();
        assert!(add_note(&conn, "quokka", "   ").is_err());
        let person = stats::person(&conn, "quokka").unwrap().unwrap();
        assert_eq!(person.rating, Rating::Vouched);
        assert_eq!(person.notes.len(), 1);
        assert_eq!(person.notes[0].text, "met at a meetup");
        assert!(
            delete_note(&conn, "marlowe", person.notes[0].id).is_err(),
            "not theirs"
        );
        delete_note(&conn, "quokka", person.notes[0].id).unwrap();

        set_invited_by(&conn, "quokka", Some("rachel")).unwrap();
        set_invited_by(&conn, "marlowe", Some("quokka")).unwrap();
        assert!(
            set_invited_by(&conn, "rachel", Some("marlowe")).is_err(),
            "a circle"
        );
        assert!(set_invited_by(&conn, "rachel", Some("rachel")).is_err());
        assert!(
            set_invited_by(&conn, "rachel", Some("relay")).is_err(),
            "bots do not invite"
        );
        let rachel = stats::person(&conn, "rachel").unwrap().unwrap();
        assert_eq!(rachel.invited, ["quokka"]);
        assert_eq!(
            stats::person(&conn, "marlowe")
                .unwrap()
                .unwrap()
                .invited_by
                .as_deref(),
            Some("quokka")
        );
        // Deleting the inviter leaves the invited without one.
        delete_person(&conn, "quokka").unwrap();
        let marlowe = stats::person(&conn, "marlowe").unwrap().unwrap();
        assert_eq!(marlowe.invited_by, None);
        set_invited_by(&conn, "marlowe", None).unwrap();
    }

    #[test]
    fn links_network_tags_into_one_network() {
        let root = TempDir::new("decide-networks");
        for (folder, day) in [
            ("old.example.net", "Sun Nov 01"),
            ("irc.example.net", "Mon Nov 02"),
        ] {
            fs::create_dir_all(root.0.join(folder)).unwrap();
            fs::write(
                root.0.join(folder).join("#analysis.log"),
                format!(
                    "--- Log opened {day} 22:00:00 2026\n\
                     22:00 -!- quokka [quokkawet@hostQ] has joined #analysis\n\
                     22:01 <quokka> hello\n"
                ),
            )
            .unwrap();
        }
        let mut conn = db::open(&root.0.join("wot.sqlite")).unwrap();
        let inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        identity::rebuild(&mut conn).unwrap();
        let quokkas = |conn: &Connection| -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT DISTINCT identity_id FROM identity_keys WHERE nick = 'quokka'")
                .unwrap();
            let ids = stmt
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            ids
        };
        let before = quokkas(&conn);
        assert_eq!(before.len(), 2, "one identity per tag");
        create_person(&mut conn, "quokka", Kind::Person, &before[..1], None).unwrap();

        let tags = ["old.example.net".to_string(), "IRC.example.net".to_string()];
        create_network(&mut conn, "example", &tags, Some("renamed server")).unwrap();
        assert!(create_network(&mut conn, "EXAMPLE", &tags[..1], None).is_err());
        assert!(add_to_network(&mut conn, "example", &["nowhere".into()]).is_err());
        identity::rebuild(&mut conn).unwrap();

        // One identity across both tags, still the person's.
        let after = quokkas(&conn);
        assert_eq!(after.len(), 1);
        assert!(before.contains(&after[0]), "an old id is kept");
        let person = stats::person(&conn, "quokka").unwrap().unwrap();
        assert_eq!(person.identities[0].current, after[0]);
        let tagged: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE tag = 'example'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tagged, 2);
        let listed = folder_tags(&conn).unwrap();
        assert!(
            listed
                .iter()
                .all(|tag| tag.network.as_deref() == Some("example"))
        );
        assert_eq!(networks(&conn).unwrap()[0].tags.len(), 2);
        assert!(listed.iter().all(|tag| !tag.bouncer));
        mark_bouncer(&conn, "IRC.example.net").unwrap();
        assert!(
            mark_bouncer(&conn, "irc.example.net").is_err(),
            "marked already"
        );
        assert!(mark_bouncer(&conn, "nowhere").is_err());
        let marked: Vec<String> = folder_tags(&conn)
            .unwrap()
            .into_iter()
            .filter(|tag| tag.bouncer)
            .map(|tag| tag.folder)
            .collect();
        assert_eq!(marked, ["irc.example.net"]);
        unmark_bouncer(&conn, "irc.example.net").unwrap();
        assert!(unmark_bouncer(&conn, "irc.example.net").is_err());

        // Taken apart again, each tag has its own identity, one with its id.
        remove_from_network(&mut conn, &tags[..1]).unwrap();
        assert!(remove_from_network(&mut conn, &tags[..1]).is_err());
        identity::rebuild(&mut conn).unwrap();
        let split = quokkas(&conn);
        assert_eq!(split.len(), 2);
        assert!(split.contains(&after[0]));
        delete_network(&mut conn, "example").unwrap();
        assert!(networks(&conn).unwrap().is_empty());
        let separate: i64 = conn
            .query_row(
                "SELECT count(*) FROM files WHERE tag = folder_tag",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(separate, 2);
    }

    #[test]
    fn ignores_a_tag_and_deletes_its_lines() {
        let (root, mut conn) = setup("decide-ignore");
        fs::create_dir_all(root.0.join("rizon")).unwrap();
        fs::write(
            root.0.join("rizon/#busy.log"),
            "--- Log opened Sun Nov 01 01:00:00 2026\n01:00 <someone> hi\n",
        )
        .unwrap();
        let mut inventory = discover::discover(&[root.0.clone()]).unwrap();
        db::import(&mut conn, &inventory.files);
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT count(*) FROM files WHERE tag = 'rizon'"), 1);

        let deleted = ignore_tag(&mut conn, "RIZON", Some("public network")).unwrap();
        assert_eq!(deleted, Deleted { files: 1, lines: 2 });
        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT count(*) FROM files WHERE tag = 'rizon'"), 0);
        assert_eq!(
            count("SELECT count(*) FROM events WHERE text = 'hi'"),
            0,
            "its lines are gone"
        );
        assert!(count("SELECT count(*) FROM files WHERE tag = 'wetfish'") > 0);
        assert!(
            ignore_tag(&mut conn, "rizon", None).is_err(),
            "ignored already"
        );

        // Later imports leave its files out.
        let left_out = leave_out_ignored(&conn, &mut inventory.files).unwrap();
        assert_eq!(left_out, [("rizon".to_string(), 1)]);
        assert!(inventory.files.iter().all(|file| file.tag == "wetfish"));
        identity::rebuild(&mut conn).unwrap();

        let mut out = Vec::new();
        list_tags(&conn, &mut out).unwrap();
        let listed = String::from_utf8(out).unwrap();
        assert!(listed.contains("wetfish"));
        assert!(listed.contains("public network"));

        unignore_tag(&conn, "Rizon").unwrap();
        assert!(unignore_tag(&conn, "rizon").is_err());
        // A tag never imported can be ignored before its first import.
        assert_eq!(
            ignore_tag(&mut conn, "libera", None).unwrap(),
            Deleted::default()
        );
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
