// Copyright 2023 The Jujutsu Authors
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(missing_docs)]

use std::cmp::Ordering;
use std::cmp::Reverse;
use std::cmp::max;
use std::collections::BinaryHeap;
use std::collections::HashSet;
use std::collections::binary_heap;
use std::iter;
use std::mem;
use std::sync::Arc;
use std::sync::Mutex;

use itertools::Itertools as _;
use ref_cast::RefCastCustom;
use ref_cast::ref_cast_custom;

use super::entry::IndexEntry;
use super::entry::IndexPosition;
use super::entry::LocalPosition;
use super::entry::SmallIndexPositionsVec;
use super::entry::SmallLocalPositionsVec;
use super::readonly::ReadonlyIndexSegment;
use super::rev_walk::AncestorsBitSet;
use super::revset_engine;
use crate::backend::ChangeId;
use crate::backend::CommitId;
use crate::hex_util;
use crate::index::AllHeadsForGcUnsupported;
use crate::index::ChangeIdIndex;
use crate::index::Index;
use crate::index::IndexError;
use crate::object_id::HexPrefix;
use crate::object_id::ObjectId as _;
use crate::object_id::PrefixResolution;
use crate::revset::ResolvedExpression;
use crate::revset::Revset;
use crate::revset::RevsetEvaluationError;
use crate::store::Store;

pub(super) trait IndexSegment: Send + Sync {
    fn num_parent_commits(&self) -> u32;

    fn num_local_commits(&self) -> u32;

    fn parent_file(&self) -> Option<&Arc<ReadonlyIndexSegment>>;

    fn name(&self) -> Option<String>;

    fn commit_id_to_pos(&self, commit_id: &CommitId) -> Option<LocalPosition>;

    /// Suppose the given `commit_id` exists, returns the previous and next
    /// commit ids in lexicographical order.
    fn resolve_neighbor_commit_ids(
        &self,
        commit_id: &CommitId,
    ) -> (Option<CommitId>, Option<CommitId>);

    fn resolve_commit_id_prefix(&self, prefix: &HexPrefix) -> PrefixResolution<CommitId>;

    fn resolve_neighbor_change_ids(
        &self,
        change_id: &ChangeId,
    ) -> (Option<ChangeId>, Option<ChangeId>);

    fn resolve_change_id_prefix(
        &self,
        prefix: &HexPrefix,
    ) -> PrefixResolution<(ChangeId, SmallLocalPositionsVec)>;

    fn generation_number(&self, local_pos: LocalPosition) -> u32;

    fn commit_id(&self, local_pos: LocalPosition) -> CommitId;

    fn change_id(&self, local_pos: LocalPosition) -> ChangeId;

    fn num_parents(&self, local_pos: LocalPosition) -> u32;

    fn parent_positions(&self, local_pos: LocalPosition) -> SmallIndexPositionsVec;
}

pub(super) type DynIndexSegment = dyn IndexSegment;

/// Abstraction over owned and borrowed types that can be cheaply converted to
/// a `CompositeIndex` reference.
pub trait AsCompositeIndex {
    /// Returns reference wrapper that provides global access to this index.
    fn as_composite(&self) -> &CompositeIndex;
}

impl<T: AsCompositeIndex + ?Sized> AsCompositeIndex for &T {
    fn as_composite(&self) -> &CompositeIndex {
        <T as AsCompositeIndex>::as_composite(self)
    }
}

impl<T: AsCompositeIndex + ?Sized> AsCompositeIndex for &mut T {
    fn as_composite(&self) -> &CompositeIndex {
        <T as AsCompositeIndex>::as_composite(self)
    }
}

/// `CompositeIndex` provides an index of both commit IDs and change IDs.
///
/// We refer to this as a composite index because it's a composite of multiple
/// nested index segments where each parent segment is roughly twice as large
/// its child. segment. This provides a good balance between read and write
/// performance.
// Reference wrapper that provides global access to nested index segments.
#[derive(RefCastCustom)]
#[repr(transparent)]
pub struct CompositeIndex(DynIndexSegment);

impl CompositeIndex {
    #[ref_cast_custom]
    pub(super) const fn new(segment: &DynIndexSegment) -> &Self;

