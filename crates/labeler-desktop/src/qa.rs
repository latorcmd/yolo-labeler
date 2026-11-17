//! Offscreen QA drives the real view and cache without altering a project on disk.
use super::*;
use std::time::{Duration, Instant};

#[cfg(windows)]
fn working_set() -> usize {
    #[repr(C)]
    struct Counters {
        cb: u32,
        faults: u32,
        peak: usize,
        working: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_nonpaged: usize,
        quota_nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut Counters,
            size: u32,
        ) -> i32;
    }
    let mut counters: Counters = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<Counters>() as u32;
    unsafe {
        K32GetProcessMemoryInfo(
            -1isize as *mut _,
            &mut counters,
            std::mem::size_of::<Counters>() as u32,
        );
    }
    counters.working
}
#[cfg(not(windows))]
fn working_set() -> usize {
    0
}

pub fn screen(
    output: &std::path::Path,
    project: Option<Project>,
    dark: bool,
    edit: bool,
    scale: f64,
    stress: bool,
    roundtrip: bool,
) -> Result<()> {
    anyhow::ensure!(
        scale.is_finite() && (0.5..=4.).contains(&scale),
        "scale is out of range"
    );
    use anti_webui::core::{
        Ui,
        event::WindowEvent,
        geometry::{Scale, SizeI},
    };
    use anti_webui::wgpu::{Gpu, Renderer, wgpu::TextureFormat};
    let started = Instant::now();
    let small = std::env::args().any(|arg| arg == "--qa-small");
    let width = ((if small { 960. } else { 1480. }) * scale) as u32;
    let height = ((if small { 540. } else { 940. }) * scale) as u32;
    let gpu = Gpu::headless().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut renderer = Renderer::new(&gpu.device, TextureFormat::Rgba8Unorm);
    let help_screen = std::env::args().any(|arg| arg == "--qa-help");
    let mut ui = Ui::new(move || {
        let root = app_with(project, Some(dark), edit);
        if help_screen { help::content() } else { root }
    });
    ui.handle_event(WindowEvent::Resized {
        size: SizeI::new(width as i32, height as i32),
        scale: Scale::from_f64(scale),
    });
    ui.handle_event(WindowEvent::Focused(true));
    ui.update();
    ui.wait_for_images(Duration::from_secs(30));
    let c = QA_CONTROLLER.with(|v| v.borrow().as_ref().unwrap().clone());
    if roundtrip {
        let before = c.edit.get();
        batch(|| {
            c.edit.set(None);
            c.editing.set(false);
        });
        ui.update();
        set_theme(if dark { Theme::light() } else { Theme::dark() });
        ui.update();
        assert!(c.edit.with(|e| e.is_none()));
        if let Some(before) = before {
            batch(|| {
                c.edit.set(Some(before));
                c.editing.set(true);
            });
            ui.update();
            batch(|| {
                c.edit.set(None);
                c.editing.set(false);
            });
            ui.update();
        }
        assert_eq!(c.editing.get(), c.edit.with(|e| e.is_some()));
    }
    let mut navigation = vec![];
    if std::env::args().any(|arg| arg == "--qa-page-bench") {
        let origin = c.state.borrow().project.as_ref().unwrap().view.page;
        for warm in [false, true] {
            if warm {
                for page in origin..=origin + 20 {
                    c.state.borrow_mut().project.as_mut().unwrap().view.page = page;
                    c.bump();
                    ui.update();
                    ui.wait_for_images(Duration::from_secs(10));
                    ui.update();
                }
            }
            for _ in 0..20 {
                if !warm {
                    let mut state = c.state.borrow_mut();
                    state.cache.clear();
                    state.lru.clear();
                    state.cache_bytes = 0;
                }
                let start = Instant::now();
                c.page(if warm { -1 } else { 1 });
                ui.update();
                let scene = ui.paint(Instant::now());
                let _ = gpu.render_to_rgba(&mut renderer, &scene);
                let first_ms = start.elapsed().as_secs_f64() * 1000.;
                let ids = c.page_ids();
                while !ids.iter().all(|id| c.state.borrow().cache.contains_key(id))
                    && start.elapsed() < Duration::from_secs(10)
                {
                    ui.wait_for_images(Duration::from_millis(1));
                    ui.update();
                }
                ui.update();
                let scene = ui.paint(Instant::now());
                let _ = gpu.render_to_rgba(&mut renderer, &scene);
                let ready = ids.iter().all(|id| c.state.borrow().cache.contains_key(id));
                navigation.push(serde_json::json!({"warm":warm,"first_ms":first_ms,"complete_ms":start.elapsed().as_secs_f64()*1000.,"ready":ready}));
            }
        }
        anyhow::ensure!(c.flush(), "navigation benchmark final save failed");
    }
    let mut cycles = vec![];
    if stress {
        let pages = c
            .state
            .borrow()
            .project
            .as_ref()
            .map(|p| p.samples.len().div_ceil(10))
            .unwrap_or(0);
        for cycle in 0..3 {
            let start = Instant::now();
            let mut maximum = 0;
            for page in 0..pages {
                if page == 0 {
                    c.page(-(pages as isize));
                    c.bump();
                } else {
                    c.page(1);
                }
                ui.update();
                ui.wait_for_images(Duration::from_secs(10));
                ui.update();
                let scene = ui.paint(Instant::now());
                let _ = gpu.render_to_rgba(&mut renderer, &scene);
                maximum = maximum.max(working_set());
            }
            let state = c.state.borrow();
            assert!(state.cache_bytes <= 128 << 20);
            cycles.push(serde_json::json!({"cycle":cycle,"pages":pages,"ms":start.elapsed().as_millis(),"cache_bytes":state.cache_bytes,"cached_images":state.cache.len(),"working_set":working_set(),"maximum_working_set":maximum}));
        }
    }
    if stress {
        anyhow::ensure!(c.flush(), "stress final save failed");
    }
    ui.wait_for_images(Duration::from_secs(30));
    let start = Instant::now();
    for i in 0..30 {
        let now = start + Duration::from_millis(i * 16);
        ui.animate(now);
        ui.update();
        ui.paint(now);
    }
    let scene = ui.paint(Instant::now() + Duration::from_secs(1));
    let pixels = gpu.render_to_rgba(&mut renderer, &scene);
    anti_webui::app::headless::write_png(output, width, height, &pixels)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let stats = serde_json::json!({"scale":scale,"dark":dark,"roundtrip":roundtrip,"navigation":navigation,"cycles":cycles,"working_set":working_set(),"elapsed_ms":started.elapsed().as_millis()});
    std::fs::write(
        output.with_extension("metrics.json"),
        serde_json::to_vec_pretty(&stats)?,
    )?;
    Ok(())
}
