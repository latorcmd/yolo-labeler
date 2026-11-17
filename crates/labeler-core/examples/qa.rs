//! Repeatable local QA. Does not change source images or labels.
use anyhow::{Context, Result};
use labeler_core::*;
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Instant};
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("prepare") => {
            let root = PathBuf::from(args.get(1).context("source folder")?);
            let file = PathBuf::from(args.get(2).context("project.ylp2")?);
            let classes = discover_classes(&root, None)?;
            let mut p = Project::new(file, classes)?;
            p.save()?;
            let start = Instant::now();
            let paths = image_files(&root);
            p.import(&root, &paths, None, &AtomicBool::new(false), &mut |_, _| {})?;
            let import_ms = start.elapsed().as_millis();
            let start = Instant::now();
            p.analyze(&AtomicBool::new(false), &mut |_, _| {})?;
            let quality_ms = start.elapsed().as_millis();
            p.save()?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"images":p.samples.len(),"import_ms":import_ms,"quality_ms":quality_ms,"image_errors":p.samples.iter().filter(|s|s.image_error.is_some()).count(),"label_errors":p.samples.iter().filter(|s|!s.label_errors.is_empty()).count(),"project":p.file})
                )?
            );
        }
        Some("metrics") => {
            let folder = PathBuf::from(args.get(1).context("fixture folder")?);
            let output = PathBuf::from(args.get(2).context("output.json")?);
            let mut values = vec![];
            for path in image_files(&folder) {
                let im = image::open(&path)?;
                let detection = labeler_vision::detect(&im, &DetectorConfig::default())?;
                let quality = labeler_vision::quality(&path, &detection.boxes, 45.)?;
                values.push(serde_json::json!({"file":path.file_name().map(|s|s.to_string_lossy()),"detection":detection,"quality":quality}));
            }
            std::fs::write(output, serde_json::to_vec_pretty(&values)?)?;
        }
        _ => anyhow::bail!("qa prepare SOURCE PROJECT.ylp2 | qa metrics FIXTURES OUTPUT.json"),
    }
    Ok(())
}