    /// Iterates parent and its ancestor readonly index segments.
    pub(super) fn ancestor_files_without_local(
        &self,
    ) -> impl Iterator<Item = &Arc<ReadonlyIndexSegment>> {
        let parent_file = self.0.parent_file();
        iter::successors(parent_file, |file| file.parent_file())
    }

    /// Iterates self and its ancestor index segments.
    pub(super) fn ancestor_index_segments(&self) -> impl Iterator<Item = &DynIndexSegment> {
        iter::once(&self.0).chain(
            self.ancestor_files_without_local()
                .map(|file| file.as_ref() as &DynIndexSegment),
        )
    }

    pub fn num_commits(&self) -> u32 {
        self.0.num_parent_commits() + self.0.num_local_commits()
    }

    pub fn stats(&self) -> IndexStats {
        let num_commits = self.num_commits();
        let mut num_merges = 0;
        let mut max_generation_number = 0;
        let mut change_ids = HashSet::new();
        for pos in 0..num_commits {
            let entry = self.entry_by_pos(IndexPosition(pos));
            max_generation_number = max(max_generation_number, entry.generation_number());
            if entry.num_parents() > 1 {
                num_merges += 1;
            }
            change_ids.insert(entry.change_id());
        }
        let num_heads = u32::try_from(self.all_heads_pos().count()).unwrap();

        let mut levels = self
            .ancestor_index_segments()
            .map(|segment| IndexLevelStats {
                num_commits: segment.num_local_commits(),
                name: segment.name(),
            })
            .collect_vec();
        levels.reverse();

        IndexStats {
            num_commits,
            num_merges,
            max_generation_number,
            num_heads,
            num_changes: change_ids.len().try_into().unwrap(),
            levels,
        }
    }

