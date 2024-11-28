mod utils;

use std::panic;
use std::convert::TryFrom;
use futures::{pin_mut, FutureExt};

use msmp::{protocol::{StreamReaderAsync, StreamWriterAsync}, service::{BufferPool, Service}};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use wasm_streams::{ReadableStream, WritableStream};
use web_sys::{SerialPort, SerialOptions};

#[wasm_bindgen]
pub fn start() {
    panic::set_hook(Box::new(console_error_panic_hook::hook));
    wasm_logger::init(wasm_logger::Config::default());
    log::info!("Logger initialized");
}

pub async fn delay_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        web_sys::Window::set_timeout_with_callback_and_timeout_and_arguments_0(&web_sys::window().unwrap(), &resolve, ms);
    });
    let _ = JsFuture::from(promise).await;
}

struct ReadableStreamWrapper {
    stream: ReadableStream,
    buffer: Vec<u8>,
    position: usize,
}

impl ReadableStreamWrapper {
    fn new(stream: ReadableStream) -> Self {
        Self { stream, buffer: Vec::new(), position: 0}
    }
}

impl StreamReaderAsync for ReadableStreamWrapper {
    type Error = JsValue;
    async fn read(&mut self, data: &mut [u8]) -> Result<usize, Self::Error> {
        if data.len() == 0 {
            return Ok(0);
        }
        let bytes_remaining = self.buffer.len() - self.position;
        if bytes_remaining < data.len() {
            {
                let timed_out = {
                    let mut reader = self.stream.get_reader();
                    let read_future = reader.read().fuse();
                    let delay = delay_ms(10).fuse();
                    pin_mut!(read_future, delay);
                    futures::select! {
                        result = read_future => {
                            if let Some(chunk) = result? {
                                if let Ok(buffer) = js_sys::Uint8Array::try_from(chunk) {
                                    let length = buffer.length() as usize;
                                    let prev_len = self.buffer.len();
                                    self.buffer.resize(prev_len + length, 0);
                                    buffer.copy_to(&mut self.buffer[prev_len..]);
                                }
                            }
                            false
                        },
                        _ = delay => {
                            true
                        }
                    }
                };
                if timed_out {
                    //self.stream.get_reader().cancel().await?;
                    return Ok(0);
                }
            }
        }

        let bytes_remaining = self.buffer.len() - self.position;
        if bytes_remaining == 0 {
            return Ok(0);
        } else if bytes_remaining < data.len() {
            data[..bytes_remaining].copy_from_slice(&self.buffer[self.position..]);
            self.position = 0;
            self.buffer.clear();
            Ok(bytes_remaining)
        } else {
            data.copy_from_slice(&self.buffer[self.position..self.position + data.len()]);
            self.position += data.len();
            Ok(data.len())
        }
    }
}

struct WritableStreamWrapper {
    stream: WritableStream,
}

impl WritableStreamWrapper {
    fn new(stream: WritableStream) -> Self {
        Self { stream }
    }
}

impl StreamWriterAsync for WritableStreamWrapper {
    type Error = JsValue;
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        let buffer = js_sys::Uint8Array::from(data);
        let writer = self.stream.get_writer();
        pin_mut!(writer);
        writer.write(buffer.into()).await?;
        Ok(data.len())
    }
}

#[wasm_bindgen]
pub async fn run_service(port: SerialPort, cb: &js_sys::Function) -> Result<JsValue, JsValue> {
    static BUFFER_POOL: BufferPool<64, 256> = BufferPool::new();
    let mut service = Service::new(&BUFFER_POOL);

    let mut reader = ReadableStreamWrapper::new(ReadableStream::from_raw(port.readable()));
    let mut writer = WritableStreamWrapper::new(WritableStream::from_raw(port.writable()));
    
    let mut buffer = [0u8; 64];

    log::info!("Running service...");
    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read > 0 {
            log::debug!("received: {:?}", &buffer[..bytes_read]);
        }
        service.put_received_data(&buffer[..bytes_read]);
        while service.process(&mut writer, |manager, packet| {
            log::info!("Packet received: {}", &packet);
            match packet.destination_address().value() {
                0x4 => {    //MNP004
                    log::info!("MNP004 packet received");
                    if packet.message_length() == 1 {
                        if let Ok(body) = packet.body() {
                            let lamp_state = body[0] as char;
                            log::info!("MNP004 lamp state: {}", lamp_state);
                            match lamp_state {
                                lamp_state @ 'H' => { log::info!("MNP004 lamp state to {}", lamp_state); let _  = cb.call2(&JsValue::NULL, &JsValue::from_str("MNP004"), &JsValue::from_bool(true)); },
                                lamp_state @ 'L' => { log::info!("MNP004 lamp state to {}", lamp_state); let _  = cb.call2(&JsValue::NULL, &JsValue::from_str("MNP004"), &JsValue::from_bool(false)); },
                                _ => {}
                            }
                        }
                    }
                }
                0x5 => {    //ComProc MCU
                    log::info!("ComProc MCU packet received");
                    if let Ok(body) = packet.body() {
                        if body.len() > 3 {
                            if let Ok(message) = core::str::from_utf8(&body[3..]) {
                                let _ = cb.call2(&JsValue::NULL, &JsValue::from_str("ComProc MCU"), &JsValue::from_str(message));
                            }
                        }
                    }
                }
                _ => {}
            }
            packet.destination_address().value() != 0xe
        }).await.map_err(|e| JsValue::from_str(format!("{:?}", e).as_str()))? {};
    }
}
