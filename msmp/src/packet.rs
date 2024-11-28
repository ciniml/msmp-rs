use core::fmt::Display;

#[derive(Debug, Clone, Copy)]
pub struct Address(u8);

impl Address {
    /// Create a new address from a value.
    /// # Panics
    /// Panics if the value is greater than 0x0f.
    pub fn new(value: u8) -> Self {
        if value > 0x0f {
            panic!("Invalid address");
        }
        Self(value)
    }
    pub fn value(&self) -> u8 {
        self.0
    }
}

impl Into<u8> for Address {
    fn into(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Address {
    type Error = ();
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value > 0x0f {
            Err(())
        } else {
            Ok(Self(value))
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageType {
    Normal = 0,
    Reserved(u8),
}

impl TryFrom<u8> for MessageType {
    type Error = ();
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Normal),
            1..=3 => Ok(Self::Reserved(value)),
            _ => Err(()),
        }
    }
}
impl Into<u8> for MessageType {
    fn into(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Reserved(value) => value,
        }
    }
}

pub struct PacketReader<'a> {
    raw: &'a [u8],
}

impl<'a> PacketReader<'a> {
    pub fn new(raw: &'a [u8]) -> Self {
        Self::try_new(raw).unwrap()
    }

    pub fn try_new(raw: &'a [u8]) -> Result<Self, PacketError> {
        if raw.len() < 3 {
            return Err(PacketError::HeaderTooShort);
        }
        Ok(Self { raw })
    }

    pub fn destination_address(&self) -> Address {
        Address::new(self.raw[0] >> 4)
    }
    pub fn source_address(&self) -> Address {
        Address::new(self.raw[0] & 0x0f)
    }
    pub fn message_length(&self) -> usize {
        (self.raw[1] & 0x3f) as usize
    }
    pub fn message_type(&self) -> MessageType {
        MessageType::try_from(self.raw[1] >> 6).unwrap()
    }

    fn check_message_length(&self) -> Result<(), PacketError> {
        let message_length = self.message_length();
        if message_length + 2 > self.raw.len() {
            return Err(PacketError::InvalidLength);
        }
        Ok(())
    }

    pub fn body_unchecked(&self) -> &[u8] {
        &self.raw[2..self.message_length() + 2]
    }

    pub fn body(&self) -> Result<&[u8], PacketError> {
        self.check_message_length()?;
        Ok(self.body_unchecked())
    }
}

impl Display for PacketReader<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Packet {{ destination_address: {}, source_address: {}, message_length: {}, message_type: {:?}, body: {:?} }}",
            self.destination_address().value(),
            self.source_address().value(),
            self.message_length(),
            self.message_type(),
            self.body().unwrap()
        )
    }
}

pub struct PacketWriter<'a> {
    buffer: &'a mut [u8],
}
impl<'a> PacketWriter<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer }
    }

    pub fn set_destination_address(&mut self, address: Address) -> Result<(), PacketError> {
        self.buffer[0] = (address.value() << 4) | (self.buffer[0] & 0x0f);
        Ok(())
    }
    pub fn set_source_address(&mut self, address: Address) -> Result<(), PacketError> {
        self.buffer[0] = address.value() | (self.buffer[0] & 0xf0);
        Ok(())
    }
    pub fn message_length(&self) -> usize {
        (self.buffer[1] & 0x3f) as usize
    }
    pub fn set_message_length(&mut self, length: usize) -> Result<(), PacketError> {
        if length > 0x3f {
            return Err(PacketError::InvalidLength);
        }
        self.buffer[1] = (self.buffer[1] & 0xc0) | (length as u8);
        Ok(())
    }
    pub fn set_message_type(&mut self, message_type: MessageType) -> Result<(), PacketError> {
        self.buffer[1] = (self.buffer[1] & 0x3f) | (Into::<u8>::into(message_type) << 6);
        Ok(())
    }

    pub fn body_mut(&mut self) -> &mut [u8] {
        let message_length = self.message_length();
        &mut self.buffer[2..2 + message_length]
    }
}

#[derive(Debug)]
pub enum PacketError {
    HeaderTooShort,
    InvalidHeader,
    InvalidLength,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_packet_reader() {
        let raw = [0x12, 0x02, 0x56, 0x78];
        let reader = PacketReader::new(&raw);
        assert_eq!(reader.destination_address().value(), 0x01);
        assert_eq!(reader.source_address().value(), 0x02);
        assert_eq!(reader.message_length(), 0x02);
        assert_eq!(reader.message_type(), MessageType::Normal);
        assert_eq!(reader.body_unchecked(), &[0x56, 0x78]);
    }

    #[test]
    fn test_packet_writer() {
        let mut buffer = [0; 4];
        let mut writer = PacketWriter::new(&mut buffer);
        writer.set_destination_address(Address::new(0x01)).unwrap();
        writer.set_source_address(Address::new(0x02)).unwrap();
        writer.set_message_length(0x02).unwrap();
        writer.set_message_type(MessageType::Normal).unwrap();
        writer.body_mut()[0] = 0x56;
        writer.body_mut()[1] = 0x78;
        assert_eq!(writer.body_mut().len(), 2);
        assert_eq!(buffer, [0x12, 0x02, 0x56, 0x78]);
    }
}
