//! Shared "filter + sort" pipeline for wallpaper entries.
//!
//! Both `ws_server::WallpaperList` (UI browse) and `control::step`
//! (D-Bus / rotator advance) must agree on what "the wallpaper after
//! this one" means, otherwise D-Bus Next jumps to a row the user
//! doesn't see next on screen.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::control_proto as pb;
use crate::model::entities::item;
use crate::model::repo;
use crate::wallpaper_type::WallpaperEntry;
use crate::AppState;

pub type DbMetaMap = HashMap<(String, String), item::Model>;

pub async fn load_db_meta_map(app: &Arc<AppState>) -> Result<DbMetaMap> {
    let libs = repo::list_libraries(&app.db).await?;
    let lib_path_by_id: HashMap<i64, String> = libs.into_iter().map(|l| (l.id, l.path)).collect();
    let items = repo::list_items_all(&app.db).await?;
    Ok(items
        .into_iter()
        .filter_map(|it| {
            let lib_path = lib_path_by_id.get(&it.library_id)?.clone();
            let item_path = it.path.clone();
            Some(((lib_path, item_path), it))
        })
        .collect())
}

/// Apply composite sort rules in-place. Rules are applied in reverse
/// so the first rule ends up as the primary key (sort_by is stable).
pub fn apply_wallpaper_sorts(
    entries: &mut [&WallpaperEntry],
    sorts: &[pb::WallpaperSortRule],
    db_meta_map: &DbMetaMap,
) {
    use std::cmp::Ordering;

    let lookup = |e: &WallpaperEntry| {
        crate::model::sync::relative_under_root(&e.library_root, &e.resource)
            .and_then(|rel| db_meta_map.get(&(e.library_root.clone(), rel)))
    };

    for rule in sorts.iter().rev() {
        let key = match pb::WallpaperSortKey::try_from(rule.key) {
            Ok(k) if k != pb::WallpaperSortKey::Unspecified => k,
            _ => continue,
        };
        let desc = pb::SortDirection::try_from(rule.direction) == Ok(pb::SortDirection::Desc);

        entries.sort_by(|a, b| {
            let ord = match key {
                pb::WallpaperSortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                pb::WallpaperSortKey::WpType => a.wp_type.cmp(&b.wp_type),
                pb::WallpaperSortKey::Size => {
                    let sa = lookup(a).and_then(|m| m.size).or(a.size).unwrap_or(0);
                    let sb = lookup(b).and_then(|m| m.size).or(b.size).unwrap_or(0);
                    sa.cmp(&sb)
                }
                pb::WallpaperSortKey::LastModified => {
                    let ma = lookup(a).and_then(|m| m.modified_at).unwrap_or(0);
                    let mb = lookup(b).and_then(|m| m.modified_at).unwrap_or(0);
                    ma.cmp(&mb)
                }
                pb::WallpaperSortKey::Unspecified => Ordering::Equal,
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
    }
}

/// Resolve the user-visible ordered list of entry ids: snapshot →
/// filter → sort. Mirrors the WallpaperList pipeline so D-Bus
/// next/previous step in the same order the UI shows.
pub async fn ordered_entry_ids(
    app: &Arc<AppState>,
    filters: &[pb::WallpaperFilterRule],
    logics: &[pb::FilterLogic],
    sorts: &[pb::WallpaperSortRule],
) -> Result<Vec<String>> {
    let snap = app.source_snapshot.read().await;
    let raw: Vec<&WallpaperEntry> = snap.list().iter().collect();

    let matched_keys: Option<HashSet<(String, String)>> = if filters.is_empty() {
        None
    } else {
        Some(
            repo::list_item_keys_by_wallpaper_filters(&app.db, filters, logics)
                .await?
                .into_iter()
                .collect(),
        )
    };

    let mut filtered: Vec<&WallpaperEntry> = if let Some(keys) = matched_keys.as_ref() {
        raw.into_iter()
            .filter(|e| {
                crate::model::sync::relative_under_root(&e.library_root, &e.resource)
                    .map(|rel| keys.contains(&(e.library_root.clone(), rel)))
                    .unwrap_or(false)
            })
            .collect()
    } else {
        raw
    };

    if !sorts.is_empty() {
        let db_meta_map = load_db_meta_map(app).await?;
        apply_wallpaper_sorts(&mut filtered, sorts, &db_meta_map);
    }

    Ok(filtered.into_iter().map(|e| e.id.clone()).collect())
}
