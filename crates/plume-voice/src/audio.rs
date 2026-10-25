//! Microphone and speakers: capture → Opus → the voice connection, and
//! everyone's Opus → a mixer → the speakers.
//!
//! Audio callbacks only copy samples; encoding and decoding happen on two
//! small worker threads, so a slow terminal never causes crackling.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use plume_core::Snowflake;
use tracing::{debug, warn};

use crate::VoiceHandle;

/// Discord's audio format.
pub const SAMPLE_RATE: u32 = 48_000;
/// 20 ms at 48 kHz.
pub const FRAME: usize = 960;
/// Keep at most this much of someone's audio queued (latency cap).
const MAX_QUEUED: usize = SAMPLE_RATE as usize * 3 / 10;
/// Opus packets are never bigger than this.
const MAX_PACKET: usize = 1275;

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no speakers or headphones found")]
    NoOutput,
    #[error("audio device error: {0}")]
    Device(String),
    #[error("Opus: {0}")]
    Opus(#[from] opus::Error),
}

/// Shared switches the UI flips.
#[derive(Default)]
struct Controls {
    muted: AtomicBool,
    deafened: AtomicBool,
    /// Whether we're sending audio right now (for a "talking" indicator).
    talking: AtomicBool,
    stop: AtomicBool,
}

/// Running audio for one call. Dropping it stops everything.
pub struct AudioEngine {
    controls: Arc<Controls>,
    incoming: SyncSender<(Snowflake, Vec<u8>)>,
    mixer: Arc<Mutex<Mixer>>,
    worker: Option<thread::JoinHandle<()>>,
    /// Without a microphone we can still listen.
    pub has_microphone: bool,
}

impl AudioEngine {
    /// Opens the default microphone and speakers.
    pub fn start(voice: VoiceHandle) -> Result<AudioEngine, AudioError> {
        let controls = Arc::new(Controls::default());
        let mixer = Arc::new(Mutex::new(Mixer::default()));
        let (incoming, decode_rx) = mpsc::sync_channel::<(Snowflake, Vec<u8>)>(64);
        let (ready_tx, ready_rx) = mpsc::channel();

        // cpal streams aren't Send on every platform, so one thread owns them.
        let worker = {
            let controls = controls.clone();
            let mixer = mixer.clone();
            thread::Builder::new()
                .name("plume-audio".into())
                .spawn(move || {
                    let streams = open_streams(voice, controls.clone(), mixer.clone());
                    let has_microphone = match &streams {
                        Ok((_, input)) => input.is_some(),
                        Err(_) => false,
                    };
                    let _ = ready_tx.send(streams.as_ref().map(|_| has_microphone).map_err(|e| e.to_string()));
                    if streams.is_err() {
                        return;
                    }
                    decode_loop(decode_rx, &controls, &mixer);
                    drop(streams);
                })
                .map_err(|e| AudioError::Device(e.to_string()))?
        };
        let has_microphone = ready_rx
            .recv()
            .map_err(|_| AudioError::Device("audio thread died".into()))?
            .map_err(AudioError::Device)?;
        Ok(AudioEngine {
            controls,
            incoming,
            mixer,
            worker: Some(worker),
            has_microphone,
        })
    }

    /// Queues someone's Opus frame for playback.
    pub fn play(&self, user_id: Snowflake, opus: Vec<u8>) {
        if let Err(TrySendError::Full(_)) = self.incoming.try_send((user_id, opus)) {
            debug!("audio: decoder busy, dropping a frame");
        }
    }

    pub fn set_muted(&self, muted: bool) {
        self.controls.muted.store(muted, Ordering::Relaxed);
    }

    pub fn set_deafened(&self, deafened: bool) {
        self.controls.deafened.store(deafened, Ordering::Relaxed);
        if deafened && let Ok(mut mixer) = self.mixer.lock() {
            mixer.clear();
        }
    }

    pub fn is_talking(&self) -> bool {
        self.controls.talking.load(Ordering::Relaxed)
    }

