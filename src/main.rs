#![allow(warnings)]

use std::array::from_fn;
use std::collections::VecDeque;
use std::io::SeekFrom;
use std::sync::{Arc,Mutex};
use std::{env, io::Seek};
use std::process::Command;
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::{StreamExt, io};
use symphonia::core::audio::{SampleBuffer, Signal, SignalSpec};
use symphonia::core::{
    codecs:: {CODEC_TYPE_NULL, DecoderOptions},
    formats::{FormatOptions},
    io::{MediaSourceStream},
    probe::{Hint},
    errors::Error,
};


struct StreamingReader {
    buffer: Vec<u8>,
    position: usize,
}

impl StreamingReader {
    fn new() -> Self {
        let buffer: Vec<u8> = Vec::new();
        return Self {
            buffer: buffer,
            position: 0,
        }
    }
}
//
impl std::io::Read for StreamingReader {
    fn read(&mut self,buf: &mut[u8]) -> std::io::Result<usize> {
        if self.position >= self.buffer.len() {
            return Ok(0);
        }
        let remaining = self.buffer.len() - self.position;
        let to_read = remaining.min(buf.len());
        buf[..to_read].copy_from_slice(&self.buffer[self.position..self.position + to_read]);
        self.position += to_read;
        return Ok(to_read);
    }
}

impl std::io::Seek for StreamingReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            SeekFrom::Start(offset) => {
                if offset > self.buffer.len() as u64 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek out of bounds"));
                }
                self.position = offset as usize;
                return Ok(self.position as u64);
            }
            SeekFrom::Current(offset) => {
                let position = self.position as i64 + offset as i64;
                if position < 0 || position > self.buffer.len() as i64 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek out of bounds"));
                }
                self.position = position as usize;
                return Ok(self.position as u64);
            }
            SeekFrom::End(offset)=> {
                let position = self.buffer.len() as i64 + offset as i64;
                if position < 0 {
                    return Err(io::Error::new(io::ErrorKind::InvalidInput, "Seek out of bounds"));
                }
                self.position = position as usize;
                return Ok(self.position as u64);
            }
        }
    }
}

impl symphonia::core::io::MediaSource for StreamingReader {
    fn is_seekable(&self) -> bool {
        return true;
    }

    fn byte_len(&self) -> Option<u64> {
        return Some(self.buffer.len() as u64);
    }
}