    pub fn entry_by_pos(&self, pos: IndexPosition) -> IndexEntry<'_> {
        self.ancestor_index_segments()
            .find_map(|segment| {
                u32::checked_sub(pos.0, segment.num_parent_commits())
                    .map(|local_pos| IndexEntry::new(segment, pos, LocalPosition(local_pos)))
            })
            .unwrap()
    }

    pub fn entry_by_id(&self, commit_id: &CommitId) -> Option<IndexEntry<'_>> {
        self.ancestor_index_segments().find_map(|segment| {
            let local_pos = segment.commit_id_to_pos(commit_id)?;
            let pos = IndexPosition(local_pos.0 + segment.num_parent_commits());
            Some(IndexEntry::new(segment, pos, local_pos))
        })
    }

    pub fn commit_id_to_pos(&self, commit_id: &CommitId) -> Option<IndexPosition> {
        self.ancestor_index_segments().find_map(|segment| {
            let LocalPosition(local_pos) = segment.commit_id_to_pos(commit_id)?;
            Some(IndexPosition(local_pos + segment.num_parent_commits()))
        })
    }

    /// Suppose the given `commit_id` exists, returns the previous and next
    /// commit ids in lexicographical order.
    pub(super) fn resolve_neighbor_commit_ids(
        &self,
        commit_id: &CommitId,
    ) -> (Option<CommitId>, Option<CommitId>) {
        self.ancestor_index_segments()
            .map(|segment| segment.resolve_neighbor_commit_ids(commit_id))
            .reduce(|(acc_prev_id, acc_next_id), (prev_id, next_id)| {
                (
                    acc_prev_id.into_iter().chain(prev_id).max(),
                    acc_next_id.into_iter().chain(next_id).min(),
                )
            })
            .unwrap()
    }

    /// Suppose the given `change_id` exists, returns the minimum prefix length
    /// to disambiguate it within all the indexed ids including hidden ones.
    pub(super) fn shortest_unique_change_id_prefix_len(&self, change_id: &ChangeId) -> usize {
        let (prev_id, next_id) = self.resolve_neighbor_change_ids(change_id);
        itertools::chain(prev_id, next_id)
            .map(|id| hex_util::common_hex_len(change_id.as_bytes(), id.as_bytes()) + 1)
            .max()
            .unwrap_or(0)
    }

    /// Suppose the given `change_id` exists, returns the previous and next
    /// change ids in lexicographical order. The returned change ids may be
    /// hidden.
    pub(super) fn resolve_neighbor_change_ids(
        &self,
        change_id: &ChangeId,
    ) -> (Option<ChangeId>, Option<ChangeId>) {
        self.ancestor_index_segments()
            .map(|segment| segment.resolve_neighbor_change_ids(change_id))
            .reduce(|(acc_prev_id, acc_next_id), (prev_id, next_id)| {
                (
                    acc_prev_id.into_iter().chain(prev_id).max(),
                    acc_next_id.into_iter().chain(next_id).min(),
                )
            })
            .unwrap()
    }

    /// Resolves the given change id `prefix` to the associated entries. The
    /// returned entries may be hidden.
    ///
    /// The returned index positions are sorted in ascending order.
    pub(super) fn resolve_change_id_prefix(
        &self,
        prefix: &HexPrefix,
    ) -> PrefixResolution<(ChangeId, SmallIndexPositionsVec)> {
        use PrefixResolution::*;
        self.ancestor_index_segments()
            .fold(NoMatch, |acc_match, segment| {
                if acc_match == AmbiguousMatch {
                    return acc_match; // avoid checking the parent file(s)
                }
                let to_global_pos = {
                    let num_parent_commits = segment.num_parent_commits();
                    move |LocalPosition(pos)| IndexPosition(pos + num_parent_commits)
                };
                // Similar to PrefixResolution::plus(), but merges matches of the same id.
                match (acc_match, segment.resolve_change_id_prefix(prefix)) {
                    (NoMatch, local_match) => local_match.map(|(id, positions)| {
                        (id, positions.into_iter().map(to_global_pos).collect())
                    }),
                    (acc_match, NoMatch) => acc_match,
                    (AmbiguousMatch, _) => AmbiguousMatch,
                    (_, AmbiguousMatch) => AmbiguousMatch,
                    (SingleMatch((id1, _)), SingleMatch((id2, _))) if id1 != id2 => AmbiguousMatch,
                    (SingleMatch((id, mut acc_positions)), SingleMatch((_, local_positions))) => {
                        acc_positions
                            .insert_many(0, local_positions.into_iter().map(to_global_pos));
                        SingleMatch((id, acc_positions))
                    }
                }
            })
    }

    pub(super) fn is_ancestor_pos(
        &self,
        ancestor_pos: IndexPosition,
        descendant_pos: IndexPosition,
    ) -> bool {
        let ancestor_generation = self.entry_by_pos(ancestor_pos).generation_number();
        let mut work = vec![descendant_pos];
        let mut visited = HashSet::new();
        while let Some(descendant_pos) = work.pop() {
            let descendant_entry = self.entry_by_pos(descendant_pos);
            if descendant_pos == ancestor_pos {
                return true;
            }
            if !visited.insert(descendant_entry.position()) {
                continue;
            }
            if descendant_entry.generation_number() <= ancestor_generation {
                continue;
            }
            work.extend(descendant_entry.parent_positions());
        }
        false
    }

    /// Computes the greatest common ancestors.
    ///
    /// The returned index positions are sorted in descending order.
    pub(super) fn common_ancestors_pos(
        &self,
        set1: Vec<IndexPosition>,
        set2: Vec<IndexPosition>,
    ) -> Vec<IndexPosition> {
        let mut items1 = BinaryHeap::from(set1);
        let mut items2 = BinaryHeap::from(set2);
        let mut result = Vec::new();
        while let (Some(&pos1), Some(&pos2)) = (items1.peek(), items2.peek()) {
            match pos1.cmp(&pos2) {
                Ordering::Greater => shift_to_parents(&mut items1, &self.entry_by_pos(pos1)),
                Ordering::Less => shift_to_parents(&mut items2, &self.entry_by_pos(pos2)),
                Ordering::Equal => {
                    result.push(pos1);
                    dedup_pop(&mut items1).unwrap();
                    dedup_pop(&mut items2).unwrap();
                }
            }
        }
        self.heads_pos(result)
    }

    pub(super) fn all_heads(&self) -> impl Iterator<Item = CommitId> + use<'_> {
        self.all_heads_pos()
            .map(move |pos| self.entry_by_pos(pos).commit_id())
    }

    pub(super) fn all_heads_pos(&self) -> impl Iterator<Item = IndexPosition> + use<> {
        // TODO: can be optimized to use bit vec and leading/trailing_ones()
        let num_commits = self.num_commits();
        let mut not_head: Vec<bool> = vec![false; num_commits as usize];
        for pos in 0..num_commits {
            let entry = self.entry_by_pos(IndexPosition(pos));
            for IndexPosition(parent_pos) in entry.parent_positions() {
                not_head[parent_pos as usize] = true;
            }
        }
        not_head
            .into_iter()
            .enumerate()
            .filter(|&(_, b)| !b)
            .map(|(i, _)| IndexPosition(u32::try_from(i).unwrap()))
    }

    /// Returns the subset of positions in `candidate_positions` which refer to
    /// entries that are heads in the repository.
    ///
    /// The `candidate_positions` must be sorted in descending order, and have
    /// no duplicates. The returned head positions are also sorted in descending
    /// order.
    pub fn heads_pos(&self, candidate_positions: Vec<IndexPosition>) -> Vec<IndexPosition> {
        debug_assert!(
            candidate_positions
                .iter()
                .tuple_windows()
                .all(|(a, b)| a > b)
        );
        let Some(min_generation) = candidate_positions
            .iter()
            .map(|&pos| self.entry_by_pos(pos).generation_number())
            .min()
        else {
            return candidate_positions;
        };

        // Iterate though the candidates by reverse index position, keeping track of the
        // ancestors of already-found heads. If a candidate is an ancestor of an
        // already-found head, then it can be removed.
        let mut parents = BinaryHeap::new();
        let mut heads = Vec::new();
        'outer: for candidate in candidate_positions {
            while let Some(&parent) = parents.peek().filter(|&&parent| parent >= candidate) {
                let entry = self.entry_by_pos(parent);
                if entry.generation_number() <= min_generation {
                    dedup_pop(&mut parents).unwrap();
                } else {
                    shift_to_parents(&mut parents, &entry);
                }
                if parent == candidate {
                    // The candidate is an ancestor of an existing head, so we can skip it.
                    continue 'outer;
                }
            }
            // No parents matched, so this commit is a head.
            let entry = self.entry_by_pos(candidate);
            parents.extend(entry.parent_positions());
            heads.push(candidate);
        }
        heads
    }

    /// Find the heads of a range of positions `roots..heads`, applying a filter
    /// to the commits in the range. The heads are sorted in descending order.
    /// The filter will also be called in descending index position order.
    pub fn heads_from_range_and_filter<E>(
        &self,
        roots: Vec<IndexPosition>,
        heads: Vec<IndexPosition>,
        mut filter: impl FnMut(IndexPosition) -> Result<bool, E>,
    ) -> Result<Vec<IndexPosition>, E> {
        if heads.is_empty() {
            return Ok(heads);
        }
        let mut wanted_queue = BinaryHeap::from(heads);
        let mut unwanted_queue = BinaryHeap::from(roots);
        let mut found_heads = Vec::new();
        while let Some(&pos) = wanted_queue.peek() {
            if shift_to_parents_until(&mut unwanted_queue, self, pos) {
                dedup_pop(&mut wanted_queue);
                continue;
            }
            let entry = self.entry_by_pos(pos);
            if filter(pos)? {
                dedup_pop(&mut wanted_queue);
                unwanted_queue.extend(entry.parent_positions());
                found_heads.push(pos);
            } else {
                shift_to_parents(&mut wanted_queue, &entry);
            }
        }
        Ok(found_heads)
    }

    pub(super) fn evaluate_revset(
        &self,
        expression: &ResolvedExpression,
        store: &Arc<Store>,
    ) -> Result<Box<dyn Revset + '_>, RevsetEvaluationError> {
        let revset_impl = revset_engine::evaluate(expression, store, self)?;
        Ok(Box::new(revset_impl))
    }
}

