use std::io::Write;
use anyhow::anyhow;
use clap::{Args, Parser, Subcommand};
use tokio_serial::{SerialPort, SerialPortBuilderExt, SerialStream};

use msmp::packet::Address;


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
    #[clap(help = "The source address to use")]
    source_address: u8,
    #[clap(help = "The destination address to use")]
    destination_address: u8,
    #[clap(help = "The data to send")]
    data: String,
}

#[derive(Debug, Args)]
struct ServeCommandArgs {}

struct SerialReader<'a> {
    serial: &'a std::cell::RefCell<Box<dyn serialport::SerialPort>>
}
struct SerialWriter<'a> {
    serial: &'a std::cell::RefCell<Box<dyn serialport::SerialPort>>,
}
impl<'a> msmp::protocol::StreamReader for SerialReader<'a> {
    type Error = serialport::Error;
    fn read(&mut self, data: &mut [u8]) -> nb::Result<usize, Self::Error> {
        self.serial.borrow_mut().read(data).map_err(|err| nb::Error::Other(serialport::Error::from(err)))
    }
}
impl<'a> msmp::protocol::StreamWriter for SerialWriter<'a> {
    type Error = serialport::Error;
    fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error> {
        self.serial.borrow_mut().write(data).map_err(|err| nb::Error::Other(serialport::Error::from(err)))
    }
}

// StreamWriterAsync implementation for SerialWriter
#[async_trait::async_trait]
impl<'a> msmp::protocol::StreamWriterAsync for SerialWriter<'a> {
    type Error = serialport::Error;
    async fn write(&mut self, data: &[u8]) -> nb::Result<usize, Self::Error> {
        self.serial.borrow_mut().write_async
    }
}


async fn command_send(args: SendCommandArgs, writer: SerialWriter<'_>) -> anyhow::Result<()> {
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
        let bytes_written_now = writer.write(&packet_buffer[bytes_written..]).map_err(|err| anyhow!("Failed to write to serial port: {:?}", err))?;
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

    let serial = serialport::new(&cli.port, cli.baud)
        .open()
        .expect("Failed to open serial port");
    let serial = std::cell::RefCell::new(serial);
    serial.borrow_mut().set_timeout(std::time::Duration::from_millis(cli.timeout_ms as u64)).expect("Failed to set timeout");
    let mut reader = SerialReader { serial: &serial };
    let mut writer = SerialWriter { serial: &serial };
    
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
