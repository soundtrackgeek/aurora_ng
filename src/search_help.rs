//! In-app examples and completions for Aurora's search language.

use crate::{library::Artist, search};
use std::collections::BTreeSet;

const FIELDS: &str = include_str!("../assets/searchFields.txt");
const VALUES: &str = include_str!("../assets/searchValues.txt");

// Optional availability codes are Songs (s), Albums (a), Artists (t), and NG (n).
pub fn fields() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    FIELDS.lines().filter_map(|line| {
        let mut columns = line.split('|');
        let name = columns.next()?;
        let label = columns.next()?;
        let example = columns.next()?;
        if columns.next().is_some_and(|views| !views.contains('n')) {
            return None;
        }
        Some((name, label, example))
    })
}

#[derive(Default)]
pub struct CatalogValues {
    genres: Vec<String>,
    countries: Vec<String>,
}

impl CatalogValues {
    pub fn from_library(artists: &[Artist]) -> Self {
        let collect = |names: BTreeSet<&str>| names.into_iter().map(str::to_owned).collect();
        Self {
            genres: collect(
                artists
                    .iter()
                    .flat_map(|artist| &artist.albums)
                    .filter_map(|album| album.genre.as_deref())
                    .collect(),
            ),
            countries: collect(
                artists
                    .iter()
                    .filter_map(|artist| artist.country.as_deref())
                    .collect(),
            ),
        }
    }
}

fn trailing_clause(query: &str) -> (usize, Option<&str>) {
    let mut start = 0;
    let mut inherited = None;
    let mut quoted = false;
    for (index, character) in query.char_indices() {
        if character == '"' {
            quoted = !quoted;
        }
        if quoted {
            continue;
        }
        if character == ',' {
            start = index + 1;
            inherited = None;
        } else if (index == 0
            || query[..index].ends_with(char::is_whitespace)
            || query[..index].ends_with(','))
            && let Some(operator) = ["AND", "OR", "NOT"].into_iter().find(|operator| {
                query[index..].strip_prefix(operator).is_some_and(|rest| {
                    rest.is_empty()
                        || rest.starts_with(char::is_whitespace)
                        || rest.starts_with(',')
                })
            })
        {
            inherited = if operator == "OR" {
                query[start..index]
                    .trim()
                    .trim_start_matches('-')
                    .split_once([':', '='])
                    .map(|(field, _)| field.trim())
                    .or(inherited)
            } else {
                None
            };
            start = index + operator.len();
        }
    }
    (start, inherited)
}

pub fn suggestions(query: &str, catalog: &CatalogValues) -> Vec<(String, String)> {
    let (start, inherited) = trailing_clause(query);
    let clause = query[start..].trim_start();
    let input = clause.strip_prefix('-').unwrap_or(clause).trim_start();
    let prefix = &query[..query.len() - input.len()];
    let explicit = input.find([':', '=']);
    let field = explicit.map(|index| input[..index].trim()).or(inherited);

    if let Some(field) = field {
        let value_start = explicit.map_or(0, |index| index + 1);
        let value = input[value_start..].trim();
        let prefer_exact = value.starts_with('"');
        let value = search::fold(value.strip_prefix('"').unwrap_or(value));
        let field = field.to_ascii_lowercase();
        let catalog_options = match field.as_str() {
            "genre" => catalog.genres.as_slice(),
            "country" => catalog.countries.as_slice(),
            _ => &[],
        };
        let options = VALUES.lines().find_map(|line| {
            let (name, options) = line.split_once('|')?;
            (name == field.as_str()).then_some(options)
        });
        let mut seen = BTreeSet::new();
        return options
            .into_iter()
            .flat_map(|options| options.split('|'))
            .chain(catalog_options.iter().map(String::as_str))
            .filter(|option| {
                let folded = search::fold(option);
                folded.starts_with(&value) && seen.insert(folded)
            })
            .take(8)
            .map(|option| {
                let quoted = prefer_exact
                    || option.contains([',', ':', '='])
                    || option.contains(char::is_whitespace);
                let value = if quoted {
                    format!("\"{option}\"")
                } else {
                    option.to_owned()
                };
                (
                    option.to_owned(),
                    format!("{prefix}{}{value}", &input[..value_start]),
                )
            })
            .collect();
    }

    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Vec::new();
    }
    let fragment = input.to_ascii_lowercase();
    fields()
        .filter(|(name, _, _)| name.starts_with(&fragment))
        .take(8)
        .map(|(name, _, _)| (format!("{name}:"), format!("{prefix}{name}:")))
        .collect()
}

pub fn parse(query: &str) -> Result<(search::Search, search::View), String> {
    let (filter, view) = search::split_view(query)?;
    let parsed = search::parse(&filter)?;
    if view.sort == Some(search::Sort::Vibe) && parsed.vibes().is_empty() {
        return Err("sort:vibe needs a vibe: description.".to_owned());
    }
    Ok((parsed, view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guide_examples_are_supported() {
        for (_, _, example) in fields() {
            assert!(parse(example).is_ok(), "{example}");
        }
    }

    #[test]
    fn explains_missing_clause_separators_without_rejecting_free_text() {
        assert_eq!(
            parse("year:1985 genre:scores").err().as_deref(),
            Some("Add AND or a comma before genre:.")
        );
        assert!(parse("title:Book Title: Subtitle").is_ok());
    }

    #[test]
    fn completes_fields_and_values_without_changing_previous_clauses() {
        let cases = [
            ("year:1985 AND vi", "year:1985 AND vibe:"),
            ("genre:scores NOT sound=ch", "genre:scores NOT sound=choir"),
            (
                "country:France OR uni",
                "country:France OR \"United Kingdom\"",
            ),
            (
                "title:\"AND OR NOT\" AND genre:\"video",
                "title:\"AND OR NOT\" AND genre:\"video game\"",
            ),
        ];
        for (query, expected) in cases {
            assert!(
                suggestions(query, &CatalogValues::default())
                    .iter()
                    .any(|(_, query)| query == expected)
            );
        }
    }
}
