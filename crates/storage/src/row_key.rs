pub type RowKey<'a> = BinaryKey<'a>;

#[derive(Debug)]
pub struct BinaryKey<'a> {
    buf: &'a [u8],
}

impl<'a> BinaryKey<'a> {
    pub fn from_slice<T: AsRef<[u8]> + ?Sized>(b: &'a T) -> Self {
        Self { buf: b.as_ref() }
    }
}

impl Eq for BinaryKey<'_> {}

impl<'a, 'b> PartialEq<BinaryKey<'b>> for BinaryKey<'a> {
    fn eq(&self, other: &BinaryKey<'b>) -> bool {
        self.buf == other.buf
    }
}

impl<'a, 'b> PartialOrd<BinaryKey<'b>> for BinaryKey<'a> {
    fn partial_cmp(&self, other: &BinaryKey<'b>) -> Option<std::cmp::Ordering> {
        self.buf.partial_cmp(other.buf)
    }
}

impl Ord for BinaryKey<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.buf.cmp(other.buf)
    }
}
