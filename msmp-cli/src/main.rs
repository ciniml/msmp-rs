use std::io::Write;

use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use msmp::{packet::Address, protocol::StreamWriter, service::Service};


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
    Send {
        #[clap(long, help = "Source address", value_parser = clap_num::maybe_hex::<u8>)]
        source_address: u8,
        #[clap(long, help = "Destination address", value_parser = clap_num::maybe_hex::<u8>)]
        destination_address: u8,
        #[clap(long, help = "Data to send")]
        data: String,
    },
}

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

fn main() {
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
    
    match cli.subcommand {
        SubCommands::Send { source_address, destination_address, data } => {
            let source_address = Address::try_from(source_address).expect("Invalid source address");
            let destination_address = Address::try_from(destination_address).expect("Invalid destination address");
            let data_binary = hex::decode(data).expect("Failed to decode hex");
            let mut packet_buffer = Vec::new();
            packet_buffer.resize(data_binary.len() + 2, 0);
            let mut packet_writer = msmp::packet::PacketWriter::new(&mut packet_buffer);
            packet_writer.set_source_address(source_address).expect("Failed to set source address");
            packet_writer.set_destination_address(destination_address).expect("Failed to set destination address");
            packet_writer.set_message_length(data_binary.len()).expect("Failed to set message length");
            packet_writer.set_message_type(msmp::packet::MessageType::Normal).expect("Failed to set message type");
            packet_writer.body_mut().copy_from_slice(&data_binary);
            drop(packet_writer);
            
            let mut bytes_written = 0;
            while bytes_written < packet_buffer.len() {
                let bytes_written_now = writer.write(&packet_buffer[bytes_written..]).expect("Failed to write");
                bytes_written += bytes_written_now;
            }
        }
        _ => unimplemented!(),
    }
}