    /// 0.0 = silent, 1.0 = normal, up to 2.0.
    pub fn set_volume(&self, user_id: Snowflake, volume: f32) {
        if let Ok(mut mixer) = self.mixer.lock() {
            mixer.volumes.insert(user_id, volume.clamp(0.0, 2.0));
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.controls.stop.store(true, Ordering::Relaxed);
        // Wakes the decoder so the worker notices.
        let _ = self.incoming.try_send((Snowflake(0), Vec::new()));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

type Streams = (cpal::Stream, Option<cpal::Stream>);

fn open_streams(voice: VoiceHandle, controls: Arc<Controls>, mixer: Arc<Mutex<Mixer>>) -> Result<Streams, AudioError> {
    let host = cpal::default_host();
    let device_error = |e: cpal::Error| AudioError::Device(e.to_string());

    let output_device = host.default_output_device().ok_or(AudioError::NoOutput)?;
    let output_config = output_device.default_output_config().map_err(device_error)?;
    let output = match output_config.sample_format() {
        SampleFormat::F32 => build_output::<f32>(&output_device, output_config.into(), mixer),
        SampleFormat::I16 => build_output::<i16>(&output_device, output_config.into(), mixer),
        SampleFormat::U16 => build_output::<u16>(&output_device, output_config.into(), mixer),
        SampleFormat::I32 => build_output::<i32>(&output_device, output_config.into(), mixer),
        other => return Err(AudioError::Device(format!("unsupported output format {other}"))),
    }?;
    output.play().map_err(device_error)?;

    let input = match host.default_input_device() {
        None => {
            warn!("audio: no microphone; listening only");
            None
        }
        Some(device) => {
            let config = device.default_input_config().map_err(device_error)?;
            let format = config.sample_format();
            let config: StreamConfig = config.into();
            let (samples_tx, samples_rx) = mpsc::sync_channel::<Vec<f32>>(32);
            let rate = config.sample_rate;
            let encoder_controls = controls.clone();
            thread::Builder::new()
                .name("plume-encode".into())
                .spawn(move || encode_loop(samples_rx, rate, voice, &encoder_controls))
                .map_err(|e| AudioError::Device(e.to_string()))?;
            let stream = match format {
                SampleFormat::F32 => build_input::<f32>(&device, config, samples_tx),
                SampleFormat::I16 => build_input::<i16>(&device, config, samples_tx),
                SampleFormat::U16 => build_input::<u16>(&device, config, samples_tx),
                SampleFormat::I32 => build_input::<i32>(&device, config, samples_tx),
                other => return Err(AudioError::Device(format!("unsupported input format {other}"))),
            }?;
            stream.play().map_err(device_error)?;
            Some(stream)
        }
    };
    Ok((output, input))
}

fn build_input<T>(
    device: &cpal::Device,
    config: StreamConfig,
    samples: SyncSender<Vec<f32>>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels).max(1);
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                // Average the channels down to mono.
                let mono: Vec<f32> = data
                    .chunks(channels)
                    .map(|frame| frame.iter().map(|&s| f32::from_sample(s)).sum::<f32>() / channels as f32)
                    .collect();
                let _ = samples.try_send(mono);
            },
            |error| warn!("microphone: {error}"),
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))
}

fn build_output<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mixer: Arc<Mutex<Mixer>>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels).max(1);
    let mut resampler = Resampler::new(SAMPLE_RATE, config.sample_rate);
    let mut mixed = Vec::new();
    let mut out = Vec::new();
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                let frames = data.len() / channels;
                let needed = resampler.input_needed(frames);
                mixed.clear();
                mixed.resize(needed, 0.0);
                if let Ok(mut mixer) = mixer.try_lock() {
                    mixer.mix_into(&mut mixed);
                }
                out.clear();
                resampler.process(&mixed, &mut out);
                out.resize(frames, 0.0);
                for (frame, &sample) in data.chunks_mut(channels).zip(&out) {
                    frame.fill(T::from_sample(sample));
                }
            },
            |error| warn!("speakers: {error}"),
            None,
        )
        .map_err(|e| AudioError::Device(e.to_string()))
}

/// Microphone samples → 48 kHz frames → voice gate → Opus → Discord.
fn encode_loop(samples: Receiver<Vec<f32>>, rate: u32, voice: VoiceHandle, controls: &Controls) {
    let mut encoder = match new_encoder() {
        Ok(encoder) => encoder,
        Err(error) => {
            warn!("Opus encoder: {error}");
            return;
        }
    };
    let mut resampler = Resampler::new(rate, SAMPLE_RATE);
    let mut framer = Framer::default();
    let mut gate = Gate::default();
    let mut resampled = Vec::new();
    let mut packet = [0u8; MAX_PACKET];
    while let Ok(chunk) = samples.recv() {
        if controls.stop.load(Ordering::Relaxed) {
            break;
        }
        resampled.clear();
        resampler.process(&chunk, &mut resampled);
        for frame in framer.push(&resampled) {
            let open = !controls.muted.load(Ordering::Relaxed) && gate.open(&frame);
            let was_talking = controls.talking.swap(open, Ordering::Relaxed);
            if open {
                match encoder.encode_float(&frame, &mut packet) {
                    Ok(len) => voice.send_opus(packet[..len].to_vec()),
                    Err(error) => warn!("Opus encode: {error}"),
                }
            } else if was_talking {
                voice.stop_speaking();
            }
        }
    }
}

