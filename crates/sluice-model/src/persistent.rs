//! Persistent maps for the compiled plan: a candidate built from a base shares every subtree
//! the edit does not touch, so building one costs what the edit changes (O(change · log n)),
//! never a copy of the plan's maps.
//!
//! `Tree` is an AVL tree whose nodes are shared through `Arc` and copied along the path an
//! update takes. `Ordered` keys rows by name and orders them by position, as the plan's
//! collections are (`steps.position`, `inputs.position`, …): iteration is in position order,
//! lookup by key.

use std::{borrow::Borrow, cmp::Ordering, fmt, sync::Arc};

type Link<K, V> = Option<Arc<Node<K, V>>>;

#[derive(Clone)]
struct Node<K, V> {
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u8,
    len: usize,
}

fn height<K, V>(link: &Link<K, V>) -> u8 {
    link.as_ref().map_or(0, |node| node.height)
}
fn size<K, V>(link: &Link<K, V>) -> usize {
    link.as_ref().map_or(0, |node| node.len)
}

impl<K, V> Node<K, V> {
    fn update(&mut self) {
        self.height = 1 + height(&self.left).max(height(&self.right));
        self.len = 1 + size(&self.left) + size(&self.right);
    }
}

/// A persistent ordered map. Cloning is O(1); an update copies the O(log n) nodes on its path
/// and shares the rest with every other copy.
pub struct Tree<K, V> {
    root: Link<K, V>,
}
impl<K, V> Clone for Tree<K, V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
        }
    }
}
impl<K, V> Default for Tree<K, V> {
    fn default() -> Self {
        Self { root: None }
    }
}

fn rotate_right<K: Clone, V: Clone>(link: &mut Link<K, V>) {
    let mut node = link.take().expect("a node to rotate");
    let mut left = Arc::make_mut(&mut node).left.take().expect("a left child");
    Arc::make_mut(&mut node).left = Arc::make_mut(&mut left).right.take();
    Arc::make_mut(&mut node).update();
    Arc::make_mut(&mut left).right = Some(node);
    Arc::make_mut(&mut left).update();
    *link = Some(left);
}
fn rotate_left<K: Clone, V: Clone>(link: &mut Link<K, V>) {
    let mut node = link.take().expect("a node to rotate");
    let mut right = Arc::make_mut(&mut node)
        .right
        .take()
        .expect("a right child");
    Arc::make_mut(&mut node).right = Arc::make_mut(&mut right).left.take();
    Arc::make_mut(&mut node).update();
    Arc::make_mut(&mut right).left = Some(node);
    Arc::make_mut(&mut right).update();
    *link = Some(right);
}
fn rebalance<K: Clone, V: Clone>(link: &mut Link<K, V>) {
    let Some(node) = link.as_mut() else {
        return;
    };
    let node = Arc::make_mut(node);
    node.update();
    let balance = i16::from(height(&node.left)) - i16::from(height(&node.right));
    if balance > 1 {
        let left = node.left.as_ref().expect("a heavier left");
        if height(&left.left) < height(&left.right) {
            rotate_left(&mut node.left);
        }
        rotate_right(link);
    } else if balance < -1 {
        let right = node.right.as_ref().expect("a heavier right");
        if height(&right.right) < height(&right.left) {
            rotate_right(&mut node.right);
        }
        rotate_left(link);
    }
}
fn insert<K: Ord + Clone, V: Clone>(link: &mut Link<K, V>, key: K, value: V) -> Option<V> {
    let Some(node) = link.as_mut() else {
        *link = Some(Arc::new(Node {
            key,
            value,
            left: None,
            right: None,
            height: 1,
            len: 1,
        }));
        return None;
    };
    let node = Arc::make_mut(node);
    let old = match key.cmp(&node.key) {
        Ordering::Less => insert(&mut node.left, key, value),
        Ordering::Greater => insert(&mut node.right, key, value),
        Ordering::Equal => return Some(std::mem::replace(&mut node.value, value)),
    };
    rebalance(link);
    old
}
/// Remove the smallest entry under `link`.
fn take_first<K: Clone, V: Clone>(link: &mut Link<K, V>) -> (K, V) {
    let node = Arc::make_mut(link.as_mut().expect("a nonempty subtree"));
    if node.left.is_some() {
        let first = take_first(&mut node.left);
        rebalance(link);
        return first;
    }
    let right = node.right.take();
    let node = link.take().expect("the smallest node");
    *link = right;
    match Arc::try_unwrap(node) {
        Ok(node) => (node.key, node.value),
        Err(shared) => (shared.key.clone(), shared.value.clone()),
    }
}
fn remove<K, V, Q>(link: &mut Link<K, V>, key: &Q) -> Option<V>
where
    K: Ord + Clone + Borrow<Q>,
    V: Clone,
    Q: Ord + ?Sized,
{
    let order = key.cmp(link.as_ref()?.key.borrow());
    let node = Arc::make_mut(link.as_mut().expect("a node"));
    let removed = match order {
        Ordering::Less => remove(&mut node.left, key),
        Ordering::Greater => remove(&mut node.right, key),
        Ordering::Equal => {
            let value = if node.right.is_some() {
                let (k, v) = take_first(&mut node.right);
                node.key = k;
                std::mem::replace(&mut node.value, v)
            } else {
                let left = node.left.take();
                let node = link.take().expect("the removed node");
                *link = left;
                return Some(match Arc::try_unwrap(node) {
                    Ok(node) => node.value,
                    Err(shared) => shared.value.clone(),
                });
            };
            Some(value)
        }
    };
    if removed.is_some() {
        rebalance(link);
    }
    removed
}

