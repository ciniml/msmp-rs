use anyhow::anyhow;
use bytes::{Buf, BufMut};
use clap::{Args, Parser, Subcommand};
use clap_num::maybe_hex;
use futures_util::{SinkExt, StreamExt};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

use msmp::{packet::Address, protocol::{StreamReaderAsync, StreamWriterAsync}, service::{BufferPool, Service}};
use tokio_util::codec::{BytesCodec, Decoder, Framed};


#[derive(Debug, Parser)]
#[clap(name = env!("CARGO_PKG_NAME"), version = env!("CARGO_PKG_VERSION"), author = env!("CARGO_PKG_AUTHORS"), about = env!("CARGO_PKG_DESCRIPTION"), arg_required_else_help = true)]
struct Cli {
    #[clap(subcommand)]
    subcommand: SubCommands,

    #[clap(short, long, help = "The serial port to use")]
    port: String,
    #[clap(short, long, help = "The baud rate to use", default_value = "9600")]
    baud: u32,
    #[clap(short, long, help = "The timeout in milliseconds", default_value = "10")]
    timeout_ms: u32,
    #[clap(short, long, help = "The interval to send a next byte in milliseconds", default_value = "20")]
    send_interval: u64,
}

#[derive(Debug, Subcommand)]
enum SubCommands {
    Send(SendCommandArgs),
    Serve(ServeCommandArgs),
}

#[derive(Debug, Args)]
struct SendCommandArgs {
    #[clap(help = "The source address to use", value_parser=maybe_hex::<u8>)]
    source_address: u8,
    #[clap(help = "The destination address to use", value_parser=maybe_hex::<u8>)]
    destination_address: u8,
    #[clap(help = "The data to send")]
    data: String,
    #[clap(long, help = "Send sync byte before sending the frame.", default_value = "false")]
    sync: bool,
}

#[derive(Debug, Args)]
struct ServeCommandArgs {}

#[derive(Clone)]
struct SerialWrapper {
    last_read_buffer: std::rc::Rc<std::cell::RefCell<Option<bytes::BytesMut>>>,
    serial: std::rc::Rc<std::cell::RefCell<Framed<SerialStream, BytesCodec>>>,
    write_interval: std::time::Duration,
}

// StreamReadAsync implementation for SerialReader
impl msmp::protocol::StreamReaderAsync for SerialWrapper {
    type Error = tokio_serial::Error;
    async fn read(&mut self, data: &mut [u8]) -> Result<usize, Self::Error> {
        let mut last_read_buffer = self.last_read_buffer.borrow_mut();
        if let Some(buffer) = last_read_buffer.as_mut() {
            let bytes_to_read = std::cmp::min(data.len(), buffer.remaining());
            buffer.copy_to_slice(&mut data[..bytes_to_read]);
            if buffer.remaining() == 0 {
                *last_read_buffer = None;
            }
            Ok(bytes_to_read)
        } else {
            match self.serial.borrow_mut().next().await {
                None => Ok(0),
                Some(Ok(mut buffer)) => {
                    let bytes_to_read = std::cmp::min(data.len(), buffer.remaining());
                    buffer.copy_to_slice(&mut data[..bytes_to_read]);
                    if buffer.remaining() != 0 {
                        *last_read_buffer = Some(buffer);
                    }
                    Ok(bytes_to_read)
                },
                Some(Err(err)) => {
                    Err(err.into())
                }
            }
        }
    }
}

// StreamWriterAsync implementation for SerialWriter
impl msmp::protocol::StreamWriterAsync for SerialWrapper {
    type Error = tokio_serial::Error;
    async fn write(&mut self, data: &[u8]) -> Result<usize, Self::Error> {
        if self.write_interval == std::time::Duration::ZERO {
            self.serial.borrow_mut().send(bytes::BytesMut::from(data)).await?;
        } else {
            for byte in data {
                self.serial.borrow_mut().send(bytes::BytesMut::from(&[*byte][..])).await?;
                tokio::time::sleep(self.write_interval).await;
            }
        }
        Ok(data.len())
    }
}


