use serde::{Deserialize, Serialize};
use specta::Type;
use yaserde::{YaDeserialize, YaSerialize};

use super::Comic;

/// Base url of a gallery page on the Hitomi website
///
/// Deliberately not built from the `DOMAIN` constant of the `hitomi` module: that one points
/// at the cdn the gallery data is fetched from, which is not a url a reader can open.
const HITOMI_GALLERY_BASE_URL: &str = "https://hitomi.la/galleries";

/// The age rating written for every comic
///
/// Hitomi publishes no rating per gallery, so this is not read from anywhere. The site
/// carries adult content exclusively, which is what makes the constant safe to state.
const AGE_RATING: &str = "X18+";

/// ISO 639-1 codes for the languages Hitomi publishes in
///
/// Hitomi names its languages in lowercase English, which is also how they go into the
/// nozomi urls built in `hitomi::search`. `LanguageISO` is meant to carry an ISO code, so a
/// language without a 639-1 code, Cebuano for instance, is deliberately missing here and
/// ends up without the element rather than with a wrong one.
const LANGUAGE_ISO_CODES: &[(&str, &str)] = &[
    ("albanian", "sq"),
    ("arabic", "ar"),
    ("bulgarian", "bg"),
    ("catalan", "ca"),
    ("chinese", "zh"),
    ("czech", "cs"),
    ("danish", "da"),
    ("dutch", "nl"),
    ("english", "en"),
    ("esperanto", "eo"),
    ("estonian", "et"),
    ("finnish", "fi"),
    ("french", "fr"),
    ("german", "de"),
    ("greek", "el"),
    ("hebrew", "he"),
    ("hungarian", "hu"),
    ("indonesian", "id"),
    ("italian", "it"),
    ("japanese", "ja"),
    ("korean", "ko"),
    ("latin", "la"),
    ("mongolian", "mn"),
    ("norwegian", "no"),
    ("persian", "fa"),
    ("polish", "pl"),
    ("portuguese", "pt"),
    ("romanian", "ro"),
    ("russian", "ru"),
    ("serbian", "sr"),
    ("slovak", "sk"),
    ("spanish", "es"),
    ("swedish", "sv"),
    ("tagalog", "tl"),
    ("thai", "th"),
    ("turkish", "tr"),
    ("ukrainian", "uk"),
    ("vietnamese", "vi"),
];

/// <https://anansi-project.github.io/docs/comicinfo/schemas/v2.1>
///
/// The fields are declared in the order of the `xs:sequence` of the schema, because yaserde
/// writes them out in declaration order and the schema prescribes that order. Only fields
/// Hitomi actually provides data for are present, which the schema allows through
/// `minOccurs="0"`.
///
/// Everything that can be absent is an `Option`, and yaserde leaves a `None` out entirely
/// instead of writing an empty element. That matters: an empty `<Year></Year>` would not be
/// a valid `xs:int`, while a missing `Year` is perfectly valid.
#[derive(
    Default, Debug, Clone, PartialEq, Serialize, Deserialize, Type, YaSerialize, YaDeserialize,
)]
#[serde(rename_all = "camelCase")]
pub struct ComicInfo {
    /// Title of this particular work
    #[yaserde(rename = "Title")]
    pub title: Option<String>,
    /// The Japanese title, which would otherwise be lost
    #[yaserde(rename = "AlternateSeries")]
    pub alternate_series: Option<String>,
    /// Title and gallery link again, for readers that show a description
    #[yaserde(rename = "Summary")]
    pub summary: Option<String>,
    /// Where this file came from
    #[yaserde(rename = "Notes")]
    pub notes: Option<String>,
    #[yaserde(rename = "Year")]
    pub year: Option<i32>,
    #[yaserde(rename = "Month")]
    pub month: Option<i32>,
    #[yaserde(rename = "Day")]
    pub day: Option<i32>,
    /// Hitomi only names the artists of a work, so they fill every artist role
    #[yaserde(rename = "Writer")]
    pub writer: Option<String>,
    #[yaserde(rename = "Penciller")]
    pub penciller: Option<String>,
    #[yaserde(rename = "Inker")]
    pub inker: Option<String>,
    #[yaserde(rename = "Colorist")]
    pub colorist: Option<String>,
    #[yaserde(rename = "Publisher")]
    pub publisher: String,
    /// The kind of work, such as `doujinshi`, `manga` or `artistcg`
    #[yaserde(rename = "Genre")]
    pub genre: Option<String>,
    #[yaserde(rename = "Tags")]
    pub tags: Option<String>,
    /// Link to the gallery this comic was downloaded from
    #[yaserde(rename = "Web")]
    pub web: Option<String>,
    /// The number of pages in this comic
    #[yaserde(rename = "PageCount")]
    pub page_count: i32,
    #[yaserde(rename = "LanguageISO")]
    pub language_iso: Option<String>,
    #[yaserde(rename = "Manga")]
    pub manga: String,
    #[yaserde(rename = "Characters")]
    pub characters: Option<String>,
    /// The circle that published this work
    #[yaserde(rename = "Teams")]
    pub teams: Option<String>,
    #[yaserde(rename = "AgeRating")]
    pub age_rating: Option<String>,
}