impl<K: Ord + Clone, V: Clone> Tree<K, V> {
    pub fn new() -> Self {
        Self { root: None }
    }
    pub fn len(&self) -> usize {
        size(&self.root)
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut link = &self.root;
        while let Some(node) = link {
            match key.cmp(node.key.borrow()) {
                Ordering::Less => link = &node.left,
                Ordering::Greater => link = &node.right,
                Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }
    /// Insert or replace; the old value, if any.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        insert(&mut self.root, key, value)
    }
    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        remove(&mut self.root, key)
    }
    /// The entry with the largest key.
    pub fn last(&self) -> Option<(&K, &V)> {
        let mut node = self.root.as_ref()?;
        while let Some(right) = &node.right {
            node = right;
        }
        Some((&node.key, &node.value))
    }
    /// Entries in key order.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter::new(&self.root, false)
    }
    /// Entries in reverse key order.
    pub fn iter_rev(&self) -> Iter<'_, K, V> {
        Iter::new(&self.root, true)
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }
}
impl<K: Ord + Clone, V: Clone> FromIterator<(K, V)> for Tree<K, V> {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut tree = Self::new();
        for (key, value) in iter {
            tree.insert(key, value);
        }
        tree
    }
}
impl<K: Ord + Clone + PartialEq, V: Clone + PartialEq> PartialEq for Tree<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}
impl<K: Ord + Clone + fmt::Debug, V: Clone + fmt::Debug> fmt::Debug for Tree<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// An in-order (or reverse-order) walk with an explicit stack.
pub struct Iter<'a, K, V> {
    stack: Vec<&'a Node<K, V>>,
    reverse: bool,
}
impl<'a, K, V> Iter<'a, K, V> {
    fn new(root: &'a Link<K, V>, reverse: bool) -> Self {
        let mut iter = Self {
            stack: vec![],
            reverse,
        };
        iter.descend(root);
        iter
    }
    fn descend(&mut self, mut link: &'a Link<K, V>) {
        while let Some(node) = link {
            self.stack.push(node);
            link = if self.reverse {
                &node.right
            } else {
                &node.left
            };
        }
    }
}
impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        let node = self.stack.pop()?;
        self.descend(if self.reverse {
            &node.left
        } else {
            &node.right
        });
        Some((&node.key, &node.value))
    }
}

/// Rows keyed by name and ordered by a unique position, as the plan's collections are. Values
/// are shared (`Arc`), so a copy of the map never copies a row.
pub struct Ordered<K, V> {
    index: Tree<K, u64>,
    rows: Tree<u64, (K, Arc<V>)>,
}
impl<K, V> Clone for Ordered<K, V> {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
            rows: self.rows.clone(),
        }
    }
}
impl<K, V> Default for Ordered<K, V> {
    fn default() -> Self {
        Self {
            index: Tree::default(),
            rows: Tree::default(),
        }
    }
}
impl<K: Ord + Clone, V> Ordered<K, V> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.index.len()
    }
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get_shared(key).map(|value| &**value)
    }
    /// The row's shared value.
    pub fn get_shared<Q>(&self, key: &Q) -> Option<&Arc<V>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let position = self.index.get(key)?;
        self.rows.get(position).map(|(_, value)| value)
    }
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.index.contains_key(key)
    }
    /// The row's position.
    pub fn position<Q>(&self, key: &Q) -> Option<u64>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.index.get(key).copied()
    }
    /// The largest position held, if any.
    pub fn last_position(&self) -> Option<u64> {
        self.rows.last().map(|(position, _)| *position)
    }
    /// Insert or replace the row `key` at `position`. No other row may hold `position`.
    pub fn insert(&mut self, key: K, position: u64, value: Arc<V>) {
        if let Some(old) = self.index.insert(key.clone(), position)
            && old != position
        {
            self.rows.remove(&old);
        }
        let displaced = self.rows.insert(position, (key.clone(), value));
        debug_assert!(
            displaced.is_none_or(|(other, _)| other == key),
            "two rows at position {position}"
        );
    }
    pub fn remove<Q>(&mut self, key: &Q) -> Option<(u64, Arc<V>)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let position = self.index.remove(key)?;
        let (_, value) = self.rows.remove(&position).expect("an indexed row");
        Some((position, value))
    }
    /// Rows in position order.
    pub fn iter(&self) -> OrderedIter<'_, K, V> {
        OrderedIter {
            forward: self.rows.iter(),
            backward: self.rows.iter_rev(),
            left: self.rows.len(),
        }
    }
    /// Rows in position order, with their positions.
    pub fn iter_positions(&self) -> impl Iterator<Item = (u64, &K, &V)> + '_ {
        self.rows
            .iter()
            .map(|(position, (key, value))| (*position, key, &**value))
    }
    /// Rows in reverse position order, with their positions.
    pub fn iter_positions_rev(&self) -> impl Iterator<Item = (u64, &K, &V)> + '_ {
        self.rows
            .iter_rev()
            .map(|(position, (key, value))| (*position, key, &**value))
    }
    pub fn keys(&self) -> impl DoubleEndedIterator<Item = &K> + '_ {
        self.iter().map(|(key, _)| key)
    }
    pub fn values(&self) -> impl DoubleEndedIterator<Item = &V> + '_ {
        self.iter().map(|(_, value)| value)
    }
    /// The first row in position order.
    pub fn first(&self) -> Option<(&K, &V)> {
        self.iter().next()
    }
}

