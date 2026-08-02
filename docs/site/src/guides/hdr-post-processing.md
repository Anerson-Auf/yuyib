# HDR post-processing: exposure и tone mapping

Yuyib умеет опционально рисовать весь кадр в linear HDR target
`Rgba16Float`, а затем переводить его на экран через exposure и tone mapping.
Это сохраняет детали ярче `1.0`, которые при прямой записи в swapchain были бы
обрезаны, и даёт предсказуемый highlight rolloff.

## High-level: одна настройка приложения

```rust,no_run
use yuyib::{
    app::Application,
    render::ColorPostProcess,
};

let post = ColorPostProcess::filmic()
    .with_exposure_ev(0.5)?;

Application::new()
    .color_post_process(post)
    // .on_render(...)
    .run()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`ColorPostProcess::filmic()` выбирает ACES filmic approximation и neutral
exposure. `+1 EV` удваивает линейную яркость, `-1 EV` уменьшает её вдвое.
Допустимый диапазон ограничен `-16..=+16 EV`; `NaN` и infinity отклоняются до
GPU. Для большинства сцен разумная отправная точка — `-1..=+1 EV`.

Доступные curves:

- `ToneMapping::AcesFilmic` — рекомендуемый cinematic preview, сохраняющий
  контраст с мягким rolloff светов;
- `ToneMapping::Reinhard` — более нейтральная и мягкая компрессия;
- `ToneMapping::LinearClamp` — диагностика: exposure применяется, значения
  выше единицы обрезаются.

Feature **opt-in**. Без `.color_post_process(...)` renderer по-прежнему пишет
непосредственно в presentation surface, не создаёт HDR texture и не добавляет
fullscreen pass.

## Low-level Renderer

```rust,no_run
use yuyib::render::{ColorPostProcess, Renderer, ToneMapping};
# fn configure(renderer: &mut Renderer) -> Result<(), Box<dyn std::error::Error>> {
let post = ColorPostProcess::new(-0.25, ToneMapping::AcesFilmic)?;
renderer.set_color_post_process(Some(post));
# Ok(())
# }
```

Устанавливайте policy **до** создания cached render pipelines. При HDR их
color target format должен быть `renderer.color_target_format()`, а не format
swapchain. Встроенные cached 2D/3D/UI renderers уже соблюдают этот контракт;
frame-local constructors получают тот же format через `RenderFrame`.

Изменение policy во время работы безопасно для renderer-owned resources, но
переключение HDR on/off меняет target format. Приложение, которое хранит
собственные raw WGPU pipelines, должно пересоздать их. Изменение только exposure
или curve не требует pipeline rebuild.

## Что это улучшает и чего пока не делает

Tone mapping исправляет clipped highlights и даёт единый photographic control,
но сам по себе не превращает direct-light сцену в Sketchfab post-render.
Для сопоставимого результата ещё нужны отдельные этапы:

1. image-based lighting (irradiance + prefiltered specular environment) и BRDF LUT;
2. shadow maps;
3. SSAO/GTAO и contact shadows;
4. bloom для emissive surfaces;
5. temporal/FXAA anti-aliasing и color grading LUT;
6. корректная transparent composition поверх HDR сцены.

Текущий pass не имитирует эти эффекты дополнительной лампой. Это фундамент,
через который следующие passes смогут работать с настоящими HDR значениями.

## Производительность и память

При включении создаётся один full-resolution `Rgba16Float` target: 8 bytes на
pixel, то есть примерно 15.8 MiB при 1920×1080 и 63.3 MiB при 3840×2160, плюс
один fullscreen resolve pass. Bloom и history buffers пока не выделяются.
Target автоматически пересоздаётся после resize.