fn new_encoder() -> Result<opus::Encoder, opus::Error> {
    let mut encoder = opus::Encoder::new(SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)?;
    encoder.set_bitrate(opus::Bitrate::Bits(64_000))?;
    // Lets the other side rebuild a lost packet from the next one.
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(5)?;
    Ok(encoder)
}

/// Everyone's Opus → their own decoder → the mixer.
fn decode_loop(incoming: Receiver<(Snowflake, Vec<u8>)>, controls: &Controls, mixer: &Mutex<Mixer>) {
    let mut decoders: HashMap<Snowflake, opus::Decoder> = HashMap::new();
    let mut pcm = vec![0f32; FRAME * 6];
    while let Ok((user_id, opus)) = incoming.recv() {
        if controls.stop.load(Ordering::Relaxed) {
            break;
        }
        if opus.is_empty() || controls.deafened.load(Ordering::Relaxed) {
            continue;
        }
        let decoder = match decoders.entry(user_id) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                match opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono) {
                    Ok(decoder) => entry.insert(decoder),
                    Err(error) => {
                        warn!("Opus decoder: {error}");
                        continue;
                    }
                }
            }
        };
        match decoder.decode_float(&opus, &mut pcm, false) {
            Ok(samples) => {
                if let Ok(mut mixer) = mixer.lock() {
                    mixer.push(user_id, &pcm[..samples]);
                }
            }
            Err(error) => debug!("Opus decode from {user_id}: {error}"),
        }
    }
}

/// Linear-interpolation resampler for mono audio. Plenty for voice, and
/// cheap enough to run inside an audio callback.
pub struct Resampler {
    step: f64,
    /// Position of the next output sample, relative to `last`.
    position: f64,
    last: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        Resampler {
            step: f64::from(from.max(1)) / f64::from(to.max(1)),
            position: 0.0,
            last: 0.0,
        }
    }

    /// How many input samples produce about `output` samples.
    pub fn input_needed(&self, output: usize) -> usize {
        ((output as f64 * self.step) - self.position).ceil().max(0.0) as usize
    }

    pub fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if self.step == 1.0 {
            output.extend_from_slice(input);
            return;
        }
        // Sample i of the stream is `last` at i = 0 and input[i - 1] after.
        let at = |i: usize, last: f32| if i == 0 { last } else { input[i - 1] };
        while self.position < input.len() as f64 {
            let index = self.position.floor() as usize;
            let fraction = (self.position - index as f64) as f32;
            let a = at(index, self.last);
            let b = at(index + 1, self.last);
            output.push(a + (b - a) * fraction);
            self.position += self.step;
        }
        self.position -= input.len() as f64;
        if let Some(&last) = input.last() {
            self.last = last;
        }
    }
}

/// Cuts a stream of samples into 20 ms frames.
#[derive(Default)]
pub struct Framer {
    pending: Vec<f32>,
}

impl Framer {
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let mut frames = Vec::new();
        while self.pending.len() >= FRAME {
            frames.push(self.pending.drain(..FRAME).collect());
        }
        frames
    }
}

/// Voice activity: opens on speech, stays open through short pauses.
pub struct Gate {
    threshold: f32,
    hangover: u32,
    remaining: u32,
}

impl Default for Gate {
    fn default() -> Self {
        // About -45 dBFS, held open for 400 ms after the last loud frame.
        Gate {
            threshold: 0.0056,
            hangover: 20,
            remaining: 0,
        }
    }
}

impl Gate {
    pub fn open(&mut self, frame: &[f32]) -> bool {
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / frame.len().max(1) as f32).sqrt();
        if rms >= self.threshold {
            self.remaining = self.hangover;
            true
        } else if self.remaining > 0 {
            self.remaining -= 1;
            true
        } else {
            false
        }
    }
}

/// Everyone's decoded audio, waiting to be played.
#[derive(Default)]
pub struct Mixer {
    queues: HashMap<Snowflake, VecDeque<f32>>,
    volumes: HashMap<Snowflake, f32>,
}