impl AsCompositeIndex for CompositeIndex {
    fn as_composite(&self) -> &CompositeIndex {
        self
    }
}

// In revset engine, we need to convert &CompositeIndex to &dyn Index.
impl Index for &CompositeIndex {
    /// Suppose the given `commit_id` exists, returns the minimum prefix length
    /// to disambiguate it. The length to be returned is a number of hexadecimal
    /// digits.
    ///
    /// If the given `commit_id` doesn't exist, this will return the prefix
    /// length that never matches with any commit ids.
    fn shortest_unique_commit_id_prefix_len(&self, commit_id: &CommitId) -> usize {
        let (prev_id, next_id) = self.resolve_neighbor_commit_ids(commit_id);
        itertools::chain(prev_id, next_id)
            .map(|id| hex_util::common_hex_len(commit_id.as_bytes(), id.as_bytes()) + 1)
            .max()
            .unwrap_or(0)
    }

    fn resolve_commit_id_prefix(&self, prefix: &HexPrefix) -> PrefixResolution<CommitId> {
        self.ancestor_index_segments()
            .fold(PrefixResolution::NoMatch, |acc_match, segment| {
                if acc_match == PrefixResolution::AmbiguousMatch {
                    acc_match // avoid checking the parent file(s)
                } else {
                    let local_match = segment.resolve_commit_id_prefix(prefix);
                    acc_match.plus(&local_match)
                }
            })
    }

