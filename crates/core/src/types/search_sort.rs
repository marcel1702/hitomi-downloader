use serde::{Deserialize, Serialize};
use specta::Type;

/// The order search results are listed in
#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum SearchSort {
    /// Newest first
    #[default]
    Date,
    PopularToday,
    PopularWeek,
    PopularMonth,
    PopularYear,
}

impl SearchSort {
    /// The tag of the nozomi file Hitomi keeps the popularity ranking in, `None` for `Date`
    ///
    /// Hitomi ranks by period, the file of a period lies at `n/popular/{tag}-all.nozomi`.
    pub fn popular_nozomi_tag(self) -> Option<&'static str> {
        match self {
            SearchSort::Date => None,
            SearchSort::PopularToday => Some("today"),
            SearchSort::PopularWeek => Some("week"),
            SearchSort::PopularMonth => Some("month"),
            SearchSort::PopularYear => Some("year"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_popularity_period_has_its_own_nozomi_file() {
        assert_eq!(SearchSort::Date.popular_nozomi_tag(), None);
        assert_eq!(SearchSort::PopularToday.popular_nozomi_tag(), Some("today"));
        assert_eq!(SearchSort::PopularWeek.popular_nozomi_tag(), Some("week"));
        assert_eq!(SearchSort::PopularMonth.popular_nozomi_tag(), Some("month"));
        assert_eq!(SearchSort::PopularYear.popular_nozomi_tag(), Some("year"));
    }
}
