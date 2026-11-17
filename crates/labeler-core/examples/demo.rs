//! Small reproducible UI fixture: objects, backgrounds, aliases and natural names.
use anyhow::{Context, Result};
use labeler_core::{ImportOptions, Project};
use std::{path::PathBuf, sync::atomic::AtomicBool};
fn main() -> Result<()> {
    let file = PathBuf::from(std::env::args().nth(1).context("usage: demo OUTPUT.ylp2")?);
    let mut project = Project::new(file.clone(), vec!["H".into(), "S".into(), "U".into()])?;
    let source = file
        .parent()
        .unwrap()
        .join(format!("demo-input-{}", uuid::Uuid::new_v4()));
    let objects = source.join("objects");
    let backgrounds = source.join("backgrounds");
    std::fs::create_dir_all(&objects)?;
    std::fs::create_dir_all(&backgrounds)?;
    for index in [1, 2, 3, 10, 11, 20, 100, 101] {
        let mut image = image::RgbImage::from_pixel(160, 120, image::Rgb([248, 248, 248]));
        for x in 55..105 {
            for y in 25..95 {
                image.put_pixel(x, y, image::Rgb([20, 30, 40]));
            }
        }
        image.save(objects.join(format!("{index}.png")))?;
        if index <= 3 {
            std::fs::write(
                objects.join(format!("{index}.txt")),
                format!("{} 0.5 0.5 0.3125 0.583333333\n", index - 1),
            )?;
        }
    }
    for index in [4, 5, 6, 12] {
        image::RgbImage::from_pixel(160, 120, image::Rgb([248, 248, 248]))
            .save(backgrounds.join(format!("{index}.png")))?;
    }
    let stop = AtomicBool::new(false);
    let mut progress = |_, _| {};
    project.import_with_options(
        &objects,
        &labeler_core::image_files(&objects),
        None,
        &ImportOptions::default(),
        &stop,
        &mut progress,
    )?;
    project.import_with_options(
        &backgrounds,
        &labeler_core::image_files(&backgrounds),
        None,
        &ImportOptions {
            background: true,
            ..Default::default()
        },
        &stop,
        &mut progress,
    )?;
    project.set_display_names(vec!["η".into(), "σ".into(), "υ".into()])?;
    project.save()?;
    println!("{}", project.file.display());
    Ok(())
}
