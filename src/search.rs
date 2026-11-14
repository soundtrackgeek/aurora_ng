//! Aurora's catalog search language, used to filter albums.
//!
//! Ported from Aurora (`src-tauri/src/catalog.rs`) so the same queries give
//! the same albums. `docs/search.md` in Aurora describes the language. Aurora NG
//! reads the catalog only, so Aurora's pending edits and audio analysis
//! (`sonic:`) are not part of it.

use rusqlite::{Connection, params_from_iter, types::Value};
use std::collections::HashSet;

const MAX_TERMS: usize = 32;
const MAX_ALTERNATIVES: usize = 32;
pub const MAX_QUERY_CHARS: usize = 256;

/// The canonical genres `genre:scores` stands for.
const SCORE_GENRES: [&str; 13] = [
    "action",
    "animation",
    "comedy",
    "documentary",
    "drama",
    "fantasy",
    "horror",
    "sci-fi",
    "thriller",
    "tv",
    "video game",
    "western",
    "anime",
];

/// Columns an exact value without a field is compared with.
const EXACT_TEXT_COLUMNS: [&str; 6] = [
    "display_artist",
    "album_artist_display",
    "album",
    "canonical_genre",
    "publisher",
    "title",
];

#[derive(Clone, Copy, Debug, PartialEq)]
enum Extended {
    Dissolved,
    Founded,
    Dead,
    Born,
    Minutes,
    AlbumRating,
    Billboard,
    Uk,
    Vg,
    Ti,
    Nt,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Field {
    Any,
    Artist,
    AlbumArtist,
    Album,
    Genre,
    Year,
    ReleaseYear,
    Publisher,
    Country,
    Title,
    Completeness,
    Love,
    Single,
    Sonic,
    Extended(Extended),
}

impl Field {
    fn column(self) -> Option<&'static str> {
        match self {
            Self::Artist => Some("display_artist"),
            Self::AlbumArtist => Some("album_artist_display"),
            Self::Album => Some("album"),
            Self::Genre => Some("canonical_genre"),
            Self::Publisher => Some("publisher"),
            Self::Title => Some("title"),
            _ => None,
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Some(match name.trim().to_ascii_lowercase().as_str() {
            "dissolved" => Self::Extended(Extended::Dissolved),
            "founded" => Self::Extended(Extended::Founded),
            "dead" => Self::Extended(Extended::Dead),
            "born" => Self::Extended(Extended::Born),
            "minutes" => Self::Extended(Extended::Minutes),
            "ar" => Self::Extended(Extended::AlbumRating),
            "bb" => Self::Extended(Extended::Billboard),
            "uk" => Self::Extended(Extended::Uk),
            "vg" => Self::Extended(Extended::Vg),
            "ti" => Self::Extended(Extended::Ti),
            "nt" => Self::Extended(Extended::Nt),
            "artist" => Self::Artist,
            "aartist" => Self::AlbumArtist,
            "album" => Self::Album,
            "genre" => Self::Genre,
            "year" => Self::Year,
            "ryear" => Self::ReleaseYear,
            "publisher" => Self::Publisher,
            "country" => Self::Country,
            "title" => Self::Title,
            "cr" => Self::Completeness,
            "love" => Self::Love,
            "single" | "singles" => Self::Single,
            "sonic" => Self::Sonic,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Match {
    /// A full-text query for word prefixes.
    Prefix(String),
    Exact(String),
    Country {
        value: String,
        exact: bool,
    },
    ScoreGenres,
    Years {
        from: Option<i32>,
        to: Option<i32>,
    },
    Completeness {
        from: Option<u32>,
        to: Option<u32>,
    },
    LovedTracks {
        from: Option<u32>,
        to: Option<u32>,
    },
    Singles {
        from: Option<u32>,
        to: Option<u32>,
    },
    Extended {
        field: Extended,
        from: Option<f64>,
        to: Option<f64>,
        present: Option<bool>,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct Alternative {
    field: Field,
    matcher: Match,
}

/// Alternatives joined by OR; the whole group may be negated.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    negated: bool,
    alternatives: Vec<Alternative>,
}

/// A parsed query: every group must match.
#[derive(Clone, Debug, PartialEq)]
pub struct Search {
    groups: Vec<Group>,
}

impl Search {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

// ── Parsing ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
enum Operator {
    And,
    Or,
    Not,
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Text(String),
    Operator(Operator),
}

fn push_text(tokens: &mut Vec<Token>, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        tokens.push(Token::Text(value.to_owned()));
    }
}

/// Splits on commas and the uppercase words AND, OR and NOT, outside quotes.
fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let characters: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut value = String::new();
    let mut quoted = false;
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];
        if character == '"' {
            quoted = !quoted;
            value.push(character);
            index += 1;
            continue;
        }
        if !quoted && character == ',' {
            push_text(&mut tokens, &value);
            value.clear();
            tokens.push(Token::Operator(Operator::And));
            index += 1;
            continue;
        }
        let at_word_start =
            index == 0 || characters[index - 1].is_whitespace() || characters[index - 1] == ',';
        if !quoted && at_word_start && character.is_ascii_alphabetic() {
            let mut end = index;
            while end < characters.len() && characters[end].is_ascii_alphabetic() {
                end += 1;
            }
            let at_word_end = end == characters.len()
                || characters[end].is_whitespace()
                || characters[end] == ',';
            if at_word_end {
                let word: String = characters[index..end].iter().collect();
                let operator = match word.as_str() {
                    "AND" => Some(Operator::And),
                    "OR" => Some(Operator::Or),
                    "NOT" => Some(Operator::Not),
                    _ => None,
                };
                if let Some(operator) = operator {
                    push_text(&mut tokens, &value);
                    value.clear();
                    tokens.push(Token::Operator(operator));
                    index = end;
                    continue;
                }
            }
        }
        value.push(character);
        index += 1;
    }

    if quoted {
        return Err("Search quotes are not closed.".to_owned());
    }
    push_text(&mut tokens, &value);
    Ok(tokens)
}

/// `field:value` or `field=value`, when the part before the separator is a field.
fn split_field(value: &str) -> Option<(Field, &str)> {
    let (separator, _) = value
        .char_indices()
        .find(|(_, character)| matches!(character, ':' | '='))?;
    let field = Field::parse(&value[..separator])?;
    Some((field, value[separator + 1..].trim()))
}

fn parse_extended(field: Extended, value: &str) -> Result<Match, String> {
    use Extended::*;
    if matches!(field, Dissolved | Dead | Billboard | Uk | Vg | Ti | Nt)
        && matches!(value.to_ascii_lowercase().as_str(), "yes" | "no")
    {
        return Ok(Match::Extended {
            field,
            from: None,
            to: None,
            present: Some(value.eq_ignore_ascii_case("yes")),
        });
    }
    let number = |raw: &str| -> Result<f64, String> {
        let n = raw.parse::<f64>().map_err(|_| {
            "Search requires a number, range, or supported yes/no value.".to_owned()
        })?;
        let valid = n.is_finite()
            && n >= 0.0
            && match field {
                AlbumRating => n <= 5.0,
                Minutes => true,
                Dissolved | Founded | Dead | Born => {
                    n.fract() == 0.0 && (1.0..=9999.0).contains(&n)
                }
                _ => n.fract() == 0.0 && (1.0..=i32::MAX as f64).contains(&n),
            };
        if valid {
            Ok(n)
        } else {
            Err("Search bound is outside the supported range.".to_owned())
        }
    };
    let (from, to) = if let Some((from, to)) = value.split_once("..") {
        (
            (!from.is_empty()).then(|| number(from)).transpose()?,
            (!to.is_empty()).then(|| number(to)).transpose()?,
        )
    } else {
        let n = number(value)?;
        (Some(n), Some(n))
    };
    if (from.is_none() && to.is_none()) || matches!((from, to), (Some(a), Some(b)) if a > b) {
        return Err("Search range needs an ordered lower or upper bound.".to_owned());
    }
    Ok(Match::Extended {
        field,
        from,
        to,
        present: None,
    })
}

fn number_label(field: Field) -> &'static str {
    match field {
        Field::Completeness => "cr",
        Field::Single => "single",
        _ => "love",
    }
}

fn parse_number(value: &str, field: Field) -> Result<u32, String> {
    let invalid = match field {
        Field::Completeness => "cr bounds must be whole percentages from 0 through 100.".to_owned(),
        field => format!(
            "{} bounds must be non-negative whole track counts.",
            number_label(field)
        ),
    };
    let parsed = value.trim().parse::<u32>().map_err(|_| invalid.clone())?;
    if field != Field::Completeness || parsed <= 100 {
        Ok(parsed)
    } else {
        Err(invalid)
    }
}

/// `cr:80` means 0–80; `love:1` and `single:1` mean one or more; `0` means none.
fn parse_number_range(value: &str, field: Field) -> Result<(Option<u32>, Option<u32>), String> {
    let value = value.trim();
    let name = number_label(field);
    let Some((raw_from, raw_to)) = value.split_once("..") else {
        let number = parse_number(value, field)?;
        return match field {
            Field::Completeness => Ok((Some(0), Some(number))),
            _ if number == 0 => Ok((Some(0), Some(0))),
            _ if number == 1 => Ok((Some(1), None)),
            _ => Err(format!(
                "{name} must be 0, 1, or an inclusive range such as {name}:1..3."
            )),
        };
    };
    if raw_to.contains("..") {
        return Err(format!(
            "{name} range must use one '..', for example {name}:1..3."
        ));
    }
    let from = (!raw_from.trim().is_empty())
        .then(|| parse_number(raw_from, field))
        .transpose()?;
    let to = (!raw_to.trim().is_empty())
        .then(|| parse_number(raw_to, field))
        .transpose()?;
    if from.is_none() && to.is_none() {
        return Err(format!("{name} range needs a starting or ending number."));
    }
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err(format!(
            "{name} range must start at or before its ending number."
        ));
    }
    Ok((from, to))
}

/// Every word becomes a quoted FTS5 prefix, like `album : "love"*`.
fn prefix_query(input: &str, column: Option<&str>, terms: &mut usize) -> Result<String, String> {
    let mut parts = Vec::new();
    for word in input
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
    {
        if *terms >= MAX_TERMS {
            return Err(format!("Search can contain at most {MAX_TERMS} words."));
        }
        *terms += 1;
        let bounded: String = word.chars().take(64).collect();
        let prefix = format!("\"{}\"*", bounded.replace('"', "\"\""));
        parts.push(match column {
            Some(column) => format!("{column} : {prefix}"),
            None => prefix,
        });
    }
    if parts.is_empty() {
        return Err("Search needs a word or an exact quoted value.".to_owned());
    }
    Ok(parts.join(" AND "))
}

fn parse_year(value: &str, field: &str) -> Result<i32, String> {
    let year = value
        .trim()
        .parse::<i32>()
        .map_err(|_| format!("{field} must be a year between 1000 and 2999."))?;
    if !(1000..=2999).contains(&year) {
        return Err(format!("{field} must be a year between 1000 and 2999."));
    }
    Ok(year)
}

fn parse_year_range(value: &str, field: &str) -> Result<(Option<i32>, Option<i32>), String> {
    let value = value.trim();
    let Some((raw_from, raw_to)) = value.split_once("..") else {
        let year = parse_year(value, field)?;
        return Ok((Some(year), Some(year)));
    };
    if raw_to.contains("..") {
        return Err(format!(
            "{field} range must use one '..', for example {field}:1985..1987."
        ));
    }
    let from = (!raw_from.trim().is_empty())
        .then(|| parse_year(raw_from, field))
        .transpose()?;
    let to = (!raw_to.trim().is_empty())
        .then(|| parse_year(raw_to, field))
        .transpose()?;
    if from.is_none() && to.is_none() {
        return Err(format!("{field} range needs a starting or ending year."));
    }
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err(format!(
            "{field} range must start at or before its ending year."
        ));
    }
    Ok((from, to))
}