async fn stream(reader: StreamingReader) -> Result<(),Box<dyn std::error::Error>>{

    // Create the media source stream.
    let mss = MediaSourceStream::new(Box::new(reader), Default::default());

    // Create a probe hint using the file's extension. [Optional]
    let mut hint = Hint::new();
    hint.with_extension("m4a");

    // Use the default options for metadata and format readers.
    let format_opts = FormatOptions::default();
    let metadata_opts = Default::default();

    // Probe the media source.
    let probe = symphonia::default::get_probe().format(&hint, mss, &format_opts,&metadata_opts)?;

    // Get the instantiated format reader.
    let mut format = probe.format;

    // Find the first audio track with a known (decodeable) codec.
    let track = format
    .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("\x1b[91mNo Audio Tracks Found")?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();

    println!("\x1b[92mCodec: {:?}\x1b[0m", codec_params.codec);
    println!("\x1b[92mSample Rate: {:?}\x1b[0m", codec_params.sample_rate);
    println!("\x1b[92mChannels: {:?}\x1b[0m", codec_params.channels);

    let decoder_opts = DecoderOptions::default();
    let mut decoder = symphonia::default::get_codecs().make(&codec_params, &decoder_opts)?;

    let mut sample_count = 0;
    let mut sample_buffer = None;
    let mut all_samples: Vec<f32> = Vec::new();

    // The decode loop.
    loop {
        // Get the next packet from the media format.
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(Error::IoError(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                println!("\n\x1b[92mDecoding complete!\x1b[0m");
                break;
            }
            Err(err) => {
                panic!("{}",err);
            }
        };

        // Consume any new metadata that has been read since the last packet.
        while !format.metadata().is_latest() {
            // Pop the old head of the metadata queue.
            format.metadata().pop();
            // Consume the new metadata at the head of the metadata queue.
        }

        if packet.track_id() != track_id {
            continue;
        }

        match decoder.decode(&packet) {
            Ok(_decoded) => {
                if sample_buffer.is_none() {
                    let spec = *_decoded.spec();
                    let duration = _decoded.capacity() as u64;

                    sample_buffer = Some(SampleBuffer::<f32>::new(duration, spec));
                }

                if let Some(buf) = &mut sample_buffer {
                    buf.copy_interleaved_ref(_decoded);

                    all_samples.extend_from_slice(buf.samples());
                    sample_count += buf.samples().len();
                    println!("\x1b[93m\rDecoded {} samples",sample_count);
                    std::io::Write::flush(&mut std::io::stdout()).ok();
                }
            }
            Err(Error::IoError(_)) => {
                // The packet failed to decode due to an IO error, skip the packet.
                continue;
            }
            Err(Error::DecodeError(_)) => {
                // The packet failed to decode due to invalid data, skip the packet.
                continue;
            }
            Err(err) => {
                // An unrecoverable error occured, halt decoding.
                panic!("{}",err);
            }
        }
    }

    println!("\n\x1b[92mTotal samples decoded: {}\x1b[0m", all_samples.len());

    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("\x1b[91mNo Output Device Found")?;

    println!("\x1b[92mOutput device: {}\x1b[0m", device.name()?);

    let sample_rate = codec_params.sample_rate.unwrap_or(48000);

    let channels = codec_params.channels
    .unwrap_or(symphonia::core::audio::Channels::FRONT_LEFT | symphonia::core::audio::Channels::FRONT_RIGHT).count();

    let cpal_config = cpal::StreamConfig {
        channels: channels as u16,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let samples = Arc::new(Mutex::new(all_samples));
    let mut position = Arc::new(Mutex::new(0_usize));
    let total_samples = samples.lock().unwrap().len();

    let position_clone = position.clone();

    let stream = device.build_output_stream(&cpal_config, move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut samples = samples.lock().unwrap();
        let mut pos = position_clone.lock().unwrap();

        for sample in data.iter_mut() {
            *sample = if *pos < samples.len() {
                let s = samples[*pos];
                *pos += 1;
                s
            } else {
                0.0
            }
        }
    }, 
        |err| eprintln!("Stream Error: {}",err), None)?;

    stream.play()?;
    println!("\x1b[92mPlaying audio...\x1b[0m");

    while *position.lock().unwrap() < total_samples {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    println!("\x1b[92mPlayback complete!\x1b[0m");
    return Ok(());
}

#[tokio::main] 
async fn main() -> Result<(),Box<dyn std::error::Error>> {
    Command::new("clear").status()?;
    println!("\x1b[93mFetching audio URL...\x1b[0m");
    let output = Command::new("/home/neon/Downloads/clones/yt-dlp_linux")
        .args([
            "-f",
            "140",
            "-g",
            &env::args().nth(1).ok_or("\x1b[91mMissing Arguments")?
        ]).output()?;

    let url = String::from_utf8(output.stdout)?;

    if url.is_empty() {
        return Err("\x1b[91mFailed to get audio URL from yt-dlp\x1b[0m".into());
    }

    println!("\x1b[92mAudio URL Acquired\x1b[0m");

    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64)")
        .build()?;

    let response = client.get(&url)
        .header("Accept", "*/*")
        .header("Accept-Language", "en-US,en;q=0.9")
        .header("Referer", "https://www.youtube.com/")
        .header("Range", "bytes=0-")
        .send()
    .await?;

    if !response.status().is_success() {
        return Err(format!("\x1b[91mHTTP Request Failed With Status: {}", response.status()).into());
    }

    println!("\x1b[92mStatus: {}",response.status());
    println!("\x1b[91mHTTP Version: {:?}",response.version());
    println!("\x1b[93mHeaders: {{");

    for (key,value) in response.headers().iter() {
        println!("  \x1b[{}m{:?}: {:?}",96,key,value);
    }

    println!("\x1b[93m}}");
    let start_time = Instant::now();

    let mut audio_stream = response.bytes_stream();
    let mut reader = StreamingReader::new();
    let mut buffer: Vec<u8> = Vec::new();
    while let Some(block) = audio_stream.next().await {
        reader.buffer.extend_from_slice(&block?);
    }
    println!("\x1b[93mFinished fetching all blocks");
    let elasped = start_time.elapsed();
    println!("Total Took: {:?}",elasped);
    println!("\x1b[96mBuffer size: {}",reader.buffer.len());
    stream(reader).await?;
    return Ok(());
}
