# Audio: загрузка и воспроизведение

> **Статус:** Experimental  
> **Crate / module:** `yuyib::audio` (`yuyib-audio`)  
> **Backend:** rodio 0.22.2 / системное output device

Audio API воспроизводит звук через реальное default output device. Глобального
audio singleton нет: приложение создаёт и хранит собственный `AudioEngine`,
который владеет одним output stream и mixer.

## Быстрый пример

Сначала загрузите bounded encoded data, затем явно откройте output device:

```rust,no_run
use yuyib::audio::{AudioClip, AudioEngine, AudioLoadLimits};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = AudioLoadLimits::new(2 * 1024 * 1024)?;
    let clip = AudioClip::load_file("assets/click.ogg", limits)?;
    let engine = AudioEngine::open_default()?;

    // Fire-and-forget: воспроизведение продолжается до конца clip или drop engine.
    engine.play_one_shot(&clip)?;

    // Управляемый экземпляр: handle нужно сохранить.
    let sound = engine.play_clip(&clip)?;
    sound.set_volume(0.5)?;
    sound.pause();
    sound.resume();
    sound.stop();
    Ok(())
}
```

При drop `AudioEngine` завершаются все sounds его mixer. Drop управляемого
`AudioPlaybackHandle` останавливает только этот экземпляр. `play_one_shot`
отсоединяет временный control handle, но звук всё равно зависит от engine.

## Lifecycle: source, decode и device

| Шаг | API | Что происходит |
|---|---|---|
| Прочитать bounded source | `AudioClip::load_file` / `from_bytes` | сохраняются immutable encoded bytes |
| Декодировать без device | `AudioClip::decode` | создаётся `DecodedAudio` |
| Открыть output | `AudioEngine::open_default` | выбирается системное устройство |
| Подключить к mixer | `AudioEngine::play` | начинается playback decoded source |
| Короткий common path | `AudioEngine::play_clip` | decode + play |

Такое разделение позволяет проверять source validation и decoder в headless
environment. Открытие output device остаётся отдельной fallible operation.
Backend может подобрать другую поддерживаемую output configuration того же
default device.

## Поддерживаемые форматы

- WAV/PCM;
- MP3;
- Ogg Vorbis;
- FLAC.

MP4/AAC, recording, noise/dither и generic all-codecs bundle не включены.
Формат определяется по содержимому, а не extension. Повреждённый файл или
disabled codec возвращает `AudioDecodeError::Backend`.

## Resource limits и untrusted input

`AudioLoadLimits::new(max_encoded_bytes)` отклоняет ноль. `from_bytes`
проверяет длину до сохранения input. `load_file` сначала проверяет metadata,
затем читает stream с hard cap `max + 1`: файл, выросший после metadata check,
не обойдёт budget.

Лимит относится только к **encoded bytes**. Он не ограничивает decoded
duration, sample count, decoder CPU time, playback queue time или внутреннюю
память codec. Для пользовательского media host обязан добавить duration и
content-trust policy.

## API

| Задача | Type / method |
|---|---|
| Ограничить source bytes | `AudioLoadLimits` |
| Хранить encoded clip | `AudioClip` |
| Получить device-independent samples | `DecodedAudio` |
| Владеть output stream/mixer | `AudioEngine` |
| Pause/resume/stop/volume | `AudioPlaybackHandle` |

Полные signatures и errors: [`yuyib_audio`](../api/yuyib_audio/index.html).

## Limits & Caveats

- Доступен только default output device: нет enumeration, routing и capture.
- Нет spatial audio, buses, effects, streaming asset cache и hot reload.
- Volume — finite non-negative linear value. Значение выше `1.0` разрешено,
  но может clip на output.
- Нет global master volume; control принадлежит отдельному sound handle.
- В CI/headless host физического device может не быть. `AudioOutputError` —
  нормальный явный failure; load/decode validation остаётся device-free.

## См. также

- [Assets и импорт](../concepts/assets.md)
- [Background tasks](tasks.md)
- [Limits & Compatibility](../reference/limits-and-compatibility.md)

