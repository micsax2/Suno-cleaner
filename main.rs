// Hides the console/terminal window on Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([650.0, 520.0])
            .with_min_inner_size([550.0, 450.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Suno AI Stem Cleaner (Open Source)",
        options,
        Box::new(|_cc| Ok(Box::new(SunoCleanerApp::default()))),
    )
}

struct SunoCleanerApp {
    click_sensitivity: f32,
    lpf_cutoff: f32,
    status_message: String,
    is_processing: bool,
    rx: Option<Receiver<String>>,
}

impl Default for SunoCleanerApp {
    fn default() -> Self {
        Self {
            click_sensitivity: 0.04,
            lpf_cutoff: 16500.0,
            status_message: "Ready. Select a Suno audio stem to begin cleaning.".to_string(),
            is_processing: false,
            rx: None,
        }
    }
}

impl eframe::App for SunoCleanerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                if !self.status_message.is_empty() {
                    self.status_message.push('\n');
                }
                self.status_message.push_str(&msg);
                if msg.starts_with("✨") || msg.starts_with("❌") {
                    self.is_processing = false;
                }
            }
        }

        if self.is_processing {
            ctx.request_repaint();
        }

        egio_styling(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(10.0);
            ui.heading("🎵 Suno AI Stem Cleaner (Cross-Platform)");
            ui.label("Open-source DSP pipeline for removing transient clicks and digital vocoder fizz.");
            ui.separator();
            ui.add_space(10.0);

            ui.group(|ui| {
                ui.label("DSP Parameter Tuning");
                ui.add_space(5.0);

                ui.horizontal(|ui| {
                    ui.label("Click Sensitivity:");
                    ui.add(
                        egui::Slider::new(&mut self.click_sensitivity, 0.01..=0.10)
                            .text("Threshold"),
                    );
                });
                ui.small("Lower threshold catches more subtle isolated digital clicks/pops.");

                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.label("High-Frequency Silk LPF:");
                    ui.add(egui::Slider::new(&mut self.lpf_cutoff, 10000.0..=20000.0).text("Hz"));
                });
                ui.small("2nd-order Butterworth LPF smooths harsh Nyquist glare and vocoder fizz.");
            });

            ui.add_space(20.0);

            ui.horizontal(|ui| {
                if self.is_processing {
                    ui.spinner();
                    ui.label("Processing audio pipeline...");
                } else if ui.button("📁 Select & Clean Suno WAV File").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("WAV Audio", &["wav"])
                        .pick_file()
                    {
                        self.is_processing = true;
                        self.status_message.clear();
                        let sensitivity = self.click_sensitivity;
                        let cutoff = self.lpf_cutoff;

                        let (tx, rx) = mpsc::channel();
                        self.rx = Some(rx);

                        thread::spawn(move || {
                            run_dsp_pipeline(path, sensitivity, cutoff, tx);
                        });
                    }
                }
            });

            ui.add_space(20.0);
            ui.separator();
            ui.add_space(10.0);

            ui.label("Execution Log:");
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.status_message)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
        });
    }
}

fn egio_styling(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    ctx.set_style(style);
}

/// 2nd-order Butterworth Low-Pass Biquad Filter (Direct Form II Transposed)
struct BiquadLpf {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl BiquadLpf {
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        // Clamp cutoff safely below Nyquist
        let fc = cutoff_hz.clamp(20.0, sample_rate * 0.49);
        let omega = 2.0 * std::f32::consts::PI * fc / sample_rate;
        let sin_w = omega.sin();
        let cos_w = omega.cos();
        // Butterworth Q = 1 / sqrt(2)
        let alpha = sin_w / std::f32::consts::SQRT_2;

        let a0 = 1.0 + alpha;
        let b0 = ((1.0 - cos_w) * 0.5) / a0;
        let b1 = (1.0 - cos_w) / a0;
        let b2 = ((1.0 - cos_w) * 0.5) / a0;
        let a1 = (-2.0 * cos_w) / a0;
        let a2 = (1.0 - alpha) / a0;

