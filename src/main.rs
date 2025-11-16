#![allow(warnings)]

use std::sync::atomic::{AtomicBool,Ordering};
use std::{io::Cursor, process::Command, sync::Arc};
use std::env;
use futures_util::StreamExt;
use std::sync::Mutex;
use bytes::{Bytes, BytesMut, buf};

use symphonia::core::{
    audio::{AudioBufferRef, SampleBuffer, SignalSpec},
    codecs::{DecoderOptions, CODEC_TYPE_NULL},
    formats::{FormatOptions, FormatReader},
    io::{MediaSourceStream, ReadBytes},
    probe::Hint,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

struct StreamingReader {
    buffer: Arc<Mutex<BytesMut>>,
    position: usize,
    download_complete: Arc<AtomicBool>
}

impl StreamingReader {
    fn new(buffer: Arc<Mutex<BytesMut>>,download_complete: Arc<AtomicBool>) -> Self {
        return Self { buffer, position: 0, download_complete}
    }
}

impl std::io::Read for StreamingReader {
    fn read(&mut self,buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let buffer = self.buffer.lock().unwrap();

            if self.position >= buffer.len() {
                if self.download_complete.load(Ordering::Relaxed) {
                    return Ok(0);
                } else {
                    drop(buffer);
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    continue;
                }
            }
            let available = buffer.len() - self.position;
            let left_to_read = buf.len().min(available);

            buf[..left_to_read].copy_from_slice(&buffer[self.position..self.position + left_to_read]);
            self.position += left_to_read;
            return Ok(left_to_read);
        }
    }
}

impl std::io::Seek for StreamingReader {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match pos {
            std::io::SeekFrom::Start(offset) => {
                self.position = offset as usize;
                return Ok(self.position as u64);
            }
            std::io::SeekFrom::Current(offset) => {
                self.position = (self.position as i64 + offset) as usize;
                return Ok(self.position as u64);
            }
            std::io::SeekFrom::End(offset) => {
                let buffer = self.buffer.lock().expect("\x1b[91mCouldnt Get Mutex Lock On Buffer");
                self.position = (buffer.len() as i64 + offset) as usize;
                return Ok(self.position as u64);
            }
        }
    }
}

impl symphonia::core::io::MediaSource for StreamingReader{
    fn is_seekable(&self) -> bool {
        return true;
    }

    fn byte_len(&self) -> Option<u64> {
        let buf = self.buffer.lock().expect("\x1b[91mCouldnt Get Mutex Lock On Buffer");
        return Some(buf.len() as u64);
    }
}