/// The value inside quotes, or `None` for an unquoted value.
fn exact_value(value: &str) -> Result<Option<String>, String> {
    let value = value.trim();
    let starts = value.starts_with('"');
    let ends = value.len() > 1 && value.ends_with('"');
    if starts != ends || (!starts && value.contains('"')) {
        return Err("Quotes must wrap one complete search value.".to_owned());
    }
    if !starts {
        return Ok(None);
    }
    let exact = value[1..value.len() - 1].trim();
    if exact.is_empty() {
        return Err("Exact search quotes cannot be empty.".to_owned());
    }
    Ok(Some(exact.replace("\"\"", "\"")))
}

pub fn parse(input: &str) -> Result<Search, String> {
    if input.chars().count() > MAX_QUERY_CHARS {
        return Err(format!(
            "Search can be at most {MAX_QUERY_CHARS} characters."
        ));
    }
    let mut groups = Vec::new();
    let mut current: Option<Group> = None;
    let mut inherited_field = None;
    let mut pending_not = false;
    let mut after_or = false;
    let mut terms = 0;
    let mut alternatives = 0;

    for token in tokenize(input)? {
        match token {
            Token::Text(raw) => {
                let mut raw = raw.trim();
                let negative = raw.starts_with('-');
                if negative {
                    raw = raw[1..].trim();
                    if raw.is_empty() {
                        return Err("Negative search needs a value after '-'.".to_owned());
                    }
                    if current.is_some() {
                        return Err(
                            "Use a comma, AND, or NOT before a negative '-' clause.".to_owned()
                        );
                    }
                }
                if current.is_none() {
                    current = Some(Group {
                        negated: pending_not || negative,
                        alternatives: Vec::new(),
                    });
                    pending_not = false;
                }

                // Alternatives after OR use the field before them.
                let (field, value) = match split_field(raw) {
                    Some((field, value)) => {
                        inherited_field = Some(field);
                        (field, value)
                    }
                    None => (inherited_field.unwrap_or(Field::Any), raw),
                };
                if value.is_empty() {
                    return Err("Search field needs a value.".to_owned());
                }
                let exact = exact_value(value)?;
                let plain = exact.as_deref().unwrap_or(value);
                let matcher = match field {
                    Field::Extended(field) => parse_extended(field, plain)?,
                    Field::Year | Field::ReleaseYear => {
                        let name = if field == Field::Year {
                            "year"
                        } else {
                            "ryear"
                        };
                        let (from, to) = parse_year_range(plain, name)?;
                        Match::Years { from, to }
                    }
                    Field::Genre
                        if exact.is_none()
                            && matches!(
                                value.to_ascii_lowercase().as_str(),
                                "score" | "scores"
                            ) =>
                    {
                        Match::ScoreGenres
                    }
                    Field::Country => match exact {
                        Some(value) => Match::Country { value, exact: true },
                        None => {
                            let words = value
                                .split(|character: char| !character.is_alphanumeric())
                                .filter(|word| !word.is_empty())
                                .count();
                            if words == 0 {
                                return Err(
                                    "Search needs a word or an exact quoted value.".to_owned()
                                );
                            }
                            terms += words;
                            if terms > MAX_TERMS {
                                return Err(format!(
                                    "Search can contain at most {MAX_TERMS} words."
                                ));
                            }
                            Match::Country {
                                value: value.trim().to_owned(),
                                exact: false,
                            }
                        }
                    },
                    Field::Completeness => {
                        let (from, to) = parse_number_range(plain, field)?;
                        Match::Completeness { from, to }
                    }
                    Field::Love => {
                        let (from, to) = parse_number_range(plain, field)?;
                        Match::LovedTracks { from, to }
                    }
                    Field::Single => {
                        let (from, to) = parse_number_range(plain, field)?;
                        Match::Singles { from, to }
                    }
                    Field::Sonic => {
                        return Err(
                            "sonic: needs Aurora's audio analysis, which Aurora NG cannot read yet."
                                .to_owned(),
                        );
                    }
                    _ => match exact {
                        Some(value) => Match::Exact(value),
                        None => Match::Prefix(prefix_query(value, field.column(), &mut terms)?),
                    },
                };
                alternatives += 1;
                if alternatives > MAX_ALTERNATIVES {
                    return Err(format!(
                        "Search can contain at most {MAX_ALTERNATIVES} alternatives."
                    ));
                }
                current
                    .as_mut()
                    .expect("a search value starts a group")
                    .alternatives
                    .push(Alternative { field, matcher });
                after_or = false;
            }
            Token::Operator(Operator::Or) => {
                if current
                    .as_ref()
                    .is_none_or(|group| group.alternatives.is_empty())
                    || after_or
                {
                    return Err("OR needs a search value on both sides.".to_owned());
                }
                after_or = true;
            }
            Token::Operator(Operator::And) => {
                if after_or {
                    return Err("OR needs a search value on both sides.".to_owned());
                }
                groups.extend(current.take());
                inherited_field = None;
                pending_not = false;
            }
            Token::Operator(Operator::Not) => {
                if after_or {
                    return Err("NOT cannot replace a value after OR.".to_owned());
                }
                groups.extend(current.take());
                inherited_field = None;
                if pending_not {
                    return Err("NOT needs one search clause.".to_owned());
                }
                pending_not = true;
            }
        }
    }
    if after_or {
        return Err("OR needs a search value on both sides.".to_owned());
    }
    if pending_not {
        return Err("NOT needs one search clause.".to_owned());
    }
    groups.extend(current);
    Ok(Search { groups })
}

