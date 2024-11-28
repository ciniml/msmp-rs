use core::{cell::RefCell, sync::atomic::{AtomicBool, Ordering}};

use crate::{packet::PacketReader, protocol::{ProtocolReader, ProtocolReaderError, StreamReader, StreamWriter, StreamWriterAsync}};

#[derive(Debug)]
pub enum ServiceError<WriterError> {
    NoBuffer,
    Protocol(ProtocolReaderError<()>),
    Writer(WriterError),
}

pub struct BufferPool<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> {
    buffers: [RefCell<[u8; BUFFER_SIZE]>; BUFFER_COUNT],
    buffer_in_use: [AtomicBool; BUFFER_COUNT],
}
unsafe impl<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> Sync for BufferPool<BUFFER_SIZE, BUFFER_COUNT> {}
unsafe impl<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> Send for BufferPool<BUFFER_SIZE, BUFFER_COUNT> {}

impl<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> BufferPool<BUFFER_SIZE, BUFFER_COUNT> {
    pub const fn new() -> Self {
        BufferPool {
            buffers: [ const {RefCell::new([0; BUFFER_SIZE])}; BUFFER_COUNT],
            buffer_in_use: [const { AtomicBool::new(false) }; BUFFER_COUNT],
        }
    }

    fn get_buffer(&self) -> Option<BufferRef<BUFFER_SIZE, BUFFER_COUNT>> {
        for i in 0..BUFFER_COUNT {
            if self.buffer_in_use[i].compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                let mut buffer = self.buffers[i].borrow_mut();
                return Some(BufferRef {
                    pool: self,
                    buffer_index: i,
                    buffer: unsafe { &mut *(buffer.as_mut_ptr() as *mut [u8; BUFFER_SIZE]) },
                });
            }
        }
        None
    }

    fn return_buffer(&self, buffer_index: usize) {
        if buffer_index < BUFFER_COUNT {
            self.buffer_in_use[buffer_index].store(false, Ordering::Release);
        }
    }
}

pub struct BufferRef<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> {
    pool: &'a BufferPool<BUFFER_SIZE, BUFFER_COUNT>,
    buffer_index: usize,
    buffer: &'a mut [u8; BUFFER_SIZE],
}

impl<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> Drop for BufferRef<'a, BUFFER_SIZE, BUFFER_COUNT> {
    fn drop(&mut self) {
        self.pool.return_buffer(self.buffer_index);
    }
}

impl<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> core::ops::Deref for BufferRef<'a, BUFFER_SIZE, BUFFER_COUNT> {
    type Target = [u8; BUFFER_SIZE];

    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> core::ops::DerefMut for BufferRef<'a, BUFFER_SIZE, BUFFER_COUNT> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}

impl<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> AsRef<[u8]> for BufferRef<'a, BUFFER_SIZE, BUFFER_COUNT> {
    fn as_ref(&self) -> &[u8] {
        self.buffer
    }
}
impl<'a, const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> AsMut<[u8]> for BufferRef<'a, BUFFER_SIZE, BUFFER_COUNT> {
    fn as_mut(&mut self) -> &mut [u8] {
        self.buffer
    }
}

pub struct BufferManager<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> 
{
    buffer_pool: &'static BufferPool<BUFFER_SIZE, BUFFER_COUNT>,
    send_buffers: [Option<BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>>; BUFFER_COUNT],
    send_buffer_index: usize,
}

impl<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> BufferManager<BUFFER_SIZE, BUFFER_COUNT> {
    pub fn new(buffer_pool: &'static BufferPool<BUFFER_SIZE, BUFFER_COUNT>) -> Self {
        Self {
            buffer_pool,
            send_buffers: [const {None}; BUFFER_COUNT],
            send_buffer_index: 0,
        }
    }

    fn find_next_send_buffer_index(&self) -> Option<usize> {
        for i in 0..BUFFER_COUNT {
            // Calculate the index from the send_buffer_index to i. 
            // Note that we cannot use the modulo operator because it is too slow for embedded systems.
            let index = if self.send_buffer_index + i >= BUFFER_COUNT { 
                self.send_buffer_index + i - BUFFER_COUNT
            } else {
                self.send_buffer_index + i
            };
            if self.send_buffers[index].is_none() {
                return Some(index);
            }
        }
        None
    }

