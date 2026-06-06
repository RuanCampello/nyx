//! A thin `Vec<T>` wrapper that only accepts a typed index `I`

use std::marker::PhantomData;
use std::ops::{Deref, DerefMut, Index, IndexMut};

pub trait Idx: Copy {
    fn to_usize(self) -> usize;
}

#[derive(Clone, PartialEq)]
pub struct IndexVec<I, T> {
    raw: Vec<T>,
    _marker: PhantomData<fn(I) -> I>,
}

impl<I, T> IndexVec<I, T> {
    pub const fn new() -> Self {
        Self { raw: Vec::new(), _marker: PhantomData }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self { raw: Vec::with_capacity(cap), _marker: PhantomData }
    }

    pub fn from_elem(value: T, n: usize) -> Self
    where
        T: Clone,
    {
        Self { raw: vec![value; n], _marker: PhantomData }
    }

    pub fn resize(&mut self, n: usize, value: T)
    where
        T: Clone,
    {
        self.raw.resize(n, value);
    }

    pub fn push(&mut self, value: T) {
        self.raw.push(value);
    }

    pub fn append(&mut self, other: &mut Self) {
        self.raw.append(&mut other.raw);
    }

    pub fn len(&self) -> usize {
        self.raw.len()
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    pub fn get(&self, idx: I) -> Option<&T>
    where
        I: Idx,
    {
        self.raw.get(idx.to_usize())
    }

    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.raw.iter_mut()
    }

    pub fn as_slice(&self) -> &[T] {
        &self.raw
    }

    pub fn into_inner(self) -> Vec<T> {
        self.raw
    }
}

impl<I, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;
    fn index(&self, idx: I) -> &T {
        &self.raw[idx.to_usize()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, idx: I) -> &mut T {
        &mut self.raw[idx.to_usize()]
    }
}

impl<I, T> Index<usize> for IndexVec<I, T> {
    type Output = T;
    fn index(&self, idx: usize) -> &T {
        &self.raw[idx]
    }
}

impl<I, T> IndexMut<usize> for IndexVec<I, T> {
    fn index_mut(&mut self, idx: usize) -> &mut T {
        &mut self.raw[idx]
    }
}

impl<I, T> Deref for IndexVec<I, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl<I, T> DerefMut for IndexVec<I, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.raw
    }
}

impl<I, T> Extend<T> for IndexVec<I, T> {
    fn extend<It: IntoIterator<Item = T>>(&mut self, iter: It) {
        self.raw.extend(iter);
    }
}

impl<I, T> FromIterator<T> for IndexVec<I, T> {
    fn from_iter<It: IntoIterator<Item = T>>(iter: It) -> Self {
        Self { raw: Vec::from_iter(iter), _marker: PhantomData }
    }
}

impl<'a, I, T> IntoIterator for &'a IndexVec<I, T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.raw.iter()
    }
}

impl<I, T> IntoIterator for IndexVec<I, T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.raw.into_iter()
    }
}

impl<I, T: std::fmt::Debug> std::fmt::Debug for IndexVec<I, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.raw.fmt(f)
    }
}
