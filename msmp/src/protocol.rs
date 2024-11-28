use crate::packet::{PacketError, PacketReader};

pub trait StreamReader {
    type Error;
    fn read(&mut self, data: &mut [u8]) -> nb::Result<usize, Self::Error>;
}

#[cfg(feature = "async")]
pub trait StreamReaderAsync {
    type Error;
    fn read(
        &mut self,
        data: &mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>>;
}

pub trait StreamWriter {
    type Error;
    fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error>;
}

#[cfg(feature = "async")]
pub trait StreamWriterAsync {
    type Error;
    fn write(
        &mut self,
        data: &[u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>>;
}

pub struct ProtocolReader<const BUFFER_SIZE: usize, T: AsRef<[u8]> + AsMut<[u8]>> {
    buffer: T,
    position: usize,
    state: ReaderState,
}

#[derive(PartialEq)]
enum ReaderState {
    Header,
    Data,
    Completed,
}

#[derive(Debug)]
pub enum ProtocolReaderError<ReaderError> {
    ReaderError(ReaderError),
    PacketError(PacketError),
    InsufficientBuffer,
}

impl From<PacketError> for ProtocolReaderError<()> {
    fn from(error: PacketError) -> Self {
        Self::PacketError(error)
    }
}

impl<const BUFFER_SIZE: usize> ProtocolReader<BUFFER_SIZE, [u8; BUFFER_SIZE]> {
    pub fn new() -> Self {
        Self {
            buffer: [0; BUFFER_SIZE],
            position: 0,
            state: ReaderState::Header,
        }
    }
}

impl<const BUFFER_SIZE: usize, T: AsRef<[u8]> + AsMut<[u8]>> ProtocolReader<BUFFER_SIZE, T> {
    pub fn new_with_buffer(buffer: T) -> Self {
        Self {
            buffer,
            position: 0,
            state: ReaderState::Header,
        }
    }

    pub fn release(self) -> T {
        self.buffer
    }

    #[cfg(feature = "async")]
    async fn read_inner_async<R: StreamReaderAsync>(
        &mut self,
        reader: &mut R,
    ) -> Result<(bool, bool), ProtocolReaderError<R::Error>> {
        let (new_state, position, fully_read) = match self.state {
            ReaderState::Header | ReaderState::Completed => {
                let bytes_read = reader
                    .read(&mut self.buffer.as_mut()[self.position..2])
                    .await
                    .map_err(ProtocolReaderError::ReaderError)?;
                if self.position + bytes_read < 2 {
                    (ReaderState::Header, bytes_read, false)
                } else {
                    (ReaderState::Data, 2, true)
                }
            }
            ReaderState::Data => {
                let length = PacketReader::new(self.buffer.as_ref()).message_length();
                let end = length + 2;
                if end > BUFFER_SIZE {
                    // The buffer is too small to hold the packet.
                    self.state = ReaderState::Header;
                    self.position = 0;
                    return Err(ProtocolReaderError::InsufficientBuffer);
                }
                let bytes_to_read = end - self.position;
                let bytes_read = if bytes_to_read == 0 {
                    0
                } else {
                    reader
                        .read(&mut self.buffer.as_mut()[self.position..end])
                        .await
                        .map_err(ProtocolReaderError::ReaderError)?
                };
                let new_position = self.position + bytes_read;
                let new_data = &self.buffer.as_ref()[self.position..new_position];
                if let Some(sync_position) = new_data.iter().rposition(|&byte| byte == 0x00) {
                    if sync_position < new_data.len() - 1 {
                        // The buffer contains a sync byte. Discard the data before the sync byte and start from the next byte.
                        let new_data_len = new_data.len();
                        self.buffer
                            .as_mut()
                            .copy_within(self.position + sync_position + 1..new_position, 0);
                        let new_position = new_data_len - (sync_position + 1);
                        if new_position < 2 {
                            (ReaderState::Header, new_position, false)
                        } else {
                            (ReaderState::Data, new_position, false)
                        }
                    } else {
                        // The sync byte is at the end of the buffer. Discard the entire buffer and start from receiving the header.
                        (ReaderState::Header, 0, false)
                    }
                } else {
                    let new_state = if new_position == end {
                        ReaderState::Completed
                    } else {
                        ReaderState::Data
                    };
                    (new_state, new_position, bytes_read == bytes_to_read)
                }
            }
        };
        self.state = new_state;
        self.position = position;
        Ok((self.state == ReaderState::Completed, fully_read))
    }

    fn read_inner<R: StreamReader>(
        &mut self,
        reader: &mut R,
    ) -> Result<(bool, bool), ProtocolReaderError<R::Error>> {
        let (new_state, position, fully_read) = match self.state {
            ReaderState::Header | ReaderState::Completed => {
                let bytes_read = match reader.read(&mut self.buffer.as_mut()[self.position..2]) {
                    Ok(bytes_read) => bytes_read,
                    Err(nb::Error::WouldBlock) => 0,
                    Err(nb::Error::Other(err)) => {
                        return Err(ProtocolReaderError::ReaderError(err))
                    }
                };
                if self.position + bytes_read < 2 {
                    (ReaderState::Header, bytes_read, false)
                } else {
                    (ReaderState::Data, 2, true)
                }
            }
            ReaderState::Data => {
                let length = PacketReader::new(self.buffer.as_ref()).message_length();
                let end = length + 2;
                if end > BUFFER_SIZE {
                    // The buffer is too small to hold the packet.
                    self.state = ReaderState::Header;
                    self.position = 0;
                    return Err(ProtocolReaderError::InsufficientBuffer);
                }
                let bytes_to_read = end - self.position;
                let bytes_read = if bytes_to_read == 0 {
                    0
                } else {
                    match reader.read(&mut self.buffer.as_mut()[self.position..end]) {
                        Ok(bytes_read) => bytes_read,
                        Err(nb::Error::WouldBlock) => 0,
                        Err(nb::Error::Other(err)) => {
                            return Err(ProtocolReaderError::ReaderError(err))
                        }
                    }
                };
                let new_position = self.position + bytes_read;
                let new_data = &self.buffer.as_mut()[self.position..new_position];
                if let Some(sync_position) = new_data.iter().rposition(|&byte| byte == 0x00) {
                    if sync_position < new_data.len() - 1 {
                        // The buffer contains a sync byte. Discard the data before the sync byte and start from the next byte.
                        let new_data_len = new_data.len();
                        self.buffer
                            .as_mut()
                            .copy_within(self.position + sync_position + 1..new_position, 0);
                        let new_position = new_data_len - (sync_position + 1);
                        if new_position < 2 {
                            (ReaderState::Header, new_position, false)
                        } else {
                            (ReaderState::Data, new_position, false)
                        }
                    } else {
                        // The sync byte is at the end of the buffer. Discard the entire buffer and start from receiving the header.
                        (ReaderState::Header, 0, false)
                    }
                } else {
                    let new_state = if new_position == end {
                        ReaderState::Completed
                    } else {
                        ReaderState::Data
                    };
                    (new_state, new_position, bytes_read == bytes_to_read)
                }
            }
        };
        self.state = new_state;
        self.position = position;
        Ok((self.state == ReaderState::Completed, fully_read))
    }

    pub fn read<R: StreamReader>(
        &mut self,
        reader: &mut R,
    ) -> Result<bool, ProtocolReaderError<R::Error>> {
        loop {
            let (completed, fully_read) = self.read_inner(reader)?;
            if completed {
                return Ok(true);
            } else if !fully_read {
                return Ok(false);
            }
        }
    }

    #[cfg(feature = "async")]
    pub async fn read_async<R: StreamReaderAsync>(
        &mut self,
        reader: &mut R,
    ) -> Result<bool, ProtocolReaderError<R::Error>> {
        loop {
            let (completed, fully_read) = self.read_inner_async(reader).await?;
            if completed {
                return Ok(true);
            } else if !fully_read {
                return Ok(false);
            }
        }
    }

    pub fn packet(&self) -> Option<PacketReader> {
        if self.state == ReaderState::Completed {
            Some(PacketReader::new(&self.buffer.as_ref()[0..self.position]))
        } else {
            None
        }
    }
}

impl StreamReader for &[u8] {
    type Error = ();
    fn read(&mut self, data: &mut [u8]) -> nb::Result<usize, Self::Error> {
        let len = core::cmp::min(data.len(), self.len());
        data[..len].copy_from_slice(&self[..len]);
        *self = &self[len..];
        Ok(len)
    }
}
pub struct BufferStreamWriter<'a> {
    buffer: &'a mut [u8],
    position: usize,
}
impl<'a> BufferStreamWriter<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, position: 0 }
    }
}
impl StreamWriter for BufferStreamWriter<'_> {
    type Error = ();
    fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error> {
        let len = core::cmp::min(data.len(), self.buffer.len() - self.position);
        self.buffer[self.position..self.position + len].copy_from_slice(&data[..len]);
        self.position += len;
        Ok(len)
    }
}
#[cfg(feature = "async")]
impl StreamWriterAsync for BufferStreamWriter<'_> {
    type Error = ();
    fn write(
        &mut self,
        data: &[u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> {
        async move {
            let len = core::cmp::min(data.len(), self.buffer.len() - self.position);
            self.buffer[self.position..self.position + len].copy_from_slice(&data[..len]);
            self.position += len;
            Ok(len)
        }
    }
}

pub struct StreamWrapper<'a, T> {
    inner: &'a mut T,
}
impl<'a, T> StreamWrapper<'a, T> {
    pub fn new(inner: &'a mut T) -> Self {
        Self { inner }
    }
}