    fn get_send_buffer(&self) -> Option<&BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>> {
        self.send_buffers[self.send_buffer_index].as_ref()
    }

    fn next_buffer(&mut self) {
        self.send_buffers[self.send_buffer_index] = None;
        self.send_buffer_index = if self.send_buffer_index + 1 >= BUFFER_COUNT { 0 } else { self.send_buffer_index + 1 };
    }

    pub fn allocate_send_buffer(&self) -> Option<BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>> {
        self.buffer_pool.get_buffer()
    }

    pub fn register_buffer_to_send(&mut self, buffer: BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT> ) -> Result<(), BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>> {
        if let Some(send_buffer_index) = self.find_next_send_buffer_index() {
            self.send_buffers[send_buffer_index] = Some(buffer);
            Ok(())
        } else {
            Err(buffer)
        }
    }

    pub fn has_data_to_send(&self) -> bool {
        self.send_buffers[self.send_buffer_index].is_some()
    }
}

pub struct Service<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> 
{
    buffer_manager: BufferManager<BUFFER_SIZE, BUFFER_COUNT>,
    reader: Option<ProtocolReader<BUFFER_SIZE, BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>>>,
    bytes_sent: usize,
}

impl<const BUFFER_SIZE: usize, const BUFFER_COUNT: usize> Service<BUFFER_SIZE, BUFFER_COUNT> {
    pub fn new(buffer_pool: &'static BufferPool<BUFFER_SIZE, BUFFER_COUNT>) -> Self {
        Self {
            buffer_manager: BufferManager::new(buffer_pool),
            reader: None,
            bytes_sent: 0,
        }
    }

    fn ensure_reader(&mut self) -> Result<&mut ProtocolReader<BUFFER_SIZE, BufferRef<'static, BUFFER_SIZE, BUFFER_COUNT>>, ServiceError<()>> {
        if self.reader.is_none() {
            if let Some(buffer) = self.buffer_manager.buffer_pool.get_buffer() {
                self.reader = Some(ProtocolReader::new_with_buffer(buffer));
            } else {
                return Err(ServiceError::NoBuffer);
            }
        }
        Ok(self.reader.as_mut().unwrap())
    }

    pub fn put_received_data(&mut self, mut data: &[u8]) -> Result<(), ServiceError<()>> {
        let reader = self.ensure_reader()?;
        reader.read(&mut data).map_err(|err| ServiceError::Protocol(err))?;
        Ok(())
    }

    pub async fn process<W: StreamWriterAsync, Handler: FnOnce(&mut BufferManager<BUFFER_SIZE, BUFFER_COUNT>, PacketReader<'_>) -> bool>(&mut self, writer: &mut W, handler: Handler) -> Result<bool, ServiceError<W::Error>> {
        let buffer_to_forward = if let Some(packet) = self.reader.as_ref().map(|reader| reader.packet()).flatten() {
            // A packet is received. process it.
            let must_be_forwarded = handler(&mut self.buffer_manager, packet);
            // Release the reader.
            let reader = self.reader.take().unwrap();
            let buffer = reader.release();
            // If this packet must be forwarded, return the buffer for forwarding.
            if must_be_forwarded {
                Some(buffer)
            } else {
                None
            }
        } else {
            None
        };
        
        if let Some(buffer_to_forward) = buffer_to_forward {
            // Forward the received packet.
            self.buffer_manager.register_buffer_to_send(buffer_to_forward).ok();
        }
        if let Some(buffer) = self.buffer_manager.get_send_buffer() {
            // There is a packet to send. Send it.
            let bytes_to_send = PacketReader::new(buffer.as_ref()).message_length() + 2;
            let bytes_sent = writer.write(&buffer[self.bytes_sent..bytes_to_send]).await.map_err(|err| ServiceError::Writer(err))?;
            
            self.bytes_sent += bytes_sent;

            // If all bytes are sent, release the current buffer and advance to the next buffer.
            if self.bytes_sent == bytes_to_send {
                self.buffer_manager.next_buffer();
                self.bytes_sent = 0;
            }
        }
        Ok(self.buffer_manager.has_data_to_send())
    }
}

#[cfg(test)]
mod test {
    use crate::protocol::BufferStreamWriter;

    use super::*;

