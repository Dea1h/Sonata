#![allow(warnings)]
use std::io::{BufRead, BufReader, Cursor, Read};
use std::process::{Command, Output};
use std::{env, hint};

use rodio::{OutputStream, OutputStreamBuilder, Sink};

use symphonia::core::audio::Signal;
use symphonia::core::codecs::{Decoder, DecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::{MediaSourceStream, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::{Hint, ProbeResult};

fn main() {
    let args: Vec<String> = env::args().collect();
    let output: Output = Command::new("python3")
        .args([
            "/home/neon/Downloads/clones/__main__.py",
            "-f",
            "bestaudio",
            "--get-url",
            &args[1],
        ])
        .output()
        .expect("__SONATA__: Command Failed");
    let url: String = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("__SONATA__: Url LINK {}...", &url[..50]);

    let mut res: reqwest::blocking::Response = reqwest::blocking::get(&url).unwrap();
    println!("__SONATA__: Reqwesting ...");
    let reader: BufReader<reqwest::blocking::Response> = BufReader::new(res);
    let src: ReadOnlySource<BufReader<reqwest::blocking::Response>> = ReadOnlySource::new(reader);

    let mss: MediaSourceStream = MediaSourceStream::new(Box::new(src), Default::default());

    let hint: Hint = Hint::new();
    let probed: ProbeResult = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .expect("__SONATA__: Format Probing Failed");

    let track: &Track = probed
        .format
        .default_track()
        .expect("__SONATA__: Track Not Found");

    let decoder_options: DecoderOptions = DecoderOptions::default();

    let mut decoder: Box<dyn Decoder> = symphonia::default::get_codecs()
        .make(&track.codec_params, &decoder_options)
        .expect("__SONATA__: Make Decoder Failed");

    let handle: OutputStream = OutputStreamBuilder::open_default_stream().unwrap();
    println!("__SONATA__: Getting OutputStream Handle");
    let sink = Sink::connect_new(&handle.mixer());

    loop {
        match probed.format.next_packet() {
            Ok(packet) => match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = decoded.spec();
                    match decoded {
                        symphonia::core::audio::AudioBufferRef::U8(buf) => {
                            let samples = buf.chan_mut(spec.channels);
                            let source = rodio::buffer::SamplesBuffer::new(
                                spec.channels.count() as u16,
                                spec.rate,
                                samples,
                            );
                            sink.append(source);
                        }
                    }
                }
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => {
                    eprintln!("__SONATA__: Decode error: {:?}", e);
                    break;
                }
            },
            Err(_) => break,
        }
    }
    sink.sleep_until_end();
    return;
}
