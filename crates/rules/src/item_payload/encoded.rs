//! Small persistence values retain an allocation-free codec while exposing only
//! their used legacy prefix plus optional reference section.
use super::{Error, PayloadId};
#[derive(Clone, Copy, Debug)]
pub struct Encoded<const N: usize> {
    bytes: [u8; N],
    used: usize,
    referenced: bool,
}
impl<const N: usize> Encoded<N> {
    pub fn from_prefix(prefix: &[u8]) -> Self {
        assert!(prefix.len() <= N, "bounded codec prefix");
        let mut bytes = [0; N];
        bytes[..prefix.len()].copy_from_slice(prefix);
        Self {
            bytes,
            used: prefix.len(),
            referenced: false,
        }
    }
    pub fn append_refs(
        &mut self,
        refs: impl IntoIterator<Item = (usize, Option<PayloadId>)>,
    ) -> Result<(), Error> {
        if self.referenced {
            return Err(Error::Encoding);
        }
        let mut count = 0u16;
        let mut last = None;
        let original = self.used;
        for (slot, id) in refs {
            if let Some(id) = id {
                if slot > u16::MAX as usize
                    || id.0 == 0
                    || last.is_some_and(|s| s >= slot)
                    || count as usize >= super::refs::MAX_REFERENCE_SLOTS
                {
                    self.used = original;
                    return Err(Error::Encoding);
                }
                if count == 0 {
                    if self.used + 6 > N {
                        self.used = original;
                        return Err(Error::Bounds);
                    }
                    self.bytes[self.used..self.used + 4].copy_from_slice(b"XRF1");
                    self.used += 6;
                }
                if self.used + 10 > N {
                    self.used = original;
                    return Err(Error::Bounds);
                }
                self.bytes[self.used..self.used + 2].copy_from_slice(&(slot as u16).to_le_bytes());
                self.bytes[self.used + 2..self.used + 10].copy_from_slice(&id.0.to_le_bytes());
                self.used += 10;
                count += 1;
                last = Some(slot);
            }
        }
        if count > 0 {
            self.bytes[original + 4..original + 6].copy_from_slice(&count.to_le_bytes());
            self.referenced = true;
        }
        Ok(())
    }
}
impl<const N: usize> PartialEq for Encoded<N> {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
impl<const N: usize> Eq for Encoded<N> {}
impl<const N: usize> std::ops::Deref for Encoded<N> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes[..self.used]
    }
}
impl<const N: usize> std::ops::DerefMut for Encoded<N> {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[..self.used]
    }
}
impl<const N: usize> AsRef<[u8]> for Encoded<N> {
    fn as_ref(&self) -> &[u8] {
        self
    }
}