        Self {
            b0,
            b1,
            b2,
            a1,
            a2,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Detects isolated 1-2 sample digital clicks using 2nd-order difference (curvature)
/// compared against the local surrounding slope so normal high-frequency audio is untouched.
fn declick_channel(channel: &mut [f32], sensitivity: f32) -> usize {
    if channel.len() < 8 {
        return 0;
    }

    let orig = channel.to_vec();
    let mut click_count = 0;

    for i in 3..orig.len() - 3 {
        // Linear prediction from immediate neighbors
        let predicted = 0.5 * (orig[i - 1] + orig[i + 1]);
        let impulse_error = (orig[i] - predicted).abs();

        // Local surrounding activity (average step size outside the spike)
        let local_activity = 0.25
            * ((orig[i - 1] - orig[i - 2]).abs()
                + (orig[i - 2] - orig[i - 3]).abs()
                + (orig[i + 2] - orig[i + 1]).abs()
                + (orig[i + 3] - orig[i + 2]).abs());

        // Check that neighbors themselves return to the normal trajectory
        let neighbor_continuity = (orig[i + 1] - orig[i - 1]).abs();

        // A true click spikes sharply above both the absolute threshold AND local signal slope,
        // while the samples on either side remain relatively continuous.
        if impulse_error > sensitivity
            && impulse_error > local_activity * 4.5
            && neighbor_continuity < impulse_error * 1.2
        {
            // Cubic interpolation using [-2, -1, +1, +2]
            channel[i] = (-orig[i - 2] + 4.0 * orig[i - 1] + 4.0 * orig[i + 1] - orig[i + 2]) / 6.0;
            click_count += 1;
        }
    }

    click_count
}

fn run_dsp_pipeline(input_path: PathBuf, sensitivity: f32, lpf_cutoff: f32, tx: Sender<String>) {
    let file_name = input_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("input.wav");
    let _ = tx.send(format!("Loading audio file: {}", file_name));

    let mut reader = match WavReader::open(&input_path) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(format!("❌ Error opening file: {}", e));
            return;
        }
    };

    let spec: WavSpec = reader.spec();
    let sample_rate = spec.sample_rate as f32;
    let channels = spec.channels as usize;

    if channels == 0 {
        let _ = tx.send("❌ Invalid channel count (0).".into());
        return;
    }

    let _ = tx.send(format!(
        "Sample Rate: {} Hz | Channels: {} | Format: {:?} {}-bit",
        spec.sample_rate, channels, spec.sample_format, spec.bits_per_sample
    ));

    // Properly decode based on both SampleFormat and bits_per_sample
    let interleaved: Result<Vec<f32>, _> = match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / 32_768.0))
            .collect(),
        (SampleFormat::Int, 24) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 8_388_608.0))
            .collect(),
        (SampleFormat::Int, 32) => reader
            .samples::<i32>()
            .map(|s| s.map(|v| v as f32 / 2_147_483_648.0))
            .collect(),
        (SampleFormat::Float, 32) => reader.samples::<f32>().collect(),
        _ => {
            let _ = tx.send(format!(
                "❌ Unsupported format: {:?} {}-bit",
                spec.sample_format, spec.bits_per_sample
            ));
            return;
        }
    };

    let data = match interleaved {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(format!("❌ Error reading samples: {}", e));
            return;
        }
    };

    let num_frames = data.len() / channels;

    // De-interleave into separate per-channel buffers
    let mut channel_buffers: Vec<Vec<f32>> = vec![Vec::with_capacity(num_frames); channels];
    for (idx, &sample) in data.iter().enumerate() {
        channel_buffers[idx % channels].push(sample);
    }

    let _ = tx.send("Stage 1: De-clicking transient spikes per channel...".into());
    let mut total_clicks = 0;
    for ch_buf in channel_buffers.iter_mut() {
        total_clicks += declick_channel(ch_buf, sensitivity);
    }
    let _ = tx.send(format!(" -> Repaired {} click anomalies.", total_clicks));

    let _ = tx.send(format!(
        "Stage 2: Applying 2nd-order Butterworth LPF ({:.0} Hz)...",
        lpf_cutoff
    ));
    for ch_buf in channel_buffers.iter_mut() {
        let mut lpf = BiquadLpf::new(lpf_cutoff, sample_rate);
        for sample in ch_buf.iter_mut() {
            *sample = lpf.process(*sample);
        }
    }

    let stem = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let output_path = input_path.with_file_name(format!("{}_cleaned.wav", stem));

    // Preserve original sample format and bit depth for zero quality loss
    let mut writer = match WavWriter::create(&output_path, spec) {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(format!("❌ Error creating output file: {}", e));
            return;
        }
    };

    // Re-interleave and write back using original bit depth
    for frame in 0..num_frames {
        for ch in 0..channels {
            let s = channel_buffers[ch][frame].clamp(-1.0, 1.0);
            let write_res = match (spec.sample_format, spec.bits_per_sample) {
                (SampleFormat::Int, 16) => {
                    writer.write_sample((s * 32_767.0).round() as i16)
                }
                (SampleFormat::Int, 24) => {
                    writer.write_sample((s * 8_388_607.0).round() as i32)
                }
                (SampleFormat::Int, 32) => {
                    writer.write_sample((s as f64 * 2_147_483_647.0).round() as i32)
                }
                (SampleFormat::Float, 32) => writer.write_sample(s),
                _ => unreachable!(),
            };
            if let Err(e) = write_res {
                let _ = tx.send(format!("❌ Error writing sample: {}", e));
                return;
            }
        }
    }

    if let Err(e) = writer.finalize() {
        let _ = tx.send(format!("❌ Error saving file: {}", e));
        return;
    }

    let _ = tx.send(format!(
        "✨ Success! Cleaned file saved to:\n{}",
        output_path.display()
    ));
}
