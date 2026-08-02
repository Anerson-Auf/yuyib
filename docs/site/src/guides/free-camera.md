# 3D: готовая свободная камера

> **Статус:** Experimental  
> **Модули:** `yuyib::input`, `yuyib::app`, `yuyib::platform`, `yuyib::render_3d`  
> **Платформа:** Windows

`FreeCameraController3d` — короткий готовый путь для проверки 3D-карты и
первых игровых сцен. Он даёт `WASD`, подъём/спуск, ускорение, относительный
поворот мышью и `Camera3d` для рендерера. Это свободный полёт: столкновения,
ступени и ходьба по поверхности принадлежат будущему игровому контроллеру, а
не камере просмотра.

По умолчанию при активном окне курсор скрыт и удерживается в нём. На Windows
сначала запрашивается настоящий lock, затем безопасный fallback — удержание в
границах окна. При потере фокуса клавиши сбрасываются, а курсор возвращается
в обычный режим. Направления обеих осей задаются явно через `invert_x` и
`invert_y`; по умолчанию они не инвертированы.

## Быстрый путь

```rust,no_run
use std::{cell::RefCell, rc::Rc};
use yuyib::{
    app::{Application, RenderLoop},
    input::{FreeCameraConfig3d, FreeCameraController3d},
};

let camera = Rc::new(RefCell::new(FreeCameraController3d::looking_at(
    FreeCameraConfig3d::default(),
    [0.0, 2.0, 5.0],
    [0.0, 1.0, 0.0],
)?));
let initial_cursor = camera.borrow().initial_cursor_control();

let window_camera = Rc::clone(&camera);
let device_camera = Rc::clone(&camera);
let frame_camera = Rc::clone(&camera);
Application::new()
    .render_loop(RenderLoop::Continuous)
    .cursor_control(initial_cursor)
    .on_window_event(move |event, context| {
        let result = window_camera.borrow_mut().handle_window_event(event);
        if let Some(cursor) = result.cursor_control {
            context.set_cursor_control(cursor);
        }
        if result.exit_requested {
            context.request_exit();
        }
    })
    .on_device_event(move |event, _context| {
        device_camera.borrow_mut().handle_device_event(event);
    })
    .on_frame(move |frame| {
        frame_camera
            .borrow_mut()
            .step(frame.frame().delta.as_secs_f32())
            .expect("validated camera input");
    });
# Ok::<(), Box<dyn std::error::Error>>(())
```

В `on_render` передайте `camera.borrow().camera()` в рендерер. `Esc` по
умолчанию вызывает `request_exit`; задайте `bindings.exit: None`, если выход
должен решаться вашим меню.

Если камера появляется не при создании окна, а после фоновой загрузки,
сохраните курсор свободным в `Application::cursor_control` и в кадре
публикации вызовите `frame.set_cursor_control(camera.initial_cursor_control())`.
Так загрузочный экран остаётся обычным окном, а игровой режим включает захват
без ожидания следующего события Winit.

## Настройка

Все обычные параметры находятся в одной структуре — без скрытой глобальной
настройки:

```rust
use yuyib::input::{FreeCameraBindings3d, FreeCameraConfig3d};
use yuyib::platform::winit::keyboard::KeyCode;

let config = FreeCameraConfig3d {
    move_speed: 4.0,
    sprint_multiplier: 2.0,
    mouse_sensitivity: 0.0015,
    invert_x: false,
    lock_cursor: true,
    bindings: FreeCameraBindings3d {
        down: KeyCode::KeyC,
        ..FreeCameraBindings3d::default()
    },
    ..FreeCameraConfig3d::default()
};
```

`max_delta_seconds` защищает от рывка после отладки, сворачивания или долгой
загрузки кадра. Большое время кадра не увеличивает один шаг движения больше
указанного предела.

## Низкоуровневое вмешательство

Контроллер не заставляет использовать Winit. Собственная система действий,
сеть или тест могут напрямую вызывать `set_action`, `add_mouse_delta` и
`step`. Это та же математика и те же ограничения времени кадра, что у готового
пути:

```rust
use yuyib::input::{FreeCameraAction3d, FreeCameraConfig3d, FreeCameraController3d};

let mut camera = FreeCameraController3d::new(FreeCameraConfig3d::default())?;
camera.set_action(FreeCameraAction3d::Forward, true);
camera.add_mouse_delta(-12.0, 3.0);
camera.step(1.0 / 60.0)?;
let renderer_camera = camera.camera();
# let _ = renderer_camera;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Если приложению нужен свой cursor policy, не передавайте
`initial_cursor_control` в `Application` и игнорируйте `cursor_control` из
`FreeCameraEvent3d`. В этом случае камера продолжит обрабатывать движение, но
окно останется полностью под контролем приложения.

## Limits & Caveats

- Поворот использует `DeviceEvent::MouseMotion`, а не абсолютную координату:
  это необходимо для устойчивого управления у края удерживаемого окна.
- Есть только клавиатура и мышь. Gamepad, переназначение во время работы,
  сохранение раскладки и чувствительности ещё не добавлены.
- Свободная камера не является `CharacterMotor3d`: у неё нет пола,
  коллизий, прыжка и гравитации.

Полные типы: [input API](../api/yuyib_input/index.html),
[application API](../api/yuyib_app/index.html) и
[platform API](../api/yuyib_platform/index.html).
