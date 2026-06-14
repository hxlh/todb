use crate::{errors::StorageResult, iterators::storage_iter::{ForwardIter, ReverseIter, StorageIter}};

enum Current {
    A,
    B,
}

/// Two-way merge iterator. A (memtable side) wins on equal keys, B is skipped.
///
/// A and B must share the same key and value types. B's key type is constrained
/// to equal A's via `for<'a> B: StorageIter<Key<'a> = A::Key<'a>>`, and likewise
/// for value type.
pub struct TwoMergeIter<A, B> {
    a: A,
    b: B,
    current: Option<Current>,
}

impl<A, B> TwoMergeIter<A, B>
where
    A: 'static + StorageIter,
    B: 'static + for<'a> StorageIter<Key<'a> = A::Key<'a>, Value<'a> = A::Value<'a>>,
{
    pub fn new(mut a: A, mut b: B) -> StorageResult<Self> {
        a.seek_to_first()?;
        b.seek_to_first()?;
        let mut s = Self {
            a,
            b,
            current: None,
        };
        s.skip_b_if_equal()?;
        s.current = s.choose();
        Ok(s)
    }

    fn choose(&self) -> Option<Current> {
        match (self.a.valid(), self.b.valid()) {
            (false, false) => None,
            (true, false) => Some(Current::A),
            (false, true) => Some(Current::B),
            (true, true) => {
                let ak = self.a.key().unwrap();
                let bk = self.b.key().unwrap();
                if ak <= bk {
                    Some(Current::A)
                } else {
                    Some(Current::B)
                }
            }
        }
    }

    fn skip_b_if_equal(&mut self) -> StorageResult<()> {
        while self.a.valid() && self.b.valid() && self.a.key() == self.b.key() {
            self.b.next()?;
        }
        Ok(())
    }
    fn choose_reverse(&self) -> Option<Current> {
        match (self.a.valid(), self.b.valid()) {
            (false, false) => None,
            (true, false) => Some(Current::A),
            (false, true) => Some(Current::B),
            (true, true) => {
                let ak = self.a.key().unwrap();
                let bk = self.b.key().unwrap();
                if ak >= bk {
                    Some(Current::A)
                } else {
                    Some(Current::B)
                }
            }
        }
    }

    fn skip_b_if_equal_rev(&mut self) -> StorageResult<()> {
        while self.a.valid() && self.b.valid() && self.a.key() == self.b.key() {
            self.b.prev()?;
        }
        Ok(())
    }
}

impl<A, B> ForwardIter for TwoMergeIter<A, B>
where
    A: 'static + StorageIter,
    B: 'static + for<'a> StorageIter<Key<'a> = A::Key<'a>, Value<'a> = A::Value<'a>>,
{
    type Key<'a> = A::Key<'a>;
    type Value<'a>
        = A::Value<'a>
    where
        Self: 'a;

    fn seek_to_first(&mut self) -> StorageResult<()> {
        self.a.seek_to_first()?;
        self.b.seek_to_first()?;
        self.skip_b_if_equal()?;
        self.current = self.choose();
        Ok(())
    }

    fn seek<'k>(&mut self, target: &Self::Key<'k>) -> StorageResult<()> {
        self.a.seek(target)?;
        self.b.seek(target)?;
        self.skip_b_if_equal()?;
        self.current = self.choose();
        Ok(())
    }

    fn next(&mut self) -> StorageResult<()> {
        match self.current {
            Some(Current::A) => self.a.next()?,
            Some(Current::B) => self.b.next()?,
            None => return Ok(()),
        }
        self.skip_b_if_equal()?;
        self.current = self.choose();
        Ok(())
    }
}

impl<A, B> ReverseIter for TwoMergeIter<A, B>
where
    A: 'static + StorageIter,
    B: 'static + for<'a> StorageIter<Key<'a> = A::Key<'a>, Value<'a> = A::Value<'a>>,
{
    fn seek_to_last(&mut self) -> StorageResult<()> {
        self.a.seek_to_last()?;
        self.b.seek_to_last()?;
        self.skip_b_if_equal_rev()?;
        self.current = self.choose_reverse();
        Ok(())
    }

    fn seek_for_prev(&mut self, target: &Self::Key<'_>) -> StorageResult<()> {
        self.a.seek_for_prev(target)?;
        self.b.seek_for_prev(target)?;
        self.skip_b_if_equal_rev()?;
        self.current = self.choose_reverse();
        Ok(())
    }

    fn prev(&mut self) -> StorageResult<()> {
        match self.current {
            Some(Current::A) => self.a.prev()?,
            Some(Current::B) => self.b.prev()?,
            None => return Ok(()),
        }
        self.skip_b_if_equal_rev()?;
        self.current = self.choose_reverse();
        Ok(())
    }
}