async fn stream(response: reqwest::Response) -> Result<(),Box<dyn std::error::Error>> {
    let buffer = Arc::new(Mutex::new(BytesMut::new()));
    let buffer_clone = buffer.clone();
    let download_complete = Arc::new(AtomicBool::new(false));
    let download_complete_clone = download_complete.clone();
    let handle = tokio::spawn(async move {
        let mut stream = response.bytes_stream();
        let mut chunk_count: u16 = 0;

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(chunk) => {
                    let buf_len = {
                        let mut buf = buffer_clone.lock().unwrap();
                        buf.extend_from_slice(&chunk);
                        buf.len()
                    };
                    chunk_count += 1;
                    // if chunk_count % 10 == 0 {
                    //     println!("\x1b[93mChunk Count: {}, Total: {} KB\x1b[0m", 
                    //         chunk_count, buf_len / 1024);
                    // }
                }
                Err(e) =>  {
                    eprintln!("\x1b[91mChunk Failed: {}\x1b[0m", e);
                    break;
                }
            }
        }
        download_complete_clone.store(true, Ordering::Relaxed);
        println!("\x1b[93mFinished fetching all chunks\x1b[0m");
    });
    println!("\x1b[93mBuffering...\x1b[0m");

    loop {
        let buf_len = {
            let buf = buffer.lock().unwrap();
            buf.len()
        };

        if buf_len >= 1 * 1024 * 1024 {
            println!("\x1b[96mBuffered {} KB\x1b[0m", buf_len / 1024);
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    {
        let buf = buffer.lock().expect("\x1b[91mCouldnt Get Mutex Lock On Buffer");
        println!("\x1b[96mBuffer size: {} bytes\x1b[0m", buf.len());
    }

    let reader = StreamingReader::new(buffer.clone(),download_complete.clone());
    let mss = MediaSourceStream::new(Box::new(reader), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("m4a");
    let format_opts = FormatOptions::default();
    let metadata_opts = Default::default();

    println!("\x1b[93mProbing format...\x1b[0m");
    let probed = symphonia::default::get_probe().format(&hint, mss, &format_opts, &metadata_opts)?;

    let mut format = probed.format;

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

    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("\x1b[91mNo Output Device Found")?;

    println!("\x1b[92mOutput device: {}\x1b[0m", device.name()?);

    let config = device.default_output_config()?;

    println!("\x1b[92mOutput config: {:?}\x1b[0m", config);

    let sample_rate = codec_params.sample_rate.unwrap_or(48000);
    let channels = codec_params.channels
        .unwrap_or(symphonia::core::audio::Channels::FRONT_LEFT | symphonia::core::audio::Channels::FRONT_RIGHT).count();

    let spec = SignalSpec::new(sample_rate, symphonia::core::audio::Channels::FRONT_LEFT | symphonia::core::audio::Channels::FRONT_RIGHT);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();

    std::thread::spawn(move || {
        let config = cpal::StreamConfig {
            channels: channels as u16,
            sample_rate: cpal::SampleRate(sample_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let mut sample_queue: Vec<f32> = Vec::new();

        let stream = device.build_output_stream(&config, move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let mut idx = 0;
            while idx < data.len() && !sample_queue.is_empty() {
                data[idx] = sample_queue.remove(0);
                idx += 1;
            }

            while idx < data.len() {
                if let Ok(samples) = rx.try_recv() {
                    for sample in samples {
                        if idx < data.len() {
                            data[idx] = sample;
                            idx += 1;
                        } else {
                            sample_queue.push(sample);
                        }
                    }
                } else {
                    break;
                }
            }

            for i in idx..data.len() {
                data[i] = 0.0;
            }
        }, |err| eprintln!("\x1b[91mStream Error: {}\x1b[0m",err),
            None).unwrap();

        stream.play().unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });

    println!("\x1b[92mPlaying...\x1b[0m");

    let mut sample_buf = None;
    let mut packet_count = 0;

    loop {
        let buffer_len = {
            let buf = buffer.lock().unwrap();
            buf.len()
        };

        if buffer_len < 8192 {
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            continue;
        }

        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(e))
            if e.kind() == std::io::ErrorKind::WouldBlock => {
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                continue;
            }
            Err(symphonia::core::errors::Error::IoError(e))
            if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                if download_complete.load(Ordering::Relaxed) {
                    println!("\x1b[92mEnd of stream\x1b[0m");
                    break;
                } else {

                    println!("\x1b[93mWaiting for more data...\x1b[0m");
                    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                    continue;
                }
            }
            Err(e) => {
                eprintln!("\x1b[91mFormat error: {}\x1b[0m", e);
                break;
            }
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(e) => {
                eprintln!("\x1b[91mDecode error: {}\x1b[0m", e);
                continue;
            }
        };

        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec()));
        }

        if let Some(ref mut buf) = sample_buf {
            buf.copy_interleaved_ref(decoded);
            let samples = buf.samples().to_vec();

            if tx.send(samples).is_err() {
                break;
            }

            packet_count += 1;
            if packet_count % 100 == 0 {
                println!("\x1b[96mDecode {} packets...\x1b[0m",packet_count);
            }
        }
    }
    println!("\x1b[92mFinished Decoding. Total packets decoded: {}\x1b[0m", packet_count);
    drop(tx);

    let estimated_remaining_secs = (packet_count as f64 * 1024.0) / (sample_rate as f64);
    println!("\x1b[96mWaiting for audio to finish (~{:.1} seconds)...\x1b[0m", estimated_remaining_secs);

    tokio::time::sleep(tokio::time::Duration::from_secs_f64(estimated_remaining_secs + 2.0)).await;

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
            &env::args().nth(1).ok_or("Missing Arguments")?
        ]).output()?;

    let url = String::from_utf8(output.stdout)?;

    if url.is_empty() {
        return Err("\x1b[91mFailed to get audio URL from yt-dlp\x1b[0m".into());
    }

    println!("\x1b[92mAudio URL Acquired\x1b[0m");

    let client = reqwest::Client::new();

    let response: reqwest::Response = client.get(&url).send().await?;
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
    stream(response).await?;

    return Ok(());
}