#[cfg(feature = "std")]
extern crate std;

#[cfg(feature = "std")]
impl<'a, T: std::io::Read> StreamReader for StreamWrapper<'a, T> {
    type Error = std::io::Error;
    fn read(&mut self, data: &mut [u8]) -> nb::Result<usize, Self::Error> {
        std::io::Read::read(self.inner, data).map_err(|err| nb::Error::Other(err))
    }
}
#[cfg(feature = "std")]
impl<'a, T: std::io::Write> StreamWriter for StreamWrapper<'a, T> {
    type Error = std::io::Error;
    fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error> {
        std::io::Write::write(self.inner, data).map_err(|err| nb::Error::Other(err))
    }
}

#[cfg(feature = "std")]
impl StreamReader for std::sync::mpsc::Receiver<u8> {
    type Error = ();
    fn read(&mut self, data: &mut [u8]) -> nb::Result<usize, Self::Error> {
        let mut bytes_read = 0;
        for i in 0..data.len() {
            match self.try_recv() {
                Ok(byte) => {
                    data[i] = byte;
                    bytes_read += 1;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    if bytes_read == 0 {
                        return Err(nb::Error::WouldBlock);
                    } else {
                        break;
                    }
                }
                Err(_err) => return Err(nb::Error::Other(())),
            }
        }
        Ok(bytes_read)
    }
}

