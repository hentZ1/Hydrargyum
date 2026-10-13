use cpal::traits::HostTrait;
use hydrargyum::modules::audio_io::{
    devices::{input_config, output_config},
    ring_buff, stream,
};

fn main() {
    let hosts = cpal::default_host();
    let devices_list = hosts.devices().expect("error while getting devices");
    println!("INPUT:  {:?}\n", input_config());
    println!("OUTPUT: {:?}\n", output_config());
    println!("HOST(backend): {:?}\n", cpal::available_hosts());
}