impl ComicInfo {
    /// Create the `ComicInfo` of `comic`
    ///
    /// The type and the tags are Hitomi's own vocabulary and are written as they come, so
    /// the metadata of an archive does not depend on the language of the interface.
    pub fn from_comic(comic: &Comic) -> Self {
        // Hitomi names the artists of a work without saying who did what,
        // so every artist role gets all of them
        let artists = join_non_empty(&comic.artists);

        let (year, month, day) = match parse_date(&comic.date) {
            Some((year, month, day)) => (Some(year), Some(month), Some(day)),
            None => (None, None, None),
        };

        let tags: Vec<String> = comic.tags.iter().map(|tag| tag.tag.clone()).collect();

        let web = format!("{HITOMI_GALLERY_BASE_URL}/{}.html", comic.id);
        // Readers put the summary where a description belongs, while `Title` and `Web` tend
        // to sit in a detail view, so both are repeated here where they are actually seen
        let summary = match non_empty(comic.title.clone()) {
            Some(title) => format!("{title}\n{web}"),
            None => web.clone(),
        };

        ComicInfo {
            title: non_empty(comic.title.clone()),
            alternate_series: non_empty(comic.japanese_title.clone()),
            summary: Some(summary),
            // The crate version is the version the app reports, which `versions_are_in_step`
            // keeps true, so it is safe to state here
            notes: Some(format!(
                "Downloaded from Hitomi with hitomi-downloader {}",
                env!("CARGO_PKG_VERSION")
            )),
            year,
            month,
            day,
            writer: artists.clone(),
            penciller: artists.clone(),
            inker: artists.clone(),
            colorist: artists,
            publisher: "Hitomi".to_string(),
            genre: non_empty(comic.type_field.clone()),
            tags: join_non_empty(&tags),
            web: Some(web),
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            page_count: comic.files.len() as i32,
            language_iso: language_iso_code(&comic.language),
            manga: "Yes".to_string(),
            characters: join_non_empty(&comic.characters),
            teams: join_non_empty(&comic.groups),
            age_rating: Some(AGE_RATING.to_string()),
        }
    }
}

/// Keep `value` unless it holds nothing worth writing
fn non_empty(value: String) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

/// Join `values` the way `ComicInfo` lists them, or nothing when there is nothing to list
fn join_non_empty(values: &[String]) -> Option<String> {
    let joined = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect::<Vec<&str>>()
        .join(", ");

    non_empty(joined)
}