// ── Sorting ─────────────────────────────────────────────────────────────

/// How the cover wall orders its albums. Aurora has no `sort:`; it is
/// Aurora NG's own, and it does not filter anything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sort {
    Random,
    /// Oldest first.
    YearAscending,
    /// Newest first.
    YearDescending,
    Album {
        descending: bool,
    },
    AlbumArtist {
        descending: bool,
    },
}

const SORT_HELP: &str = "Use sort:random, sort:year.., sort:..year, sort:az, sort:za, sort:album az or sort:aartist za.";

/// Takes the `sort:` clause out of a query, wherever it is, and returns the
/// rest of the query with the sort. `sort:album` and `sort:aartist` may be
/// followed by `az` or `za` after a space.
pub fn split_sort(input: &str) -> Result<(String, Option<Sort>), String> {
    let is_break = |ch: char| ch.is_whitespace() || ch == ',';
    let mut rest = String::new();
    let mut sort = None;
    let mut quoted = false;
    let mut ix = 0;
    while let Some(ch) = input[ix..].chars().next() {
        let tail = &input[ix..];
        let at_word_start = input[..ix].chars().next_back().is_none_or(is_break);
        let is_sort = !quoted
            && at_word_start
            && tail
                .get(..4)
                .is_some_and(|word| word.eq_ignore_ascii_case("sort"))
            && matches!(tail[4..].chars().next(), Some(':' | '='));
        if !is_sort {
            if ch == '"' {
                quoted = !quoted;
            }
            rest.push(ch);
            ix += ch.len_utf8();
            continue;
        }

        let after = &tail[5..];
        let end = after.find(is_break).unwrap_or(after.len());
        let value = after[..end].to_ascii_lowercase();
        let mut consumed = 5 + end;
        // An optional direction after `album` or `aartist`.
        let mut descending = false;
        if matches!(value.as_str(), "album" | "albums" | "aartist") {
            let next = &after[end..];
            let spaces = next.len() - next.trim_start_matches(char::is_whitespace).len();
            let word_end = next[spaces..]
                .find(is_break)
                .map_or(next.len(), |end| spaces + end);
            match next[spaces..word_end].to_ascii_lowercase().as_str() {
                "az" => consumed += word_end,
                "za" => {
                    descending = true;
                    consumed += word_end;
                }
                _ => {}
            }
        }
        let parsed = match value.as_str() {
            "random" => Sort::Random,
            "year.." => Sort::YearAscending,
            "..year" => Sort::YearDescending,
            "az" => Sort::Album { descending: false },
            "za" => Sort::Album { descending: true },
            "album" | "albums" => Sort::Album { descending },
            "aartist" => Sort::AlbumArtist { descending },
            "" => return Err(format!("sort: needs a value. {SORT_HELP}")),
            other => return Err(format!("Unknown sort \"{other}\". {SORT_HELP}")),
        };
        if sort.replace(parsed).is_some() {
            return Err("Use one sort: per search.".to_owned());
        }
        ix += consumed;
    }
    Ok((rest, sort))
}