impl<A, B> StorageIter for TwoMergeIter<A, B>
where
    A: 'static + StorageIter,
    B: 'static + for<'a> StorageIter<Key<'a> = A::Key<'a>, Value<'a> = A::Value<'a>>,
{
    fn valid(&self) -> bool {
        self.current.is_some()
    }

    fn key(&self) -> Option<Self::Key<'_>> {
        match self.current {
            Some(Current::A) => self.a.key(),
            Some(Current::B) => self.b.key(),
            None => None,
        }
    }

    fn value(&self) -> Option<Self::Value<'_>> {
        match self.current {
            Some(Current::A) => self.a.value(),
            Some(Current::B) => self.b.value(),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iterators::storage_iter::{ForwardIter, ReverseIter, StorageIter};

    struct VecIter {
        data: Vec<(&'static [u8], &'static [u8])>,
        pos: usize,
    }

    impl VecIter {
        fn new(data: Vec<(&'static [u8], &'static [u8])>) -> Self {
            Self {
                data,
                pos: usize::MAX,
            }
        }
    }

    impl ForwardIter for VecIter {
        type Key<'a> = &'a [u8];
        type Value<'a>
            = &'a [u8]
        where
            Self: 'a;

        fn seek_to_first(&mut self) -> crate::errors::StorageResult<()> {
            self.pos = if self.data.is_empty() { usize::MAX } else { 0 };
            Ok(())
        }

        fn seek<'a>(&mut self, target: &Self::Key<'a>) -> crate::errors::StorageResult<()> {
            self.pos = self.data.partition_point(|(k, _)| k < target);
            if self.pos >= self.data.len() {
                self.pos = usize::MAX;
            }
            Ok(())
        }

        fn next(&mut self) -> crate::errors::StorageResult<()> {
            if self.valid() {
                self.pos += 1;
                if self.pos >= self.data.len() {
                    self.pos = usize::MAX;
                }
            }
            Ok(())
        }
    }

    impl ReverseIter for VecIter {
        fn seek_to_last(&mut self) -> crate::errors::StorageResult<()> {
            self.pos = if self.data.is_empty() { usize::MAX } else { self.data.len() - 1 };
            Ok(())
        }

        fn seek_for_prev<'a>(
            &mut self,
            target: &Self::Key<'a>,
        ) -> crate::errors::StorageResult<()> {
            let lo = self.data.partition_point(|(k, _)| k < target);
            if lo == 0 && (self.data.is_empty() || self.data[0].0 > *target) {
                self.pos = usize::MAX;
            } else if lo < self.data.len() && self.data[lo].0 == *target {
                self.pos = lo;
            } else {
                self.pos = lo - 1;
            }
            Ok(())
        }

        fn prev(&mut self) -> crate::errors::StorageResult<()> {
            if self.valid() {
                if self.pos == 0 {
                    self.pos = usize::MAX;
                } else {
                    self.pos -= 1;
                }
            }
            Ok(())
        }
    }

    impl StorageIter for VecIter {
        fn valid(&self) -> bool {
            self.pos < self.data.len()
        }

        fn key(&self) -> Option<Self::Key<'_>> {
            if self.valid() {
                Some(self.data[self.pos].0)
            } else {
                None
            }
        }

        fn value(&self) -> Option<Self::Value<'_>> {
            if self.valid() {
                Some(self.data[self.pos].1)
            } else {
                None
            }
        }
    }

    fn collect(iter: &mut TwoMergeIter<VecIter, VecIter>) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut out = vec![];
        while iter.valid() {
            out.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
            iter.next().unwrap();
        }
        out
    }

    #[test]
    fn test_no_overlap_merge() {
        let a = VecIter::new(vec![(b"a", b"va"), (b"c", b"vc")]);
        let b = VecIter::new(vec![(b"b", b"vb"), (b"d", b"vd")]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        let result = collect(&mut iter);
        assert_eq!(
            result,
            vec![
                (b"a".to_vec(), b"va".to_vec()),
                (b"b".to_vec(), b"vb".to_vec()),
                (b"c".to_vec(), b"vc".to_vec()),
                (b"d".to_vec(), b"vd".to_vec()),
            ]
        );
    }

    #[test]
    fn test_a_wins_on_equal_key() {
        let a = VecIter::new(vec![(b"k", b"from_a")]);
        let b = VecIter::new(vec![(b"k", b"from_b")]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        let result = collect(&mut iter);
        assert_eq!(result, vec![(b"k".to_vec(), b"from_a".to_vec())]);
    }

    #[test]
    fn test_a_empty() {
        let a = VecIter::new(vec![]);
        let b = VecIter::new(vec![(b"x", b"vx"), (b"y", b"vy")]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        let result = collect(&mut iter);
        assert_eq!(
            result,
            vec![
                (b"x".to_vec(), b"vx".to_vec()),
                (b"y".to_vec(), b"vy".to_vec()),
            ]
        );
    }

    #[test]
    fn test_b_empty() {
        let a = VecIter::new(vec![(b"x", b"vx")]);
        let b = VecIter::new(vec![]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        let result = collect(&mut iter);
        assert_eq!(result, vec![(b"x".to_vec(), b"vx".to_vec())]);
    }

    #[test]
    fn test_seek() {
        let a = VecIter::new(vec![(b"a", b"va"), (b"c", b"vc")]);
        let b = VecIter::new(vec![(b"b", b"vb"), (b"d", b"vd")]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        iter.seek(&b"c".as_ref()).unwrap();
        let result = collect(&mut iter);
        assert_eq!(
            result,
            vec![
                (b"c".to_vec(), b"vc".to_vec()),
                (b"d".to_vec(), b"vd".to_vec()),
            ]
        );
    }

    #[test]
    fn test_a_skips_all_equal_b_keys() {
        let a = VecIter::new(vec![(b"k", b"from_a"), (b"z", b"vz")]);
        let b = VecIter::new(vec![(b"k", b"from_b1"), (b"k", b"from_b2"), (b"y", b"vy")]);
        let mut iter = TwoMergeIter::new(a, b).unwrap();
        let result = collect(&mut iter);
        assert_eq!(
            result,
            vec![
                (b"k".to_vec(), b"from_a".to_vec()),
                (b"y".to_vec(), b"vy".to_vec()),
                (b"z".to_vec(), b"vz".to_vec()),
            ]
        );
    }
}
