//! Translates search queries and browsing scopes into SQL.

use medianest_core::query::{Filter, Query, Term};
use medianest_core::{AlbumId, FileStatus, Format, PathBytes, SourceId, normalize_tag};
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

/// Which part of the library a view looks at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    /// Everything not hidden, trashed, or a Live Photo's video half.
    Library,
    /// Recently deleted items.
    Trash,
    /// The contents of a manual album.
    Album(AlbumId),
    /// Assets with a file inside this folder of a source (recursively).
    Directory {
        source: SourceId,
        relpath: PathBytes,
    },
    /// Assets added since this time.
    AddedSince(i64),
}

/// Ordering of a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    NewestFirst,
    OldestFirst,
    RecentlyAdded,
    /// The album's own order. Only meaningful with [`Scope::Album`].
    AlbumOrder,
}

/// A scope narrowed by a query, in a given order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub scope: Scope,
    pub query: Query,
    pub sort: Sort,
}

impl View {
    #[must_use]
    pub fn library() -> Self {
        Self {
            scope: Scope::Library,
            query: Query::default(),
            sort: Sort::NewestFirst,
        }
    }

    #[must_use]
    pub fn with_query(mut self, query: Query) -> Self {
        self.query = query;
        self
    }

    #[must_use]
    pub fn sorted(mut self, sort: Sort) -> Self {
        self.sort = sort;
        self
    }
}

/// SQL fragments for `FROM … WHERE … ORDER BY …` with positional parameters.
pub(crate) struct CompiledView {
    pub from_where: String,
    pub order_by: &'static str,
    pub params: Vec<Value>,
}

/// Builds an FTS5 query that requires every word, each as a prefix.
pub(crate) fn fts_match(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|ch: char| !ch.is_alphanumeric() && ch != '\'' && ch != '_')
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

fn date_bounds(range: &medianest_core::query::DateRange) -> (i64, i64) {
    let start = range
        .start
        .and_hms_opt(0, 0, 0)
        .map(|value| value.and_utc().timestamp_millis());
    let end = range
        .end
        .and_hms_opt(0, 0, 0)
        .map(|value| value.and_utc().timestamp_millis());
    (start.unwrap_or(i64::MIN), end.unwrap_or(i64::MAX))
}

/// Upper bound for a byte-string prefix range: every path starting with
/// `prefix/` sorts below `prefix0` because `0` follows `/` in ASCII.
pub(crate) fn directory_bounds(relpath: &PathBytes) -> (Vec<u8>, Vec<u8>) {
    let mut lower = relpath.as_bytes().to_vec();
    while lower.last() == Some(&b'/') {
        lower.pop();
    }
    if lower.is_empty() {
        return (Vec::new(), vec![0xff; 8]);
    }
    let mut upper = lower.clone();
    lower.push(b'/');
    upper.push(b'0');
    (lower, upper)
}

fn term_sql(term: &Term, params: &mut Vec<Value>) -> Option<String> {
    let mut push = |value: Value| {
        params.push(value);
        "?".to_owned()
    };
    let sql = match &term.filter {
        Filter::Text(text) => {
            let matcher = fts_match(text)?;
            let placeholder = push(Value::Text(matcher));
            format!("a.id IN (SELECT rowid FROM assets_fts WHERE assets_fts MATCH {placeholder})")
        }
        Filter::Tag(name) => {
            let placeholder = push(Value::Text(normalize_tag(name)));
            format!(
                "EXISTS (SELECT 1 FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
                 WHERE at.asset_id = a.id AND t.norm = {placeholder})"
            )
        }
        Filter::Album(name) => {
            let placeholder = push(Value::Text(name.trim().to_owned()));
            format!(
                "EXISTS (SELECT 1 FROM album_assets aa JOIN albums al ON al.id = aa.album_id \
                 WHERE aa.asset_id = a.id AND al.name = {placeholder} COLLATE NOCASE)"
            )
        }
        Filter::Folder(fragment) => {
            let placeholder = push(Value::Text(fragment.to_lowercase()));
            format!(
                "EXISTS (SELECT 1 FROM files f JOIN sources s ON s.id = f.source_id \
                 WHERE f.asset_id = a.id AND instr(lower(CAST(s.path || '/' || f.relpath AS TEXT)), {placeholder}) > 0)"
            )
        }
        Filter::Camera(value) => {
            let placeholder = push(Value::Text(format!("%{}%", like_escape(value))));
            format!(
                "(coalesce(a.camera_make, '') || ' ' || coalesce(a.camera_model, '')) LIKE {placeholder} ESCAPE '\\'"
            )
        }
        Filter::Place(value) => {
            let placeholder = push(Value::Text(format!("%{}%", like_escape(value))));
            format!(
                "(coalesce(a.place_city, '') || ' ' || coalesce(a.place_region, '') || ' ' || coalesce(a.place_country, '')) LIKE {placeholder} ESCAPE '\\'"
            )
        }
        Filter::Date(range) => {
            let (start, end) = date_bounds(range);
            let start = push(Value::Integer(start));
            let end = push(Value::Integer(end));
            format!("(a.taken_local >= {start} AND a.taken_local < {end})")
        }
        Filter::Month(month) => {
            let placeholder = push(Value::Integer(i64::from(*month)));
            format!(
                "CAST(strftime('%m', a.taken_local / 1000, 'unixepoch') AS INTEGER) = {placeholder}"
            )
        }
        Filter::DateUnknown => "a.taken_local IS NULL".to_owned(),
        Filter::DateUncertain => {
            "(a.taken_local IS NOT NULL AND a.taken_confidence <= 1)".to_owned()
        }
        Filter::Kind(kind) => format!("a.kind = {}", kind.code()),
        Filter::Format(format) => {
            let placeholder = push(Value::Text(format.code().to_owned()));
            format!("a.format = {placeholder}")
        }
        Filter::Favorite => "a.favorite = 1".to_owned(),
        Filter::Rating(comparison, value) => {
            format!("a.rating {} {}", comparison.sql(), value)
        }
        Filter::HasLocation => "a.latitude IS NOT NULL".to_owned(),
        Filter::HasTags => {
            "EXISTS (SELECT 1 FROM asset_tags at WHERE at.asset_id = a.id)".to_owned()
        }
        Filter::Live => "(a.live_video_id IS NOT NULL OR a.motion_offset IS NOT NULL)".to_owned(),
        Filter::Raw => {
            let raws: Vec<String> = Format::ALL
                .iter()
                .filter(|format| format.is_raw())
                .map(|format| format!("'{}'", format.code()))
                .collect();
            format!("a.format IN ({})", raws.join(", "))
        }
        Filter::Screenshot => "a.screenshot = 1".to_owned(),
        Filter::Hidden => "a.hidden = 1".to_owned(),
        Filter::Problem => format!(
            "a.file_state IN ({}, {})",
            FileStatus::Missing.code(),
            FileStatus::Changed.code()
        ),
    };
    Some(if term.negated {
        format!("NOT coalesce(({sql}), 0)")
    } else {
        sql
    })
}