    fn has_id(&self, commit_id: &CommitId) -> bool {
        self.commit_id_to_pos(commit_id).is_some()
    }

    fn is_ancestor(&self, ancestor_id: &CommitId, descendant_id: &CommitId) -> bool {
        let ancestor_pos = self.commit_id_to_pos(ancestor_id).unwrap();
        let descendant_pos = self.commit_id_to_pos(descendant_id).unwrap();
        self.is_ancestor_pos(ancestor_pos, descendant_pos)
    }

    fn common_ancestors(&self, set1: &[CommitId], set2: &[CommitId]) -> Vec<CommitId> {
        let pos1 = set1
            .iter()
            .map(|id| self.commit_id_to_pos(id).unwrap())
            .collect_vec();
        let pos2 = set2
            .iter()
            .map(|id| self.commit_id_to_pos(id).unwrap())
            .collect_vec();
        self.common_ancestors_pos(pos1, pos2)
            .iter()
            .map(|pos| self.entry_by_pos(*pos).commit_id())
            .collect()
    }

    fn all_heads_for_gc(
        &self,
    ) -> Result<Box<dyn Iterator<Item = CommitId> + '_>, AllHeadsForGcUnsupported> {
        Ok(Box::new(self.all_heads()))
    }

    fn heads(
        &self,
        candidate_ids: &mut dyn Iterator<Item = &CommitId>,
    ) -> Result<Vec<CommitId>, IndexError> {
        let mut candidate_positions = candidate_ids
            .map(|id| self.commit_id_to_pos(id).unwrap())
            .collect_vec();
        candidate_positions.sort_unstable_by_key(|&pos| Reverse(pos));
        candidate_positions.dedup();

        Ok(self
            .heads_pos(candidate_positions)
            .iter()
            .map(|pos| self.entry_by_pos(*pos).commit_id())
            .collect())
    }

    fn evaluate_revset<'index>(
        &'index self,
        expression: &ResolvedExpression,
        store: &Arc<Store>,
    ) -> Result<Box<dyn Revset + 'index>, RevsetEvaluationError> {
        CompositeIndex::evaluate_revset(self, expression, store)
    }
}

pub(super) struct ChangeIdIndexImpl<I> {
    index: I,
    reachable_set: Mutex<AncestorsBitSet>,
}

impl<I: AsCompositeIndex> ChangeIdIndexImpl<I> {
    pub fn new(index: I, heads: &mut dyn Iterator<Item = &CommitId>) -> ChangeIdIndexImpl<I> {
        let composite = index.as_composite();
        let mut reachable_set = AncestorsBitSet::with_capacity(composite.num_commits());
        for id in heads {
            reachable_set.add_head(composite.commit_id_to_pos(id).unwrap());
        }
        ChangeIdIndexImpl {
            index,
            reachable_set: Mutex::new(reachable_set),
        }
    }
}

