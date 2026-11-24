//! RFC 9580 section 4: the packet framing, and the reader shared by the
//! packet parsers.

pub(super) struct Reader<'a>(pub(super) &'a [u8]);

impl<'a> Reader<'a> {
    pub(super) fn bytes(&mut self, len: usize) -> Result<&'a [u8], String> {
        let (head, tail) = self.0.split_at_checked(len).ok_or("truncated data")?;
        self.0 = tail;
        Ok(head)
    }

    pub(super) fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(
            self.bytes(2)?.try_into().expect("2 bytes"),
        ))
    }

    pub(super) fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(
            self.bytes(4)?.try_into().expect("4 bytes"),
        ))
    }

    /// Section 3.2: a big-endian integer prefixed by its bit count.
    pub(super) fn mpi(&mut self) -> Result<&'a [u8], String> {
        let bits = self.u16()?;
        self.bytes(usize::from(bits).div_ceil(8))
    }

    pub(super) fn rest(&mut self) -> &'a [u8] {
        std::mem::take(&mut self.0)
    }

    /// Trailing data is an error.
    pub(super) fn finish(&self) -> Result<(), String> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err("unexpected trailing data".into())
        }
    }
}

pub(super) struct Packet<'a> {
    pub(super) tag: u8,
    pub(super) body: &'a [u8],
}

/// The packets of a binary OpenPGP message.
pub(super) struct Packets<'a>(pub(super) &'a [u8]);

impl<'a> Iterator for Packets<'a> {
    type Item = Result<Packet<'a>, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let packet = next_packet(&mut Reader(self.0));
        Some(match packet {
            Ok((packet, rest)) => {
                self.0 = rest;
                Ok(packet)
            }
            Err(err) => {
                // Stop at the first malformed packet.
                self.0 = &[];
                Err(err)
            }
        })
    }
}

fn next_packet<'a>(reader: &mut Reader<'a>) -> Result<(Packet<'a>, &'a [u8]), String> {
    let header = reader.u8()?;
    if header & 0x80 == 0 {
        return Err("invalid packet header".into());
    }
    let (tag, len) = if header & 0x40 != 0 {
        // Section 4.2.1: OpenPGP format.
        let len = match reader.u8()? {
            first @ 0..192 => first.into(),
            first @ 192..224 => ((usize::from(first) - 192) << 8) + usize::from(reader.u8()?) + 192,
            255 => reader.u32()?.try_into().map_err(|_| "packet too large")?,
            _ => return Err("partial body lengths are not supported".into()),
        };
        (header & 0x3F, len)
    } else {
        // Section 4.2.2: legacy format.
        let len = match header & 0x03 {
            0 => reader.u8()?.into(),
            1 => reader.u16()?.into(),
            2 => reader.u32()?.try_into().map_err(|_| "packet too large")?,
            _ => return Err("indeterminate packet lengths are not supported".into()),
        };
        ((header >> 2) & 0x0F, len)
    };
    let body = reader.bytes(len)?;
    Ok((Packet { tag, body }, reader.rest()))
}

/// Section 5.2.3.7: the subpackets of a signature, as
/// `(type, critical, data)`.
pub(super) struct Subpackets<'a>(pub(super) &'a [u8]);

impl<'a> Iterator for Subpackets<'a> {
    type Item = Result<(u8, bool, &'a [u8]), String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.0.is_empty() {
            return None;
        }
        let mut reader = Reader(self.0);
        let subpacket = (|| {
            let len = match reader.u8()? {
                first @ 0..192 => first.into(),
                first @ 192..255 => {
                    ((usize::from(first) - 192) << 8) + usize::from(reader.u8()?) + 192
                }
                255 => reader
                    .u32()?
                    .try_into()
                    .map_err(|_| "subpacket too large")?,
            };
            let data = reader.bytes(len)?;
            let (&kind, data) = data.split_first().ok_or("empty subpacket")?;
            Ok((kind & 0x7F, kind & 0x80 != 0, data))
        })();
        self.0 = if subpacket.is_ok() { reader.0 } else { &[] };
        Some(subpacket)
    }
}