fn like_escape(value: &str) -> String {
    value
        .trim()
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn compile(view: &View) -> CompiledView {
    let mut params = Vec::new();
    let mut from = String::from("FROM assets a");
    let mut conditions: Vec<String> = Vec::new();
    match &view.scope {
        Scope::Library | Scope::AddedSince(_) => {
            conditions.push("a.trashed_ms IS NULL".into());
            conditions.push("a.live_component = 0".into());
            if !view.query.wants_hidden() {
                conditions.push("a.hidden = 0".into());
            }
            if let Scope::AddedSince(since) = view.scope {
                params.push(Value::Integer(since));
                conditions.push("a.added_ms >= ?".into());
            }
        }
        Scope::Trash => conditions.push("a.trashed_ms IS NOT NULL".into()),
        Scope::Album(album) => {
            params.push(Value::Integer(album.get()));
            from.push_str(" JOIN album_assets aa ON aa.asset_id = a.id AND aa.album_id = ?");
            conditions.push("a.trashed_ms IS NULL".into());
        }
        Scope::Directory { source, relpath } => {
            let (lower, upper) = directory_bounds(relpath);
            params.push(Value::Integer(source.get()));
            params.push(Value::Blob(lower));
            params.push(Value::Blob(upper));
            conditions.push(
                "EXISTS (SELECT 1 FROM files f WHERE f.asset_id = a.id AND f.source_id = ? \
                 AND f.relpath >= ? AND f.relpath < ?)"
                    .into(),
            );
            conditions.push("a.trashed_ms IS NULL".into());
            conditions.push("a.live_component = 0".into());
        }
    }
    for term in &view.query.terms {
        if let Some(sql) = term_sql(term, &mut params) {
            conditions.push(sql);
        }
    }
    let order_by = match (view.sort, &view.scope) {
        (Sort::AlbumOrder, Scope::Album(_)) => "ORDER BY aa.position, a.id",
        (Sort::OldestFirst, _) => "ORDER BY a.taken_local IS NULL, a.taken_local ASC, a.id ASC",
        (Sort::RecentlyAdded, _) => "ORDER BY a.added_ms DESC, a.id DESC",
        (Sort::NewestFirst | Sort::AlbumOrder, _) => {
            "ORDER BY a.taken_local IS NULL, a.taken_local DESC, a.id DESC"
        }
    };
    let from_where = if conditions.is_empty() {
        from
    } else {
        format!("{from} WHERE {}", conditions.join(" AND "))
    };
    CompiledView {
        from_where,
        order_by,
        params,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_match_quotes_and_prefixes_words() {
        assert_eq!(
            fts_match("beach day").as_deref(),
            Some("\"beach\"* \"day\"*")
        );
        assert_eq!(fts_match("say \"hi\"").as_deref(), Some("\"say\"* \"hi\"*"));
        assert_eq!(fts_match(" - "), None);
    }

    #[test]
    fn directory_bounds_cover_children_only() {
        let (lower, upper) = directory_bounds(&PathBytes::from_vec(b"2023/trip/".to_vec()));
        assert_eq!(lower, b"2023/trip/".to_vec());
        assert_eq!(upper, b"2023/trip0".to_vec());
        let child: &[u8] = b"2023/trip/a.jpg";
        let sibling: &[u8] = b"2023/trips/a.jpg";
        assert!(child >= lower.as_slice() && child < upper.as_slice());
        assert!(!(sibling >= lower.as_slice() && sibling < upper.as_slice()));
    }
}