#[cfg(feature = "std")]
impl StreamWriter for std::sync::mpsc::Sender<u8> {
    type Error = ();
    fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error> {
        let mut bytes_written = 0;
        for byte in data {
            match self.send(*byte) {
                Ok(()) => {
                    bytes_written += 1;
                }
                Err(_err) => return Err(nb::Error::Other(())),
            }
        }
        Ok(bytes_written)
    }
}

#[cfg(feature = "tokio")]
impl StreamReaderAsync for tokio::sync::mpsc::Receiver<u8> {
    type Error = ();
    fn read(
        &mut self,
        data: &mut [u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> {
        async move {
            let mut bytes_read = 0;
            for i in 0..data.len() {
                match self.try_recv() {
                    Ok(byte) => {
                        data[i] = byte;
                        bytes_read += 1;
                    }
                    Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                    Err(_err) => return Err(()),
                }
            }
            Ok(bytes_read)
        }
    }
}

#[cfg(feature = "tokio")]
impl StreamWriterAsync for tokio::sync::mpsc::Sender<u8> {
    type Error = ();
    fn write(
        &mut self,
        data: &[u8],
    ) -> impl core::future::Future<Output = Result<usize, Self::Error>> {
        async move {
            let mut bytes_written = 0;
            for byte in data {
                match self.send(*byte).await {
                    Ok(()) => {
                        bytes_written += 1;
                    }
                    Err(_err) => return Err(()),
                }
            }
            Ok(bytes_written)
        }
    }
}

#[cfg_attr(feature = "std", cfg(test))]
mod test_std {
    use crate::packet::MessageType;

    use super::*;
    extern crate std;
    use std::io::Cursor;

    #[test]
    fn test_protocol_reader() {
        let mut reader = ProtocolReader::<6, [u8; 6]>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[test]
    fn test_protocol_reader_ref() {
        let mut buffer = [0; 6];
        let mut reader = ProtocolReader::<6, _>::new_with_buffer(&mut buffer);
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_async() {
        let mut reader = ProtocolReader::<6, [u8; 6]>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw {
            tx.send(*byte).await.unwrap();
        }

        let result = reader.read_async(&mut rx).await;
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[test]
    fn test_protocol_reader_insuffucient_buffer() {
        let mut reader = ProtocolReader::<5, _>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        match result {
            Err(ProtocolReaderError::InsufficientBuffer) => {}
            _ => panic!("Unexpected result: {:?}", result),
        }
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_insuffucient_buffer_async() {
        let mut reader = ProtocolReader::<5, _>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw {
            tx.send(*byte).await.unwrap();
        }

        let result = reader.read_async(&mut rx).await;
        match result {
            Err(ProtocolReaderError::InsufficientBuffer) => {}
            _ => panic!("Unexpected result: {:?}", result),
        }
    }

    #[test]
    fn test_protocol_reader_two_phase() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(&raw[0..4]);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(!result.unwrap());

        let mut stream = Cursor::new(&raw[4..]);
        let mut stream = StreamWrapper::new(&mut stream);
        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader.packet().unwrap();
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_two_phase_async() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw[0..4] {
            tx.send(*byte).await.unwrap();
        }

        let result = reader.read_async(&mut rx).await;
        assert!(!result.unwrap());

        for byte in &raw[4..] {
            tx.send(*byte).await.unwrap();
        }
        let result = reader.read_async(&mut rx).await;
        assert!(result.unwrap());

        let packet = reader.packet().unwrap();
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[test]
    fn test_protocol_reader_resync_partial_header() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x34, 0x03, 0xde, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(!result.unwrap());
        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_resync_partial_header_async() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x34, 0x03, 0xde, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw {
            tx.send(*byte).await.unwrap();
        }
        let result = reader.read_async(&mut rx).await;
        assert!(!result.unwrap());
        let result = reader.read_async(&mut rx).await;
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[test]
    fn test_protocol_reader_resync_full_header() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x12, 0x04, 0xde, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(!result.unwrap());
        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_resync_full_header_async() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [0x12, 0x04, 0xde, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw {
            tx.send(*byte).await.unwrap();
        }

        let result = reader.read_async(&mut rx).await;
        assert!(!result.unwrap());
        let result = reader.read_async(&mut rx).await;
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[test]
    fn test_protocol_reader_resync_last() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [
            0x12, 0x04, 0xde, 0xad, 0xbe, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef,
        ];
        let mut stream = Cursor::new(raw);
        let mut stream = StreamWrapper::new(&mut stream);

        let result = reader.read(&mut stream);
        assert!(!result.unwrap());
        let result = reader.read(&mut stream);
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn test_protocol_reader_resync_last_async() {
        let mut reader = ProtocolReader::<6, _>::new();
        let raw = [
            0x12, 0x04, 0xde, 0xad, 0xbe, 0x00, 0x12, 0x04, 0xde, 0xad, 0xbe, 0xef,
        ];
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(16);
        for byte in &raw {
            tx.send(*byte).await.unwrap();
        }

        let result = reader.read_async(&mut rx).await;
        assert!(!result.unwrap());
        let result = reader.read_async(&mut rx).await;
        assert!(result.unwrap());

        let packet = reader
            .packet()
            .expect("packet() must return Some after reading a packet");
        assert_eq!(packet.body().unwrap(), &[0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(packet.source_address().value(), 0x02);
        assert_eq!(packet.destination_address().value(), 0x01);
        assert_eq!(packet.message_length(), 0x04);
        assert_eq!(packet.message_type(), MessageType::Normal);
    }
}