/// Read the date part of the timestamp Hitomi puts on a gallery
///
/// The timestamp looks like `2023-04-18 12:30:00-05`, but nothing guarantees that shape, so
/// only the part before the first space is looked at and every component has to be a number
/// in a sensible range. Anything else yields `None` and leaves the date out of the metadata:
/// an archive without a date is valid, one carrying an invented date is misleading.
fn parse_date(date: &str) -> Option<(i32, i32, i32)> {
    let mut parts = date.split_whitespace().next()?.split('-');

    let year: i32 = parts.next()?.parse().ok()?;
    let month: i32 = parts.next()?.parse().ok()?;
    let day: i32 = parts.next()?.parse().ok()?;

    if parts.next().is_some() {
        return None;
    }

    if !(1900..=2999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    Some((year, month, day))
}

/// The ISO 639-1 code of the Hitomi language named `language`
fn language_iso_code(language: &str) -> Option<String> {
    let language = language.trim().to_ascii_lowercase();

    LANGUAGE_ISO_CODES
        .iter()
        .find(|(name, _)| *name == language)
        .map(|(_, code)| (*code).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Tag;

    /// The element order of the `xs:sequence` in `tests/ComicInfo-v2.1.xsd`
    const SCHEMA_ELEMENT_ORDER: &[&str] = &[
        "Title",
        "Series",
        "Number",
        "Count",
        "Volume",
        "AlternateSeries",
        "AlternateNumber",
        "AlternateCount",
        "Summary",
        "Notes",
        "Year",
        "Month",
        "Day",
        "Writer",
        "Penciller",
        "Inker",
        "Colorist",
        "Letterer",
        "CoverArtist",
        "Editor",
        "Translator",
        "Publisher",
        "Imprint",
        "Genre",
        "Tags",
        "Web",
        "PageCount",
        "LanguageISO",
        "Format",
        "BlackAndWhite",
        "Manga",
        "Characters",
        "Teams",
        "Locations",
        "ScanInformation",
        "StoryArc",
        "StoryArcNumber",
        "SeriesGroup",
        "AgeRating",
        "Pages",
        "CommunityRating",
        "MainCharacterOrTeam",
        "Review",
        "GTIN",
    ];

    fn serialize(comic_info: &ComicInfo) -> String {
        let cfg = yaserde::ser::Config {
            perform_indent: true,
            ..Default::default()
        };
        yaserde::ser::to_string_with_config(comic_info, &cfg).unwrap()
    }

    /// A page of a gallery, as Hitomi describes it
    fn page(index: i32) -> crate::hitomi::GalleryFiles {
        crate::hitomi::GalleryFiles {
            width: 1280,
            height: 1810,
            hash: format!("{index:064x}"),
            name: format!("{index:03}.jpg"),
            haswebp: 1,
            hasavif: 1,
            hasjxl: 0,
        }
    }

    /// A comic with every field Hitomi can provide filled in
    fn full_comic() -> Comic {
        Comic {
            id: 2_829_145,
            title: "Example Work".to_string(),
            japanese_title: "\u{4f5c}\u{54c1}".to_string(),
            language: "japanese".to_string(),
            type_field: "doujinshi".to_string(),
            date: "2023-04-18 12:30:00-05".to_string(),
            artists: vec!["Artist One".to_string(), "Artist Two".to_string()],
            groups: vec!["Example Circle".to_string()],
            parodys: vec!["Example Series".to_string()],
            tags: vec![
                Tag {
                    tag: "full color".to_string(),
                    female: 0,
                    male: 0,
                },
                Tag {
                    tag: "sole female".to_string(),
                    female: 1,
                    male: 0,
                },
            ],
            characters: vec!["Character A".to_string(), "Character B".to_string()],
            files: (0..24).map(page).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn elements_follow_the_schema_sequence() {
        let xml = serialize(&ComicInfo::from_comic(&full_comic()));

        let written: Vec<&str> = xml
            .lines()
            .filter_map(|line| line.trim().strip_prefix('<'))
            .filter(|rest| !rest.starts_with('/') && !rest.starts_with('?'))
            .filter_map(|rest| rest.split(['>', ' ']).next())
            .filter(|name| *name != "ComicInfo")
            .collect();

        assert!(!written.is_empty(), "nothing was written:\n{xml}");

        let mut previous = 0;
        for name in &written {
            let position = SCHEMA_ELEMENT_ORDER
                .iter()
                .position(|schema_name| schema_name == name)
                .unwrap_or_else(|| panic!("`{name}` is not an element of the schema"));
            assert!(
                position >= previous,
                "`{name}` comes out of schema order:\n{xml}"
            );
            previous = position;
        }
    }

    #[test]
    fn missing_data_writes_no_empty_elements() {
        let xml = serialize(&ComicInfo::from_comic(&Comic::default()));

        for element in ["Title", "AlternateSeries", "Characters", "Teams"] {
            assert!(
                !xml.contains(&format!("<{element}")),
                "`{element}` should be left out entirely:\n{xml}"
            );
        }
        // A date that cannot be read must not turn into a made up one
        assert!(!xml.contains("<Year"), "{xml}");
    }

    #[test]
    fn artists_fill_every_artist_role() {
        let comic_info = ComicInfo::from_comic(&full_comic());
        let artists = Some("Artist One, Artist Two".to_string());

        assert_eq!(comic_info.writer, artists);
        assert_eq!(comic_info.penciller, artists);
        assert_eq!(comic_info.inker, artists);
        assert_eq!(comic_info.colorist, artists);
    }

    #[test]
    fn the_title_and_the_link_are_written() {
        let comic_info = ComicInfo::from_comic(&full_comic());

        assert_eq!(comic_info.title.as_deref(), Some("Example Work"));
        assert_eq!(comic_info.teams.as_deref(), Some("Example Circle"));
        assert_eq!(
            comic_info.web.as_deref(),
            Some("https://hitomi.la/galleries/2829145.html")
        );
    }

    #[test]
    fn the_summary_repeats_the_title_and_the_link() {
        assert_eq!(
            ComicInfo::from_comic(&full_comic()).summary.as_deref(),
            Some("Example Work\nhttps://hitomi.la/galleries/2829145.html")
        );

        // Without a title the link is all there is to say
        assert_eq!(
            ComicInfo::from_comic(&Comic::default()).summary.as_deref(),
            Some("https://hitomi.la/galleries/0.html")
        );
    }

    #[test]
    fn dates_are_read_or_left_out() {
        assert_eq!(parse_date("2023-04-18 12:30:00-05"), Some((2023, 4, 18)));
        assert_eq!(parse_date("2023-04-18"), Some((2023, 4, 18)));
        assert_eq!(parse_date("2023-4-8 01:02:03+09"), Some((2023, 4, 8)));

        for invalid in [
            "",
            "not a date",
            "2023-13-01",
            "2023-00-01",
            "2023-04-32",
            "2023-04",
            "2023-04-18-11",
            "23-04-18",
        ] {
            assert_eq!(parse_date(invalid), None, "`{invalid}` must not parse");
        }
    }

    #[test]
    fn languages_map_to_iso_codes_or_nothing() {
        assert_eq!(language_iso_code("japanese").as_deref(), Some("ja"));
        assert_eq!(language_iso_code("English").as_deref(), Some("en"));
        assert_eq!(language_iso_code(" german ").as_deref(), Some("de"));

        // Cebuano has no ISO 639-1 code, so no element is better than a wrong one
        assert_eq!(language_iso_code("cebuano"), None);
        assert_eq!(language_iso_code(""), None);
    }

    /// `Notes` states the version of the app, taken from the version of the crate
    ///
    /// The app itself reports the version out of `tauri.conf.json`, so the two have to
    /// agree or the metadata would name a version that never existed. They drifted apart
    /// once already, which is what this guards against.
    #[test]
    fn versions_are_in_step() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        for (label, path) in [
            ("tauri.conf.json", root.join("tauri.conf.json")),
            ("package.json", root.join("..").join("package.json")),
        ] {
            let contents = std::fs::read_to_string(&path).unwrap();
            let value: serde_json::Value = serde_json::from_str(&contents).unwrap();
            let version = value.get("version").and_then(serde_json::Value::as_str);

            assert_eq!(
                version,
                Some(env!("CARGO_PKG_VERSION")),
                "the version in `{label}` differs from the one in Cargo.toml"
            );
        }
    }

    #[test]
    fn the_notes_name_the_version() {
        let comic_info = ComicInfo::from_comic(&full_comic());

        assert_eq!(
            comic_info.notes.as_deref(),
            Some(
                format!(
                    "Downloaded from Hitomi with hitomi-downloader {}",
                    env!("CARGO_PKG_VERSION")
                )
                .as_str()
            )
        );
    }

    #[test]
    fn validates_against_the_schema() {
        let schema = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("ComicInfo-v2.1.xsd");

        for (name, comic) in [("full", full_comic()), ("empty", Comic::default())] {
            let xml = serialize(&ComicInfo::from_comic(&comic));
            let path = std::env::temp_dir().join(format!("comic-info-{name}.xml"));
            std::fs::write(&path, &xml).unwrap();

            let output = match std::process::Command::new("xmllint")
                .arg("--noout")
                .arg("--schema")
                .arg(&schema)
                .arg(&path)
                .output()
            {
                Ok(output) => output,
                // Without xmllint the other tests still cover order and empty elements
                Err(err) => {
                    eprintln!("skipping schema validation, xmllint unavailable: {err}");
                    return;
                }
            };

            assert!(
                output.status.success(),
                "`{name}` does not validate:\n{xml}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
