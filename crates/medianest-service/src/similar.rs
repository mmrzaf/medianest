//! Finding near-duplicate photos: bursts, resized copies, re-saved files.

use std::collections::HashMap;

use medianest_core::AssetId;
use medianest_media::hash_distance;

use crate::{Library, Result};

/// Photos that look alike.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimilarGroup {
    /// Best first: highest resolution, then favorites, then oldest.
    pub assets: Vec<AssetId>,
}

/// Differences up to this many bits (out of 64) count as similar.
const THRESHOLD: u32 = 6;

struct BkTree {
    nodes: Vec<(u64, usize, HashMap<u32, usize>)>,
}

impl BkTree {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn insert(&mut self, hash: u64, item: usize) {
        if self.nodes.is_empty() {
            self.nodes.push((hash, item, HashMap::new()));
            return;
        }
        let mut current = 0;
        loop {
            let distance = hash_distance(self.nodes[current].0, hash);
            match self.nodes[current].2.get(&distance) {
                Some(&child) => current = child,
                None => {
                    let index = self.nodes.len();
                    self.nodes.push((hash, item, HashMap::new()));
                    self.nodes[current].2.insert(distance, index);
                    return;
                }
            }
        }
    }

    fn within(&self, hash: u64, radius: u32, out: &mut Vec<usize>) {
        if self.nodes.is_empty() {
            return;
        }
        let mut stack = vec![0usize];
        while let Some(node) = stack.pop() {
            let (value, item, children) = &self.nodes[node];
            let distance = hash_distance(*value, hash);
            if distance <= radius {
                out.push(*item);
            }
            let low = distance.saturating_sub(radius);
            let high = distance + radius;
            for (edge, child) in children {
                if (low..=high).contains(edge) {
                    stack.push(*child);
                }
            }
        }
    }
}

fn find(parents: &mut [usize], item: usize) -> usize {
    let mut root = item;
    while parents[root] != root {
        root = parents[root];
    }
    let mut current = item;
    while parents[current] != root {
        let next = parents[current];
        parents[current] = root;
        current = next;
    }
    root
}

/// Groups hashes within [`THRESHOLD`] bits of each other.
fn group(hashes: &[u64]) -> Vec<Vec<usize>> {
    let mut tree = BkTree::new();
    for (index, hash) in hashes.iter().enumerate() {
        tree.insert(*hash, index);
    }
    let mut parents: Vec<usize> = (0..hashes.len()).collect();
    let mut neighbours = Vec::new();
    for (index, hash) in hashes.iter().enumerate() {
        neighbours.clear();
        tree.within(*hash, THRESHOLD, &mut neighbours);
        for &other in &neighbours {
            let (a, b) = (find(&mut parents, index), find(&mut parents, other));
            if a != b {
                parents[a] = b;
            }
        }
    }
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for index in 0..hashes.len() {
        let root = find(&mut parents, index);
        groups.entry(root).or_default().push(index);
    }
    groups
        .into_values()
        .filter(|members| members.len() > 1)
        .collect()
}

impl Library {
    /// Groups of similar-looking photos, largest groups first. Pairs the
    /// user said are different are kept apart.
    pub fn similar_groups(&self) -> Result<Vec<SimilarGroup>> {
        let (items, dismissed) =
            self.read(|db| Ok((db.perceptual_hashes()?, db.dismissed_similar()?)))?;
        // Near-identical hashes for unrelated flat images (all white, all
        // black) are common; ignore hashes with almost no structure.
        let items: Vec<_> = items
            .into_iter()
            .filter(|(_, hash, _)| (4..=60).contains(&hash.count_ones()))
            .collect();
        let hashes: Vec<u64> = items.iter().map(|(_, hash, _)| *hash).collect();
        let mut result = Vec::new();
        for members in group(&hashes) {
            let mut ids: Vec<AssetId> = members.iter().map(|&index| items[index].0).collect();
            let members = ids.clone();
            ids.retain(|id| ids_without_dismissal(*id, &members, &dismissed));
            if ids.len() < 2 {
                continue;
            }
            let assets = self.read(|db| db.assets(&ids))?;
            let mut ranked: Vec<_> = assets
                .iter()
                .map(|asset| {
                    let pixels =
                        u64::from(asset.width.unwrap_or(0)) * u64::from(asset.height.unwrap_or(0));
                    (
                        std::cmp::Reverse(pixels),
                        !asset.favorite,
                        asset.added_ms,
                        asset.id,
                    )
                })
                .collect();
            ranked.sort();
            result.push(SimilarGroup {
                assets: ranked.into_iter().map(|(_, _, _, id)| id).collect(),
            });
        }
        result.sort_by_key(|group| std::cmp::Reverse(group.assets.len()));
        Ok(result)
    }

    /// Records that two photos are different, so they are no longer grouped.
    pub fn dismiss_similar(&self, a: AssetId, b: AssetId) -> Result<()> {
        self.write(|writer| writer.dismiss_similar(a, b))
    }
}

/// Keeps an asset unless the user dismissed its pairing with every other member.
fn ids_without_dismissal(
    id: AssetId,
    members: &[AssetId],
    dismissed: &std::collections::HashSet<(AssetId, AssetId)>,
) -> bool {
    members.iter().any(|other| {
        *other != id && {
            let pair = if id <= *other {
                (id, *other)
            } else {
                (*other, id)
            };
            !dismissed.contains(&pair)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_close_hashes_transitively() {
        let base = 0b1011_0110_1100_1010_1111_0000_1010_0101_u64 << 16;
        let hashes = [
            base,
            base ^ 0b11,
            base ^ 0b1111,
            !base,
            0x0123_4567_89ab_cdef,
        ];
        let mut groups = group(&hashes);
        groups.iter_mut().for_each(|g| g.sort_unstable());
        assert_eq!(groups, vec![vec![0, 1, 2]]);
    }
}
