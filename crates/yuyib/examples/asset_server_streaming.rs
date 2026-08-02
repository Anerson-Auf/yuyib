//! Stable loading handles followed by bounded main-thread publication.
//!
//! This example is intentionally headless: it validates CPU handles and a fake
//! bounded GPU publication queue, so opening a native window would obscure its
//! focused contract. It needs no external asset files.

use std::{error::Error, thread};

use yuyib::assets::{
    AssetMetadata, AssetServer, AssetState, AssetUploadBudget, AssetUploadPriority,
    AssetUploadQueue, AssetUploadQueueConfig, Assets,
};
use yuyib::tasks::TaskPoolConfig;

#[derive(Default)]
struct FakeGpu {
    uploaded: Vec<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut cpu_assets = Assets::<Vec<u8>>::new();
    let mut server = AssetServer::<Vec<u8>, &'static str>::new(TaskPoolConfig::new(2, 8)?)?;
    let texture = server.try_load(
        &mut cpu_assets,
        "prototype texture",
        AssetMetadata {
            source: Some("memory://prototype-texture".to_owned()),
            importer_version: Some("example@1".to_owned()),
            cpu_bytes: Some(4),
            gpu_bytes: Some(4),
            ..AssetMetadata::default()
        },
        |progress| {
            progress.set_total_work(4);
            progress.decoding();
            progress.advance(4);
            Ok(vec![255, 0, 255, 255])
        },
    )?;
    assert_eq!(cpu_assets.state(texture), Some(AssetState::Loading));

    while cpu_assets.state(texture) == Some(AssetState::Loading) {
        server.update(&mut cpu_assets)?;
        thread::yield_now();
    }
    let pixels = cpu_assets.get(texture).expect("CPU asset became resident");

    let mut uploads =
        AssetUploadQueue::<FakeGpu, String, &'static str>::new(AssetUploadQueueConfig::new(8)?);
    let pixel_count = pixels.len();
    uploads.try_enqueue(
        AssetUploadPriority::Required,
        "prototype texture GPU upload",
        u64::try_from(pixel_count)?,
        move |gpu| {
            let label = format!("RGBA8 texture ({pixel_count} bytes)");
            gpu.uploaded.push(label.clone());
            Ok(label)
        },
    )?;

    let mut gpu = FakeGpu::default();
    let update = uploads.process(&mut gpu, AssetUploadBudget::new(1024, 2)?);
    assert_eq!(update.results.len(), 1);
    assert_eq!(update.remaining_jobs, 0);
    println!(
        "resident CPU handle and uploaded GPU resource: {:?}",
        gpu.uploaded
    );
    Ok(())
}