    #[test]
    fn test_buffer_pool() {
        static BUFFER_POOL: BufferPool<4, 2> = BufferPool::new();
        let mut buffer1 = BUFFER_POOL.get_buffer().expect("buffer1 must be available");
        let mut buffer2 = BUFFER_POOL.get_buffer().expect("buffer2 must be available");
        buffer1[0] = 0x01;
        buffer2[0] = 0x02;
        assert_eq!(buffer1[0], 0x01);
        assert_eq!(buffer2[0], 0x02);
        assert!(BUFFER_POOL.get_buffer().is_none());
        drop(buffer1);
        let buffer3 = BUFFER_POOL.get_buffer().expect("buffer3 must be available");
        assert!(BUFFER_POOL.get_buffer().is_none());
        assert_eq!(buffer3[0], 0x01);
        drop(buffer2);
        drop(buffer3);
        let buffer4 = BUFFER_POOL.get_buffer().expect("buffer4 must be available");
        let buffer5 = BUFFER_POOL.get_buffer().expect("buffer5 must be available");
        assert!(BUFFER_POOL.get_buffer().is_none());
        assert_eq!(buffer4[0], 0x01);
        assert_eq!(buffer5[0], 0x02);
        drop(buffer4);
        drop(buffer5);
    }

    #[test]
    fn test_buffer_manager() {
        static BUFFER_POOL: BufferPool<4, 2> = BufferPool::new();
        let mut buffer_manager = BufferManager::new(&BUFFER_POOL);
        let buffer1 = buffer_manager.allocate_send_buffer().expect("buffer1 must be available");
        let buffer2 = buffer_manager.allocate_send_buffer().expect("buffer2 must be available");
        assert!(buffer_manager.allocate_send_buffer().is_none());
        buffer_manager.register_buffer_to_send(buffer1).map_err(|_| ()).expect("buffer1 must be registered");
        buffer_manager.register_buffer_to_send(buffer2).map_err(|_| ()).expect("buffer2 must be registered");
        assert!(buffer_manager.allocate_send_buffer().is_none());
        buffer_manager.next_buffer();
        let buffer3 = buffer_manager.allocate_send_buffer().expect("buffer3 must be available");
        buffer_manager.next_buffer();
        let buffer4 = buffer_manager.allocate_send_buffer().expect("buffer4 must be available");
        assert!(buffer_manager.allocate_send_buffer().is_none());
        buffer_manager.register_buffer_to_send(buffer3).map_err(|_| ()).expect("buffer3 must be registered");
        buffer_manager.register_buffer_to_send(buffer4).map_err(|_| ()).expect("buffer4 must be registered");
        assert!(buffer_manager.allocate_send_buffer().is_none());
    }

    #[cfg(feature = "async")]
    #[tokio::test]
    async fn test_service() {
        static BUFFER_POOL: BufferPool<7, 2> = BufferPool::new();
        let mut service = Service::new(&BUFFER_POOL);

        let mut send_data = [0u8; 13];
        let mut writer = BufferStreamWriter::new(&mut send_data);

        service.put_received_data(&[0xf1, 0x04, 0x01, 0x02, 0x03, 0x04]).expect("data must be processed");
        let has_data_to_send = service.process(&mut writer, |buffer_manager, packet| {
            assert_eq!(packet.source_address().value(), 0x01);
            assert_eq!(packet.destination_address().value(), 0x0f);
            assert_eq!(packet.message_length(), 4);
            assert_eq!(packet.message_type(), crate::packet::MessageType::Normal);
            assert_eq!(packet.body().unwrap(), &[0x01, 0x02, 0x03, 0x04]);

            let mut send_buffer = buffer_manager.allocate_send_buffer().expect("buffer must be available");
            send_buffer[..7].copy_from_slice(&[0x12, 0x05, 0xff, 0xfe, 0xfd, 0xfc, 0xfb]);
            buffer_manager.register_buffer_to_send(send_buffer).map_err(|_| ()).expect("buffer must be registered");
            packet.destination_address().value() == 0x0f
        }).await.expect("data must be processed");
        assert!(has_data_to_send);
        let has_data_to_send = service.process(&mut writer, |_, _| false).await.expect("data must be processed");
        assert!(!has_data_to_send);
        
        drop(writer);
        assert_eq!(&send_data, &[
            0x12, 0x05, 0xff, 0xfe, 0xfd, 0xfc, 0xfb,  // The new data must be written
            0xf1, 0x04, 0x01, 0x02, 0x03, 0x04,         // The received data must be forwarded.
        ]);
    }
}