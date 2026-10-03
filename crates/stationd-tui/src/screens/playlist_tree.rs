//! A group graph projected into selectable tree occurrences. Shared members
//! keep one occurrence per parent; cycles never recurse into an ancestor.
use stationd_proto::playlist::PlaylistSummary;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Deref;

pub struct Node<'a> {
    pub playlist: &'a PlaylistSummary,
    pub branch: Vec<String>,
}
impl Deref for Node<'_> {
    type Target = PlaylistSummary;
    fn deref(&self) -> &Self::Target {
        self.playlist
    }
}
impl Node<'_> {
    pub fn prefix(&self, collapsed: &BTreeSet<Vec<String>>, filtering: bool) -> String {
        let marker = if self.mode == "group" {
            if collapsed.contains(&self.branch) && !filtering {
                "▸ "
            } else {
                "▾ "
            }
        } else {
            "· "
        };
        format!(
            "{}{marker}",
            "  ".repeat(self.branch.len().saturating_sub(1))
        )
    }
}

pub fn key(p: &PlaylistSummary) -> String {
    if p.rel_path.is_empty() {
        p.id.clone()
    } else {
        p.rel_path.clone()
    }
}

pub fn visible<'a>(
    ordered: &[&'a PlaylistSummary],
    needle: &str,
    collapsed: &BTreeSet<Vec<String>>,
) -> Vec<Node<'a>> {
    let mut children = BTreeMap::<String, Vec<&PlaylistSummary>>::new();
    let groups: BTreeSet<_> = ordered
        .iter()
        .filter(|p| p.mode == "group")
        .map(|p| key(p).to_lowercase())
        .collect();
    let mut roots = Vec::new();
    for p in ordered {
        let mut parents = BTreeSet::new();
        for group in &p.groups {
            let group = group.to_lowercase();
            if groups.contains(&group) && parents.insert(group.clone()) {
                children.entry(group).or_default().push(*p);
            }
        }
        if parents.is_empty() {
            roots.push(*p);
        }
    }
    // Include orphaned/cyclic components too, without exposing members of a
    // collapsed branch as new roots.
    fn cover(
        p: &PlaylistSummary,
        children: &BTreeMap<String, Vec<&PlaylistSummary>>,
        reached: &mut BTreeSet<String>,
    ) {
        let id = key(p).to_lowercase();
        if !reached.insert(id.clone()) {
            return;
        }
        for child in children.get(&id).into_iter().flatten() {
            cover(child, children, reached);
        }
    }
    let mut reached = BTreeSet::new();
    for p in &roots {
        cover(p, &children, &mut reached);
    }
    for p in ordered {
        if !reached.contains(&key(p).to_lowercase()) {
            roots.push(*p);
            cover(p, &children, &mut reached);
        }
    }
    fn visit<'a>(
        p: &'a PlaylistSummary,
        children: &BTreeMap<String, Vec<&'a PlaylistSummary>>,
        needle: &str,
        collapsed: &BTreeSet<Vec<String>>,
        branch: &mut Vec<String>,
        inherited_match: bool,
        out: &mut Vec<Node<'a>>,
    ) {
        let id = key(p);
        if branch
            .iter()
            .any(|ancestor| ancestor.eq_ignore_ascii_case(&id))
        {
            return;
        }
        branch.push(id.clone());
        let matches = inherited_match
            || needle.is_empty()
            || id.to_lowercase().contains(needle)
            || p.name.to_lowercase().contains(needle);
        let mut descendants = Vec::new();
        if p.mode == "group" && (!collapsed.contains(branch) || !needle.is_empty()) {
            for child in children.get(&id.to_lowercase()).into_iter().flatten() {
                visit(
                    child,
                    children,
                    needle,
                    collapsed,
                    branch,
                    matches,
                    &mut descendants,
                );
            }
        }
        if matches || !descendants.is_empty() {
            out.push(Node {
                playlist: p,
                branch: branch.clone(),
            });
            out.extend(descendants);
        }
        branch.pop();
    }
    let mut out = Vec::new();
    for p in roots {
        visit(
            p,
            &children,
            needle,
            collapsed,
            &mut Vec::new(),
            false,
            &mut out,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn playlist(reference: &str, mode: &str, groups: &[&str]) -> PlaylistSummary {
        PlaylistSummary {
            rel_path: reference.into(),
            name: reference.into(),
            mode: mode.into(),
            groups: groups.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        }
    }
    #[test]
    fn nested_groups_shared_members_and_standalone_playlists_keep_distinct_occurrences() {
        let rows = [
            playlist("a", "group", &[]),
            playlist("nested", "group", &["a"]),
            playlist("song", "static", &["nested", "b"]),
            playlist("b", "group", &[]),
            playlist("solo", "dynamic", &[]),
        ];
        let ordered: Vec<_> = rows.iter().collect();
        let tree = visible(&ordered, "", &BTreeSet::new());
        assert_eq!(
            tree.iter().map(|n| n.rel_path.as_str()).collect::<Vec<_>>(),
            ["a", "nested", "song", "b", "song", "solo"]
        );
        assert_eq!(tree[2].branch, ["a", "nested", "song"]);
        assert_eq!(tree[4].branch, ["b", "song"]);
        let collapsed = BTreeSet::from([vec!["a".into()]]);
        let tree = visible(&ordered, "", &collapsed);
        assert_eq!(
            tree.iter().map(|n| n.rel_path.as_str()).collect::<Vec<_>>(),
            ["a", "b", "song", "solo"]
        );
    }
    #[test]
    fn search_keeps_ancestors_and_opens_collapsed_matches() {
        let rows = [
            playlist("root", "group", &[]),
            playlist("child", "group", &["root"]),
            playlist("target", "static", &["child"]),
            playlist("other", "queue", &[]),
        ];
        let ordered: Vec<_> = rows.iter().collect();
        let collapsed = BTreeSet::from([vec!["root".into()]]);
        let tree = visible(&ordered, "target", &collapsed);
        assert_eq!(
            tree.iter().map(|n| n.rel_path.as_str()).collect::<Vec<_>>(),
            ["root", "child", "target"]
        );
        assert_eq!(visible(&ordered, "root", &collapsed).len(), 3);
    }
    #[test]
    fn cycles_and_missing_parents_still_show_every_playlist() {
        let rows = [
            playlist("a", "group", &["b"]),
            playlist("b", "group", &["a"]),
            playlist("leaf", "static", &["b"]),
            playlist("orphan", "static", &["missing"]),
        ];
        let ordered: Vec<_> = rows.iter().collect();
        let tree = visible(&ordered, "", &BTreeSet::new());
        let shown: BTreeSet<_> = tree.iter().map(|n| n.rel_path.as_str()).collect();
        assert_eq!(shown, BTreeSet::from(["a", "b", "leaf", "orphan"]));
        assert_eq!(tree.len(), 4);
    }
}