async fn command_send(args: SendCommandArgs, mut writer: SerialWrapper) -> anyhow::Result<()> {
    let source_address = Address::try_from(args.source_address).map_err(|_| anyhow!("Invalid source address"))?;
    let destination_address = Address::try_from(args.destination_address).map_err(|_| anyhow!("Invalid destination address"))?;
    let data_binary = hex::decode(args.data).expect("Failed to decode hex");
    let mut packet_buffer = Vec::new();
    packet_buffer.resize(data_binary.len() + 2, 0);
    let mut packet_writer = msmp::packet::PacketWriter::new(&mut packet_buffer);
    packet_writer.set_source_address(source_address).map_err(|_| anyhow!("Failed to set source address"))?;
    packet_writer.set_destination_address(destination_address).map_err(|_| anyhow!("Failed to set destination address"))?;
    packet_writer.set_message_length(data_binary.len()).map_err(|_| anyhow!("Failed to set message length"))?;
    packet_writer.set_message_type(msmp::packet::MessageType::Normal).map_err(|_| anyhow!("Failed to set message type"))?;
    packet_writer.body_mut().copy_from_slice(&data_binary);
    drop(packet_writer);

    if args.sync {
        writer.write(&[0x00]).await.map_err(|err| anyhow!("Failed to write to serial port: {:?}", err))?;
    }

    let mut bytes_written = 0;
    while bytes_written < packet_buffer.len() {
        let bytes_written_now = writer.write(&packet_buffer[bytes_written..]).await.map_err(|err| anyhow!("Failed to write to serial port: {:?}", err))?;
        bytes_written += bytes_written_now;
    }
    Ok(())
}

async fn command_serve(_args: ServeCommandArgs, mut reader: SerialWrapper, mut writer: SerialWrapper) -> anyhow::Result<()> {
    static BUFFER_POOL: BufferPool<64, 256> = BufferPool::new();
    let mut service = Service::new(&BUFFER_POOL);
    log::info!("Running service...");
    let mut buffer = [0u8; 64];

    loop {
        let bytes_read = reader.read(&mut buffer).await?;
        if bytes_read > 0 {
            log::debug!("received: {:?}", &buffer[..bytes_read]);
        }
        service.put_received_data(&buffer[..bytes_read]).map_err(|e| anyhow!("{e:?}"))?;
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
                                lamp_state @ 'H' => { log::info!("MNP004 lamp state to {}", lamp_state); },
                                lamp_state @ 'L' => { log::info!("MNP004 lamp state to {}", lamp_state); },
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
                                log::info!("ComProc MCU message: {}", message);
                            }
                        }
                    }
                }
                _ => {}
            }
            packet.destination_address().value() != 0xe
        }).await.map_err(|e| anyhow!("{e:?}"))? {}
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .init();
    let cli = Cli::parse();

    let mut serial = tokio_serial::new(&cli.port, cli.baud)
        .open_native_async()
        .expect("Failed to open serial port");
    serial.set_timeout(std::time::Duration::from_millis(cli.timeout_ms as u64)).expect("Failed to set timeout");
    let framed = tokio_util::codec::BytesCodec::new().framed(serial);
    let serial = std::rc::Rc::new(std::cell::RefCell::new(framed));

    let write_interval = std::time::Duration::from_millis(cli.send_interval);
    let reader = SerialWrapper { serial, last_read_buffer: std::rc::Rc::new(std::cell::RefCell::new(None)), write_interval };
    let writer = reader.clone();
    
    let result = match cli.subcommand {
        SubCommands::Send(args) => {
            command_send(args, writer).await
        }
        SubCommands::Serve(args) => {
            command_serve(args, reader, writer).await
        }
        _ => unimplemented!(),
    };
    if let Err(err) = result {
        log::error!("Error: {:?}", err);
    }
}
