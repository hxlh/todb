pub type RowKey<'a> = BinaryKey<'a>;

#[derive(Debug)]
pub struct BinaryKey<'a> {
    buf: &'a [u8],
}

impl<'a> BinaryKey<'a> {
    pub fn as_bytes(&self) -> &[u8] {
        self.buf
    }
}

impl<'a, T> From<&'a T> for BinaryKey<'a>
where
    T: AsRef<[u8]> + ?Sized,
{
    fn from(buf: &'a T) -> Self {
        Self { buf: buf.as_ref() }
    }
}

impl<'a> AsRef<[u8]> for BinaryKey<'a> {
    fn as_ref(&self) -> &[u8] {
        self.buf
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