/// A seed for `sort:random`; a new one gives a new draw.
pub fn new_seed() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish()
}

/// The cover wall's order, as positions in the albums listed artist by
/// artist (each artist's albums oldest first). The sorts are stable, so ties
/// keep that order: same-year albums by artist then title, same titles by
/// artist then year.
pub fn wall_order(artists: &[crate::library::Artist], sort: Sort, seed: u64) -> Vec<usize> {
    let albums: Vec<(&str, &crate::library::Album)> = artists
        .iter()
        .flat_map(|artist| {
            artist
                .albums
                .iter()
                .map(move |album| (artist.sort_key.as_str(), album))
        })
        .collect();
    let mut order: Vec<usize> = (0..albums.len()).collect();
    match sort {
        Sort::Random => shuffle(&mut order, seed),
        Sort::YearAscending | Sort::YearDescending => {
            let newest_first = sort == Sort::YearDescending;
            // Albums without a year go last either way.
            order.sort_by(|&a, &b| match (albums[a].1.year, albums[b].1.year) {
                (Some(a), Some(b)) if newest_first => b.cmp(&a),
                (Some(a), Some(b)) => a.cmp(&b),
                (a, b) => b.is_some().cmp(&a.is_some()),
            });
        }
        Sort::Album { descending } => {
            let titles: Vec<String> = albums
                .iter()
                .map(|(_, album)| album.title.to_lowercase())
                .collect();
            order.sort_by(|&a, &b| {
                let order = titles[a].cmp(&titles[b]);
                if descending { order.reverse() } else { order }
            });
        }
        // Already in artist order.
        Sort::AlbumArtist { descending: false } => {}
        Sort::AlbumArtist { descending: true } => {
            order.sort_by(|&a, &b| albums[b].0.cmp(albums[a].0));
        }
    }
    order
}

