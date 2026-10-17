use crate::{context::Context, hitomi::GalleryInfo};
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use specta::Type;

use super::Comic;

#[derive(Default, Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    comics: Vec<Comic>,
    current_page: usize,
    total_page: usize,
    pub ids: Vec<i32>,
}
impl SearchResult {
    /// The comics on this page
    pub fn comics(&self) -> &[Comic] {
        &self.comics
    }

    pub async fn from_gallery_infos(
        context: &Context,
        gallery_infos: Vec<GalleryInfo>,
        current_page: usize,
        total_page: usize,
        ids: Vec<i32>,
    ) -> anyhow::Result<SearchResult> {
        let from_comic_tasks = gallery_infos.into_iter().map(|gallery_info| async {
            let id = gallery_info.id;
            Comic::from_gallery_info(context, gallery_info)
                .await
                .context(format!("Failed to create Comic from gallery_info `{id}`"))
        });

        let comics = futures::future::try_join_all(from_comic_tasks).await?;

        let search_result = SearchResult {
            comics,
            current_page,
            total_page,
            ids,
        };

        Ok(search_result)
    }
}