/// `Ordered`'s rows in position order (either end).
pub struct OrderedIter<'a, K, V> {
    forward: Iter<'a, u64, (K, Arc<V>)>,
    backward: Iter<'a, u64, (K, Arc<V>)>,
    left: usize,
}
impl<'a, K, V> Iterator for OrderedIter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        self.forward.next().map(|(_, (key, value))| (key, &**value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}
impl<K, V> DoubleEndedIterator for OrderedIter<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        self.backward
            .next()
            .map(|(_, (key, value))| (key, &**value))
    }
}
impl<K, V> ExactSizeIterator for OrderedIter<'_, K, V> {}

impl<'a, K: Ord + Clone, V> IntoIterator for &'a Ordered<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = OrderedIter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<K, V, Q> std::ops::Index<&Q> for Ordered<K, V>
where
    K: Ord + Clone + Borrow<Q>,
    Q: Ord + ?Sized,
{
    type Output = V;
    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("a row of the map")
    }
}
impl<K: Ord + Clone + PartialEq, V: PartialEq> PartialEq for Ordered<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter_positions()
                .zip(other.iter_positions())
                .all(|(a, b)| a == b)
    }
}
impl<K: Ord + Clone + fmt::Debug, V: fmt::Debug> fmt::Debug for Ordered<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A deterministic byte source for the model checks below.
    struct Seed(u64);
    impl Seed {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn check<K: Ord + Clone, V: Clone>(link: &Link<K, V>) -> (u8, usize) {
        let Some(node) = link else {
            return (0, 0);
        };
        let (lh, ll) = check(&node.left);
        let (rh, rl) = check(&node.right);
        assert!((i16::from(lh) - i16::from(rh)).abs() <= 1, "balanced");
        assert_eq!(node.height, 1 + lh.max(rh));
        assert_eq!(node.len, 1 + ll + rl);
        if let Some(left) = &node.left {
            assert!(left.key < node.key);
        }
        if let Some(right) = &node.right {
            assert!(right.key > node.key);
        }
        (node.height, node.len)
    }

    #[test]
    fn the_tree_agrees_with_a_btree_map_and_old_versions_stay_as_they_were() {
        let mut seed = Seed(0x9e37_79b9_7f4a_7c15);
        let mut tree = Tree::new();
        let mut model = BTreeMap::new();
        let mut versions = vec![];
        for round in 0..4000 {
            let key = seed.next() % 300;
            if seed.next().is_multiple_of(3) {
                assert_eq!(tree.remove(&key), model.remove(&key));
            } else {
                assert_eq!(tree.insert(key, round), model.insert(key, round));
            }
            if round % 400 == 0 {
                versions.push((tree.clone(), model.clone()));
            }
            check(&tree.root);
        }
        assert!(tree.iter().map(|(k, v)| (*k, *v)).eq(model.clone()));
        assert!(
            tree.iter_rev()
                .map(|(k, v)| (*k, *v))
                .eq(model.into_iter().rev())
        );
        for (tree, model) in versions {
            assert!(tree.iter().map(|(k, v)| (*k, *v)).eq(model));
        }
    }

    #[test]
    fn ordered_rows_iterate_by_position_and_move_without_disturbing_others() {
        let mut rows: Ordered<String, u32> = Ordered::new();
        for (i, name) in ["c", "a", "b"].into_iter().enumerate() {
            rows.insert(name.into(), i as u64 * 10, Arc::new(i as u32));
        }
        let base = rows.clone();
        rows.insert("c".into(), 25, Arc::new(7));
        rows.remove("a");
        assert_eq!(rows.keys().collect::<Vec<_>>(), ["b", "c"]);
        assert_eq!(rows["c"], 7);
        assert_eq!(rows.position("c"), Some(25));
        assert_eq!(rows.last_position(), Some(25));
        assert_eq!(base.keys().collect::<Vec<_>>(), ["c", "a", "b"]);
        assert_eq!(base.keys().rev().collect::<Vec<_>>(), ["b", "a", "c"]);
        assert_eq!(base["c"], 0);
    }
}
