// Hides the console/terminal window on Windows
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use hound::{WavReader, WavSpec, WavWriter};
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
            if let Ok(msg) = rx.try_recv() {
                self.status_message = msg.clone();
                if msg.starts_with("✨") || msg.starts_with("❌") {
                    self.is_processing = false;
                }
            }
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
                    ui.add(egui::Slider::new(&mut self.click_sensitivity, 0.01..=0.08).text("Threshold"));
                });
                ui.small("Lower threshold catches more subtle digital clicks/pops.");
                
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.label("High-Frequency Silk LPF:");
                    ui.add(egui::Slider::new(&mut self.lpf_cutoff, 10000.0..=20000.0).text("Hz"));
                });
                ui.small("Smooths out harsh high-frequency Nyquist glare and vocoder fizz.");
            });

            ui.add_space(20.0);

            ui.horizontal(|ui| {
                if self.is_processing {
                    ui.spinner();
                    ui.label("Processing audio pipeline...");
                } else {
                    if ui.button("📁 Select & Clean Suno WAV File").clicked() {
                        if let Some(path) = rfd::FileDialog::new().add_filter("WAV Audio", &["wav"]).pick_file() {
                            self.is_processing = true;
                            let sensitivity = self.click_sensitivity;
                            let cutoff = self.lpf_cutoff;
                            
                            let (tx, rx) = mpsc::channel();
                            self.rx = Some(rx);

                            thread::spawn(move || {
                                run_dsp_pipeline(path, sensitivity, cutoff, tx);
                            });
                        }
                    }
                }
            });

            ui.add_space(20.0);
            ui.separator();
            ui.add_space(10.0);

            ui.label("Execution Log:");
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.status_message)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
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

fn run_dsp_pipeline(input_path: PathBuf, sensitivity: f32, lpf_cutoff: f32, tx: Sender<String>) {
    let _ = tx.send(format!("Loading audio file: {:?}", input_path.file_name().unwrap()));

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

    let _ = tx.send(format!("Sample Rate: {} Hz | Channels: {}", sample_rate, channels));
    let _ = tx.send("Stage 1: De-clicking transient spikes...".into());

    let samples: Result<Vec<f32>, _> = match spec.bits_per_sample {
        16 => reader.samples::<i16>().map(|s| s.map(|v| v as f32 / 32768.0)).collect(),
        24 | 32 => reader.samples::<i32>().map(|s| s.map(|v| v as f32 / 2147483648.0)).collect(),
        _ => {
            let _ = tx.send("❌ Unsupported bit depth.".into());
            return;
        }
    };

    let mut data = match samples {
        Ok(d) => d,
        Err(e) => {
            let _ = tx.send(format!("❌ Error reading samples: {}", e));
            return;
        }
    };

    let mut click_count = 0;
    for i in 1..data.len() - 1 {
        let diff = (data[i] - data[i - 1]).abs();
        if diff > sensitivity {
            data[i] = (data[i - 1] + data[i + 1]) / 2.0;
            click_count += 1;
        }
    }

    let _ = tx.send(format!(" -> Repaired {} click anomalies.", click_count));
    let _ = tx.send(format!("Stage 2: Applying high-frequency silk LPF ({} Hz)...", lpf_cutoff));

    let dt = 1.0 / sample_rate;
    let rc = 1.0 / (2.0 * std::f32::consts::PI * lpf_cutoff);
    let alpha = dt / (rc + dt);
    
    let mut prev = 0.0;
    for sample in data.iter_mut() {
        prev = prev + alpha * (*sample - prev);
        *sample = prev;
    }

    let output_path = input_path.with_file_name(
        format!("{}_cleaned.wav", input_path.file_stem().unwrap().to_str().unwrap())
    );

    let spec_out = WavSpec {
        channels: channels as u16,
        sample_rate: spec.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = match WavWriter::create(&output_path, spec_out) {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(format!("❌ Error creating output file: {}", e));
            return;
        }
    };

    for sample in data {
        let scaled = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
        let _ = writer.write_sample(scaled);
    }

    if let Err(e) = writer.finalize() {
        let _ = tx.send(format!("❌ Error saving file: {}", e));
        return;
    }

    let _ = tx.send(format!("✨ Success! Cleaned file saved to:\n{:?}", output_path));
}