/// Fisher–Yates with SplitMix64: the same seed always gives the same order.
fn shuffle(order: &mut [usize], seed: u64) {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for i in (1..order.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
}

// ── SQL ─────────────────────────────────────────────────────────────────

/// `column >= ? AND column <= ?` for the bounds that are set.
fn range_sql<T: Copy>(
    column: &str,
    from: Option<T>,
    to: Option<T>,
    value: impl Fn(T) -> Value,
    params: &mut Vec<Value>,
) -> String {
    let mut parts = Vec::new();
    if let Some(from) = from {
        params.push(value(from));
        parts.push(format!("{column} >= ?"));
    }
    if let Some(to) = to {
        params.push(value(to));
        parts.push(format!("{column} <= ?"));
    }
    format!("({})", parts.join(" AND "))
}

/// The key Music Library files artist info under: trimmed, lowercase, with
/// Unicode dashes made plain.
pub fn artist_key_sql(field: &str) -> String {
    let normalized = [8208, 8209, 8210, 8211, 8212, 8722]
        .iter()
        .fold(format!("COALESCE({field}, '')"), |expression, codepoint| {
            format!("REPLACE({expression}, CHAR({codepoint}), '-')")
        });
    format!("COALESCE(NULLIF(TRIM(unicode_lower({normalized})), ''), 'unknown')")
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Track conditions that are not full-text prefixes.
fn track_predicate(
    alias: &str,
    alternative: &Alternative,
    params: &mut Vec<Value>,
) -> Option<String> {
    match &alternative.matcher {
        Match::Exact(value) => {
            let column = alternative.field.column();
            // An exact value contains all its words, so the full-text index
            // narrows a million tracks down to a few before comparing.
            let narrow = match prefix_query(value, column, &mut 0) {
                Ok(query) => {
                    params.push(Value::Text(query));
                    format!(
                        "{alias}.id IN (SELECT CAST(track_id AS INTEGER) FROM track_search_fts WHERE track_search_fts MATCH ?) AND "
                    )
                }
                // Only punctuation, like "!!!": compare every track.
                Err(_) => String::new(),
            };
            let columns: &[&str] = match column {
                Some(column) => &[column][..],
                None => &EXACT_TEXT_COLUMNS[..],
            };
            let parts: Vec<String> = columns
                .iter()
                .map(|column| {
                    params.push(Value::Text(value.clone()));
                    format!("TRIM(COALESCE({alias}.{column}, '')) = ? COLLATE NOCASE")
                })
                .collect();
            Some(format!("({narrow}({}))", parts.join(" OR ")))
        }
        Match::Country { value, exact } => {
            let key = artist_key_sql(&format!("{alias}.album_artist_display"));
            let normalized = value.trim().to_lowercase();
            let condition = if *exact {
                params.push(Value::Text(normalized.clone()));
                params.push(Value::Text(normalized));
                "(unicode_lower(TRIM(COALESCE(origin.country_name, ''))) = ? OR unicode_lower(TRIM(COALESCE(origin.country_code, ''))) = ?)".to_owned()
            } else {
                normalized
                    .split(|character: char| !character.is_alphanumeric())
                    .filter(|word| !word.is_empty())
                    .map(|word| {
                        let pattern = format!("% {}%", escape_like(word));
                        params.push(Value::Text(pattern.clone()));
                        params.push(Value::Text(pattern));
                        "((' ' || unicode_lower(REPLACE(TRIM(COALESCE(origin.country_name, '')), '-', ' '))) LIKE ? ESCAPE '\\' OR (' ' || unicode_lower(TRIM(COALESCE(origin.country_code, '')))) LIKE ? ESCAPE '\\')".to_owned()
                    })
                    .collect::<Vec<_>>()
                    .join(" AND ")
            };
            Some(format!(
                "EXISTS (SELECT 1 FROM musicbrainz_artist_origin_countries AS origin WHERE origin.local_artist_key = {key} AND ({condition}))"
            ))
        }
        Match::ScoreGenres => {
            let placeholders: Vec<&str> = SCORE_GENRES
                .iter()
                .map(|genre| {
                    params.push(Value::Text((*genre).to_owned()));
                    "?"
                })
                .collect();
            Some(format!(
                "LOWER(TRIM(COALESCE({alias}.canonical_genre, ''))) IN ({})",
                placeholders.join(", ")
            ))
        }
        Match::Years { from, to } => {
            let column = if alternative.field == Field::Year {
                "year"
            } else {
                "release_year"
            };
            if from == to {
                params.push(Value::Integer(i64::from(from.expect("one year"))));
                return Some(format!("{alias}.{column} = ?"));
            }
            Some(range_sql(
                &format!("{alias}.{column}"),
                *from,
                *to,
                |year| Value::Integer(i64::from(year)),
                params,
            ))
        }
        _ => None,
    }
}

/// Conditions on the album's own totals.
fn album_predicate(
    alias: &str,
    matcher: &Match,
    singles: bool,
    params: &mut Vec<Value>,
) -> Option<String> {
    let count = |n: u32| Value::Integer(i64::from(n));
    match matcher {
        // Stored as a fraction, 0–1.
        Match::Completeness { from, to } => Some(range_sql(
            &format!("{alias}.rating_completeness"),
            *from,
            *to,
            |percent| Value::Real(f64::from(percent) / 100.0),
            params,
        )),
        Match::LovedTracks { from, to } => Some(range_sql(
            &format!("{alias}.loved_tracks"),
            *from,
            *to,
            count,
            params,
        )),
        Match::Singles { from, to } => {
            // Catalogs from before Music Library knew singles have none.
            let is_single = if singles {
                "EXISTS (SELECT 1 FROM track_singles AS single WHERE single.confidence = 'confirmed' \
                 AND single.artist_key = lower(trim(single_track.album_artist_display)) \
                 AND single.track_key = lower(trim(single_track.title)))"
            } else {
                "0"
            };
            let column = format!(
                "(SELECT COUNT(*) FROM tracks AS single_track WHERE single_track.album_id = {alias}.id AND {is_single})"
            );
            Some(range_sql(&column, *from, *to, count, params))
        }
        _ => None,
    }
}

/// Artist lifespans, length, album rating and chart ranks.
fn extended_predicate(alias: &str, matcher: &Match, params: &mut Vec<Value>) -> Option<String> {
    use Extended::*;
    let Match::Extended {
        field,
        from,
        to,
        present,
    } = matcher
    else {
        return None;
    };
    let real = |n: f64| Value::Real(n);
    if matches!(field, Dissolved | Founded | Dead | Born) {
        let kind = if matches!(field, Dead | Born) {
            "Person"
        } else {
            "Group"
        };
        let key = artist_key_sql(&format!("{alias}.album_artist_display"));
        let condition = match present {
            Some(true) => {
                "(i.life_ended = 1 OR NULLIF(TRIM(i.life_end_date), '') IS NOT NULL)".to_owned()
            }
            Some(false) => {
                "(i.life_ended = 0 AND NULLIF(TRIM(i.life_end_date), '') IS NULL)".to_owned()
            }
            None => {
                let date = if matches!(field, Born | Founded) {
                    "i.life_begin_date"
                } else {
                    "i.life_end_date"
                };
                format!(
                    "substr({date}, 1, 4) GLOB '[0-9][0-9][0-9][0-9]' AND {}",
                    range_sql(
                        &format!("CAST(substr({date}, 1, 4) AS INTEGER)"),
                        *from,
                        *to,
                        real,
                        params
                    )
                )
            }
        };
        return Some(format!(
            "EXISTS (SELECT 1 FROM musicbrainz_artist_infos AS i WHERE i.local_artist_key = {key} AND i.artist_type = '{kind}' COLLATE NOCASE AND {condition})"
        ));
    }
    // Songs-only charts have no album ranks.
    if matches!(field, Ti | Nt) {
        return Some("0".to_owned());
    }
    let column = match field {
        Minutes => format!("{alias}.total_seconds / 60.0"),
        AlbumRating => format!(
            "COALESCE({alias}.effective_album_rating, {alias}.calculated_album_rating, {alias}.album_rating) / 20.0"
        ),
        Billboard => format!("{alias}.billboard_rank"),
        Uk => format!("{alias}.official_uk_rank"),
        Vg => format!("{alias}.vg_lista_rank"),
        _ => unreachable!("handled above"),
    };
    Some(match present {
        Some(true) => format!("COALESCE({column}, 0) > 0"),
        Some(false) => format!("COALESCE({column}, 0) <= 0"),
        None => {
            let range = range_sql(&column, *from, *to, real, params);
            if matches!(field, Billboard | Uk | Vg) {
                format!("COALESCE(({column} > 0 AND {range}), 0)")
            } else {
                format!("COALESCE({range}, 0)")
            }
        }
    })
}

/// Appends ` AND (…)` for every group, testing albums aliased `a`.
fn push_album_predicates(
    sql: &mut String,
    params: &mut Vec<Value>,
    search: &Search,
    singles: bool,
) {
    for group in &search.groups {
        let mut alternatives = Vec::new();

        let prefixes: Vec<String> = group
            .alternatives
            .iter()
            .filter_map(|alternative| match &alternative.matcher {
                Match::Prefix(query) => Some(format!("({query})")),
                _ => None,
            })
            .collect();
        if !prefixes.is_empty() {
            alternatives.push(
                "a.id IN (SELECT album_id FROM track_search_fts WHERE track_search_fts MATCH ?)"
                    .to_owned(),
            );
            params.push(Value::Text(prefixes.join(" OR ")));
        }

        // Build the matching album set once, rather than probing per album.
        let track_predicates: Vec<String> = group
            .alternatives
            .iter()
            .filter(|alternative| {
                !matches!(
                    alternative.matcher,
                    Match::ScoreGenres | Match::Country { .. }
                )
            })
            .filter_map(|alternative| track_predicate("search_track", alternative, params))
            .collect();
        if !track_predicates.is_empty() {
            alternatives.push(format!(
                "a.id IN (SELECT search_track.album_id FROM tracks AS search_track WHERE search_track.album_id IS NOT NULL AND ({}))",
                track_predicates.join(" OR ")
            ));
        }

        // Country belongs to the album artist, so test albums, not tracks.
        alternatives.extend(
            group
                .alternatives
                .iter()
                .filter(|alternative| matches!(alternative.matcher, Match::Country { .. }))
                .filter_map(|alternative| track_predicate("a", alternative, params)),
        );

        // The score genres have no index, so probe each candidate album's tracks.
        for alternative in &group.alternatives {
            if alternative.matcher == Match::ScoreGenres
                && let Some(predicate) = track_predicate("score_track", alternative, params)
            {
                alternatives.push(format!(
                    "EXISTS (SELECT 1 FROM tracks AS score_track WHERE score_track.album_id = a.id AND {predicate})"
                ));
            }
        }

        alternatives.extend(
            group.alternatives.iter().filter_map(|alternative| {
                album_predicate("a", &alternative.matcher, singles, params)
            }),
        );
        alternatives.extend(
            group
                .alternatives
                .iter()
                .filter_map(|alternative| extended_predicate("a", &alternative.matcher, params)),
        );

        sql.push_str(if group.negated {
            " AND NOT ("
        } else {
            " AND ("
        });
        sql.push_str(&alternatives.join(" OR "));
        sql.push(')');
    }
}

/// The ids of every album that matches.
pub fn album_ids(connection: &Connection, search: &Search) -> Result<HashSet<String>, String> {
    let singles: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'track_singles')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not search: {error}"))?;
    let mut sql = String::from("SELECT a.id FROM albums AS a WHERE 1 = 1");
    let mut params = Vec::new();
    push_album_predicates(&mut sql, &mut params, search, singles);
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| format!("Could not search: {error}"))?;
    statement
        .query_map(params_from_iter(params.iter()), |row| {
            row.get::<_, String>(0)
        })
        .map_err(|error| format!("Could not search: {error}"))?
        .collect::<Result<_, _>>()
        .map_err(|error| format!("Could not search: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_sort_out_of_a_query() {
        let split = |query: &str| split_sort(query).unwrap();
        assert_eq!(split("sort:random"), (String::new(), Some(Sort::Random)));
        assert_eq!(
            split("year:1981 sort:az"),
            (
                "year:1981 ".to_owned(),
                Some(Sort::Album { descending: false })
            )
        );
        assert_eq!(
            split("sort:aartist za, genre:scores"),
            (
                ", genre:scores".to_owned(),
                Some(Sort::AlbumArtist { descending: true })
            )
        );
        assert_eq!(
            split("genre:rock AND sort=..year"),
            ("genre:rock AND ".to_owned(), Some(Sort::YearDescending))
        );
        assert_eq!(
            split("SORT:Albums ZA year:1999"),
            (
                " year:1999".to_owned(),
                Some(Sort::Album { descending: true })
            )
        );
        assert_eq!(
            split("sort:album year:1999").1,
            Some(Sort::Album { descending: false })
        );
        // Not a clause: inside quotes, or in the middle of a word.
        assert_eq!(split("title:\"sort:az\"").1, None);
        assert_eq!(split("aartist:resort:az").1, None);
        assert!(parse(&split("year:1981 AND sort:year..").0).is_ok());

        assert!(split_sort("sort:").is_err());
        assert!(split_sort("sort:sideways").is_err());
        assert!(split_sort("sort:az sort:za").is_err());
    }

    #[test]
    fn orders_the_wall() {
        use crate::library::{Album, Artist};
        let album = |title: &str, year: Option<i64>| Album {
            id: title.to_owned(),
            title: title.to_owned(),
            year,
            publisher: None,
            genre: None,
            total_tracks: None,
            total_seconds: None,
            rating: None,
            score: None,
            folder: None,
            cover: None,
            charts: Vec::new(),
        };
        let artist = |name: &str, albums: Vec<Album>| Artist {
            name: name.to_owned(),
            sort_key: name.to_lowercase(),
            flag: None,
            albums,
        };
        // Positions: 0 ABBA/Waterloo, 1 ABBA/Arrival, 2 Kiss/Destroyer, 3 Kiss/Unknown.
        let artists = [
            artist(
                "ABBA",
                vec![album("Waterloo", Some(1974)), album("Arrival", Some(1976))],
            ),
            artist(
                "Kiss",
                vec![album("Destroyer", Some(1976)), album("Unknown", None)],
            ),
        ];
        let order = |sort| wall_order(&artists, sort, 7);
        assert_eq!(order(Sort::YearAscending), [0, 1, 2, 3]);
        assert_eq!(order(Sort::YearDescending), [1, 2, 0, 3]);
        assert_eq!(order(Sort::Album { descending: false }), [1, 2, 3, 0]);
        assert_eq!(order(Sort::Album { descending: true }), [0, 3, 2, 1]);
        assert_eq!(order(Sort::AlbumArtist { descending: false }), [0, 1, 2, 3]);
        assert_eq!(order(Sort::AlbumArtist { descending: true }), [2, 3, 0, 1]);

        let mut shuffled = order(Sort::Random);
        assert_eq!(
            shuffled,
            order(Sort::Random),
            "the same seed, the same draw"
        );
        shuffled.sort();
        assert_eq!(shuffled, [0, 1, 2, 3]);
    }

    fn ids(connection: &Connection, query: &str) -> Vec<String> {
        let search = parse(query).unwrap();
        let mut ids: Vec<String> = album_ids(connection, &search)
            .unwrap()
            .into_iter()
            .collect();
        ids.sort();
        ids
    }

    /// A tiny catalog: (id, album artist, genre, year, completeness, loved tracks).
    fn catalog() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        crate::library::register_functions(&connection).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE albums (id TEXT PRIMARY KEY, album TEXT, album_artist_display TEXT,
                    canonical_genre TEXT, publisher TEXT, year INTEGER, release_year INTEGER,
                    rating_completeness REAL, loved_tracks INTEGER, total_seconds INTEGER,
                    album_rating INTEGER, calculated_album_rating INTEGER, effective_album_rating INTEGER,
                    billboard_rank INTEGER, official_uk_rank INTEGER, vg_lista_rank INTEGER);
                 CREATE TABLE tracks (id INTEGER PRIMARY KEY, album_id TEXT, title TEXT,
                    display_artist TEXT, album_artist_display TEXT, album TEXT, canonical_genre TEXT,
                    publisher TEXT, year INTEGER, release_year INTEGER);
                 CREATE VIRTUAL TABLE track_search_fts USING fts5(track_id UNINDEXED, album_id UNINDEXED,
                    title, display_artist, album, album_artist_display, canonical_genre, publisher,
                    file_path, filename);",
            )
            .unwrap();
        let albums = [
            ("bj", "Bon Jovi", "Hard Rock", 1986, 1.0, 2),
            ("jbj", "Jon Bon Jovi", "Soundtrack", 1990, 0.5, 0),
            ("zimmer", "Hans Zimmer", "Action", 2005, 0.4, 0),
            ("williams", "John Williams", "Fantasy", 1999, 0.2, 0),
            ("vangelis", "Vangelis", "Sci-Fi", 1982, 0.9, 1),
            ("blade", "Vangelis", "Soundtrack", 1983, 0.5, 3),
            ("synth", "Kim Wilde", "Synthpop", 1984, 0.3, 1),
            ("full", "Kim Wilde", "Synthpop", 1981, 1.0, 4),
        ];
        for (track, (id, artist, genre, year, completeness, loved)) in albums.iter().enumerate() {
            connection
                .execute(
                    "INSERT INTO albums (id, album, album_artist_display, canonical_genre, year,
                        rating_completeness, loved_tracks, total_seconds)
                     VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, 2400)",
                    rusqlite::params![id, artist, genre, year, completeness, loved],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO tracks (id, album_id, title, display_artist, album_artist_display,
                        album, canonical_genre, year) VALUES (?1, ?2, 'Song', ?3, ?3, ?2, ?4, ?5)",
                    rusqlite::params![track as i64, id, artist, genre, year],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO track_search_fts (track_id, album_id, title, display_artist, album,
                        album_artist_display, canonical_genre) VALUES (?1, ?2, 'Song', ?3, ?2, ?3, ?4)",
                    rusqlite::params![track.to_string(), id, artist, genre],
                )
                .unwrap();
        }
        connection
    }

    #[test]
    fn exact_album_artist_skips_longer_names() {
        let connection = catalog();
        assert_eq!(ids(&connection, "aartist:\"bon jovi\""), ["bj"]);
        assert_eq!(ids(&connection, "aartist:bon jovi"), ["bj", "jbj"]);
    }

    #[test]
    fn scores_in_a_decade_that_are_not_fully_rated() {
        let connection = catalog();
        assert_eq!(
            ids(&connection, "year:2000..2009 AND genre:scores AND cr:..99"),
            ["zimmer"]
        );
        assert_eq!(
            ids(&connection, "year:1990..2009 AND genre:scores AND cr:..99"),
            ["williams", "zimmer"]
        );
    }

    #[test]
    fn excludes_score_and_soundtrack_genres() {
        let connection = catalog();
        assert_eq!(
            ids(
                &connection,
                "year:1980..1985 AND cr:..99 AND love:1.. NOT genre:scores OR soundtrack"
            ),
            ["synth"]
        );
    }

    #[test]
    fn plain_words_search_everything() {
        let connection = catalog();
        assert_eq!(ids(&connection, "wilde"), ["full", "synth"]);
        assert_eq!(ids(&connection, "genre:synthpop,-year:1984"), ["full"]);
    }

    #[test]
    fn alternatives_inherit_their_field() {
        let search = parse("year:1985 OR 1987").unwrap();
        assert_eq!(search.groups.len(), 1);
        assert!(
            search.groups[0]
                .alternatives
                .iter()
                .all(|alternative| alternative.field == Field::Year)
        );
        let search = parse("genre:scores NOT genre:\"video game\"").unwrap();
        assert_eq!(search.groups.len(), 2);
        assert!(search.groups[1].negated);
        assert_eq!(
            search.groups[1].alternatives[0].matcher,
            Match::Exact("video game".to_owned())
        );
    }

    #[test]
    fn lowercase_operators_are_words() {
        let search = parse("rock and roll").unwrap();
        assert_eq!(search.groups.len(), 1);
        assert_eq!(search.groups[0].alternatives.len(), 1);
    }

    #[test]
    fn number_shorthands() {
        let cr = parse("cr:80").unwrap();
        assert_eq!(
            cr.groups[0].alternatives[0].matcher,
            Match::Completeness {
                from: Some(0),
                to: Some(80)
            }
        );
        let love = parse("love=1").unwrap();
        assert_eq!(
            love.groups[0].alternatives[0].matcher,
            Match::LovedTracks {
                from: Some(1),
                to: None
            }
        );
    }

    #[test]
    fn rejects_malformed_queries() {
        for query in [
            "aartist:\"Kiss",
            "year:1990..1980",
            "year:..",
            "cr:101",
            "love:3",
            "genre:rock OR",
            "NOT",
            "artist:",
            "ar:6",
        ] {
            assert!(parse(query).is_err(), "{query} should be rejected");
        }
    }

    /// Runs the user's example queries on the real catalog and prints timings:
    /// `cargo test real_catalog -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_catalog() {
        let catalog =
            crate::library::Catalog::open(&crate::library::catalog_path().unwrap()).unwrap();
        for query in [
            "year:2000..2009 AND genre:scores AND cr:..99",
            "aartist:\"bon jovi\"",
            "aartist:bon jovi",
            "year:1980..1985 AND cr:..99 AND love:1.. NOT genre:scores OR soundtrack",
            "country:norway AND year:1990..",
            "single:3..",
            "bb:..5 AND uk:yes",
        ] {
            let started = std::time::Instant::now();
            let found = catalog.search_album_ids(&parse(query).unwrap()).unwrap();
            println!(
                "{:>6} albums in {:>5} ms: {query}",
                found.len(),
                started.elapsed().as_millis()
            );
        }
    }
}
