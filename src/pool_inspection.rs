//! Read-only pool inspection, shared by schedule preview and future CLI tools.
//! Uses the same materialization as playback, but never chooses a track,
//! reads/advances cursors or group_state, or calls filter_pool plugins.

use sqlx::SqlitePool;

use crate::playlist::{MemberQuota, Mode, Playlist, PlaylistError, Selection, Strategy};
use crate::selection::{materialize_dynamic, materialize_static, SelectionError};

/// Count and duration are independently optional: a remote/queue member can
/// have a declared runtime even though its media count is unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub selected_count: Option<u64>,
    pub total_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedMember {
    pub r#ref: String,
    pub quota: Option<MemberQuota>,
    pub offset_secs: Option<u64>,
    pub stats: PoolStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedGroup {
    pub strategy: Strategy,
    pub members: Vec<InspectedMember>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolInspection {
    pub stats: PoolStats,
    pub group: Option<InspectedGroup>,
}

/// Inspect the currently indexed pool for a playlist. The present selection
/// predicates have no clock argument: future occurrences use today's index,
/// not a prediction of future imports, removals or playback history.
///
/// Group totals sum the member pools (not a distinct union across members).
/// Shared media therefore count once per member. Quotas never truncate local
/// pools; remote/queue members use their declared runtime when one exists.
/// Missing refs, invalid filters and nested groups are explicit errors, not
/// empty pools or guessed zero durations.
pub async fn inspect_ref(
    pool: &SqlitePool,
    playlist_ref: &str,
) -> Result<PoolInspection, SelectionError> {
    let playlist = load_playlist(pool, playlist_ref).await?;
    let sel = &playlist.selection;
    if sel.mode != Mode::Group {
        return Ok(PoolInspection {
            stats: inspect_leaf(pool, sel)
                .await
                .map_err(|e| identify_playlist(e, playlist_ref))?,
            group: None,
        });
    }

    let strategy = sel
        .strategy
        .ok_or_else(|| SelectionError::Unsupported("group without strategy".into()))?;
    // Reuse the existing take/runtime and offset projection unchanged.
    let projection = playlist.project_group_members();
    let mut members = Vec::with_capacity(sel.members.len());
    let mut total = PoolStats {
        selected_count: Some(0),
        total_duration_ms: Some(0),
    };
    for (i, member) in sel.members.iter().enumerate() {
        let child = load_playlist(pool, &member.r#ref).await?;
        let mut stats = inspect_leaf(pool, &child.selection)
            .await
            .map_err(|e| identify_playlist(e, &member.r#ref))?;
        if matches!(child.selection.mode, Mode::Remote | Mode::Queue) {
            if let Some(runtime) = &member.runtime {
                let seconds = crate::playlist::parse_duration_secs(runtime)
                    .map_err(|e| SelectionError::Parse(PlaylistError::Validation(e)))?;
                stats.total_duration_ms = Some(seconds.checked_mul(1000).ok_or_else(overflow)?);
            }
        }
        total.selected_count = add_known(total.selected_count, stats.selected_count)?;
        total.total_duration_ms = add_known(total.total_duration_ms, stats.total_duration_ms)?;
        let projected = projection.as_ref().and_then(|g| g.members.get(i));
        members.push(InspectedMember {
            r#ref: member.r#ref.clone(),
            quota: projected.map(|m| m.quota.clone()),
            offset_secs: projected.and_then(|m| m.offset_secs),
            stats,
        });
    }
    Ok(PoolInspection {
        stats: total,
        group: Some(InspectedGroup { strategy, members }),
    })
}

// Keep the typed error (and therefore its gRPC status), but identify the
// actual leaf playlist when inspection was reached through a group.
fn identify_playlist(error: SelectionError, reference: &str) -> SelectionError {
    match error {
        SelectionError::BadFilterValue { field, reason } => SelectionError::BadFilterValue {
            field,
            reason: format!("playlist `{reference}`: {reason}"),
        },
        other => other,
    }
}

async fn load_playlist(pool: &SqlitePool, reference: &str) -> Result<Playlist, SelectionError> {
    let toml = crate::store::playlist_toml_by_ref(pool, reference)
        .await?
        .ok_or_else(|| SelectionError::PlaylistNotFound(reference.into()))?;
    let playlist = Playlist::parse(&toml)?;
    playlist.validate()?;
    Ok(playlist)
}

async fn inspect_leaf(pool: &SqlitePool, sel: &Selection) -> Result<PoolStats, SelectionError> {
    let candidates = match sel.mode {
        Mode::Dynamic => materialize_dynamic(pool, sel).await?,
        Mode::Static => materialize_static(pool, &sel.files).await?,
        // No indexed media/duration source exists for these modes yet.
        // A containing member can still supply its explicit runtime above.
        Mode::Remote | Mode::Queue => return Ok(PoolStats::default()),
        Mode::Group => return Err(SelectionError::Unsupported("nested group pools".into())),
    };
    // SUM(duration_ms) over the materialized pool, preserving milliseconds.
    // Empty is known zero. Materialization already excludes unavailable files
    // and deduplicates static entries; selection order does not change a pool.
    let duration = candidates.iter().try_fold(0_u64, |sum, c| {
        sum.checked_add(c.duration_ms).ok_or_else(overflow)
    })?;
    Ok(PoolStats {
        selected_count: Some(candidates.len() as u64),
        total_duration_ms: Some(duration),
    })
}

fn add_known(a: Option<u64>, b: Option<u64>) -> Result<Option<u64>, SelectionError> {
    match (a, b) {
        (Some(a), Some(b)) => a.checked_add(b).map(Some).ok_or_else(overflow),
        _ => Ok(None),
    }
}

fn overflow() -> SelectionError {
    SelectionError::Parse(PlaylistError::Validation("pool statistics overflow".into()))
}
