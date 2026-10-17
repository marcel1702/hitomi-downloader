use anyhow::Context;
use futures::future::try_join_all;
use indexmap::IndexSet;

use crate::types::SearchSort;

use super::{get_gallery_ids_for_query, get_gallery_ids_from_nozomi};

/// Gallery ids every positive term is matched against, `None` if the positive terms alone decide
async fn get_base_results(
    sort: SearchSort,
    positive_terms: &[String],
) -> anyhow::Result<Option<IndexSet<i32>>> {
    if let Some(period) = sort.popular_nozomi_tag() {
        get_gallery_ids_from_nozomi(Some("popular"), period, "all")
            .await
            .map(Some)
    } else if positive_terms.is_empty() {
        get_gallery_ids_from_nozomi(None, "index", "all")
            .await
            .map(Some)
    } else {
        Ok(None)
    }
}

/// Keep the ids matched by every positive term and by none of the negative terms
///
/// The order of the first set is kept, so a popularity sorted base stays sorted.
fn combine_results(
    base: Option<IndexSet<i32>>,
    positive_results: Vec<IndexSet<i32>>,
    negative_results: &[IndexSet<i32>],
) -> IndexSet<i32> {
    let mut results = base;
    for new_results in positive_results {
        results = Some(match results {
            None => new_results,
            Some(mut results) => {
                results.retain(|id| new_results.contains(id));
                results
            }
        });
    }

    let mut results = results.unwrap_or_default();
    for new_results in negative_results {
        results.retain(|id| !new_results.contains(id));
    }

    results
}

pub async fn do_search(query: String, sort: SearchSort) -> anyhow::Result<IndexSet<i32>> {
    let terms: Vec<String> = query
        .trim()
        .strip_prefix('?')
        .unwrap_or(&query)
        .to_lowercase()
        .split_whitespace()
        .map(|s| s.replace('_', " "))
        .collect();

    let mut positive_terms = Vec::new();
    let mut negative_terms = Vec::new();

    for term in terms {
        if let Some(negative_term) = term.strip_prefix('-') {
            negative_terms.push(negative_term.to_string());
        } else if !term.is_empty() {
            positive_terms.push(term);
        }
    }

    let get_base_results_task = get_base_results(sort, &positive_terms);
    let get_positive_results_tasks = positive_terms.iter().map(|term| async move {
        get_gallery_ids_for_query(term)
            .await
            .context(format!("Failed to search for `{term}`"))
    });
    let get_negative_results_tasks = negative_terms.iter().map(|term| async move {
        get_gallery_ids_for_query(term)
            .await
            .context(format!("Failed to search for `-{term}`"))
    });

    // A term that could not be fetched fails the search instead of counting as "no match",
    // which would silently turn the results into something the query never asked for
    let (base_results, positive_results, negative_results) = tokio::try_join!(
        get_base_results_task,
        try_join_all(get_positive_results_tasks),
        try_join_all(get_negative_results_tasks)
    )?;

    Ok(combine_results(
        base_results,
        positive_results,
        &negative_results,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[i32]) -> IndexSet<i32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn empty_intersection_stays_empty() {
        let results = combine_results(None, vec![set(&[1, 2]), set(&[3, 4]), set(&[1, 3, 5])], &[]);
        assert!(results.is_empty(), "got {results:?}");
    }

    #[test]
    fn positive_terms_are_intersected() {
        let results = combine_results(None, vec![set(&[3, 1, 2]), set(&[2, 3, 4])], &[]);
        assert_eq!(results, set(&[3, 2]));
    }

    #[test]
    fn base_order_is_kept() {
        let popular = set(&[9, 5, 7, 1]);
        let results = combine_results(Some(popular), vec![set(&[1, 7, 9])], &[]);
        assert_eq!(results.into_iter().collect::<Vec<_>>(), vec![9, 7, 1]);
    }

    #[test]
    fn negative_terms_are_removed() {
        let results = combine_results(Some(set(&[1, 2, 3, 4])), vec![], &[set(&[2]), set(&[4])]);
        assert_eq!(results, set(&[1, 3]));
    }

    #[test]
    fn term_without_match_empties_results() {
        let results = combine_results(None, vec![set(&[1, 2]), set(&[])], &[]);
        assert!(results.is_empty());
    }
}