impl<I: AsCompositeIndex + Send + Sync> ChangeIdIndex for ChangeIdIndexImpl<I> {
    // Resolves change id prefix among all ids, then filters out hidden
    // entries.
    //
    // If `SingleMatch` is returned, the commits including in the set are all
    // visible. `AmbiguousMatch` may be returned even if the prefix is unique
    // within the visible entries.
    fn resolve_prefix(&self, prefix: &HexPrefix) -> PrefixResolution<Vec<CommitId>> {
        let index = self.index.as_composite();
        match index.resolve_change_id_prefix(prefix) {
            PrefixResolution::NoMatch => PrefixResolution::NoMatch,
            PrefixResolution::SingleMatch((_change_id, positions)) => {
                debug_assert!(positions.iter().tuple_windows().all(|(a, b)| a < b));
                let mut reachable_set = self.reachable_set.lock().unwrap();
                reachable_set.visit_until(index, *positions.first().unwrap());
                let reachable_commit_ids = positions
                    .iter()
                    .filter(|&&pos| reachable_set.contains(pos))
                    .map(|&pos| index.entry_by_pos(pos).commit_id())
                    .collect_vec();
                if reachable_commit_ids.is_empty() {
                    PrefixResolution::NoMatch
                } else {
                    PrefixResolution::SingleMatch(reachable_commit_ids)
                }
            }
            PrefixResolution::AmbiguousMatch => PrefixResolution::AmbiguousMatch,
        }
    }

    // Calculates the shortest prefix length of the given `change_id` among all
    // IDs, including hidden entries.
    //
    // The returned length is usually a few digits longer than the minimum
    // length necessary to disambiguate within the visible entries since hidden
    // entries are also considered when determining the prefix length.
    fn shortest_unique_prefix_len(&self, change_id: &ChangeId) -> usize {
        self.index
            .as_composite()
            .shortest_unique_change_id_prefix_len(change_id)
    }
}

pub struct IndexLevelStats {
    pub num_commits: u32,
    pub name: Option<String>,
}

pub struct IndexStats {
    pub num_commits: u32,
    pub num_merges: u32,
    pub max_generation_number: u32,
    pub num_heads: u32,
    pub num_changes: u32,
    pub levels: Vec<IndexLevelStats>,
}

/// Repeatedly `shift_to_parents` until reaching a target position. Returns true
/// if the target position matched a position in the queue.
fn shift_to_parents_until(
    queue: &mut BinaryHeap<IndexPosition>,
    index: &CompositeIndex,
    target_pos: IndexPosition,
) -> bool {
    while let Some(&pos) = queue.peek().filter(|&&pos| pos >= target_pos) {
        shift_to_parents(queue, &index.entry_by_pos(pos));
        if pos == target_pos {
            return true;
        }
    }
    false
}

/// Removes an entry from the queue and replace it with its parents.
fn shift_to_parents(items: &mut BinaryHeap<IndexPosition>, entry: &IndexEntry) {
    let mut parent_positions = entry.parent_positions().into_iter();
    if let Some(parent_pos) = parent_positions.next() {
        assert!(parent_pos < entry.position());
        dedup_replace(items, parent_pos).unwrap();
    } else {
        dedup_pop(items).unwrap();
        return;
    }
    for parent_pos in parent_positions {
        assert!(parent_pos < entry.position());
        items.push(parent_pos);
    }
}

/// Removes the greatest items (including duplicates) from the heap, returns
/// one.
fn dedup_pop<T: Ord>(heap: &mut BinaryHeap<T>) -> Option<T> {
    let item = heap.pop()?;
    remove_dup(heap, &item);
    Some(item)
}

/// Removes the greatest items (including duplicates) from the heap, inserts
/// lesser `new_item` to the heap, returns the removed one.
///
/// This is faster than calling `dedup_pop(heap)` and `heap.push(new_item)`
/// especially when `new_item` is the next greatest item.
fn dedup_replace<T: Ord>(heap: &mut BinaryHeap<T>, new_item: T) -> Option<T> {
    let old_item = {
        let mut x = heap.peek_mut()?;
        mem::replace(&mut *x, new_item)
    };
    remove_dup(heap, &old_item);
    Some(old_item)
}

fn remove_dup<T: Ord>(heap: &mut BinaryHeap<T>, item: &T) {
    while let Some(x) = heap.peek_mut().filter(|x| **x == *item) {
        binary_heap::PeekMut::pop(x);
    }
}
