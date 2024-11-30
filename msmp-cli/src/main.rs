use anyhow::anyhow;
use bytes::{Buf, BufMut};
use clap::{Args, Parser, Subcommand};
use clap_num::maybe_hex;
use futures_util::{SinkExt, StreamExt};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

use msmp::{packet::Address, protocol::StreamWriterAsync};
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
    #[clap(short, long, help = "The timeout in milliseconds every ID", default_value = "10")]
    timeout_ms: u32,
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
}

#[derive(Debug, Args)]
struct ServeCommandArgs {}

#[derive(Clone)]
struct SerialWrapper {
    last_read_buffer: std::rc::Rc<std::cell::RefCell<Option<bytes::BytesMut>>>,
    serial: std::rc::Rc<std::cell::RefCell<Framed<SerialStream, BytesCodec>>>,
}

// StreamReadAsync implementation for SerialReader
impl msmp::protocol::StreamReaderAsync for SerialWrapper {
    type Error = tokio_serial::Error;
    async fn read(&mut self, data: &mut [u8]) -> Result<usize, Self::Error> {
        let mut last_read_buffer = self.last_read_buffer.borrow_mut();
        if let Some(buffer) = last_read_buffer.as_mut() {
            let bytes_to_read = std::cmp::min(data.len(), buffer.remaining_mut());
            buffer.copy_to_slice(&mut data[..bytes_to_read]);
            buffer.advance(bytes_to_read);
            if buffer.remaining() == 0 {
                *last_read_buffer = None;
            }
            Ok(bytes_to_read)
        } else {
            match self.serial.borrow_mut().next().await {
                None => Ok(0),
                Some(Ok(mut buffer)) => {
                    let bytes_to_read = std::cmp::min(data.len(), buffer.remaining_mut());
                    buffer.copy_to_slice(&mut data[..bytes_to_read]);
                    buffer.advance(bytes_to_read);
                    if buffer.remaining() == 0 {
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
        self.serial.borrow_mut().send(bytes::BytesMut::from(data)).await?;
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
    
    let mut bytes_written = 0;
    while bytes_written < packet_buffer.len() {
        let bytes_written_now = writer.write(&packet_buffer[bytes_written..]).await.map_err(|err| anyhow!("Failed to write to serial port: {:?}", err))?;
        bytes_written += bytes_written_now;
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

    
    let reader = SerialWrapper { serial, last_read_buffer: std::rc::Rc::new(std::cell::RefCell::new(None)) };
    let writer = reader.clone();
    
    let result = match cli.subcommand {
        SubCommands::Send(args) => {
            command_send(args, writer).await
        }
        _ => unimplemented!(),
    };
    if let Err(err) = result {
        log::error!("Error: {:?}", err);
    }
}