impl Mixer {
    pub fn push(&mut self, user_id: Snowflake, samples: &[f32]) {
        let queue = self.queues.entry(user_id).or_default();
        queue.extend(samples);
        // Behind by too much (a network hiccup): skip ahead.
        if queue.len() > MAX_QUEUED {
            let excess = queue.len() - MAX_QUEUED;
            queue.drain(..excess);
        }
    }

    /// Adds everyone's next samples into `out`, which starts as silence.
    pub fn mix_into(&mut self, out: &mut [f32]) {
        for (user, queue) in &mut self.queues {
            let volume = self.volumes.get(user).copied().unwrap_or(1.0);
            let n = out.len().min(queue.len());
            for (slot, sample) in out.iter_mut().zip(queue.drain(..n)) {
                *slot += sample * volume;
            }
        }
        for sample in out.iter_mut() {
            // Soft clip so several loud voices don't crackle.
            *sample = sample.clamp(-1.5, 1.5);
            *sample -= *sample * *sample * *sample / 6.75;
        }
        self.queues.retain(|_, queue| !queue.is_empty());
    }

    pub fn clear(&mut self) {
        self.queues.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|i| (i as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn resampling_keeps_length_and_shape() {
        for (from, to) in [(44_100, 48_000), (48_000, 44_100), (16_000, 48_000), (48_000, 48_000)] {
            let mut resampler = Resampler::new(from, to);
            let mut out = Vec::new();
            // In uneven chunks, like a sound card delivers them.
            let input = sine(from, 440.0, from as usize);
            for chunk in input.chunks(441) {
                resampler.process(chunk, &mut out);
            }
            let expected = to as usize;
            assert!(out.len().abs_diff(expected) <= 2, "{from}->{to}: {} samples", out.len());
            let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!((0.45..=0.51).contains(&peak), "{from}->{to}: peak {peak}");
        }
        let resampler = Resampler::new(48_000, 44_100);
        assert!(resampler.input_needed(441).abs_diff(480) <= 1);
    }

    #[test]
    fn frames_and_voice_gate() {
        let mut framer = Framer::default();
        assert!(framer.push(&[0.0; 500]).is_empty());
        let frames = framer.push(&[0.0; 1500]);
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() == FRAME));

        let mut gate = Gate::default();
        assert!(!gate.open(&[0.0; FRAME]), "silence stays closed");
        assert!(gate.open(&sine(48_000, 200.0, FRAME)));
        for _ in 0..20 {
            assert!(gate.open(&[0.0; FRAME]), "held through a short pause");
        }
        assert!(!gate.open(&[0.0; FRAME]));
    }

    #[test]
    fn mixer_sums_caps_and_respects_volume() {
        let mut mixer = Mixer::default();
        mixer.push(Snowflake(1), &[0.25; 10]);
        mixer.push(Snowflake(2), &[0.25; 5]);
        mixer.volumes.insert(Snowflake(2), 0.0);
        let mut out = [0.0; 10];
        mixer.mix_into(&mut out);
        assert!((out[0] - 0.2477).abs() < 0.001, "{}", out[0]);
        assert!(mixer.queues.is_empty(), "drained queues are dropped");

        mixer.push(Snowflake(1), &vec![0.1; MAX_QUEUED * 2]);
        assert_eq!(mixer.queues[&Snowflake(1)].len(), MAX_QUEUED, "latency capped");
        let mut loud = [0.0; 4];
        mixer.push(Snowflake(3), &[5.0; 4]);
        mixer.mix_into(&mut loud);
        assert!(loud.iter().all(|s| s.abs() <= 1.0), "soft clipped: {loud:?}");
    }

    #[test]
    fn opus_round_trip() {
        let mut encoder = new_encoder().unwrap();
        let mut decoder = opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).unwrap();
        let input = sine(48_000, 300.0, FRAME * 10);
        let mut packet = [0u8; MAX_PACKET];
        let mut pcm = vec![0f32; FRAME * 6];
        let mut energy = 0.0;
        for frame in input.chunks(FRAME) {
            let len = encoder.encode_float(frame, &mut packet).unwrap();
            assert!(len > 0 && len < 400, "about 64 kbps: {len} bytes");
            let samples = decoder.decode_float(&packet[..len], &mut pcm, false).unwrap();
            assert_eq!(samples, FRAME);
            energy += pcm[..samples].iter().map(|s| s * s).sum::<f32>();
        }
        assert!(energy > 100.0, "the tone survives: {energy}");
    }
}
