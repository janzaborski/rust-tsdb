use crate::model::LabelSet;
use std::hash::{Hash, Hasher};
use xxhash_rust::xxh3::Xxh3Default;

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct PackedLabelOffset {
    name_start: u32,
    value_start: u32,
}

#[derive(Clone, Debug)]
pub struct PackedLabelSet {
    data: Box<[u8]>,
    offsets: Box<[PackedLabelOffset]>,
    fingerprint: u64,
}

impl PackedLabelSet {
    pub fn from_label_set(label_set: &LabelSet) -> Self {
        let mut offsets: Vec<PackedLabelOffset> = Vec::with_capacity(label_set.len());
        let mut data: Vec<u8> = Vec::new();
        let mut fingerprint = Xxh3Default::new();

        let mut running_index = 0u32;
        for (name, value) in label_set {
            let name_bytes = name.as_bytes();
            let value_bytes = value.as_bytes();
            let name_start = running_index;
            let value_start = running_index + name.len() as u32;
            running_index = value_start + value.len() as u32;
            offsets.push(PackedLabelOffset {
                name_start,
                value_start,
            });
            fingerprint.update(&name.len().to_le_bytes());
            fingerprint.update(name_bytes);
            fingerprint.update(&value.len().to_le_bytes());
            fingerprint.update(value_bytes);
            data.extend_from_slice(name_bytes);
            data.extend_from_slice(value_bytes);
        }

        Self {
            data: data.into_boxed_slice(),
            offsets: offsets.into_boxed_slice(),
            fingerprint: fingerprint.digest(),
        }
    }

    #[inline]
    pub fn iter(&self) -> PackedLabelIter<'_> {
        PackedLabelIter {
            data: &self.data,
            offsets: &self.offsets,
            index: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }
}

impl Default for PackedLabelSet {
    fn default() -> Self {
        let hasher = Xxh3Default::new();
        Self {
            data: Box::new([]),
            offsets: Box::new([]),
            fingerprint: hasher.digest(),
        }
    }
}

impl PartialEq for PackedLabelSet {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.fingerprint == other.fingerprint
            && self.offsets == other.offsets
            && self.data == other.data
    }
}

impl Eq for PackedLabelSet {}

impl Hash for PackedLabelSet {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.fingerprint);
    }
}

pub struct PackedLabelIter<'a> {
    data: &'a [u8],
    offsets: &'a [PackedLabelOffset],
    index: usize,
}

impl<'a> Iterator for PackedLabelIter<'a> {
    type Item = (&'a str, &'a str);

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;

        if index == self.offsets.len() {
            return None;
        }

        let current = unsafe { *self.offsets.get_unchecked(index) };

        let name_start = current.name_start as usize;
        let value_start = current.value_start as usize;

        let next_index = index + 1;

        let value_end = if next_index == self.offsets.len() {
            self.data.len()
        } else {
            unsafe { self.offsets.get_unchecked(next_index).name_start as usize }
        };

        self.index = next_index;

        unsafe {
            let name_bytes = self.data.get_unchecked(name_start..value_start);
            let value_bytes = self.data.get_unchecked(value_start..value_end);

            Some((
                std::str::from_utf8_unchecked(name_bytes),
                std::str::from_utf8_unchecked(value_bytes),
            ))
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.offsets.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for PackedLabelIter<'_> {}
impl std::iter::FusedIterator for PackedLabelIter<'_> {}

impl<'a> IntoIterator for &'a PackedLabelSet {
    type Item = (&'a str, &'a str);
    type IntoIter = PackedLabelIter<'a>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
